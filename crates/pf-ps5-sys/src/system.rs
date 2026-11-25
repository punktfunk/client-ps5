//! libSceSystemService: the splash, the system's language, and the one correct way out of a title.

use core::ffi::c_char;

/// The `param_id` of the system language: one of the `SCE_SYSTEM_PARAM_LANG_*` values.
pub const PARAM_ID_LANG: i32 = 1;

extern "C" {
    /// Reads one numeric system parameter (`PARAM_ID_*`) into `value`.
    pub fn sceSystemServiceParamGetInt(param_id: i32, value: *mut i32) -> i32;
    /// Before the first present: the shell's splash covers the title until then.
    pub fn sceSystemServiceHideSplashScreen() -> i32;
    /// `("exit", null)` returns the console to the home screen and the shell ends the process
    /// asynchronously. Returning from `main` or calling `exit` is recorded as a crash.
    pub fn sceSystemServiceLoadExec(path: *const c_char, argv: *const *const c_char) -> i32;
}

/// `sceSystemServiceGetStatus`'s answer and the head of its record, as hex: an event count,
/// then the flags a title reads for the system's overlay and for running in the background
/// (shadPS4 `systemservice.h`). `Err` is the lookup's or the call's code.
#[cfg(target_vendor = "sie")]
pub fn status_probe() -> Result<String, i32> {
    use crate::kernel::{export, module};
    let get = export(module("libSceSystemService.sprx")?, c"sceSystemServiceGetStatus")?;
    // SAFETY: the module's export under its own name: one pointer to the record it fills.
    let get = unsafe { core::mem::transmute::<*mut core::ffi::c_void, unsafe extern "C" fn(*mut u8) -> i32>(get) };
    // The header names the record's first nine bytes and leaves its length open.
    let mut record = [0_u8; 1024];
    // SAFETY: `record` is live and longer than the record.
    let rc = unsafe { get(record.as_mut_ptr()) };
    if rc != 0 {
        return Err(rc);
    }
    let head: Vec<String> = record[..16].iter().map(|b| format!("{b:02x}")).collect();
    Ok(head.join(" "))
}
