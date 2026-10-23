//! The media pipeline's vocabulary: what a decode backend must offer, and what a stage above it
//! is allowed to assume.
//!
//! Two lanes decode video here (`PyroWave` on the GPU, Videodec2 for H.264/HEVC) and the
//! pipeline in `session` is written against these traits rather than against either. The
//! platform crates implement them and `session` consumes them.
//!
//! **The traits describe capability, not policy.** A sink says what it can take
//! ([`VideoSinkCaps`]) and does it; anchoring, freeze-until-reanchor, backlog metering and
//! concealment are the stages' business, above this seam and identical on every backend.
use anyhow::Result;
use punktfunk_core::quic;

/// Feed refused: the decoder is still opening. Distinct from a decode error.
#[derive(Debug)]
pub struct NotReady;

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decode pipeline not loaded yet — holding")
    }
}

impl std::error::Error for NotReady {}

/// Video backend capabilities. Every `false` disables a stage behaviour; avoids per-backend
/// matches scattered across the pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSinkCaps {
    /// Feed carries a presentation timestamp.
    pub pts: bool,
    /// Slice-progressive delivery. Requires repeatable PTS, implies [`Self::pts`].
    pub partial_au: bool,
    /// Decoder has a droppable render queue.
    pub flush: bool,
    /// Every frame decodes on its own (PyroWave): loss never needs a re-anchor.
    pub intra_only: bool,
}

impl VideoSinkCaps {
    /// Narrowest backend: bare feed.
    pub const FEED_ONLY: Self = Self {
        pts: false,
        partial_au: false,
        flush: false,
        intra_only: false,
    };
}

/// Per-delivery facts a decoder may need beside the bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedFlags {
    /// The AU's chunks start on shard boundaries (`USER_FLAG_CHUNK_ALIGNED`).
    pub chunk_aligned: bool,
    /// Every shard of the AU arrived.
    pub complete: bool,
    /// The host's capture PTS, unmapped: the overlay's per-picture key.
    pub host_pts_ns: u64,
}

/// Monotonic clock for presentation pacing. A decoder's is unrelated to host capture or wall-clock;
/// mapping is `session::timeline`'s job.
pub trait MediaClock: Send + Sync {
    /// Nanoseconds since the decoder loaded.
    fn now_ns(&self) -> u64;
}

/// One loaded video decoder.
pub trait VideoSink: Send {
    fn name(&self) -> &'static str;

    fn caps(&self) -> VideoSinkCaps;

    /// Feed one AU (or one piece). PTS is in this sink's clock; ignored if caps deny timestamps.
    fn feed(&self, au: &[u8], pts_ns: u64, flags: FeedFlags) -> Result<()>;

    /// Drop the render queue. No-op where [`VideoSinkCaps::flush`] is false.
    fn flush(&self) -> Result<()> {
        Ok(())
    }

    /// Queued frames, or `None` if backend can't report. `None` ≠ empty; stages treat differently.
    fn queue_depth(&self) -> Option<u32> {
        None
    }

    /// Negotiated colorimetry, plus HDR mastering metadata where the session applies it.
    fn set_color(&self, _meta: Option<&quic::HdrMeta>, _color: quic::ColorInfo) -> Result<()> {
        Ok(())
    }

    fn clock(&self) -> Option<&dyn MediaClock> {
        None
    }

    /// How long the decoder itself took over the last fed picture, where the sink can tell it
    /// from the rest of [`feed`](Self::feed) (copies, waits on the presenter). `None` = the
    /// whole feed is the decode.
    fn decode_us(&self) -> Option<u32> {
        None
    }

    /// Decoder failed unrecoverably (no re-anchor works). Ends session if true.
    fn is_dead(&self) -> bool {
        false
    }
}

/// Audio format a sink declares. Stage produces exactly this; no conversion after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    /// Interleaved f32 in punktfunk's channel order (libopus output). No conversion needed.
    PcmF32 { channels: u8, sample_rate: u32 },
}

impl AudioFormat {
    /// Channels to speaker. Never folded (route is pre-negotiated to match). Mismatch is a bug.
    pub fn channels(self) -> u8 {
        match self {
            Self::PcmF32 { channels, .. } => channels,
        }
    }
}

/// One packet on its way to a sink, in whatever shape that sink declared.
pub enum Samples<'a> {
    F32(&'a [f32]),
}

/// Somewhere a session's audio can go: the console's `AudioOut` port behind `session::audio_out`,
/// or a test's recorder.
pub trait AudioSink: Send + Sync {
    fn name(&self) -> &'static str;

    fn format(&self) -> AudioFormat;

    /// Feed packet stamped in host capture clock. Timeline sinks map it; others ignore it.
    fn feed(&self, samples: Samples<'_>, host_pts_ns: u64) -> Result<()>;

    /// Queue depth in ms, or `None` if sink can't report.
    fn depth_ms(&self) -> Option<i64> {
        None
    }

    /// Number of output grains that had to be padded because the queue ran short.
    fn underruns(&self) -> Option<u64> {
        None
    }
}
