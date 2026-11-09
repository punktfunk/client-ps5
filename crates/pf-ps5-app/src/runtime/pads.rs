//! Pad samples into what the loops act on: wire input for the host, menu events for the console,
//! the dial's own presses, and the shortcuts. Pure: samples in, events out.
//!
//! Each pad runs a shortcut [`Engine`] first; only what it forwards reaches the host or the
//! open dial. The defaults are `Client::Ps5`'s: the touchpad click stands in for Create.

use std::time::Instant;

use pf_client_core::menu_nav::{MenuEvent, MenuSample};
use pf_client_core::overlay_actions::{OverlayConfig, RingPlatform, SlotId};
use pf_client_core::ring::RingCommand;
use pf_client_core::shortcuts::{resolve, Action, Binding, Client, Device, Engine, Event, Out, Phase};
use punktfunk_core::input::{gamepad, key_vk, InputEvent, InputKind};

use crate::core::dial::{PadDial, PadRoute};
use crate::core::model::GamepadType;
use crate::platform::{PadKind, PadSample};

/// Slots the console reports: one per signed-in user.
pub const SLOTS: usize = 4;

/// Every button bit a `DualSense` reports, for releases.
const BUTTONS: [u32; 18] = [
    gamepad::BTN_A,
    gamepad::BTN_B,
    gamepad::BTN_X,
    gamepad::BTN_Y,
    gamepad::BTN_BACK,
    gamepad::BTN_GUIDE,
    gamepad::BTN_START,
    gamepad::BTN_LS_CLICK,
    gamepad::BTN_RS_CLICK,
    gamepad::BTN_LB,
    gamepad::BTN_RB,
    gamepad::BTN_DPAD_UP,
    gamepad::BTN_DPAD_DOWN,
    gamepad::BTN_DPAD_LEFT,
    gamepad::BTN_DPAD_RIGHT,
    gamepad::BTN_TOUCHPAD,
    gamepad::BTN_MISC1,
    gamepad::BTN_PADDLE1,
];

/// What one step produced besides wire input.
#[derive(Clone, Debug, PartialEq)]
pub enum Routed {
    /// A shortcut opened or closed the dial, or the dial's Back closed it.
    ToggleDial,
    /// A press the open dial takes.
    Dial(MenuEvent),
    /// A shortcut ran one of the dial's commands.
    Command(RingCommand),
}

/// The ring blob's key combinations: id, then the keys as VKs.
type Keys = [(String, Vec<u8>)];

struct Slot {
    last: PadSample,
    dial: PadDial,
    shortcuts: Engine,
}

/// Per-slot state across samples.
pub struct PadRouter {
    slots: [Option<Slot>; SLOTS],
    /// What each pad's engine starts from.
    bindings: Vec<Binding>,
    keys: Vec<(String, Vec<u8>)>,
}

impl PadRouter {
    /// `shortcuts` and `overlay_actions` are the settings blobs; an empty one is the defaults.
    /// Keeps the bindings this console acts on: the dial, the commands [`command`] maps, and
    /// the ring's key combinations.
    pub fn new(shortcuts: &str, overlay_actions: &str) -> Self {
        let keys: Vec<(String, Vec<u8>)> = OverlayConfig::parse(overlay_actions, RingPlatform::Desktop)
            .shortcuts
            .into_iter()
            .filter_map(|s| {
                let vks: Option<Vec<u8>> = s.keys.iter().map(|k| key_vk(k)).collect();
                vks.filter(|v| !v.is_empty()).map(|v| (s.id, v))
            })
            .collect();
        let bindings = resolve(shortcuts, Client::Ps5, false, "")
            .bindings
            .into_iter()
            .filter(|b| match &b.action {
                Action::Ring => true,
                Action::Slot(SlotId::Shortcut(id)) => keys.iter().any(|(k, _)| k == id),
                Action::Slot(slot) => command(slot).is_some(),
                Action::Unknown(_) => false,
            })
            .collect();
        Self {
            slots: Default::default(),
            bindings,
            keys,
        }
    }

