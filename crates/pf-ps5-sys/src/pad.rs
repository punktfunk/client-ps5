//! libScePad: one handle per signed-in user, a 120-byte state sample, motors, light bar and the
//! `DualSense` trigger effects.

use core::ffi::c_void;

pub const PORT_STANDARD: i32 = 0;
/// `scePadSetVibrationMode`: the classic two-motor rumble.
pub const VIBRATION_MODE_RUMBLE: i32 = 2;

pub const BUTTON_CREATE: u32 = 0x0000_0001;
pub const BUTTON_L3: u32 = 0x0000_0002;
pub const BUTTON_R3: u32 = 0x0000_0004;
pub const BUTTON_OPTIONS: u32 = 0x0000_0008;
pub const BUTTON_UP: u32 = 0x0000_0010;
pub const BUTTON_RIGHT: u32 = 0x0000_0020;
pub const BUTTON_DOWN: u32 = 0x0000_0040;
pub const BUTTON_LEFT: u32 = 0x0000_0080;
pub const BUTTON_L2: u32 = 0x0000_0100;
pub const BUTTON_R2: u32 = 0x0000_0200;
pub const BUTTON_L1: u32 = 0x0000_0400;
pub const BUTTON_R1: u32 = 0x0000_0800;
pub const BUTTON_TRIANGLE: u32 = 0x0000_1000;
pub const BUTTON_CIRCLE: u32 = 0x0000_2000;
pub const BUTTON_CROSS: u32 = 0x0000_4000;
pub const BUTTON_SQUARE: u32 = 0x0000_8000;
pub const BUTTON_PS: u32 = 0x0001_0000;
pub const BUTTON_TOUCHPAD: u32 = 0x0010_0000;
/// The shell owns the pad (home menu, a system dialog): ignore the sample.
pub const BUTTON_INTERCEPTED: u32 = 0x8000_0000;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScePadTouch {
    pub x: u16,
    pub y: u16,
    pub id: u8,
    pub reserved: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScePadExtensionUnitData {
    pub id: u32,
    pub reserved: u8,
    pub len: u8,
    pub data: [u8; 10],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScePadData {
    pub buttons: u32,
    /// Sticks 0..=255, 128 centre, +y down.
    pub left_x: u8,
    pub left_y: u8,
    pub right_x: u8,
    pub right_y: u8,
    pub l2: u8,
    pub r2: u8,
    pub padding: u16,
    pub orientation: [f32; 4],
    pub acceleration: [f32; 3],
    pub angular_velocity: [f32; 3],
    pub touch_count: u8,
    pub touch_reserved: [u8; 3],
    pub touch_reserved2: u32,
    pub touch: [ScePadTouch; 2],
    pub connected: u8,
    pub connected_padding: [u8; 3],
    pub timestamp_us: u64,
    pub ext: ScePadExtensionUnitData,
    pub connected_count: u8,
    pub reserved: [u8; 2],
    pub device_unique_len: u8,
    pub device_unique: [u8; 12],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScePadVibrationParam {
    pub large_motor: u8,
    pub small_motor: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScePadLightBarParam {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub reserved: u8,
}

pub const TRIGGER_L2: u8 = 0x01;
pub const TRIGGER_R2: u8 = 0x02;
pub const TRIGGER_MODE_OFF: u32 = 0;
pub const TRIGGER_MODE_FEEDBACK: u32 = 1;
pub const TRIGGER_MODE_WEAPON: u32 = 2;
pub const TRIGGER_MODE_VIBRATION: u32 = 3;
pub const TRIGGER_MODE_MULTI_FEEDBACK: u32 = 4;
pub const TRIGGER_MODE_SLOPE_FEEDBACK: u32 = 5;
pub const TRIGGER_MODE_MULTI_VIBRATION: u32 = 6;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ScePadTriggerEffectCommand {
    pub mode: u32,
    pub padding: [u8; 4],
    pub data: [u8; 48],
}

impl Default for ScePadTriggerEffectCommand {
    fn default() -> Self {
        Self {
            mode: TRIGGER_MODE_OFF,
            padding: [0; 4],
            data: [0; 48],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ScePadTriggerEffectParam {
    pub trigger_mask: u8,
    pub padding: [u8; 7],
    /// `[L2, R2]`.
    pub command: [ScePadTriggerEffectCommand; 2],
}

extern "C" {
    pub fn scePadInit() -> i32;
    pub fn scePadOpen(user_id: i32, port_type: i32, index: i32, param: *const c_void) -> i32;
    pub fn scePadClose(handle: i32) -> i32;
    pub fn scePadReadState(handle: i32, data: *mut ScePadData) -> i32;
    pub fn scePadSetVibrationMode(handle: i32, mode: i32) -> i32;
    pub fn scePadSetVibration(handle: i32, param: *const ScePadVibrationParam) -> i32;
    pub fn scePadSetLightBar(handle: i32, param: *const ScePadLightBarParam) -> i32;
    pub fn scePadResetLightBar(handle: i32) -> i32;
    pub fn scePadSetTriggerEffect(handle: i32, param: *const ScePadTriggerEffectParam) -> i32;
}

/// Turns a pad's motion sensor on (the PS4's `scePadSetMotionSensorState(handle, true)`), so
/// its state carries acceleration and angular velocity. Looked up by name: the SDK's stub may
/// lack it, and a firmware without it answers with a code. Returns the call's own answer.
#[cfg(target_vendor = "sie")]
pub fn motion_sensor_on(handle: i32) -> Result<i32, i32> {
    use crate::kernel::{export, module};
    let set = export(module("libScePad.sprx")?, c"scePadSetMotionSensorState")?;
    // SAFETY: the module's export under its own name: a handle and a flag.
    let set = unsafe { core::mem::transmute::<*mut c_void, unsafe extern "C" fn(i32, bool) -> i32>(set) };
    // SAFETY: a handle the caller opened.
    Ok(unsafe { set(handle, true) })
}
