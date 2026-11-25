//! The pads' voice coils. The host sends a pad's haptics as 5 ms Opus frames; core decodes them
//! and conceals what the wire lost, and this plays the coils' pair on the pad's own port. While
//! frames arrive the coils are the haptics'; [`COILS`] tells the input thread, which then holds
//! the motors' rumble back.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use punktfunk_core::audio::pad_mix::{
    is_haptics_evidence, HapticsLiveness, PadDecode, QuadMixer, MAX_FRAME_SAMPLES, PAD_CHANNELS,
};
use punktfunk_core::client::NativeClient;

use crate::platform::{AudioOut, AudioPort};

/// Which pads' haptics frames are arriving right now, by wire pad.
pub static COILS: HapticsLiveness = HapticsLiveness::new();

/// The mixer's bound: 100 ms at 48 kHz. Only a port that is gone lets it fill.
const MIXER_FRAMES: usize = 4800;
/// Grains kept for a port that fell behind. Haptics felt late are worse than haptics skipped.
const BACKLOG_GRAINS: usize = 3;
/// How long a port that would not open is left alone.
const RETRY: Duration = Duration::from_secs(4);
/// Longest wait for a frame, so the stop flag is seen.
const WAIT: Duration = Duration::from_millis(10);

/// Starts the haptics thread. It ends with the session, or when `stop` is set.
pub fn spawn(
    client: Arc<NativeClient>,
    audio: Arc<dyn AudioOut>,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("pf-haptics".into())
        .spawn(move || run(&client, audio.as_ref(), &stop))
        .context("spawn haptics thread")
}

fn run(client: &NativeClient, audio: &dyn AudioOut, stop: &AtomicBool) {
    // One pad's coils play: the first pad whose frames arrive.
    let mut stage = PadDecode::new(true, false);
    let mut mixer = QuadMixer::<f32>::new(MIXER_FRAMES);
    let mut pcm = vec![0f32; MAX_FRAME_SAMPLES * 2];
    let mut quad = Vec::new();
    let mut coils = Coils::default();
    let mut pad: Option<u8> = None;
    let mut port: Option<Box<dyn AudioPort>> = None;
    let mut retry_at = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let Some(frame) = client.next_pad_audio(WAIT) else {
            if client.is_session_ended() {
                break;
            }
            continue;
        };
        if !stage.wants(frame.kind) || *pad.get_or_insert(frame.pad) != frame.pad {
            continue;
        }
        if is_haptics_evidence(&frame, port.is_some()) {
            COILS.note(frame.pad);
        }
        stage.decode_frame(&frame, &mut pcm, &mut mixer);
        if port.is_none() && Instant::now() >= retry_at {
            match audio.open_haptics(frame.pad) {
                Ok(opened) => port = Some(opened),
                Err(e) => {
                    tracing::warn!("haptics: no port on pad {}: {e:#}", frame.pad);
                    retry_at = Instant::now() + RETRY;
                }
            }
        }
        quad.clear();
        mixer.pop(&mut quad, Instant::now());
        let Some(open) = port.as_mut() else {
            continue;
        };
        coils.push(&quad, open.grain_frames());
        while let Some(grain) = coils.grain(open.grain_frames()) {
            if let Err(e) = open.write(grain) {
                tracing::warn!("haptics: port on pad {} ended: {e:#}", frame.pad);
                port = None;
                retry_at = Instant::now() + RETRY;
                break;
            }
        }
    }
    if let Some(pad) = pad {
        COILS.clear(pad);
    }
}

/// The coils' stereo samples between the mixer and the port's grains.
#[derive(Default)]
struct Coils {
    samples: VecDeque<f32>,
    grain: Vec<f32>,
}

impl Coils {
    /// Takes the coils' pair out of the pad's four-channel frames, dropping the oldest grains
    /// past [`BACKLOG_GRAINS`].
    fn push(&mut self, quad: &[f32], grain_frames: usize) {
        for frame in quad.chunks_exact(PAD_CHANNELS) {
            self.samples.extend([frame[2], frame[3]]);
        }
        let keep = grain_frames * 2 * BACKLOG_GRAINS;
        let over = self.samples.len().saturating_sub(keep);
        self.samples.drain(..over - over % 2);
    }

    /// The next whole grain, if the samples for one are in.
    fn grain(&mut self, grain_frames: usize) -> Option<&[f32]> {
        let want = grain_frames * 2;
        if self.samples.len() < want {
            return None;
        }
        self.grain.clear();
        self.grain.extend(self.samples.drain(..want));
        Some(&self.grain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `frames` four-channel frames whose coil pair counts up from `first`.
    fn quad(first: usize, frames: usize) -> Vec<f32> {
        (first..first + frames)
            .flat_map(|n| [9.0, 9.0, n as f32, -(n as f32)])
            .collect()
    }

    #[test]
    fn the_coils_pair_comes_out_in_whole_grains() {
        let mut coils = Coils::default();
        coils.push(&quad(0, 3), 4);
        assert_eq!(coils.grain(4), None, "three frames are no grain of four");
        coils.push(&quad(3, 2), 4);
        assert_eq!(coils.grain(4), Some(&[0.0, -0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0][..]));
        assert_eq!(coils.grain(4), None);
    }

    #[test]
    fn a_port_that_fell_behind_plays_the_newest() {
        let mut coils = Coils::default();
        coils.push(&quad(0, 4 * (BACKLOG_GRAINS + 2)), 4);
        let first = coils.grain(4).unwrap()[0];
        assert_eq!(first, 8.0, "the two oldest grains are gone");
    }
}
