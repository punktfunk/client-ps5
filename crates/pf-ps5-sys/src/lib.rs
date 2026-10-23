//! The console's C ABI, hand-written, and nothing else.
//!
//! Every struct the client passes to the console is pinned by `tests/layout.rs` against the
//! sizes and offsets the sources in `docs/NOTES.md` agree on. The functions resolve at link time
//! through the payload SDK's stub libraries (and `scripts/stubs/` for the modules the SDK has no
//! stub for); nothing here links on another OS, and nothing outside the platform crates calls it.
//!
//! Conventions: every SCE struct is zeroed and its leading `size` set before use; errors are
//! negative `i32`s, `0x8xxxxxxx`.
#![allow(non_camel_case_types, non_snake_case)]

pub mod audio;
pub mod kernel;
pub mod net;
pub mod pad;
pub mod shim;
pub mod sysmodule;
pub mod system;
pub mod user;
pub mod videodec2;
#[cfg(target_vendor = "sie")]
pub mod videoout;
pub mod vk;

/// An SCE error code as the operator log line spells it.
pub fn sce_error(code: i32) -> String {
    format!("0x{:08x}", code as u32)
}
