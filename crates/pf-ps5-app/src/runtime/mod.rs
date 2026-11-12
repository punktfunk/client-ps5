//! The title's life: start-up, then the menu and the stream in turn, then the one exit.
//!
//! [`run`] never returns. The console records a title that returns from `main` as a crash, so
//! every way out goes through [`crate::platform::Lifecycle::exit`] after the logs are flushed.

mod connect;
mod hud;
mod menu;
pub mod pads;
mod stream;
pub mod testrun;

use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::model::BITRATE_MAX_KBPS;
use crate::platform::{Display, Lifecycle, Pads, Platform, SystemEvent};
use crate::services::power::ExitPlan;
use crate::services::store;
use crate::session::{self, Media};

/// The halves of the platform the loops share, owned by the main thread.
pub(crate) struct App {
    pub display: Box<dyn Display>,
    /// `None` while the stream's input thread holds them.
    pub pads: Option<Box<dyn Pads>>,
    pub lifecycle: Box<dyn Lifecycle>,
    pub media: Media,
    pub identity: (String, String),
    /// A scripted run from `test-run.json`, taken at start-up.
    pub test_run: Option<testrun::TestRun>,
    pub device: crate::platform::DeviceInfo,
}

impl App {
    /// System events since the last call. `Suspend` also sets [`SUSPENDED`].
    fn system_events(&mut self) -> Vec<SystemEvent> {
        let mut events = Vec::new();
        self.lifecycle.poll_events(&mut events);
        for event in &events {
            tracing::info!("system: {event:?}");
            match event {
                SystemEvent::Suspend => SUSPENDED.store(true, Ordering::Relaxed),
                SystemEvent::Resume => SUSPENDED.store(false, Ordering::Relaxed),
                SystemEvent::FocusLost | SystemEvent::FocusGained => {}
            }
        }
        events
    }
}

/// Rest mode is on its way: no new session may start.
static SUSPENDED: AtomicBool = AtomicBool::new(false);

/// How one pass through the menu ended.
enum UiOutcome {
    Launch(Box<connect::ConnectOutcome>),
    /// Leave the title, with the selected host's exit action unfired.
    Quit(Option<ExitPlan>),
}

enum StreamOutcome {
    /// Back to the menu, with why if the stream did not end on the player's word.
    ReturnToMenu(Option<String>),
    Quit(Option<ExitPlan>),
}

/// The startup link probe's burst: the connection test's proven-safe target.
const ABR_PROBE_KBPS: u32 = 320_000;

/// The two automatic-bitrate knobs punktfunk-core reads from the environment. Set before any
/// thread exists: `set_var` is not thread-safe.
fn set_abr_env() {
    std::env::set_var("PUNKTFUNK_ABR_PROBE_KBPS", ABR_PROBE_KBPS.to_string());
    std::env::set_var("PUNKTFUNK_ABR_MAX_MBPS", (BITRATE_MAX_KBPS / 1_000).to_string());
}

/// Runs the title until the player leaves it. Never returns.
pub fn run(platform: Platform) -> ! {
    set_abr_env();
    let Platform {
        display,
        video,
        audio,
        pads,
        storage,
        lifecycle,
        log,
        device,
    } = platform;
    // Before any text is read: the menu's, an error's, a notice's.
    let language = pf_l10n::set_languages(&device.languages);
    crate::services::paths::install(storage.data_dir());
    // Without a log there is nothing to diagnose a failure from; the title still runs.
    let flush = crate::logger::init_subscriber(&crate::services::paths::logs_dir(), log).ok();
    install_panic_hook();
    tracing::info!(
        "punktfunk-ps5 {} on {} ({} firmware {}), gpu {}, ui {}",
        crate::core::build_id(),
        device.platform,
        device.model,
        device.firmware,
        device.gpu,
        device.ui,
    );
    tracing::info!("language {language} from {:?}", device.languages);
    crate::core::caps::install(video.caps());
    // ureq's agents resolve their provider from the process default and panic without one.
    punktfunk_core::tls::install_default_provider();
    let mut app = App {
        display,
        pads: Some(pads),
        lifecycle,
        media: Media { video, audio },
        identity: (String::new(), String::new()),
        test_run: testrun::take(),
        device,
    };
    let plan = match store::load_or_create_identity() {
        Ok(identity) => {
            app.identity = identity;
            app.lifecycle.hide_splash();
            alternate(&mut app)
        }
        Err(e) => {
            tracing::error!("client identity: {e:#}");
            None
        }
    };
    // A connect abandoned mid-handshake holds a session the host would refuse the action over.
    connect::await_abandoned();
    if let Some(plan) = plan {
        plan.run();
    }
    tracing::info!("exiting");
    drop(flush);
    app.lifecycle.exit()
}

/// The menu and the stream in turn, until one of them quits.
fn alternate(app: &mut App) -> Option<ExitPlan> {
    let mut notice = None;
    loop {
        let outcome = match menu::run(app, notice.take()) {
            Ok(outcome) => outcome,
            Err(e) => {
                tracing::error!("menu: {e:#}");
                return None;
            }
        };
        match outcome {
            UiOutcome::Quit(plan) => return plan,
            UiOutcome::Launch(launch) => match stream::run(app, *launch) {
                StreamOutcome::ReturnToMenu(why) => notice = why,
                StreamOutcome::Quit(plan) => return plan,
            },
        }
    }
}

/// The system's font files ([`crate::platform::DeviceInfo::fonts`]) as faces for the kit's
/// fallback. A file that is missing or not a face is logged and left out: the kit refuses all
/// faces for one bad one.
fn system_faces(paths: &[std::path::PathBuf]) -> Vec<Vec<u8>> {
    let started = std::time::Instant::now();
    let mgr = skia_safe::FontMgr::new();
    let faces: Vec<Vec<u8>> = paths
        .iter()
        .filter_map(|path| match std::fs::read(path) {
            Ok(bytes) if mgr.new_from_data(skia_safe::Data::new_copy(&bytes), None).is_some() => Some(bytes),
            Ok(_) => {
                tracing::warn!(path = %path.display(), "system font is not a face");
                None
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "system font unreadable");
                None
            }
        })
        .collect();
    if !paths.is_empty() {
        tracing::info!(
            "fonts: {} of {} system faces, {} KiB, in {} ms",
            faces.len(),
            paths.len(),
            faces.iter().map(Vec::len).sum::<usize>() / 1024,
            started.elapsed().as_millis()
        );
    }
    faces
}

/// A panic on any thread goes to the log with its frames, then the process aborts.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            "PANIC on thread {:?}: {info}\n{backtrace}",
            std::thread::current().name().unwrap_or("unnamed"),
        );
        crate::logger::flush();
    }));
}

/// Ends a session nothing will stream, in the order the host expects.
fn teardown(connected: session::Connected) {
    connected.disconnect_quit();
    connected.shutdown_and_quit();
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_or_foreign_file_is_no_face() {
        let dir = std::env::temp_dir().join(format!("pf-faces-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("not-a-font.otf");
        std::fs::write(&text, b"not a font").unwrap();
        assert!(super::system_faces(&[text, dir.join("missing.ttf")]).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
