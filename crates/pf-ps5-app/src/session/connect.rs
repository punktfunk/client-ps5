//! Bringing up a streaming session: capability negotiation and the handshake, then handing the
//! result to `pipeline`, which builds everything that decodes it.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use punktfunk_core::client::NativeClient;
use punktfunk_core::config::{CompositorPref, Mode};
use punktfunk_core::quic;

use crate::core::caps::video_caps;
use crate::services::join::{join_with_timeout, SHUTDOWN_JOIN_TIMEOUT};
use crate::services::store::{CodecPref, GamepadType};
use crate::session::pipeline::{Media, MediaPipeline};
use crate::session::StreamStats;

#[derive(Default)]
pub struct ConnectAttempt(std::sync::Mutex<AttemptState>);

#[derive(Default)]
struct AttemptState {
    cancelled: bool,
    media_started: bool,
}

impl ConnectAttempt {
    /// Refuses media from here on. Returns whether media had already started, in which case
    /// the session's own teardown releases it.
    pub fn cancel(&self) -> bool {
        let mut state = self.0.lock().expect("connect attempt poisoned");
        state.cancelled = true;
        state.media_started
    }

    fn enter_media(&self) -> Result<()> {
        let mut state = self.0.lock().expect("connect attempt poisoned");
        anyhow::ensure!(!state.cancelled, "connection cancelled");
        state.media_started = true;
        Ok(())
    }
}

pub struct Connected {
    pub client: Arc<NativeClient>,
    pub stop: Arc<AtomicBool>,
    input_state: Arc<std::sync::Mutex<crate::core::input::HeldInputs>>,
    /// Live pump counters for stats overlay; see `StreamStats`.
    pub stats: Arc<StreamStats>,
    /// The decode pipeline and the threads that drive it. Kept alive so `shutdown()` can join
    /// them, and so the QUIC close frame goes out before exit.
    pipeline: MediaPipeline,
}

impl Connected {
    /// Stop and join threads, then drop `NativeClient`. Call `disconnect_quit()` first for a
    /// graceful end. Returns `false` if any step outlived `SHUTDOWN_JOIN_TIMEOUT`.
    pub fn shutdown(self) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        let mut clean = self.pipeline.join();
        // `NativeClient::drop` joins its own QUIC-close worker; bound it the same way.
        let client = self.client;
        clean &= join_with_timeout(
            std::thread::spawn(move || drop(client)),
            SHUTDOWN_JOIN_TIMEOUT,
            "client-drop",
            || (),
        );
        clean
    }

    /// [`Self::shutdown`], logging a wedge.
    pub fn shutdown_and_quit(self) {
        if !self.shutdown() {
            tracing::warn!("session teardown timed out; a media thread is still running");
        }
    }
}

/// Everything [`connect`] needs from the chosen target and the user's settings.
///
/// A struct rather than a parameter list: every field comes from exactly those two places, and a
/// positional list of fifteen mostly-scalar arguments is one where a swapped pair still compiles.
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    /// Requested capture mode; the host clamps it and echoes the result in `client.mode()`.
    pub mode: Mode,
    pub bitrate_kbps: u32,
    /// The `PyroWave` quality row, in hundredths of a bit per pixel. The host pins `PyroWave`
    /// from this, never from `bitrate_kbps`.
    pub pyrowave_bpp_x100: u16,
    pub hdr_enabled: bool,
    /// Main10 at BT.709 without HDR. Subsumed by `hdr_enabled`.
    pub ten_bit_sdr: bool,
    pub audio_channels: u8,
    /// This client's TLS identity, `(cert_pem, key_pem)`.
    pub identity: (String, String),
    /// Trusted host fingerprint from a prior pairing; `None` = trust-on-first-use.
    pub pin: Option<[u8; 32]>,
    /// Game/app handle for the host to launch once the session is up.
    pub launch: Option<String>,
    /// Handshake budget.
    pub timeout: Duration,
    pub codec: CodecPref,
    /// Advisory: the host falls back to its own default when this one is unavailable.
    pub compositor: CompositorPref,
    pub gamepad_type: GamepadType,
    pub cursor_capture: bool,
    /// Pad-audio render bits (`session::pad_audio::CAP_*`); non-zero advertises
    /// `CLIENT_CAP_PAD_AUDIO`. The per-pad declaration rides the arrival, not the handshake.
    pub pad_audio_caps: u8,
    pub present_priority: pf_client_core::trust::PresentPriority,
    /// The host profile to play as; `None` lets the host choose.
    pub profile: Option<String>,
}