    /// Diffs one batch against the last. `dial_open` is whether the dial is up now; `kind` is
    /// the pad kind the session asked for (`Auto` mirrors the pad). Wire events land in `wire`.
    pub fn step(
        &mut self,
        batch: &[PadSample],
        now: Instant,
        dial_open: bool,
        kind: GamepadType,
        wire: &mut Vec<InputEvent>,
        routed: &mut Vec<Routed>,
    ) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let pad = index as u8;
            let Some(sample) = batch.iter().find(|s| usize::from(s.slot) == index) else {
                if let Some(gone) = slot.take() {
                    gone.release(&self.keys, wire);
                    wire.push(remove_event(pad));
                }
                continue;
            };
            let state = slot.get_or_insert_with(|| {
                wire.extend(arrival_event(resolve_kind(kind, sample.kind), pad));
                Slot {
                    last: PadSample {
                        slot: sample.slot,
                        kind: sample.kind,
                        ..PadSample::default()
                    },
                    dial: PadDial::default(),
                    shortcuts: Engine::new(&self.bindings, Device::Pad),
                }
            });
            diff(state, sample, now, dial_open, &self.keys, wire, routed);
        }
    }

    /// The dial opened: release what the host still holds from every pad.
    pub fn dial_opened(&mut self, wire: &mut Vec<InputEvent>) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(slot) = slot {
                let sent = slot.dial.opened();
                for bit in BUTTONS.into_iter().filter(|b| sent & b != 0) {
                    wire.push(bit_event(bit, false, index as u8));
                }
            }
        }
    }

    pub fn dial_closed(&mut self) {
        for slot in self.slots.iter_mut().flatten() {
            slot.dial.closed();
        }
    }

    /// Lets go of everything the host holds and forgets the pads, so the next step re-declares
    /// them. For focus loss and every way out of a stream.
    pub fn release_all(&mut self, wire: &mut Vec<InputEvent>) {
        for slot in &mut self.slots {
            if let Some(gone) = slot.take() {
                gone.release(&self.keys, wire);
            }
        }
    }
}

impl Slot {
    /// The engine's outputs: forwarded edges to the open dial or the host, lifts as releases,
    /// fired shortcuts to the loops or the host's keyboard.
    fn apply(
        &mut self,
        out: Vec<Out>,
        dial_open: bool,
        keys: &Keys,
        wire: &mut Vec<InputEvent>,
        routed: &mut Vec<Routed>,
    ) {
        for o in out {
            match o {
                Out::Forward(bit, down) => self.route(bit, down, dial_open, wire, routed),
                Out::Lift(bits) => {
                    for bit in bits {
                        self.route(bit, false, dial_open, wire, routed);
                    }
                }
                Out::Fire(Action::Ring, _) => routed.push(Routed::ToggleDial),
                Out::Fire(Action::Slot(SlotId::Shortcut(id)), phase) => {
                    if let Some((_, vks)) = keys.iter().find(|(k, _)| *k == id) {
                        key_events(vks, phase, wire);
                    }
                }
                Out::Fire(Action::Slot(slot), Phase::Once) => routed.extend(command(&slot).map(Routed::Command)),
                Out::Fire(..) => {}
            }
        }
    }

    /// One edge the engine let through: the open dial takes it, else the host. The dial also
    /// drops a release whose press it took, so the host never sees half a press.
    fn route(&mut self, bit: u32, down: bool, dial_open: bool, wire: &mut Vec<InputEvent>, routed: &mut Vec<Routed>) {
        match self.dial.button(bit, down, dial_open) {
            PadRoute::Forward => wire.push(bit_event(bit, down, self.last.slot)),
            PadRoute::Drop => {}
            PadRoute::Menu(MenuEvent::Back) => routed.push(Routed::ToggleDial),
            PadRoute::Menu(ev) => routed.push(Routed::Dial(ev)),
        }
    }

    /// The pad is going: the host lets go of its buttons and a held key combination, and its
    /// axes centre. Every button the host holds is one the engine forwarded or still owes.
    fn release(mut self, keys: &Keys, wire: &mut Vec<InputEvent>) {
        let out = self.shortcuts.flush();
        self.apply(out, false, keys, wire, &mut Vec::new());
        let last = self.last;
        let axes = [
            (gamepad::AXIS_LS_X, last.lx),
            (gamepad::AXIS_LS_Y, last.ly),
            (gamepad::AXIS_RS_X, last.rx),
            (gamepad::AXIS_RS_Y, last.ry),
            (gamepad::AXIS_LT, i16::from(last.lt)),
            (gamepad::AXIS_RT, i16::from(last.rt)),
        ];
        for (axis, value) in axes {
            if value != 0 {
                wire.push(axis_event(axis, 0, last.slot));
            }
        }
    }
}

