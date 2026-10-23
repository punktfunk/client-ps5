//! The console's audio route: decoded PCM into a ring, a writer thread draining it into the
//! platform's [`AudioPort`] one grain at a time.
//!
//! The port blocks for a grain and nothing rate-matches it to the host, so depth is steered by
//! punktfunk-core's shared [`JitterPolicy`], the same one every client's ring runs.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use punktfunk_core::audio::{crossfade_drop, crossfade_insert, JitterPolicy, JitterTuning};

use crate::core::media::{AudioFormat, AudioSink, Samples};
use crate::platform::AudioPort;

/// No base target: the policy's floor (one 5.33 ms `AudioOut` grain plus a packet) is the depth,
/// and only underruns grow it. The ceilings and the de-prime stay WASAPI's, so a Wi-Fi link still
/// has room to grow; a clean link never reaches them.
const TUNING: JitterTuning = JitterTuning {
    base_target_ms: 0,
    headroom_ms: 15,
    ..JitterTuning::WASAPI
};

fn playout(channels: u8) -> punktfunk_core::audio::AudioPlayout {
    punktfunk_core::audio::AudioPlayout {
        tuning: TUNING,
        format: punktfunk_core::audio::plane::PlaneFormat::from_welcome(
            punktfunk_core::quic::AUDIO_CODEC_OPUS,
            crate::session::audio::SAMPLE_RATE,
            16,
            channels,
            0,
            0,
        ),
    }
}

struct Ring {
    samples: VecDeque<f32>,
    policy: JitterPolicy,
    depth_ms: u32,
}

struct Shared {
    ring: Mutex<Ring>,
    fed: Condvar,
    stop: AtomicBool,
    underruns: AtomicU64,
}

/// An [`AudioSink`] over one open port, plus the thread that plays it.
pub struct AudioOutSink {
    shared: Arc<Shared>,
    channels: u8,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl AudioOutSink {
    pub fn start(port: Box<dyn AudioPort>, channels: u8) -> Result<Self> {
        let rate = crate::session::audio::SAMPLE_RATE;
        let shared = Arc::new(Shared {
            ring: Mutex::new(Ring {
                samples: VecDeque::with_capacity(rate as usize * usize::from(channels) / 5),
                policy: playout(channels).jitter(),
                depth_ms: 0,
            }),
            fed: Condvar::new(),
            stop: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
        });
        let thread_shared = shared.clone();
        let writer = std::thread::Builder::new()
            .name("pf-audio-out".into())
            .spawn(move || write_loop(&thread_shared, port, usize::from(channels)))
            .context("spawn audio writer")?;
        Ok(Self {
            shared,
            channels,
            writer: Some(writer),
        })
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }
}

impl Drop for AudioOutSink {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.fed.notify_all();
        if let Some(writer) = self.writer.take() {
            crate::services::join::join_with_timeout(
                writer,
                crate::services::join::SHUTDOWN_JOIN_TIMEOUT,
                "audio-out",
                || (),
            );
        }
    }
}

impl AudioSink for AudioOutSink {
    fn name(&self) -> &'static str {
        "AudioOut"
    }

    fn format(&self) -> AudioFormat {
        AudioFormat::PcmF32 {
            channels: self.channels,
            sample_rate: crate::session::audio::SAMPLE_RATE,
        }
    }

    fn feed(&self, samples: Samples<'_>, _host_pts_ns: u64) -> Result<()> {
        let Samples::F32(pcm) = samples;
        let mut ring = self.shared.ring.lock().expect("audio ring poisoned");
        ring.samples.extend(pcm.iter().copied());
        drop(ring);
        self.shared.fed.notify_one();
        Ok(())
    }

    fn depth_ms(&self) -> Option<i64> {
        Some(i64::from(
            self.shared.ring.lock().expect("audio ring poisoned").depth_ms,
        ))
    }

    fn underruns(&self) -> Option<u64> {
        Some(self.shared.underruns.load(Ordering::Relaxed))
    }
}

/// Plays grains until stopped. A grain the ring cannot fill plays the policy's silence or the
/// short read padded with zeros, so the port's clock never stalls.
fn write_loop(shared: &Shared, mut port: Box<dyn AudioPort>, channels: usize) {
    let want = port.grain_frames() * channels;
    let mut grain = vec![0f32; want];
    while !shared.stop.load(Ordering::Relaxed) {
        {
            let mut ring = shared.ring.lock().expect("audio ring poisoned");
            if ring.samples.is_empty() && !ring.policy.is_primed() {
                // Nothing to play yet: wait for the decoder rather than spinning on silence.
                ring = shared
                    .fed
                    .wait_timeout(ring, Duration::from_millis(20))
                    .expect("audio ring poisoned")
                    .0;
            }
            fill(&mut ring, &mut grain, &shared.underruns);
        }
        if let Err(e) = port.write(&grain) {
            tracing::warn!("audio out: {e:#}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// One policy step and one read into `grain`.
fn fill(ring: &mut Ring, grain: &mut [f32], underruns: &AtomicU64) {
    let want = grain.len();
    let step = ring.policy.step(ring.samples.len(), want);
    if step.drop_front > 0 {
        crossfade_drop(&mut ring.samples, step.drop_front, step.crossfade);
    }
    if step.insert_front > 0 {
        crossfade_insert(&mut ring.samples, step.insert_front, step.crossfade);
    }
    if step.silence {
        grain.fill(0.0);
        ring.policy.note_read(false);
    } else {
        let take = want.min(ring.samples.len());
        for (dst, src) in grain.iter_mut().zip(ring.samples.drain(..take)) {
            *dst = src;
        }
        grain[take..].fill(0.0);
        let short = take < want;
        if short {
            underruns.fetch_add(1, Ordering::Relaxed);
        }
        ring.policy.note_read(short);
    }
    ring.depth_ms = ring.policy.depth_ms(ring.samples.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::FakeAudio;
    use crate::platform::AudioOut;

    /// Fed audio comes out of the port in whole grains, primed before the first non-silent one.
    #[test]
    fn fed_audio_reaches_the_port_in_grains() {
        let audio = FakeAudio::default();
        let sink = AudioOutSink::start(audio.open(2, 48_000).unwrap(), 2).unwrap();
        let tone = vec![0.5f32; 240 * 2];
        for _ in 0..40 {
            sink.feed(Samples::F32(&tone), 0).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if audio.grains.lock().unwrap().iter().any(|g| g.contains(&0.5)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(sink);
        let grains = audio.grains.lock().unwrap();
        assert!(grains.iter().all(|g| g.len() == 512));
        assert!(
            grains.iter().any(|g| g.contains(&0.5)),
            "no fed sample reached the port"
        );
    }
}
