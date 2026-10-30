//! The audio stage: transport packets → decoded PCM in the session's [`AudioSink`].
//!
//! libopus decodes here, with concealment written once into a reused buffer. **Nothing is
//! mixed down**: a layout the output cannot play is never requested (`core::caps`), so a width
//! mismatch is a bug and is reported as one.
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Result};
use punktfunk_core::audio::{layout_for, AudioGapTracker, AudioLayout, DroughtConceal};

use crate::core::media::{AudioSink, Samples};

/// 48 kHz, 5 ms frames — punktfunk's fixed audio framing.
pub const SAMPLE_RATE: u32 = 48_000;
const SAMPLES_PER_FRAME: usize = 240;
/// One packet, in ms, as the concealment arithmetic needs it.
const FRAME_MS: i64 = 5;
/// Max channels punktfunk ever negotiates (7.1) — sizes the scratch decode buffer.
const MAX_CHANNELS: usize = 8;

/// Decodes one session's audio into its sink.
pub struct AudioStage {
    sink: Arc<dyn AudioSink>,
    decoder: opus::MSDecoder,
    /// Negotiated channel count — the decode width.
    channels: usize,
    /// Detects packets lost on the wire so they are concealed rather than skipped.
    gaps: AudioGapTracker,
    /// Conceals a wire gone quiet, which `gaps` only sees once a later packet arrives.
    drought: DroughtConceal,
    last_packet: Instant,
    /// Concealment frames then the packet, filled only when there was concealment.
    f32: Vec<f32>,
    /// Samples in `pcm` from the last decode.
    last: usize,
    /// libopus's output, one frame at the widest layout. A field so it is not zeroed per packet.
    pcm: Box<[f32; SAMPLES_PER_FRAME * MAX_CHANNELS]>,
}

impl AudioStage {
    /// `channels` and `layout` are host-resolved: the decoder is built from what the handshake
    /// settled on, never from what was requested.
    pub fn new(sink: Arc<dyn AudioSink>, channels: u8, layout: u8) -> Result<Self> {
        let format = sink.format();
        if format.channels() != channels {
            bail!(
                "{} takes {} channel(s), the session negotiated {channels}",
                sink.name(),
                format.channels(),
            );
        }
        let Some(layout) = AudioLayout::from_wire(layout) else {
            bail!("the host encodes audio layout {layout}, which this client does not know");
        };
        let wire = layout_for(channels, layout);
        let decoder = opus::MSDecoder::new(SAMPLE_RATE, wire.streams, wire.coupled, wire.mapping)
            .map_err(|e| anyhow::anyhow!("opus MSDecoder::new: {e}"))?;
        Ok(Self {
            sink,
            decoder,
            channels: wire.channels as usize,
            gaps: AudioGapTracker::new(),
            drought: crate::session::audio_out::playout(channels).drought(),
            last_packet: Instant::now(),
            f32: Vec::with_capacity(SAMPLES_PER_FRAME * MAX_CHANNELS * 2),
            last: 0,
            pcm: Box::new([0.0; SAMPLES_PER_FRAME * MAX_CHANNELS]),
        })
    }

