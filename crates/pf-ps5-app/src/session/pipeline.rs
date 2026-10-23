//! Assembling one session's media pipeline: the decoder the platform opens for the negotiated
//! stream, the audio port, and the threads that feed them. The pumps and stages above are
//! written against `core::media`'s traits and never learn which lane they got.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use punktfunk_core::client::NativeClient;
use punktfunk_core::session::Frame;
use punktfunk_core::PunktfunkError;

use crate::core::media::{AudioSink, VideoSink};
use crate::platform::{AudioOut, StreamSpec, VideoDecoders};
use crate::services::join::{join_with_timeout, SHUTDOWN_JOIN_TIMEOUT};
use crate::session::audio::AudioStage;
use crate::session::audio_out::AudioOutSink;
use crate::session::connect::ConnectParams;
use crate::session::pump::{spawn_audio_feed, video_pump};
use crate::session::stage::{SinkConfig, VideoStage};
use crate::session::StreamStats;

/// The platform's media halves a session builds on.
#[derive(Clone)]
pub struct Media {
    pub video: Arc<dyn VideoDecoders>,
    pub audio: Arc<dyn AudioOut>,
}

/// The threads driving one session. Dropping this does NOT stop them — the session's `stop`
/// flag does, and [`Self::join`] waits them out.
pub struct MediaPipeline {
    video_thread: std::thread::JoinHandle<()>,
    audio_thread: std::thread::JoinHandle<()>,
}

impl MediaPipeline {
    /// Open the decoder and the audio port, and start both pumps. A thread already started is
    /// stopped and joined before an error returns.
    pub fn build(
        params: &ConnectParams,
        media: &Media,
        client: &Arc<NativeClient>,
        stop: &Arc<AtomicBool>,
        stats: &Arc<StreamStats>,
    ) -> Result<Self> {
        let mut opener = Opener {
            decoders: media.video.clone(),
            excluded: 0,
        };
        let (sink, first) = opener.open(client, stop)?;
        let port = media
            .audio
            .open(client.audio_channels, crate::session::audio::SAMPLE_RATE)
            .context("open the audio port")?;
        let audio_sink: Arc<dyn AudioSink> = Arc::new(AudioOutSink::start(port, client.audio_channels)?);
        let stage = AudioStage::new(audio_sink, client.audio_channels, client.audio_layout)?;
        let video_thread = spawn_video_thread(client, sink, first, opener, stop, stats, params.present_priority)?;
        let audio_thread = match spawn_audio_feed(client.clone(), stage, stop.clone()) {
            Ok(handle) => handle,
            Err(e) => {
                stop.store(true, Ordering::Relaxed);
                join_thread(video_thread, "video");
                return Err(e);
            }
        };
        Ok(Self {
            video_thread,
            audio_thread,
        })
    }

    /// Waits the threads out, bounded. `false` means one is still running.
    pub fn join(self) -> bool {
        join_thread(self.video_thread, "video") & join_thread(self.audio_thread, "audio")
    }
}

fn join_thread(handle: std::thread::JoinHandle<()>, what: &'static str) -> bool {
    join_with_timeout(handle, SHUTDOWN_JOIN_TIMEOUT, what, || ())
}

/// How long a codec ask waits for the first frame in the host's next codec.
const CODEC_ASK_WAIT: Duration = Duration::from_secs(3);
/// Longest one `next_frame` call parks while a codec ask waits.
const ASK_POLL: Duration = Duration::from_millis(100);

/// Opens the session's decoders: at the start, and on every format the host moves the stream to.
pub(super) struct Opener {
    decoders: Arc<dyn VideoDecoders>,
    /// `CODEC_*` bits no decoder here opened, so an ask never sends the host back to one.
    excluded: u8,
}

impl Opener {
    /// A decoder for the stream's live format. A refused open asks the host to stream on without
    /// that codec and opens the one it moves to; `Some` is then that codec's first frame.
    pub(super) fn open(
        &mut self,
        client: &NativeClient,
        stop: &AtomicBool,
    ) -> Result<(Box<dyn VideoSink>, Option<Frame>)> {
        let mut first = None;
        loop {
            let spec = spec_of(client);
            let refused = match self.decoders.open(&spec) {
                Ok(sink) => {
                    tracing::info!(
                        "{} opened (codec 0x{:02x} {}x{}@{}fps, {}-bit, colour {:?})",
                        sink.name(),
                        spec.codec,
                        spec.width,
                        spec.height,
                        spec.refresh_hz,
                        spec.bit_depth,
                        spec.color,
                    );
                    return Ok((sink, first));
                }
                Err(e) => e.context("open the video decoder"),
            };
            self.excluded |= spec.codec;
            tracing::warn!("{refused:#} — asking the host for another codec");
            first = Some(ask_other_codec(client, self.excluded, stop).ok_or(refused)?);
        }
    }
}

/// The decoder's view of the stream's live format.
fn spec_of(client: &NativeClient) -> StreamSpec {
    let (mode, format) = (client.mode(), client.video_format());
    StreamSpec {
        codec: format.codec,
        width: mode.width,
        height: mode.height,
        refresh_hz: mode.refresh_hz,
        color: format.color,
        bit_depth: format.bit_depth,
        chroma444: format.chroma_format == punktfunk_core::quic::CHROMA_IDC_444,
        shard_payload: client.shard_payload as usize,
        hud: client.hud_shared(),
    }
}

/// Asks the host to drop `excluded` and waits for the first frame of the epoch it moves to.
/// `None` when the session has no format epochs, the host refused, or [`CODEC_ASK_WAIT`] passed.
fn ask_other_codec(client: &NativeClient, excluded: u8, stop: &AtomicBool) -> Option<Frame> {
    client.request_codec_change(excluded).ok()?;
    let asked = Instant::now();
    while !stop.load(Ordering::Relaxed) && client.codec_refused() == 0 && asked.elapsed() < CODEC_ASK_WAIT {
        match client.next_frame(ASK_POLL) {
            Ok(frame) if frame.new_format.is_some() => return Some(frame),
            // The refused codec's frames: nothing here decodes them.
            Ok(_) | Err(PunktfunkError::NoFrame) => {}
            Err(_) => return None,
        }
    }
    None
}

fn spawn_video_thread(
    client: &Arc<NativeClient>,
    sink: Box<dyn VideoSink>,
    first: Option<Frame>,
    opener: Opener,
    stop: &Arc<AtomicBool>,
    stats: &Arc<StreamStats>,
    present_priority: pf_client_core::trust::PresentPriority,
) -> Result<std::thread::JoinHandle<()>> {
    let cfg = SinkConfig {
        stream_hz: client.mode().refresh_hz,
        report_decode_latency: client.wants_decode_latency(),
        present_priority,
    };
    // VideoOut takes no mastering metadata, so the host's has nowhere to go.
    let is_hdr = false;
    let (client, stop, stats) = (client.clone(), stop.clone(), stats.clone());
    std::thread::Builder::new()
        .name("pf-video-pump".into())
        .spawn(move || {
            let stage = VideoStage::new(sink, stats.clone(), &cfg);
            video_pump(client, stage, first, opener, stop, stats, is_hdr);
        })
        .context("spawn video thread")
}
