//! The seams between the application and whatever it runs on.
//!
//! Three implementations exist: the console (`pf-ps5-platform`), a developer box
//! (`pf-ps5-host`) and [`fake`] for tests. The binary builds one [`Platform`] and hands it to
//! [`crate::run`]; nothing in this crate names a console API, a Vulkan handle or a file path
//! outside [`Storage::data_dir`]. The contract tests in `tests/seams.rs` are the spec.

pub mod fake;

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use punktfunk_core::quic;

use crate::core::caps::VideoCaps;
use crate::core::media::VideoSink;

/// Everything the application needs from below, built once by the binary.
pub struct Platform {
    pub display: Box<dyn Display>,
    pub video: Arc<dyn VideoDecoders>,
    pub audio: Arc<dyn AudioOut>,
    pub pads: Box<dyn Pads>,
    pub storage: Arc<dyn Storage>,
    pub lifecycle: Box<dyn Lifecycle>,
    /// A second log destination beside the file: klog on the console, stderr on a dev box.
    pub log: Option<Box<dyn Write + Send>>,
    pub device: DeviceInfo,
}

/// What the logs and the About screen say about the machine.
#[derive(Clone, Debug, Default)]
pub struct DeviceInfo {
    /// `ps5`, `host` or `fake`.
    pub platform: &'static str,
    pub model: String,
    pub firmware: String,
    /// The graphics driver and its pin (`RADV <mesa> <patch set>`).
    pub gpu: String,
    /// How the UI layer draws: backend, size, and why it fell back to the CPU.
    pub ui: String,
    /// The system's languages as BCP-47 tags, most preferred first. Empty reads English.
    pub languages: Vec<String>,
}

/// Refresh rate of the scan-out, in millihertz (59 940, 119 880).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refresh(pub u32);

impl Refresh {
    pub fn hz(self) -> u32 {
        (self.0 + 500) / 1000
    }

    pub fn period_ns(self) -> u64 {
        1_000_000_000_000 / u64::from(self.0.max(1))
    }
}

/// How a stream picture sits in the output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    /// Scaled to fit with bars; never cropped.
    #[default]
    Contain,
    /// Scaled to fill; the overflow is cropped.
    Cover,
    /// Stretched to the output.
    Stretch,
}

/// The one output and its frame loop. Lives on the main thread: Skia is not `Send`.
pub trait Display {
    /// The swapchain extent (3840×2160 on the console).
    fn size(&self) -> (u32, u32);

    /// The UI canvas [`Self::frame`] hands out; may be smaller than the output and scaled up.
    fn ui_size(&self) -> (u32, u32) {
        self.size()
    }

    fn refresh(&self) -> Refresh;

    /// Draws one frame and presents it at the next vblank: the newest due stream picture
    /// (when [`Self::set_stream`] is on), then `ui` over it on a transparent layer. Blocks
    /// until a swapchain image is free, so this paces the main loop.
    fn frame(&mut self, ui: &mut dyn FnMut(&skia_safe::Canvas)) -> Result<FrameReport>;

    /// Whether the stream layer shows. Off, the UI draws over black.
    fn set_stream(&mut self, on: bool, fit: Fit);

    /// The last presented frame as tightly packed RGBA, for test-run bundles.
    fn read_back(&mut self) -> Result<Option<Rgba>>;
}

/// What one presented frame cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameReport {
    /// A vblank went by with nothing new presented.
    pub missed: bool,
    /// The stream picture shown this frame was new.
    pub new_picture: bool,
    /// Decode-complete to present, for the picture shown, when it was new.
    pub decode_to_present_us: Option<u32>,
}

/// A read-back frame.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The decode lanes. A sink opened here feeds the [`Display`]'s stream layer.
pub trait VideoDecoders: Send + Sync {
    /// What this device decodes; read once at startup into [`crate::core::caps`].
    fn caps(&self) -> VideoCaps;

    /// Opens the decoder for the negotiated stream (`codec` is one `quic::CODEC_*` bit).
    fn open(&self, spec: &StreamSpec) -> Result<Box<dyn VideoSink>>;
}

/// What the handshake settled on, as a decoder needs it.
#[derive(Clone)]
pub struct StreamSpec {
    pub codec: u8,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub color: quic::ColorInfo,
    pub bit_depth: u8,
    pub chroma444: bool,
    /// Bytes per shard on the wire, which a chunk-aligned AU is cut into.
    pub shard_payload: usize,
    /// The session's overlay figures: lanes note decode, the presenter notes display.
    pub hud: Arc<punktfunk_core::hud::Stats>,
}

/// The console's audio output. Opening a port is cheap; a session holds one.
pub trait AudioOut: Send + Sync {
    fn open(&self, channels: u8, rate: u32) -> Result<Box<dyn AudioPort>>;
}

/// One open output port. Interleaved f32 at the opened rate and width.
pub trait AudioPort: Send {
    /// Frames per [`Self::write`]; fixed for the port's life.
    fn grain_frames(&self) -> usize;

    /// Plays exactly one grain, blocking until the device takes it.
    fn write(&mut self, pcm: &[f32]) -> Result<()>;
}

/// One pad's state at one instant, already in punktfunk's wire vocabulary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadSample {
    /// Stable slot, 0..4: one per signed-in user.
    pub slot: u8,
    pub kind: PadKind,
    /// `punktfunk_core::input::gamepad::BTN_*` bits.
    pub buttons: u32,
    /// Sticks in the wire's range and sign (+y is up).
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u8,
    pub rt: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PadKind {
    #[default]
    DualSense,
    DualSenseEdge,
    Other,
}

/// What the host asked a pad to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PadFeedback {
    /// Motor levels, 0..=65535.
    Rumble {
        low: u16,
        high: u16,
    },
    Light {
        r: u8,
        g: u8,
        b: u8,
    },
    /// The `DualSense` trigger parameter block for L2 (`which` 0) or R2 (1). Empty turns it off.
    Trigger {
        which: u8,
        effect: Vec<u8>,
    },
}

/// Every pad the console reports. Polled by the input thread.
pub trait Pads: Send {
    /// Appends one sample per connected pad.
    fn poll(&mut self, out: &mut Vec<PadSample>);

    fn feedback(&mut self, slot: u8, feedback: &PadFeedback);

    /// Puts every pad's lightbar, triggers and motors back as the system had them.
    fn release_all(&mut self);
}

/// Where identity, settings, logs and bundles live.
pub trait Storage: Send + Sync {
    fn data_dir(&self) -> &Path;
}

/// What the system tells the title, polled once a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemEvent {
    /// The home menu opened over the title: release held input.
    FocusLost,
    FocusGained,
    /// The console is going to rest mode: end the session cleanly.
    Suspend,
    Resume,
}

/// The title's life. [`Self::exit`] is the only way out: returning from `main` is a crash on
/// the console.
pub trait Lifecycle {
    fn hide_splash(&self);
    fn poll_events(&mut self, out: &mut Vec<SystemEvent>);
    fn exit(&self) -> !;
}
