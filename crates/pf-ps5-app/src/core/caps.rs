//! What this device decodes and presents: one fact, read by everything that must agree about it.
//! `session::connect` advertises from it; `Settings::clamp_to_caps` normalises stored documents
//! to it. The platform installs it once at startup from `VideoDecoders::caps`; unset reads as
//! [`VideoCaps::PS5`].
use std::sync::OnceLock;

use crate::core::model::CodecPref;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoCaps {
    /// GPU wavelet decode on the shared device (`pf-client-video`).
    pub pyrowave: bool,
    pub h264: bool,
    pub hevc: bool,
    /// Main10 HEVC decodes and presents.
    pub ten_bit: bool,
    /// PQ streams scan out as HDR10; without it they are tonemapped to SDR.
    pub hdr: bool,
    /// Widest layout the audio output plays.
    pub max_channels: u8,
}

impl VideoCaps {
    /// The console with both lanes up.
    pub const PS5: Self = Self {
        pyrowave: true,
        h264: true,
        hevc: true,
        ten_bit: true,
        hdr: true,
        max_channels: 2,
    };

    /// The codec preferences worth offering, in display order. The one place the set is
    /// spelled, so the stored-document clamp and the advertised wire set agree.
    pub fn codec_prefs(self) -> Vec<CodecPref> {
        let mut prefs = Vec::with_capacity(4);
        if self.h264 || self.hevc {
            prefs.push(CodecPref::Auto);
        }
        if self.pyrowave {
            prefs.push(CodecPref::PyroWave);
        }
        if self.h264 {
            prefs.push(CodecPref::H264);
        }
        if self.hevc {
            prefs.push(CodecPref::Hevc);
        }
        prefs
    }
}

static INSTALLED: OnceLock<VideoCaps> = OnceLock::new();

/// Publish the platform's caps. Call once, before settings load; later calls are ignored.
pub fn install(caps: VideoCaps) {
    if INSTALLED.set(caps).is_err() {
        tracing::warn!("video caps already installed — ignoring {caps:?}");
    }
}

pub fn video_caps() -> VideoCaps {
    INSTALLED.get().copied().unwrap_or(VideoCaps::PS5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyrowave_alone_offers_no_automatic() {
        let caps = VideoCaps {
            h264: false,
            hevc: false,
            ..VideoCaps::PS5
        };
        assert_eq!(caps.codec_prefs(), [CodecPref::PyroWave]);
        assert_eq!(
            VideoCaps::PS5.codec_prefs(),
            [CodecPref::Auto, CodecPref::PyroWave, CodecPref::H264, CodecPref::Hevc]
        );
    }
}
