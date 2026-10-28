//! This client's typed view of the shared settings document (`pf_client_core::trust::Settings`),
//! the one settings type it stores and edits. Every row here is a shared field; a row only the
//! console has would live in `extra` under a `ps5.` prefix, and none exists.

use pf_client_core::trust::Settings;
use punktfunk_core::config::GamepadPref;

use crate::core::model::{CodecPref, GamepadType};

/// This client's codec pick as punktfunk's wire name, and back.
fn shared_codec(c: CodecPref) -> &'static str {
    match c {
        CodecPref::Auto => "auto",
        CodecPref::PyroWave => "pyrowave",
        CodecPref::H264 => "h264",
        CodecPref::Hevc => "hevc",
    }
}

fn local_codec(name: &str) -> CodecPref {
    match name {
        "pyrowave" => CodecPref::PyroWave,
        "h264" => CodecPref::H264,
        "hevc" => CodecPref::Hevc,
        // "av1" too: the console cannot decode it, so it reads as the host's choice.
        _ => CodecPref::Auto,
    }
}

/// The typed reads and writes this client makes on the shared document.
pub trait Ps5Settings {
    fn codec_pref(&self) -> CodecPref;
    fn set_codec_pref(&mut self, codec: CodecPref);
    fn gamepad_type(&self) -> GamepadType;
    fn set_gamepad_type(&mut self, kind: GamepadType);
    /// Normalise to what this console can present (`core::caps`). Called on load and on every
    /// launch's resolved settings, so a profile override cannot put an unpresentable pick on
    /// the wire.
    fn clamp_to_caps(&mut self);
}

impl Ps5Settings for Settings {
    fn codec_pref(&self) -> CodecPref {
        local_codec(&self.codec)
    }

    fn set_codec_pref(&mut self, codec: CodecPref) {
        self.codec = shared_codec(codec).to_string();
    }

    fn gamepad_type(&self) -> GamepadType {
        GamepadPref::from_name(&self.gamepad)
            .and_then(GamepadType::from_core)
            .unwrap_or_default()
    }

    fn set_gamepad_type(&mut self, kind: GamepadType) {
        self.gamepad = kind.to_core().as_str().to_string();
    }

    fn clamp_to_caps(&mut self) {
        let caps = crate::core::caps::video_caps();
        let codecs = caps.codec_prefs();
        let codec = self.codec_pref();
        if !codecs.contains(&codec) {
            if let Some(&first) = codecs.first() {
                tracing::info!("settings: {codec:?} isn't decodable here — using {first:?}");
                self.set_codec_pref(first);
            }
        }
        if self.hdr_enabled && !caps.hdr {
            tracing::info!("settings: the display isn't showing HDR — turning HDR off");
            self.hdr_enabled = false;
        }
        if self.audio_channels > caps.max_channels {
            tracing::info!(
                "settings: {} audio channels is more than this console plays ({}) — clamping",
                self.audio_channels,
                caps.max_channels,
            );
            self.audio_channels = caps.max_channels;
        }
    }
}

/// The document a fresh install starts from.
pub fn default_document() -> Settings {
    Settings::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip_through_the_shared_names() {
        let mut s = default_document();
        s.set_codec_pref(CodecPref::PyroWave);
        s.set_gamepad_type(GamepadType::DualSense);
        assert_eq!(s.codec, "pyrowave");
        let back: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back.codec_pref(), CodecPref::PyroWave);
        assert_eq!(back.gamepad_type(), GamepadType::DualSense);
    }

    #[test]
    fn clamp_keeps_hdr_and_narrows_channels() {
        let mut s = default_document();
        s.hdr_enabled = true;
        s.audio_channels = 8;
        s.codec = "av1".into();
        s.clamp_to_caps();
        assert!(s.hdr_enabled);
        assert_eq!(s.audio_channels, 2);
        assert_eq!(s.codec_pref(), CodecPref::Auto);
    }
}
