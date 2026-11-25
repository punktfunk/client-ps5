//! Connects to a punktfunk host and drives the media pipeline.
//!
//! Video runs on its own thread (`pump`): access units off the transport into `stage`'s
//! `VideoStage` — PTS anchoring, freeze-until-reanchor, backlog metering — written against
//! `core::media`'s `VideoSink`, which the platform's decode lanes implement. Audio decodes on a
//! second thread into `audio_out`'s ring, which a third drains into the console's port.
//!
//! Split by phase: `connect` runs the handshake, `pipeline` builds the decode path, `pump`
//! keeps it fed, and `probe` holds the handshake-only connections (pairing, speed test).
pub mod audio;
pub mod audio_out;
mod connect;
pub mod haptics;
mod pipeline;
pub mod probe;
mod pump;
mod stage;
mod stats;
mod timeline;

pub use connect::{connect, ConnectAttempt, ConnectParams, Connected, InputSender};
pub use pipeline::Media;
pub use stats::StreamStats;