/// The dial command a shortcut runs: only those `stream::run_command` acts on.
fn command(slot: &SlotId) -> Option<RingCommand> {
    match slot {
        SlotId::EndStream | SlotId::DisconnectLinger | SlotId::Stats => RingCommand::for_slot(slot),
        _ => None,
    }
}

/// A key combination on the host: every key down in order, up in reverse, or both for a tap.
fn key_events(vks: &[u8], phase: Phase, wire: &mut Vec<InputEvent>) {
    let key = |kind, vk: u8| InputEvent {
        kind,
        _pad: [0; 3],
        code: u32::from(vk),
        x: 0,
        y: 0,
        flags: 0,
    };
    if phase != Phase::Up {
        wire.extend(vks.iter().map(|&vk| key(InputKind::KeyDown, vk)));
    }
    if phase != Phase::Down {
        wire.extend(vks.iter().rev().map(|&vk| key(InputKind::KeyUp, vk)));
    }
}

fn resolve_kind(setting: GamepadType, pad: PadKind) -> GamepadType {
    match (setting, pad) {
        (GamepadType::Auto, PadKind::DualSense) => GamepadType::DualSense,
        (GamepadType::Auto, PadKind::DualSenseEdge) => GamepadType::DualSenseEdge,
        (setting, _) => setting,
    }
}

/// One pad's sample against its last: the engine's due tick, its button edges, then the axes.
fn diff(
    state: &mut Slot,
    now: &PadSample,
    at: Instant,
    dial_open: bool,
    keys: &Keys,
    wire: &mut Vec<InputEvent>,
    routed: &mut Vec<Routed>,
) {
    let pad = state.last.slot;
    let out = state.shortcuts.tick(at);
    state.apply(out, dial_open, keys, wire, routed);
    let changed = state.last.buttons ^ now.buttons;
    for bit in BUTTONS.into_iter().filter(|b| changed & b != 0) {
        let ev = if now.buttons & bit != 0 {
            Event::Press(bit)
        } else {
            Event::Release(bit)
        };
        let out = state.shortcuts.step(ev, at);
        state.apply(out, dial_open, keys, wire, routed);
    }
    let axes = [
        (gamepad::AXIS_LS_X, state.last.lx, now.lx),
        (gamepad::AXIS_LS_Y, state.last.ly, now.ly),
        (gamepad::AXIS_RS_X, state.last.rx, now.rx),
        (gamepad::AXIS_RS_Y, state.last.ry, now.ry),
    ];
    for (axis, was, is) in axes {
        if was == is {
            continue;
        }
        let left = axis == gamepad::AXIS_LS_X || axis == gamepad::AXIS_LS_Y;
        if left && dial_open {
            // The dial aims with the menu convention, +y down.
            let value = if axis == gamepad::AXIS_LS_Y {
                is.saturating_neg()
            } else {
                is
            };
            if let Some(ev) = state.dial.left_stick(axis == gamepad::AXIS_LS_X, value, true) {
                routed.push(Routed::Dial(ev));
            }
            continue;
        }
        wire.push(axis_event(axis, i32::from(is), pad));
    }
    for (axis, was, is) in [
        (gamepad::AXIS_LT, state.last.lt, now.lt),
        (gamepad::AXIS_RT, state.last.rt, now.rt),
    ] {
        if was != is {
            wire.push(axis_event(axis, i32::from(is), pad));
        }
    }
    state.last = *now;
}

/// The pads folded into the one sample the shared menu synthesizer steps: buttons OR'd, the
/// stick from whichever is furthest off centre.
pub fn menu_sample(batch: &[PadSample]) -> MenuSample {
    let mut out = MenuSample::default();
    let mut best = -1i64;
    for s in batch {
        let b = |bit: u32| s.buttons & bit != 0;
        for (o, v) in out.buttons.iter_mut().zip([
            b(gamepad::BTN_A),
            b(gamepad::BTN_B),
            b(gamepad::BTN_X),
            b(gamepad::BTN_Y),
            b(gamepad::BTN_LB),
            b(gamepad::BTN_RB),
        ]) {
            *o |= v;
        }
        for (o, v) in out.dpad.iter_mut().zip([
            b(gamepad::BTN_DPAD_UP),
            b(gamepad::BTN_DPAD_DOWN),
            b(gamepad::BTN_DPAD_LEFT),
            b(gamepad::BTN_DPAD_RIGHT),
        ]) {
            *o |= v;
        }
        let mag = i64::from(s.lx).pow(2) + i64::from(s.ly).pow(2);
        if mag > best {
            best = mag;
            out.lx = s.lx;
            // The synthesizer reads +y as down; the wire's +y is up.
            out.ly = s.ly.saturating_neg();
        }
    }
    out
}

