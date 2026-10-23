//! A launch on its way to a stream: the connect thread, what it dialled, and the plan for
//! leaving the title afterwards.

use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};
use punktfunk_core::config::{CompositorPref, Mode};

use crate::core::model::{ConnectTarget, GamepadType};
use crate::core::settings::Ps5Settings;
use crate::services::power::ExitPlan;
use crate::services::store::console::ConsoleStore;
use crate::services::store::{self, shared};
use crate::session::{self, Media};
use crate::ui::Service;

/// A connect in flight. Dropping it cancels the attempt and winds a landed session down off the
/// calling thread.
pub(super) struct PendingConnect {
    handle: Option<std::thread::JoinHandle<Result<session::Connected>>>,
    attempt: Arc<session::ConnectAttempt>,
}

impl PendingConnect {
    pub(super) fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(std::thread::JoinHandle::is_finished)
    }

    pub(super) fn join(mut self) -> std::thread::Result<Result<session::Connected>> {
        self.handle.take().expect("pending connect handle").join()
    }
}

impl Drop for PendingConnect {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else { return };
        self.attempt.cancel();
        let joiner = std::thread::spawn(move || {
            if let Ok(Ok(connected)) = handle.join() {
                super::teardown(connected);
            }
        });
        let mut abandoned = ABANDONED.lock().unwrap_or_else(PoisonError::into_inner);
        abandoned.retain(|j| !j.is_finished());
        abandoned.push(joiner);
    }
}

/// Joiners left by dropped connects. Only the exit waits on them: the host refuses a power
/// action while a session it holds for us is still live.
static ABANDONED: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

pub(super) fn await_abandoned() {
    let abandoned = std::mem::take(&mut *ABANDONED.lock().unwrap_or_else(PoisonError::into_inner));
    for joiner in abandoned {
        let _ = joiner.join();
    }
}

/// A launch handed from the menu to the stream loop.
pub(super) struct ConnectOutcome {
    pub connected: Result<session::Connected>,
    /// What was dialled, so a lost link can be dialled again.
    pub target: ConnectTarget,
    pub settings: store::Settings,
    pub exit_plan: Option<ExitPlan>,
}

/// The mode to ask for: the document's explicit pick, or the console's own output where it
/// says "Native" (`0`). The host refuses a 0×0 request.
pub(super) fn stream_mode(settings: &store::Settings, native: Mode) -> Mode {
    let explicit = !settings.match_window && settings.width != 0 && settings.height != 0;
    Mode {
        width: if explicit { settings.width } else { native.width },
        height: if explicit { settings.height } else { native.height },
        refresh_hz: if settings.refresh_hz == 0 {
            native.refresh_hz
        } else {
            settings.refresh_hz
        },
    }
}

/// Starts the connect on its own thread.
pub(super) fn spawn(
    media: &Media,
    identity: (String, String),
    target: ConnectTarget,
    settings: store::Settings,
    native: Mode,
) -> Result<PendingConnect> {
    let attempt = Arc::new(session::ConnectAttempt::default());
    let worker_attempt = attempt.clone();
    let media = media.clone();
    std::thread::Builder::new()
        .name("pf-connect".into())
        .spawn(move || {
            let mode = stream_mode(&settings, native);
            tracing::info!("requesting {}x{}@{}", mode.width, mode.height, mode.refresh_hz);
            session::connect(
                &session::ConnectParams {
                    host: target.host,
                    port: target.port,
                    mode,
                    bitrate_kbps: settings.bitrate_kbps,
                    pyrowave_bpp_x100: settings.pyrowave_bpp_x100(),
                    hdr_enabled: settings.hdr_enabled,
                    ten_bit_sdr: settings.ten_bit_sdr,
                    audio_channels: settings.audio_channels,
                    identity,
                    pin: Some(target.fingerprint),
                    launch: target.launch,
                    profile: target.profile,
                    timeout: crate::services::budget::CONNECT,
                    codec: settings.codec_pref(),
                    compositor: CompositorPref::from_name(&settings.compositor).unwrap_or(CompositorPref::Auto),
                    gamepad_type: settings.gamepad_type(),
                    // Pad-only: the host draws the cursor.
                    cursor_capture: true,
                    pad_audio_caps: 0,
                    present_priority: settings.present_priority(),
                },
                &media,
                &worker_attempt,
            )
        })
        .map(|handle| PendingConnect {
            handle: Some(handle),
            attempt,
        })
        .context("spawn connect thread")
}