    pub fn sink_name(&self) -> &'static str {
        self.sink.name()
    }

    /// One packet, concealment included, into the sink. Concealment fills the hole BEFORE the
    /// packet, so it is stamped that many milliseconds earlier than the packet. Frames
    /// [`Self::quiet`] already played are not concealed twice.
    pub fn play(&mut self, seq: u32, pts_ns: u64, payload: &[u8]) -> Result<()> {
        self.last_packet = Instant::now();
        let Self {
            decoder,
            f32,
            last,
            pcm,
            channels,
            gaps,
            drought,
            sink,
            ..
        } = self;
        let channels = *channels;
        let missing = gaps.missing_before(seq).saturating_sub(drought.packet());
        f32.clear();
        // A concealment frame gets exactly one frame of buffer: with no packet libopus takes
        // `out.len() / channels` as the frame size and rejects an illegal one.
        let cap = |i: u32| {
            if i < missing {
                SAMPLES_PER_FRAME * channels
            } else {
                SAMPLES_PER_FRAME * MAX_CHANNELS
            }
        };
        let stamp = |i: u32| pts_ns.saturating_sub((i64::from(missing - i) * FRAME_MS) as u64 * 1_000_000);
        for i in 0..=missing {
            let input: &[u8] = if i < missing { &[] } else { payload };
            let frames = decoder
                .decode_float(input, &mut pcm[..cap(i)], false)
                .map_err(|e| anyhow::anyhow!("opus decode: {e}"))?;
            let decoded = &pcm[..frames * channels];
            *last = decoded.len();
            if missing > 0 {
                f32.extend_from_slice(decoded);
            }
        }
        if missing == 0 {
            return sink.feed(Samples::F32(&pcm[..*last]), stamp(0));
        }
        sink.feed(Samples::F32(f32), stamp(0))
    }

    /// Nothing arrived for a packet's length: one concealed frame if the ring is draining with
    /// the wire, bounded so a dead stream goes silent. Called once a packet, so it keeps pace
    /// with playout. A sink that reports no depth is never concealed for.
    pub fn quiet(&mut self) -> Result<()> {
        let depth_ms = self
            .sink
            .depth_ms()
            .map_or(u32::MAX, |ms| ms.clamp(0, i64::from(u32::MAX)) as u32);
        if self.last == 0 || !self.drought.conceal(self.last_packet.elapsed(), depth_ms) {
            return Ok(());
        }
        let width = SAMPLES_PER_FRAME * self.channels;
        let frames = self
            .decoder
            .decode_float(&[], &mut self.pcm[..width], false)
            .map_err(|e| anyhow::anyhow!("opus decode: {e}"))?;
        self.last = frames * self.channels;
        self.sink.feed(Samples::F32(&self.pcm[..self.last]), 0)
    }

    /// Milliseconds concealed for a quiet wire this session. Low underruns bought with a
    /// climbing figure here are a link in trouble.
    pub fn concealed_ms(&self) -> u64 {
        self.drought.total_ms()
    }

    /// The sink's queue depth in ms, where it knows one.
    pub fn depth_ms(&self) -> Option<i64> {
        self.sink.depth_ms()
    }

    /// Output-grain underruns, when the sink can report them.
    pub fn underruns(&self) -> Option<u64> {
        self.sink.underruns()
    }

    /// Peak sample of the last decoded frame: tells "the host sends silence" from "no sound".
    pub fn peak(&self) -> f32 {
        self.pcm[..self.last].iter().fold(0f32, |m, &s| m.max(s.abs()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::core::media::AudioFormat;

    /// Keeps every buffer it is fed, with its stamp, and reports an empty queue.
    struct Recorder(Mutex<Vec<(usize, u64)>>, u8);

    impl AudioSink for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }

        fn format(&self) -> AudioFormat {
            AudioFormat::PcmF32 {
                channels: self.1,
                sample_rate: SAMPLE_RATE,
            }
        }

        fn feed(&self, samples: Samples<'_>, host_pts_ns: u64) -> Result<()> {
            let Samples::F32(pcm) = samples;
            self.0.lock().unwrap().push((pcm.len(), host_pts_ns));
            Ok(())
        }

        fn depth_ms(&self) -> Option<i64> {
            Some(0)
        }
    }

    fn packet(encoder: &mut opus::Encoder) -> Vec<u8> {
        let frame = vec![0.25f32; SAMPLES_PER_FRAME * 2];
        let mut out = [0u8; 1500];
        let n = encoder.encode_float(&frame, &mut out).unwrap();
        out[..n].to_vec()
    }

    /// A lost packet comes out as one concealed frame ahead of the next, stamped 5 ms earlier.
    #[test]
    fn a_gap_is_concealed_before_the_next_packet() {
        let sink = Arc::new(Recorder(Mutex::new(Vec::new()), 2));
        let mut stage = AudioStage::new(sink.clone(), 2, AudioLayout::Standard.wire()).unwrap();
        let mut encoder = opus::Encoder::new(SAMPLE_RATE, opus::Channels::Stereo, opus::Application::LowDelay).unwrap();
        stage.play(0, 100_000_000, &packet(&mut encoder)).unwrap();
        stage.play(2, 110_000_000, &packet(&mut encoder)).unwrap();
        let fed = sink.0.lock().unwrap().clone();
        assert_eq!(fed[0], (SAMPLES_PER_FRAME * 2, 100_000_000));
        assert_eq!(fed[1], (SAMPLES_PER_FRAME * 2 * 2, 105_000_000));
        assert!(stage.peak() > 0.0);
    }

    /// A quiet wire is concealed once it has lasted two frames, and the packet that ends it does
    /// not conceal those frames again.
    #[test]
    fn a_drought_is_concealed_while_it_lasts_and_not_again_after() {
        let sink = Arc::new(Recorder(Mutex::new(Vec::new()), 2));
        let mut stage = AudioStage::new(sink.clone(), 2, AudioLayout::Standard.wire()).unwrap();
        let mut encoder = opus::Encoder::new(SAMPLE_RATE, opus::Channels::Stereo, opus::Application::LowDelay).unwrap();
        stage.quiet().unwrap();
        assert!(sink.0.lock().unwrap().is_empty(), "nothing to conceal from yet");
        stage.play(0, 100_000_000, &packet(&mut encoder)).unwrap();
        stage.quiet().unwrap();
        assert_eq!(sink.0.lock().unwrap().len(), 1, "one packet's silence is not a drought");
        std::thread::sleep(std::time::Duration::from_millis(12));
        stage.quiet().unwrap();
        assert_eq!(sink.0.lock().unwrap().len(), 2);
        assert_eq!(stage.concealed_ms(), 5);
        stage.play(2, 110_000_000, &packet(&mut encoder)).unwrap();
        assert_eq!(
            sink.0.lock().unwrap()[2],
            (SAMPLES_PER_FRAME * 2, 110_000_000),
            "the lost packet was the concealed frame"
        );
    }

    /// A width the sink does not play, or a layout this build does not know, is refused.
    #[test]
    fn mismatches_are_refused() {
        let stereo = Arc::new(Recorder(Mutex::new(Vec::new()), 2));
        assert!(AudioStage::new(stereo.clone(), 6, AudioLayout::Standard.wire()).is_err());
        assert!(AudioStage::new(stereo, 2, 9).is_err());
    }
}