/// One `quic::CODEC_*` bit, or 0 where the preference names no single codec.
fn codec_bit(pref: CodecPref) -> u8 {
    match pref {
        CodecPref::Auto => 0,
        CodecPref::PyroWave => quic::CODEC_PYROWAVE,
        CodecPref::H264 => quic::CODEC_H264,
        CodecPref::Hevc => quic::CODEC_HEVC,
    }
}

/// What this client advertises on the wire, clamped to what the console decodes.
struct Negotiated {
    audio_channels: u8,
    /// `quic::VIDEO_CAP_*` bitfield.
    video_caps: u8,
    /// `quic::CODEC_*` bitfield: every codec this client can present.
    video_codecs: u8,
    /// A single `quic::CODEC_*` bit, or 0 for auto.
    preferred_codec: u8,
    /// The capture mode to ask for: the user's, at a rate the chosen decoder keeps up with.
    mode: Mode,
}

/// The highest pixel rate Videodec2 decodes as H.264: 4K60. Its decoder takes one picture at a
/// time, 13.5 ms of 16.7 at 4K60 and 9-17 ms of 8.3 at 4K120, where the queue it leaves behind
/// is flushed every 2 s and read as congestion down to 2 Mbps (`docs/NOTES.md`).
const H264_MAX_PIXELS_PER_SECOND: u64 = 3840 * 2160 * 60;

/// `mode` with its refresh lowered to 60 Hz where H.264 would outrun the decoder above it.
fn decodable_mode(mode: Mode, codec: CodecPref) -> Mode {
    let rate = u64::from(mode.width) * u64::from(mode.height) * u64::from(mode.refresh_hz);
    if codec != CodecPref::H264 || rate <= H264_MAX_PIXELS_PER_SECOND || mode.refresh_hz <= 60 {
        return mode;
    }
    tracing::info!(
        "H.264 at {}x{}@{} outruns this console's decoder — asking for {}x{}@60",
        mode.width,
        mode.height,
        mode.refresh_hz,
        mode.width,
        mode.height,
    );
    Mode { refresh_hz: 60, ..mode }
}

impl Negotiated {
    /// **The authoritative capability gate.** Codec, colour path and channel count are settled by
    /// the handshake, BEFORE any decoder opens, so a document carried over from another client
    /// must be clamped here and not merely hidden in the UI: HEVC negotiated onto an H.264-only
    /// decoder is a frozen black stream with no second chance once `Welcome` has resolved.
    fn clamp(params: &ConnectParams) -> Self {
        let caps = video_caps();
        let audio_channels = params.audio_channels.min(caps.max_channels);
        if audio_channels < params.audio_channels {
            tracing::info!(
                "audio: {} channel(s) requested, asking for {audio_channels} — this console plays no more",
                params.audio_channels,
            );
        }
        let codecs = caps.codec_prefs();
        let codec_pref = if codecs.contains(&params.codec) {
            params.codec
        } else {
            codecs.first().copied().unwrap_or_default()
        };
        // 10-bit rides HEVC or PyroWave, never H.264. HDR is BT.2020 PQ and needs 10-bit.
        let hdr = params.hdr_enabled && caps.hdr && caps.ten_bit && codec_pref != CodecPref::H264;
        let ten_bit = hdr || (params.ten_bit_sdr && caps.ten_bit && caps.hevc && codec_pref != CodecPref::H264);
        Self {
            audio_channels,
            // Multi-slice: Videodec2 spreads a picture's slices across its cores; one slice
            // serializes it. The host slices only toward this bit; `dial` names the count.
            video_caps: quic::VIDEO_CAP_MULTI_SLICE
                | if ten_bit { quic::VIDEO_CAP_10BIT } else { 0 }
                | if hdr { quic::VIDEO_CAP_HDR } else { 0 },
            // Folded from the one codec list so the host's ladder never picks an undecodable lane.
            video_codecs: codecs.iter().fold(0, |set, &pref| set | codec_bit(pref)),
            preferred_codec: codec_bit(codec_pref),
            mode: decodable_mode(params.mode, codec_pref),
        }
    }
}

