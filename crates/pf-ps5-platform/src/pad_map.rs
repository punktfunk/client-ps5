//! scePad samples into the app's [`PadSample`]: wire button bits, sticks in the wire's range
//! with +y up, triggers 0..=255, touchpad contacts and motion in the wire's units. Pure, so it
//! tests anywhere.

use pf_ps5_app::platform::{PadKind, PadMotion, PadSample, PadTouch};
use pf_ps5_sys::pad::*;
use punktfunk_core::input::gamepad as wire;

const BUTTONS: [(u32, u32); 15] = [
    (BUTTON_CROSS, wire::BTN_A),
    (BUTTON_CIRCLE, wire::BTN_B),
    (BUTTON_SQUARE, wire::BTN_X),
    (BUTTON_TRIANGLE, wire::BTN_Y),
    (BUTTON_L1, wire::BTN_LB),
    (BUTTON_R1, wire::BTN_RB),
    (BUTTON_L3, wire::BTN_LS_CLICK),
    (BUTTON_R3, wire::BTN_RS_CLICK),
    (BUTTON_OPTIONS, wire::BTN_START),
    (BUTTON_CREATE, wire::BTN_BACK),
    (BUTTON_UP, wire::BTN_DPAD_UP),
    (BUTTON_DOWN, wire::BTN_DPAD_DOWN),
    (BUTTON_LEFT, wire::BTN_DPAD_LEFT),
    (BUTTON_RIGHT, wire::BTN_DPAD_RIGHT),
    (BUTTON_TOUCHPAD, wire::BTN_TOUCHPAD),
];

/// The `DualSense`'s touch surface as scePad reports it: 1920 by 1080.
const TOUCH_MAX: (u16, u16) = (1919, 1079);
/// scePad's angular velocity is rad/s, its acceleration g.
const GYRO_LSB_PER_RAD_S: f32 = wire::MOTION_GYRO_LSB_PER_DEG_S as f32 * 180.0 / std::f32::consts::PI;
const ACCEL_LSB_PER_G: f32 = wire::MOTION_ACCEL_LSB_PER_G as f32;

/// A surface coordinate to the wire's `0..=65535`.
fn span(raw: u16, max: u16) -> u16 {
    (u32::from(raw.min(max)) * 65535 / u32::from(max)) as u16
}

/// A sensor axis to the wire's i16, clipped where the wire's range ends.
fn sensor(value: f32, scale: f32) -> i16 {
    (value * scale).clamp(-32768.0, 32767.0) as i16
}

/// 0..=255 (128 centre) to the wire's i16 range.
fn stick(raw: u8) -> i16 {
    let centred = i32::from(raw) - 128;
    (centred * 32767 / 127).clamp(-32768, 32767) as i16
}

/// One pad's sample, or `None` when it is not connected. A sample the shell has intercepted
/// (home menu, a system dialog) reads as a pad with nothing held, so the host gets releases.
pub fn sample(slot: u8, data: &ScePadData) -> Option<PadSample> {
    if data.connected == 0 {
        return None;
    }
    let mut out = PadSample {
        slot,
        kind: PadKind::DualSense,
        ..PadSample::default()
    };
    if data.buttons & BUTTON_INTERCEPTED != 0 {
        return Some(out);
    }
    out.buttons = BUTTONS
        .iter()
        .filter(|(sce, _)| data.buttons & sce != 0)
        .fold(0, |acc, (_, w)| acc | w);
    out.lx = stick(data.left_x);
    out.ly = stick(data.left_y).saturating_neg();
    out.rx = stick(data.right_x);
    out.ry = stick(data.right_y).saturating_neg();
    out.lt = data.l2;
    out.rt = data.r2;
    for (finger, touch) in data.touch.iter().take(usize::from(data.touch_count)).enumerate() {
        out.touch[finger] = Some(PadTouch {
            id: touch.id,
            x: span(touch.x, TOUCH_MAX.0),
            y: span(touch.y, TOUCH_MAX.1),
        });
    }
    // Gravity never reads zero on every axis: that is a sensor that is off, not a pad in free
    // fall, and the wire's zero would say free fall.
    out.motion = (data.acceleration != [0.0; 3]).then(|| PadMotion {
        gyro: data.angular_velocity.map(|v| sensor(v, GYRO_LSB_PER_RAD_S)),
        accel: data.acceleration.map(|v| sensor(v, ACCEL_LSB_PER_G)),
    });
    Some(out)
}

/// 0..=65535 motor level to scePad's 0..=255.
pub fn motor(level: u16) -> u8 {
    (level >> 8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(buttons: u32) -> ScePadData {
        ScePadData {
            buttons,
            connected: 1,
            left_x: 128,
            left_y: 128,
            right_x: 128,
            right_y: 128,
            ..ScePadData::default()
        }
    }

    #[test]
    fn buttons_map_to_wire_bits() {
        let s = sample(0, &data(BUTTON_CROSS | BUTTON_OPTIONS | BUTTON_TOUCHPAD)).unwrap();
        assert_eq!(s.buttons, wire::BTN_A | wire::BTN_START | wire::BTN_TOUCHPAD);
        assert_eq!((s.lx, s.ly), (0, 0));
    }

    #[test]
    fn up_on_the_stick_is_positive_on_the_wire() {
        let mut d = data(0);
        d.left_y = 0;
        d.right_x = 255;
        let s = sample(0, &d).unwrap();
        assert!(s.ly > 32_000, "{}", s.ly);
        assert_eq!(s.rx, 32767);
    }

    #[test]
    fn an_intercepted_sample_holds_nothing() {
        let mut d = data(BUTTON_CROSS | BUTTON_INTERCEPTED);
        d.touch_count = 1;
        let s = sample(1, &d).unwrap();
        assert_eq!(s.buttons, 0);
        assert_eq!((s.touch, s.motion), ([None; 2], None));
        assert!(sample(0, &ScePadData::default()).is_none());
    }

    #[test]
    fn touches_span_the_wire_and_keep_their_ids() {
        let mut d = data(0);
        d.touch_count = 2;
        d.touch[0] = ScePadTouch {
            x: 0,
            y: 1079,
            id: 7,
            ..ScePadTouch::default()
        };
        d.touch[1] = ScePadTouch {
            x: 1919,
            y: 0,
            id: 8,
            ..ScePadTouch::default()
        };
        let s = sample(0, &d).unwrap();
        assert_eq!(s.touch[0], Some(PadTouch { id: 7, x: 0, y: 65535 }));
        assert_eq!(s.touch[1], Some(PadTouch { id: 8, x: 65535, y: 0 }));
        d.touch_count = 1;
        assert_eq!(
            sample(0, &d).unwrap().touch[1],
            None,
            "a stale second record is no finger"
        );
    }

    #[test]
    fn motion_lands_in_the_wires_units() {
        let mut d = data(0);
        d.acceleration = [0.0, 1.0, 0.0];
        d.angular_velocity = [std::f32::consts::PI, 0.0, -1000.0];
        let m = sample(0, &d).unwrap().motion.unwrap();
        assert_eq!(
            m.accel,
            wire::MOTION_NEUTRAL_ACCEL,
            "a pad lying flat reads the rest pose"
        );
        assert_eq!(m.gyro[0], 180 * wire::MOTION_GYRO_LSB_PER_DEG_S as i16);
        assert_eq!(m.gyro[2], i16::MIN, "past the wire's range clips");
        assert_eq!(
            sample(0, &data(0)).unwrap().motion,
            None,
            "a sensor that is off sends nothing"
        );
    }
}
