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
