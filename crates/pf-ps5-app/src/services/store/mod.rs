//! Persistence: the `settings.json` document, plus the client identity PEMs beside it.
//!
//! - [`load`] / [`save`] read and write the whole [`Persisted`] document.
//! - [`StateWriter`] is what the app actually saves through: off-thread, coalescing.
//! - [`load_or_create_identity`] handles the PEM pair, which stays outside the document.
pub mod console;
mod identity;
pub mod shared;
mod writer;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};

use crate::core::VERSION;

pub use crate::core::model::{
    seed_new_host_profiles, upsert_known_host, CodecPref, ExitAction, GamepadType, Persisted,
};
pub use crate::core::settings::Ps5Settings;
pub use crate::services::paths::app_dir;
pub use identity::load_or_create_identity;
pub use pf_client_core::trust::Settings;
pub use writer::StateWriter;

fn path() -> PathBuf {
    app_dir().join("settings.json")
}

/// Set by a [`load`] that found the document and could not read it: [`save`] then refuses,
/// so one bad read never replaces every pairing with defaults.
static UNREADABLE: AtomicBool = AtomicBool::new(false);

/// Loads the whole persisted document. Absent, unreadable and unparseable all answer with
/// defaults: a bad file must not take the app down.
pub fn load() -> Persisted {
    let (state, readable) = read(&path());
    UNREADABLE.store(!readable, Ordering::Relaxed);
    // No migration: a document that doesn't deserialize answers with defaults.
    let mut state = state.unwrap_or_default();
    stamp_version(&mut state);
    // A document written by another client can hold picks this console cannot present.
    state.settings.clamp_to_caps();
    state
}

/// The document at `path`, and whether a save may replace it. A missing file is a first run.
/// One that does not parse is copied to `<name>.bad` first. One that exists but can't be read
/// stays as it is: the app runs on defaults and saves nothing.
fn read(path: &Path) -> (Option<Persisted>, bool) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (None, true),
        Err(e) => {
            tracing::warn!(error = %e, "settings.json not read, so nothing is saved over it");
            return (None, false);
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(state) => (Some(state), true),
        Err(e) => {
            tracing::warn!(error = %e, "settings.json did not parse, so defaults replace it");
            set_aside(path, &bytes);
            (None, true)
        }
    }
}

/// Keeps an unparsable document's bytes as `<name>.bad`, so the defaults saved next do not
/// destroy them. The first copy stays until it is deleted. Best effort.
fn set_aside(path: &Path, bytes: &[u8]) {
    let mut bad = path.as_os_str().to_os_string();
    bad.push(".bad");
    let bad = PathBuf::from(bad);
    if bad.exists() {
        return;
    }
    match crate::services::atomic::write_parts(&bad, &[bytes], "settings.json.bad") {
        Ok(()) => tracing::warn!(kept = %bad.display(), "unparsable settings.json kept"),
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "unparsable settings.json not kept"),
    }
}

/// Brings [`Persisted::version`] up to this build's.
///
/// Written synchronously so it happens once — `StateWriter`'s baseline is taken after this,
/// and an unstamped document that never gets saved would stamp again on every launch.
fn stamp_version(state: &mut Persisted) {
    if state.version.as_deref() == Some(VERSION) {
        return;
    }
    state.version = Some(VERSION.to_string());
    match save(state) {
        Ok(()) => tracing::info!("stamped settings.json with version {VERSION}"),
        Err(e) => tracing::warn!("stamp document version: {e:#}"),
    }
}

/// Writes the whole document: its `settings` object is the shared schema verbatim (see [`shared`]).
/// Refused while the document on disk could not be read.
pub fn save(state: &Persisted) -> Result<()> {
    anyhow::ensure!(
        !UNREADABLE.load(Ordering::Relaxed),
        "settings.json exists but can't be read"
    );
    let json = serde_json::to_string_pretty(state).context("serialize app state")?;
    crate::services::atomic::write(&path(), &json, "settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a missing or unparsable document reads as empty and saveable. The unparsable one
    /// is kept as `.bad`, and one that can't be read turns saving off.
    #[test]
    fn only_a_missing_or_broken_document_may_be_replaced() {
        let dir = std::env::temp_dir().join(format!("pf-ps5-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(read(&dir.join("nope.json")), (None, true)));

        let broken = dir.join("broken.json");
        std::fs::write(&broken, b"{\"known_hosts\":[").unwrap();
        assert!(matches!(read(&broken), (None, true)));
        assert_eq!(
            std::fs::read(dir.join("broken.json.bad")).unwrap(),
            b"{\"known_hosts\":["
        );

        // A directory under the name: it exists, and no read succeeds.
        let unreadable = dir.join("unreadable.json");
        std::fs::create_dir_all(&unreadable).unwrap();
        assert!(matches!(read(&unreadable), (None, false)));

        let good = dir.join("good.json");
        std::fs::write(&good, serde_json::to_vec(&Persisted::default()).unwrap()).unwrap();
        assert!(matches!(read(&good), (Some(_), true)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