pub fn bit_event(bit: u32, pressed: bool, pad: u8) -> InputEvent {
    InputEvent {
        kind: InputKind::GamepadButton,
        _pad: [0; 3],
        code: bit,
        x: i32::from(pressed),
        y: 0,
        flags: u32::from(pad),
    }
}

/// Sticks in the wire's range and sign (+y up), triggers 0..=255.
pub fn axis_event(axis: u32, value: i32, pad: u8) -> InputEvent {
    InputEvent {
        kind: InputKind::GamepadAxis,
        _pad: [0; 3],
        code: axis,
        x: value,
        y: 0,
        flags: u32::from(pad),
    }
}

/// Declares pad `pad`'s kind to the host. `None` for `Auto`: the host's own choice is what it means.
pub fn arrival_event(kind: GamepadType, pad: u8) -> Option<InputEvent> {
    let pref = kind.to_core();
    if pref == punktfunk_core::config::GamepadPref::Auto {
        return None;
    }
    Some(InputEvent {
        kind: InputKind::GamepadArrival,
        _pad: [0; 3],
        code: u32::from(pref.to_u8()),
        x: 0,
        y: 0,
        flags: punktfunk_core::input::encode_gamepad_arrival(pad, 0),
    })
}

/// Pad `pad` is gone: the host tears its virtual device down.
pub fn remove_event(pad: u8) -> InputEvent {
    InputEvent {
        kind: InputKind::GamepadRemove,
        _pad: [0; 3],
        code: 0,
        x: 0,
        y: 0,
        flags: punktfunk_core::input::encode_gamepad_remove(pad, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_client_core::menu_nav::MenuDir;
    use pf_client_core::shortcuts::HOLD;
    use std::time::Duration;

    const TOUCHPAD: u32 = gamepad::BTN_TOUCHPAD;
    /// `Client::Ps5`'s End stream hold.
    const EXIT: u32 = gamepad::BTN_LB | gamepad::BTN_RB | gamepad::BTN_START | TOUCHPAD;

    fn router() -> PadRouter {
        PadRouter::new("", "")
    }

    fn pad(buttons: u32) -> PadSample {
        PadSample {
            slot: 0,
            buttons,
            ..PadSample::default()
        }
    }

    fn step(router: &mut PadRouter, batch: &[PadSample], at: Instant, open: bool) -> (Vec<InputEvent>, Vec<Routed>) {
        let (mut wire, mut routed) = (Vec::new(), Vec::new());
        router.step(batch, at, open, GamepadType::Auto, &mut wire, &mut routed);
        (wire, routed)
    }

    #[test]
    fn a_new_pad_arrives_as_a_dualsense_and_presses_go_out_once() {
        let mut r = router();
        let t = Instant::now();
        let (wire, _) = step(&mut r, &[pad(0)], t, false);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].kind, InputKind::GamepadArrival);
        let (wire, _) = step(&mut r, &[pad(gamepad::BTN_A)], t, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_A, true, 0)]);
        let (wire, _) = step(&mut r, &[pad(gamepad::BTN_A)], t, false);
        assert!(wire.is_empty(), "a held button is not re-sent");
    }

    #[test]
    fn a_vanished_pad_releases_then_leaves() {
        let mut r = router();
        let t = Instant::now();
        step(&mut r, &[pad(gamepad::BTN_A)], t, false);
        let (wire, _) = step(&mut r, &[], t, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_A, false, 0), remove_event(0)]);
    }

    #[test]
    fn touchpad_and_cross_open_the_dial_and_cross_never_reaches_the_host() {
        let mut r = router();
        let t = Instant::now();
        step(&mut r, &[pad(0)], t, false);
        let (wire, _) = step(&mut r, &[pad(TOUCHPAD)], t, false);
        assert_eq!(wire, [bit_event(TOUCHPAD, true, 0)]);
        let (wire, routed) = step(&mut r, &[pad(TOUCHPAD | gamepad::BTN_A)], t, false);
        assert_eq!(
            wire,
            [bit_event(TOUCHPAD, false, 0)],
            "the host lets go of the touchpad"
        );
        assert_eq!(routed, [Routed::ToggleDial]);
        let (wire, _) = step(&mut r, &[pad(0)], t, true);
        assert!(wire.is_empty(), "the chord's releases stay local");
    }

    #[test]
    fn the_open_dial_takes_presses_and_back_closes_it() {
        let mut r = router();
        let t = Instant::now();
        step(&mut r, &[pad(0)], t, true);
        let (wire, routed) = step(&mut r, &[pad(gamepad::BTN_DPAD_RIGHT)], t, true);
        assert!(wire.is_empty());
        assert_eq!(routed, [Routed::Dial(MenuEvent::Move(MenuDir::Right))]);
        let (wire, routed) = step(&mut r, &[pad(gamepad::BTN_B)], t, true);
        assert!(wire.is_empty());
        assert_eq!(routed, [Routed::ToggleDial]);
    }

    #[test]
    fn the_exit_chord_ends_the_stream_once_after_its_hold() {
        let mut r = router();
        let t = Instant::now();
        let (wire, _) = step(&mut r, &[pad(EXIT)], t, false);
        assert!(
            !wire.contains(&bit_event(TOUCHPAD, true, 0)),
            "the completing press waits"
        );
        let (_, routed) = step(&mut r, &[pad(EXIT)], t + Duration::from_millis(500), false);
        assert!(routed.is_empty());
        let (wire, routed) = step(&mut r, &[pad(EXIT)], t + HOLD, false);
        assert_eq!(routed, [Routed::Command(RingCommand::EndStream)]);
        assert!(
            wire.contains(&bit_event(gamepad::BTN_START, false, 0)),
            "the rest of the chord lifts"
        );
        assert!(!wire.contains(&bit_event(TOUCHPAD, true, 0)));
        let (_, routed) = step(&mut r, &[pad(EXIT)], t + HOLD * 2, false);
        assert!(routed.is_empty(), "re-arms only after a release");
    }

    #[test]
    fn a_key_combination_taps_on_the_host_and_an_action_this_console_lacks_is_game_input() {
        let shortcuts = r#"{"v":1,"bindings":[
            {"on":"pad","keys":["touchpad","y"],"do":"shortcut:s1"},
            {"on":"pad","keys":["touchpad","x"],"do":"mic"}]}"#;
        let ring = r#"{"v":2,"shortcuts":[{"id":"s1","keys":["alt","tab"]}]}"#;
        let mut r = PadRouter::new(shortcuts, ring);
        let t = Instant::now();
        step(&mut r, &[pad(TOUCHPAD)], t, false);
        let key = |kind, name| InputEvent {
            kind,
            _pad: [0; 3],
            code: u32::from(key_vk(name).unwrap()),
            x: 0,
            y: 0,
            flags: 0,
        };
        let (wire, _) = step(&mut r, &[pad(TOUCHPAD | gamepad::BTN_Y)], t, false);
        assert_eq!(
            wire,
            [
                bit_event(TOUCHPAD, false, 0),
                key(InputKind::KeyDown, "alt"),
                key(InputKind::KeyDown, "tab"),
                key(InputKind::KeyUp, "tab"),
                key(InputKind::KeyUp, "alt"),
            ]
        );
        let (wire, routed) = step(&mut r, &[pad(TOUCHPAD | gamepad::BTN_X)], t, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_X, true, 0)], "the console has no mic");
        assert!(routed.is_empty());
    }

    #[test]
    fn release_all_centres_sticks_and_lifts_buttons() {
        let mut r = router();
        let t = Instant::now();
        let held = PadSample {
            lx: 1000,
            rt: 200,
            ..pad(gamepad::BTN_B)
        };
        step(&mut r, &[held], t, false);
        let mut wire = Vec::new();
        r.release_all(&mut wire);
        assert!(wire.contains(&bit_event(gamepad::BTN_B, false, 0)));
        assert!(wire.contains(&axis_event(gamepad::AXIS_LS_X, 0, 0)));
        assert!(wire.contains(&axis_event(gamepad::AXIS_RT, 0, 0)));
    }

    #[test]
    fn the_menu_sample_flips_y_and_ors_buttons() {
        let a = PadSample {
            ly: 20_000,
            ..pad(gamepad::BTN_A)
        };
        let b = PadSample {
            slot: 1,
            ..pad(gamepad::BTN_DPAD_LEFT)
        };
        let s = menu_sample(&[a, b]);
        assert!(s.buttons[0] && s.dpad[2]);
        assert_eq!(s.ly, -20_000);
    }
}
