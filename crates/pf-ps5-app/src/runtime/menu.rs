//! The shared console as this title's menu. Hands a launch or a quit back as a [`UiOutcome`],
//! reloading the settings document on every entry.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pf_client_core::console::{OverlayAction, SessionPhase};
use pf_client_core::menu_nav::{MenuEvent, PadInfo};
use pf_console_ui::{Console, ConsoleEntry, ConsoleHandles, ConsoleOptions, InputSource, Platform, Viewport};
use punktfunk_core::config::Mode;

use super::connect::{self, ConnectOutcome, Launch, Started};
use super::pads::MenuPads;
use super::{testrun, App, UiOutcome, SUSPENDED};
use crate::core::model::GamepadType;
use crate::core::perf::{ArtSnapshot, Perf};
use crate::core::settings::Ps5Settings;
use crate::platform::{PadKind, PadSample};
use crate::services::store::console::ConsoleStore;
use crate::services::store::{self, StateWriter};
use crate::ui::Service;

/// The console's output as a stream mode, for a "Native" pick.
pub(super) fn native_mode(app: &App) -> Mode {
    let (width, height) = app.display.size();
    Mode {
        width,
        height,
        refresh_hz: app.display.refresh().hz(),
    }
}

/// Runs the console until it commits a launch or asks to leave.
pub(super) fn run(app: &mut App, notice: Option<String>) -> Result<UiOutcome> {
    let state = store::load();
    let writer = Arc::new(StateWriter::spawn(state.clone()));
    let store = Arc::new(ConsoleStore::new(state, writer));
    let handles = ConsoleHandles::new();
    let mut service = Service::new(handles.clone(), store.clone(), app.identity.clone());
    let caps = crate::core::caps::video_caps();
    let opts = ConsoleOptions {
        device_name: "PlayStation 5".into(),
        version: Some(crate::core::build_id()),
        system_keyboard: false,
        tv: true,
        fallback_ui: false,
        pyrowave_ok: caps.pyrowave,
        av1_ok: false,
        store: Some(store.clone()),
        platform: Platform::Ps5,
        gpu_cache_bytes: pf_console_ui::DEFAULT_GPU_CACHE_BYTES,
        screen: None,
        profiles: false,
        spatial: false,
        prompts_unlock: false,
        languages: app.device.languages.clone(),
    };
    let scripted = app.test_run.as_ref().and_then(|run| {
        let host = run.host.as_deref()?;
        let row = service.row_for(host, run.port);
        if row.is_none() {
            tracing::warn!("test run: {host} is not a paired host here");
        }
        row
    });
    let entry = match scripted {
        Some(row) => ConsoleEntry::Stream(Box::new(row)),
        None => service
            .selected_row()
            .map_or(ConsoleEntry::Home, |row| ConsoleEntry::Library(Box::new(row))),
    };
    let faces = super::system_faces(&app.device.fonts);
    let faces: Vec<&[u8]> = faces.iter().map(Vec::as_slice).collect();
    let mut console = Console::new(opts, entry, &handles)
        .and_then(|console| console.with_fallback_fonts(&faces))
        .context("console shell")?;
    let viewport = {
        let (w, h) = app.display.ui_size();
        Viewport::plain(w, h)
    };
    let warm = Instant::now();
    app.display.frame(&mut |canvas| {
        // One flush for the whole tour; flush per frame if the console's memory peaks there.
        console.warm_up(canvas, &viewport, |_| {});
    })?;
    tracing::info!("console: warmed the tabs in {} ms", warm.elapsed().as_millis());
    if let Some(notice) = notice {
        handles.console.set_notice(notice);
    }
    app.display.set_stream(false, crate::platform::Fit::Contain);

    let mut perf = Perf::new();
    let mut menu_pads = MenuPads::default();
    let mut batch: Vec<PadSample> = Vec::new();
    let mut infos: Vec<PadInfo> = Vec::new();
    let mut menu_out: Vec<MenuEvent> = Vec::new();
    let mut connect: Option<Started> = None;
    let mut frames: u64 = 0;
    let native = native_mode(app);
    let period = Duration::from_nanos(app.display.refresh().period_ns());

    let outcome = 'ui: loop {
        let frame_start = Instant::now();
        app.system_events();

        batch.clear();
        if let Some(pads) = app.pads.as_mut() {
            pads.poll(&mut batch);
        }
        menu_out.clear();
        if let Some(reading) = menu_pads.poll(&batch, Instant::now(), service.pad_test(), &mut menu_out) {
            handles.console.set_pad_test(reading);
        }
        for ev in menu_out.drain(..) {
            console.menu(ev, InputSource::Pad);
        }

        service.tick();

        while let Some(action) = console.take_action() {
            match action {
                OverlayAction::Launch {
                    addr,
                    port,
                    fp_hex,
                    launch,
                    title,
                    preset,
                    profile,
                    request_access: _,
                } => {
                    if SUSPENDED.load(std::sync::atomic::Ordering::Relaxed) {
                        continue;
                    }
                    let want = Launch {
                        addr,
                        port,
                        fp_hex,
                        launch,
                        preset,
                        profile,
                    };
                    let what = format!("{title} on {}:{}", want.addr, want.port);
                    let overlay = app.test_run.as_ref().and_then(|run| run.settings.as_ref());
                    match connect::start(&store, &app.media, &app.identity, want, native, overlay) {
                        Ok(started) => {
                            tracing::info!("console: launching {what}");
                            console.session_phase(SessionPhase::Connecting);
                            connect = Some(started);
                        }
                        Err(e) => {
                            tracing::warn!("console: launch refused: {e:#}");
                            handles
                                .console
                                .set_notice(pf_l10n::client::dial_stream_start_failed().into());
                        }
                    }
                }
                OverlayAction::CancelConnect => {
                    if connect.take().is_some() {
                        tracing::info!("console: connect cancelled");
                        console.session_phase(SessionPhase::Ended(None));
                    }
                }
                OverlayAction::Quit => {
                    tracing::info!("console: quit");
                    break 'ui UiOutcome::Quit(connect::exit_plan(&service, &app.identity));
                }
                // No clipboard, no launcher files, and the stream shows by leaving this loop.
                OverlayAction::ShowStream
                | OverlayAction::CopyText(_)
                | OverlayAction::PinShortcut { .. }
                | OverlayAction::SaveLauncherFile { .. } => {}
            }
        }

        if connect.as_ref().is_some_and(|(h, ..)| h.is_finished()) {
            let (handle, target, settings) = connect.take().expect("just checked");
            let connected = match handle.join() {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("the connect thread panicked")),
            };
            break 'ui UiOutcome::Launch(Box::new(ConnectOutcome {
                connected,
                target,
                settings,
                exit_plan: connect::exit_plan(&service, &app.identity),
            }));
        }

        let idled = if console.idle() {
            std::thread::sleep(period);
            period
        } else {
            Duration::ZERO
        };
        let stored_kind = store.with(|s| s.settings.gamepad_type());
        let label = pad_label(&batch);
        let pad_pref = label.map(|_| shown_kind(stored_kind, &batch).to_core());
        infos.clear();
        infos.extend(pad_infos(&batch, stored_kind));
        // The cost excludes the vblank wait: `frame` blocks on the swapchain.
        let before_draw = frame_start.elapsed().saturating_sub(idled);
        let mut drew = Duration::ZERO;
        app.display.frame(&mut |canvas| {
            let t = Instant::now();
            console.frame(canvas, &viewport, label, pad_pref, &infos);
            drew = t.elapsed();
        })?;
        frames += 1;
        if let Some(report) = perf.frame(before_draw + drew, art_snapshot()) {
            tracing::info!("{}", report.line());
        }
        if let Some(done) = testrun::menu_step(app, frames) {
            break 'ui done;
        }
    };

    if let Some(report) = perf.finish(art_snapshot()) {
        tracing::info!("{}", report.line());
    }
    service.stop();
    Ok(outcome)
}

