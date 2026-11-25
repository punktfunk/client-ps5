//! Touchpad contacts and motion on their way to the host: the rich-input plane beside a pad's
//! buttons and sticks. Core decides what a contact does there: the host pad's touchpad, or the
//! pointer under controller mouse. Pure, so it tests anywhere.

use punktfunk_core::quic::RichInput;

use super::pads::{resolve_kind, SLOTS};
use crate::core::model::GamepadType;
use crate::platform::{PadSample, PadTouch};

#[derive(Default)]
struct Slot {
    /// The contact each wire finger carries.
    fingers: [Option<PadTouch>; 2],
    /// The accelerometer last sent with a live gyro, which is owed its stop.
    turning: Option<[i16; 3]>,
}

/// What the host holds of each pad's touchpad and gyro.
#[derive(Default)]
pub struct RichPads {
    slots: [Slot; SLOTS],
}

impl RichPads {
    /// Diffs one batch against what the host holds. `quiet` is the dial up, or the way out
    /// of a stream: every finger lifts and every gyro stops. `kind` is the pad kind the
    /// session asked for.
    pub fn step(&mut self, batch: &[PadSample], quiet: bool, kind: GamepadType, out: &mut Vec<RichInput>) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let pad = index as u8;
            let sample = batch.iter().find(|s| usize::from(s.slot) == index).filter(|_| !quiet);
            let touches = sample.map_or([None; 2], |s| s.touch);
            for (finger, held) in slot.fingers.iter_mut().enumerate() {
                let Some(was) = *held else { continue };
                let now = touches.iter().flatten().find(|t| t.id == was.id).copied();
                if now != Some(was) {
                    *held = now;
                    out.push(contact(pad, finger, now));
                }
            }
            for touch in touches.iter().flatten() {
                let held = slot.fingers.iter().flatten().any(|h| h.id == touch.id);
                if let Some(finger) = slot.fingers.iter().position(Option::is_none).filter(|_| !held) {
                    slot.fingers[finger] = Some(*touch);
                    out.push(contact(pad, finger, Some(*touch)));
                }
            }
            let motion = sample
                .filter(|s| has_motion(resolve_kind(kind, s.kind)))
                .and_then(|s| s.motion);
            match motion {
                Some(m) => {
                    slot.turning = Some(m.accel);
                    out.push(RichInput::Motion {
                        pad,
                        gyro: m.gyro,
                        accel: m.accel,
                    });
                }
                // The host holds a gyro's last rate: a pad that stops reporting would keep
                // turning. Gravity stays.
                None => out.extend(slot.turning.take().map(|accel| RichInput::Motion {
                    pad,
                    gyro: [0; 3],
                    accel,
                })),
            }
        }
    }
}

/// The host's pad for `kind` has motion sensors. Only its Xbox pad has none.
fn has_motion(kind: GamepadType) -> bool {
    kind != GamepadType::XboxOne
}

fn contact(pad: u8, finger: usize, touch: Option<PadTouch>) -> RichInput {
    let at = touch.unwrap_or_default();
    RichInput::Touchpad {
        pad,
        finger: finger as u8,
        active: touch.is_some(),
        x: at.x,
        y: at.y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::PadMotion;

    fn touching(ids: &[u8]) -> PadSample {
        let mut sample = PadSample::default();
        for (slot, &id) in sample.touch.iter_mut().zip(ids) {
            *slot = Some(PadTouch {
                id,
                x: u16::from(id) * 100,
                y: 5,
            });
        }
        sample
    }

    fn step(rich: &mut RichPads, batch: &[PadSample], quiet: bool) -> Vec<RichInput> {
        let mut out = Vec::new();
        rich.step(batch, quiet, GamepadType::Auto, &mut out);
        out
    }

    fn down(finger: u8, id: u8) -> RichInput {
        RichInput::Touchpad {
            pad: 0,
            finger,
            active: true,
            x: u16::from(id) * 100,
            y: 5,
        }
    }

    fn lift(finger: u8) -> RichInput {
        RichInput::Touchpad {
            pad: 0,
            finger,
            active: false,
            x: 0,
            y: 0,
        }
    }

    #[test]
    fn a_finger_keeps_its_wire_slot_while_another_comes_and_goes() {
        let mut rich = RichPads::default();
        assert_eq!(step(&mut rich, &[touching(&[5])], false), [down(0, 5)]);
        assert_eq!(step(&mut rich, &[touching(&[5, 6])], false), [down(1, 6)]);
        assert_eq!(
            step(&mut rich, &[touching(&[5, 6])], false),
            [],
            "a still finger sends nothing"
        );
        // The pad packs its records: finger 5 lifts and 6 moves up to the first record.
        assert_eq!(step(&mut rich, &[touching(&[6])], false), [lift(0)]);
        assert_eq!(step(&mut rich, &[touching(&[6, 7])], false), [down(0, 7)]);
    }

    #[test]
    fn the_dial_lifts_every_finger_and_stops_the_gyro() {
        let mut rich = RichPads::default();
        let mut sample = touching(&[5]);
        sample.motion = Some(PadMotion {
            gyro: [40, 0, 0],
            accel: [0, 10_000, 0],
        });
        assert_eq!(step(&mut rich, &[sample], false).len(), 2);
        let stopped = RichInput::Motion {
            pad: 0,
            gyro: [0; 3],
            accel: [0, 10_000, 0],
        };
        assert_eq!(step(&mut rich, &[sample], true), [lift(0), stopped]);
        assert_eq!(step(&mut rich, &[sample], true), [], "nothing is owed twice");
    }

    #[test]
    fn the_hosts_xbox_pad_gets_no_motion() {
        let mut rich = RichPads::default();
        let sample = PadSample {
            motion: Some(PadMotion::default()),
            ..PadSample::default()
        };
        let mut out = Vec::new();
        rich.step(&[sample], false, GamepadType::XboxOne, &mut out);
        assert_eq!(out, []);
    }
}
