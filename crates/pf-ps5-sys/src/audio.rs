//! libSceAudioOut: a port at 48 kHz, written one grain at a time; `sceAudioOutOutput` blocks
//! for the grain.

use core::ffi::c_void;

pub const PORT_MAIN: i32 = 0;
/// A pad's voice coils: stereo, left and right actuator, opened for the user who holds the pad.
pub const PORT_VIBRATION: i32 = 10;
pub const FORMAT_S16_STEREO: u32 = 1;
pub const FORMAT_S16_8CH: u32 = 2;
pub const FORMAT_FLOAT_STEREO: u32 = 4;
/// Grain lengths the port accepts: multiples of 256 up to 2048.
pub const GRAIN_FRAMES: u32 = 256;
pub const ERROR_ALREADY_INIT: i32 = 0x8026_000e_u32 as i32;

extern "C" {
    pub fn sceAudioOutInit() -> i32;
    /// Returns a handle ≥ 1, or a negative error.
    pub fn sceAudioOutOpen(user_id: i32, port_type: i32, index: i32, length: u32, rate: u32, format: u32) -> i32;
    /// `buf` null drains the queued grain.
    pub fn sceAudioOutOutput(handle: i32, buf: *const c_void) -> i32;
    pub fn sceAudioOutClose(handle: i32) -> i32;
}