/// Runs the handshake. Everything wire-facing has already been clamped by [`Negotiated::clamp`].
fn dial(params: &ConnectParams, negotiated: &Negotiated) -> Result<NativeClient> {
    NativeClient::connect(punktfunk_core::client::ConnectParams {
        compositor: params.compositor,
        // Session-default pad kind; this client drives one pad (index 0), so it equals a
        // per-pad arrival and also reaches hosts without `HOST_CAP_GAMEPAD_STATE`.
        gamepad: params.gamepad_type.to_core(),
        bitrate_kbps: params.bitrate_kbps,
        pyrowave_bpp_x100: params.pyrowave_bpp_x100,
        // Eight, not the host's four: 4K120 HEVC decodes 0.6-0.8 ms faster and holds 60 Mbit/s.
        slices: 8,
        video_caps: negotiated.video_caps,
        // Requested only: `AudioPlayer::new` is built from the RESOLVED `client.audio_channels`.
        audio_channels: negotiated.audio_channels,
        // libopus here decodes either coupling.
        audio_layout: punktfunk_core::audio::AudioLayout::Standard,
        video_codecs: negotiated.video_codecs,
        preferred_codec: negotiated.preferred_codec,
        display_hdr: None,
        // See `store::Settings::cursor_capture` for the on/off split.
        client_caps: (if params.cursor_capture {
            0
        } else {
            quic::CLIENT_CAP_CURSOR
        }) | if params.pad_audio_caps != 0 {
            quic::CLIENT_CAP_PAD_AUDIO
        } else {
            0
        },
        // Both lanes take complete AUs, so `frame_parts` stays off.
        // The video thread reopens its decoder on a frame in a new format.
        rebuilds_decoder: true,
        launch: params.launch.clone(),
        pin: params.pin,
        identity: Some(params.identity.clone()),
        profile: params.profile.clone(),
        // Uncancelable: the connect has its own thread and the caller joins it.
        ..punktfunk_core::client::ConnectParams::new(&params.host, params.port, negotiated.mode, params.timeout)
    })
    .context("connect")
}

/// The one line that says what the handshake actually settled on.
fn log_handshake(client: &NativeClient, negotiated: &Negotiated) {
    let fp_hex = client.host_fingerprint.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    tracing::info!(
        "connected: codec={} (offered=0x{:02x} preferred=0x{:02x}) \
         compositor={:?} audio_ch={} audio_layout={} color={:?} wire_budget_kbps={} \
         decode_latency={} caps=0x{:02x} fp={fp_hex}",
        client.codec,
        negotiated.video_codecs,
        negotiated.preferred_codec,
        client.resolved_compositor,
        client.audio_channels,
        client.audio_layout,
        client.color,
        client.resolved_bitrate_kbps,
        client.wants_decode_latency(),
        negotiated.video_caps,
    );
}

