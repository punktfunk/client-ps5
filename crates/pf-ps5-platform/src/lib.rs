//! The console's side of the PS5 client's seams.
//!
//! The pure halves (pad mapping, trigger translation) build and test anywhere; everything that
//! calls the console is `cfg(target_vendor = "sie")`. The binary calls [`platform`] once and
//! hands the result to `pf_ps5_app::run`.

pub mod fonts;
pub mod language;
pub mod pad_map;
pub mod triggers;

#[cfg(target_vendor = "sie")]
mod console;

#[cfg(target_vendor = "sie")]
pub use console::{platform, probe_parts};