/// What the console committed to launch.
pub(super) struct Launch {
    pub addr: String,
    pub port: u16,
    pub fp_hex: String,
    pub launch: Option<String>,
    pub preset: Option<String>,
    pub profile: Option<String>,
}

/// A connect in flight with what it dialled and the session settings.
pub(super) type Started = (PendingConnect, ConnectTarget, store::Settings);

/// Resolves a launch against the document and starts its connect.
pub(super) fn start(
    store: &Arc<ConsoleStore>,
    media: &Media,
    identity: &(String, String),
    want: Launch,
    native: Mode,
    overlay: Option<&pf_client_core::presets::SettingsOverlay>,
) -> Result<Started> {
    let state = store.snapshot();
    let known = state
        .known_hosts
        .iter()
        .find(|h| h.addr == want.addr && h.port == want.port);
    let fingerprint = known
        .and_then(crate::core::model::KnownHost::fingerprint)
        .or_else(|| shared::parse_fp(&want.fp_hex))
        .context("that host isn't paired with this console yet")?;
    let mut settings = shared::launch_settings(
        &state,
        &want.addr,
        want.port,
        want.launch.as_deref(),
        want.preset.as_deref(),
    );
    // The shared settings screen has no HDR row on this console, so nothing could turn it on.
    if crate::core::caps::video_caps().hdr {
        settings.hdr_enabled = true;
    }
    if let Some(overlay) = overlay {
        settings = overlay.apply(&settings);
        settings.clamp_to_caps();
        tracing::info!(
            "test run: settings for this run codec={} bitrate_kbps={} {}x{}@{} pyrowave_bpp={} hdr={}",
            settings.codec,
            settings.bitrate_kbps,
            settings.width,
            settings.height,
            settings.refresh_hz,
            settings.pyrowave_bpp,
            settings.hdr_enabled,
        );
    }
    // "Automatic" mirrors the pad in hand, which on this console is a DualSense.
    if settings.gamepad_type() == GamepadType::Auto {
        settings.set_gamepad_type(GamepadType::DualSense);
    }
    let target = ConnectTarget {
        host: want.addr,
        port: want.port,
        fingerprint,
        launch: want.launch,
        profile: want.profile,
    };
    let handle = spawn(media, identity.clone(), target.clone(), settings.clone(), native)?;
    Ok((handle, target, settings))
}

/// What to do to the selected host on the way out: only a host that answered its last
/// reachability check, since a host already down costs the whole budget.
pub(super) fn exit_plan(service: &Service, identity: &(String, String)) -> Option<ExitPlan> {
    let state = service.store.snapshot();
    let (host, port) = state.selected_host.clone()?;
    let known = state.known_hosts.iter().find(|h| h.addr == host && h.port == port)?;
    known.exit_action.action_id()?;
    if !service.is_online(known) {
        tracing::debug!("exit action skipped: {host} was not reachable");
        return None;
    }
    Some(ExitPlan {
        addr: known.addr.clone(),
        mgmt_port: known.mgmt_port.unwrap_or(crate::services::library::DEFAULT_MGMT_PORT),
        identity: identity.clone(),
        pin: Some(known.fingerprint()?),
        action: known.exit_action,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Native" (0) dials the console's output; an explicit pick dials itself.
    #[test]
    fn a_native_mode_dials_the_output() {
        let output = Mode {
            width: 3840,
            height: 2160,
            refresh_hz: 120,
        };
        let mut s = store::Settings::default();
        assert_eq!(stream_mode(&s, output), output);
        s.width = 1920;
        s.height = 1080;
        s.refresh_hz = 60;
        assert_eq!(
            stream_mode(&s, output),
            Mode {
                width: 1920,
                height: 1080,
                refresh_hz: 60
            }
        );
    }
}