/// Connects to a punktfunk host and starts the media pipeline. Blocks until the handshake
/// completes or `params.timeout` elapses.
pub fn connect(params: &ConnectParams, media: &Media, attempt: &ConnectAttempt) -> Result<Connected> {
    let negotiated = Negotiated::clamp(params);
    let client = Arc::new(dial(params, &negotiated)?);
    log_handshake(&client, &negotiated);

    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(StreamStats::default());
    let media = attempt
        .enter_media()
        .and_then(|()| MediaPipeline::build(params, media, &client, &stop, &stats));
    let pipeline = match media {
        Ok(built) => built,
        Err(e) => {
            // Past the handshake the host holds a session for us: end it deliberately.
            client.disconnect_quit();
            return Err(e);
        }
    };

    Ok(Connected {
        client,
        stop,
        input_state: Arc::default(),
        stats,
        pipeline,
    })
}

/// The input path, cloned for the input thread. Button edges go through the held-input ledger
/// so every press has exactly one release.
#[derive(Clone)]
pub struct InputSender {
    client: Arc<NativeClient>,
    held: Arc<std::sync::Mutex<crate::core::input::HeldInputs>>,
}

impl InputSender {
    pub fn send(&self, ev: &punktfunk_core::input::InputEvent) {
        if crate::core::input::HeldInputs::is_edge(ev) {
            self.held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .send(0, ev, |ev| {
                    let _ = self.client.send_input(ev);
                });
        } else {
            let _ = self.client.send_input(ev);
        }
    }

    /// Releases every key and button the host still holds from this client.
    pub fn release(&self) {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release(0, |ev| {
                let _ = self.client.send_input(ev);
            });
    }

    pub fn client(&self) -> &Arc<NativeClient> {
        &self.client
    }
}

/// What the loops need from a live session.
impl Connected {
    pub fn input(&self) -> InputSender {
        InputSender {
            client: self.client.clone(),
            held: self.input_state.clone(),
        }
    }

    pub fn is_session_ended(&self) -> bool {
        self.client.is_session_ended()
    }

    /// Why it ended; `Lost` is the one reason worth dialling again.
    pub fn end_reason(&self) -> punktfunk_core::client::PunktfunkEndReason {
        self.client.end_reason()
    }

    /// The sentence for the menu's notice when the session ended on its own.
    pub fn end_message(&self) -> String {
        use punktfunk_core::client::PunktfunkEndReason as R;
        match self.end_reason() {
            R::GameExited => "The game exited.",
            R::HostEnded => "The host ended the session.",
            R::HostError => "The host closed the session with an error.",
            R::Lost => "The connection was lost.",
            R::None | R::Local => "The host closed the connection.",
        }
        .to_string()
    }

    pub fn disconnect_quit(&self) {
        self.client.disconnect_quit();
    }

    /// The overlay's window of figures, naming the lane that decoded them.
    pub fn hud_snapshot(&self, decoder: &str) -> punktfunk_core::hud::StatsSnapshot {
        let mut snap = self.client.hud_snapshot();
        snap.decoder = decoder.to_string();
        snap
    }

    /// Sample for the overlay only while it shows.
    pub fn set_hud_enabled(&self, on: bool) {
        self.client.set_hud_enabled(on);
    }

    pub fn client(&self) -> &Arc<NativeClient> {
        &self.client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn mode(width: u32, height: u32, refresh_hz: u32) -> Mode {
        Mode {
            width,
            height,
            refresh_hz,
        }
    }

    #[test]
    fn h264_at_4k120_asks_for_4k60() {
        assert_eq!(
            decodable_mode(mode(3840, 2160, 120), CodecPref::H264),
            mode(3840, 2160, 60)
        );
    }

    #[test]
    fn h264_within_4k60_keeps_its_mode() {
        for m in [mode(3840, 2160, 60), mode(2560, 1440, 120), mode(1920, 1080, 120)] {
            assert_eq!(decodable_mode(m, CodecPref::H264), m);
        }
    }

    #[test]
    fn other_codecs_keep_4k120() {
        for codec in [CodecPref::Hevc, CodecPref::PyroWave, CodecPref::Auto] {
            assert_eq!(decodable_mode(mode(3840, 2160, 120), codec), mode(3840, 2160, 120));
        }
    }
}