/// The pad chip's label: the first pad's kind, or `None` with no pad in hand.
fn pad_label(batch: &[PadSample]) -> Option<&'static str> {
    batch.first().map(|s| match s.kind {
        PadKind::DualSense => "DualSense",
        PadKind::DualSenseEdge => "DualSense Edge",
        PadKind::Other => pf_l10n::console::console_pad_fallback_name(),
    })
}

/// The glyph legend: the stored pick, or the pad itself on Automatic.
fn shown_kind(stored: GamepadType, batch: &[PadSample]) -> GamepadType {
    match (stored, batch.first().map(|s| s.kind)) {
        (GamepadType::Auto, Some(PadKind::DualSenseEdge)) => GamepadType::DualSenseEdge,
        (GamepadType::Auto, _) => GamepadType::DualSense,
        (kind, _) => kind,
    }
}

fn pad_infos(batch: &[PadSample], stored: GamepadType) -> impl Iterator<Item = PadInfo> + '_ {
    batch.iter().map(move |s| PadInfo {
        name: pad_label(std::slice::from_ref(s))
            .unwrap_or(pf_l10n::console::console_pad_fallback_name())
            .into(),
        key: format!("slot{}", s.slot),
        pref: shown_kind(stored, std::slice::from_ref(s)).to_core(),
        steam_virtual: false,
        battery: None,
        detail: pf_l10n::console::console_pad_player(s.slot + 1),
        forwarded: true,
        rumble: true,
        // No pad audio lane here.
        audio: false,
    })
}

fn art_snapshot() -> ArtSnapshot {
    let a = pf_console_ui::art_stats();
    ArtSnapshot {
        decoded: a.decoded,
        total_us: a.total_us,
        max_us: a.max_us,
        native_scaled: a.native_scaled,
    }
}
