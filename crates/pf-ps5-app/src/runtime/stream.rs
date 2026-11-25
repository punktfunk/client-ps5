//! The stream: the picture on the display's stream layer, the dial, the stats and the start line
//! over it, and the pads on an input thread of their own.
//!
//! The main thread presents each picture as it arrives, and at the display's rate while the
//! dial shows. The input thread polls the pads at 250 Hz, sends their input straight to the
//! host, applies the host's rumble and trigger feedback, and hands the main thread only what it
//! must act on (the dial, a fired shortcut).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pf_client_core::menu_nav::MenuEvent;
use pf_client_core::ring::{RingCommand, RingFacts, RingInput};
use pf_console_ui::theme;
use pf_console_ui::widgets::{KeyMsg, Keyboard};
use punktfunk_core::client::PunktfunkEndReason;
use punktfunk_core::hud::StatsVerbosity;
use punktfunk_core::input::{InputEvent, InputKind};
use punktfunk_core::quic::v2::hello::{ScreenEcho, ScreenWish};

use super::connect::{self, ConnectOutcome};
use super::hud::{self, StatsOverlay};
use super::pads::{PadRouter, Routed};
use super::rich::RichPads;
use super::testrun::Scenario;
use super::{App, StreamOutcome};
use crate::core::model::GamepadType;
use crate::core::settings::Ps5Settings;
use crate::platform::{Fit, PadFeedback, PadSample, Pads, SystemEvent};
use crate::session::{Connected, InputSender};

/// Input thread cadence: 250 Hz, under the pad's own report rate.
const INPUT_PERIOD: Duration = Duration::from_millis(4);
/// Feedback events handled per input tick, so a flood cannot starve the pads.
const FEEDBACK_BUDGET: usize = 32;
/// Redials after a lost link, and how long each may take.
const RECONNECTS: u32 = 3;
/// Backspace and Return on the host's keyboard.
const VK_BACK: u32 = 0x08;
const VK_RETURN: u32 = 0x0D;
/// With no picture arriving, the overlay's figures still redraw this often.
const IDLE_REDRAW: Duration = Duration::from_millis(100);

/// What the main thread tells the input thread.
enum ToInput {
    DialOpened,
    DialClosed,
    ReleaseAll,
    /// Tap one system button on the host's pad (`pads::tap`).
    Tap {
        pad: u8,
        bit: u32,
    },
}

/// The input thread and its channels. Dropping it without [`Self::stop`] leaks the pads.
struct InputThread {
    stop: Arc<AtomicBool>,
    to: Sender<ToInput>,
    from: Receiver<Routed>,
    handle: std::thread::JoinHandle<Box<dyn Pads>>,
}

impl InputThread {
    fn start(pads: Box<dyn Pads>, input: InputSender, kind: GamepadType, router: PadRouter) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (to, rx) = mpsc::channel();
        let (tx, from) = mpsc::channel();
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("pf-input".into())
            .spawn(move || input_loop(pads, &input, kind, router, &thread_stop, &rx, &tx))
            .expect("spawn the input thread");
        Self { stop, to, from, handle }
    }

    /// Stops the thread, which lets go of everything, and takes the pads back.
    fn stop(self) -> Option<Box<dyn Pads>> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().ok()
    }
}

fn input_loop(
    mut pads: Box<dyn Pads>,
    input: &InputSender,
    kind: GamepadType,
    mut router: PadRouter,
    stop: &AtomicBool,
    rx: &Receiver<ToInput>,
    tx: &Sender<Routed>,
) -> Box<dyn Pads> {
    let mut batch: Vec<PadSample> = Vec::new();
    let mut wire = Vec::new();
    let mut routed = Vec::new();
    let mut rich = RichPads::default();
    let mut rich_out = Vec::new();
    let mut taps = Vec::new();
    let mut dial_open = false;
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        wire.clear();
        routed.clear();
        for msg in rx.try_iter() {
            match msg {
                ToInput::DialOpened => {
                    dial_open = true;
                    router.dial_opened(&mut wire);
                }
                ToInput::DialClosed => {
                    dial_open = false;
                    router.dial_closed();
                }
                ToInput::ReleaseAll => router.release_all(&mut wire),
                ToInput::Tap { pad, bit } => taps.extend(super::pads::tap(bit, pad, Instant::now())),
            }
        }
        batch.clear();
        pads.poll(&mut batch);
        let now = Instant::now();
        router.step(&batch, now, dial_open, kind, &mut wire, &mut routed);
        // A tap's edges in the order they fall due, each once.
        taps.retain(|(due, ev)| {
            if *due <= now {
                wire.push(*ev);
            }
            *due > now
        });
        for ev in &wire {
            input.send(ev);
        }
        rich.step(&batch, dial_open, kind, &mut rich_out);
        for r in rich_out.drain(..) {
            let _ = input.client().send_rich_input(r);
        }
        for r in routed.drain(..) {
            let _ = tx.send(r);
        }
        apply_feedback(pads.as_mut(), input);
        next += INPUT_PERIOD;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    wire.clear();
    router.release_all(&mut wire);
    for ev in &wire {
        input.send(ev);
    }
    rich.step(&[], true, kind, &mut rich_out);
    for r in rich_out {
        let _ = input.client().send_rich_input(r);
    }
    input.release();
    pads.release_all();
    pads
}

/// The host's rumble and `DualSense` feedback, onto the pads. Both planes drain every tick: a
/// bounded plane left unread drops its newest events, including the zero that stops a motor.
fn apply_feedback(pads: &mut dyn Pads, input: &InputSender) {
    use punktfunk_core::quic::HidOutput;
    let client = input.client();
    for _ in 0..FEEDBACK_BUDGET {
        let Ok(cmd) = client.next_rumble_command(Duration::ZERO) else {
            break;
        };
        let Ok(slot) = u8::try_from(cmd.pad) else { continue };
        pads.feedback(
            slot,
            &PadFeedback::Rumble {
                low: cmd.low,
                high: cmd.high,
            },
        );
    }
    for _ in 0..FEEDBACK_BUDGET {
        let Ok(event) = client.next_hidout(Duration::ZERO) else {
            break;
        };
        let (pad, feedback) = match event {
            HidOutput::Led { pad, r, g, b } => (pad, PadFeedback::Light { r, g, b }),
            HidOutput::Trigger { pad, which, effect } => (pad, PadFeedback::Trigger { which, effect }),
            _ => continue,
        };
        pads.feedback(pad, &feedback);
    }
}

/// What the main loop decided this frame.
enum Next {
    End(StreamOutcome),
    /// The link was lost: dial again.
    Redial,
}

/// Runs one launch until it ends, redialling a lost link.
pub(super) fn run(app: &mut App, launch: ConnectOutcome) -> StreamOutcome {
    let ConnectOutcome {
        connected,
        target,
        settings,
        exit_plan,
    } = launch;
    let mut connected = match connected {
        Ok(c) => c,
        Err(e) => {
            let why = crate::core::errors::friendly(&e);
            tracing::warn!("connect failed: {e:#}");
            return end_test_run(app, false, &why).unwrap_or(StreamOutcome::ReturnToMenu(Some(why)));
        }
    };
    let mut redials = 0;
    loop {
        match stream(app, &connected, &settings, &target) {
            Next::End(outcome) => {
                super::teardown(connected);
                return match outcome {
                    StreamOutcome::Quit(_) => StreamOutcome::Quit(exit_plan),
                    other => other,
                };
            }
            Next::Redial if redials < RECONNECTS => {
                redials += 1;
                super::teardown(connected);
                tracing::info!("link lost — redial {redials} of {RECONNECTS}");
                match redial(app, &target, &settings) {
                    Some(c) => connected = c,
                    None => return StreamOutcome::ReturnToMenu(Some(pf_l10n::client::ended_lost().into())),
                }
            }
            Next::Redial => {
                let why = connected.end_message();
                super::teardown(connected);
                return StreamOutcome::ReturnToMenu(Some(why));
            }
        }
    }
}

/// Dials the same target again, drawing a plain frame while it waits.
fn redial(
    app: &mut App,
    target: &crate::core::model::ConnectTarget,
    settings: &crate::services::store::Settings,
) -> Option<Connected> {
    let native = super::menu::native_mode(app);
    let pending = connect::spawn(
        &app.media,
        app.identity.clone(),
        target.clone(),
        settings.clone(),
        native,
    )
    .ok()?;
    app.display.set_stream(false, Fit::Contain);
    while !pending.is_finished() {
        let _ = app.display.frame(&mut |_| {});
    }
    match pending.join() {
        Ok(Ok(c)) => Some(c),
        Ok(Err(e)) => {
            tracing::warn!("redial: {e:#}");
            None
        }
        Err(_) => None,
    }
}

/// One session, until it ends or its link is lost.
fn stream(
    app: &mut App,
    connected: &Connected,
    settings: &crate::services::store::Settings,
    target: &crate::core::model::ConnectTarget,
) -> Next {
    let Some(pads) = app.pads.take() else {
        tracing::error!("stream: the pads are still held by another loop");
        return Next::End(StreamOutcome::ReturnToMenu(None));
    };
    let router = PadRouter::new(&settings.shortcuts, &settings.overlay_actions);
    let input = InputThread::start(pads, connected.input(), settings.gamepad_type(), router);
    let faces = super::system_faces(&app.device.fonts);
    let faces: Vec<&[u8]> = faces.iter().map(Vec::as_slice).collect();
    let fonts = match pf_console_ui::theme::build_fonts_with(&faces) {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!("overlay fonts: {e:#}");
            None
        }
    };
    let ui = theme::Frame::new(
        theme::Ink::of(pf_console_ui::palette::palette(&settings.ui_palette)),
        settings.reduce_motion,
        true,
        0.0,
    );
    let mut ring = pf_console_ui::Ring::new();
    let mode = connected.client().mode();
    let (output_w, output_h) = app.display.size();
    let (width, height) = app.display.ui_size();
    let facts = RingFacts {
        overlay_actions: settings.overlay_actions.clone(),
        stats_tier: settings.stats_verbosity().label().into(),
        mode: (mode.width, mode.height, mode.refresh_hz),
        native_mode: (output_w, output_h, app.display.refresh().hz()),
        addr: target.host.clone(),
        fp_hex: fp_hex(&target.fingerprint),
        pad_type: settings.gamepad_type().to_core(),
        ..RingFacts::default()
    };
    ring.set_facts(&facts);
    let mut dial = Dial::default();
    // A test run records everything; otherwise the player's stats setting.
    let initial_tier = if app.test_run.is_some() {
        StatsVerbosity::Detailed
    } else {
        settings.stats_verbosity()
    };
    let mut hud = StatsOverlay::new(initial_tier, settings.advanced_stats);
    connected.set_hud_enabled(initial_tier != StatsVerbosity::Off);
    let decoder = lane_name(connected.client().codec);
    let scale = (height as f32 / 800.0).clamp(0.75, 3.0);
    app.display.set_stream(true, Fit::Contain);

    let client = connected.client();
    let notice = hud::screen_line(
        client.screen() == Some(&ScreenEcho::Shared),
        ScreenWish::parse(&settings.host_screen).as_ref(),
        client.role(),
    );

    let started = Instant::now();
    let mut last_elapsed = Duration::ZERO;
    let mut dial_was_open = false;
    let mut perf = StreamPerf::default();
    let mut last_frame = Instant::now();
    let period = Duration::from_nanos(app.display.refresh().period_ns());

    let next = loop {
        let events = app.system_events();
        if events.contains(&SystemEvent::FocusLost) {
            let _ = input.to.send(ToInput::ReleaseAll);
        }
        if events.contains(&SystemEvent::Suspend) {
            tracing::info!("rest mode: ending the stream");
            break Next::End(StreamOutcome::ReturnToMenu(None));
        }
        if let Some(n) = take_routed(&input, &mut ring, &mut hud, &mut dial, connected, (width, height)) {
            break n;
        }
        if ring.damage() != 0 {
            ring.set_facts(&dial.facts(&facts, connected));
        }
        ring.tick();
        if let Some(n) = take_ring_commands(&mut ring, &mut hud, &mut dial, connected, &input.to) {
            break n;
        }
        for cmd in ring.take_cmds() {
            tracing::info!("dial: no host action {cmd:?} on this console");
        }
        // The keyboard takes the pad the way the dial does.
        let open = ring.open() || dial.typing;
        if open != dial_was_open {
            dial_was_open = open;
            let _ = input
                .to
                .send(if open { ToInput::DialOpened } else { ToInput::DialClosed });
        }
        if connected.is_session_ended() {
            tracing::info!("session ended: {:?}", connected.end_reason());
            break if connected.end_reason() == PunktfunkEndReason::Lost {
                Next::Redial
            } else {
                Next::End(StreamOutcome::ReturnToMenu(Some(connected.end_message())))
            };
        }

        // A frame goes out for something new to show: a picture, the dial moving, or the
        // overlay's figures after IDLE_REDRAW. A flip per picture is what lets the display
        // follow the stream's own cadence.
        let show = ring.damage() != 0
            || dial.typing
            || dial.seat > 0.0
            || app.display.wait_picture(period)
            || last_frame.elapsed() >= IDLE_REDRAW;
        if show {
            hud.refresh(connected, decoder);
            let mut present = pf_l10n::console::console_overlay_present_frames(perf.frames, perf.missed);
            if connected.stats.holding.load(Ordering::Relaxed) {
                present.push_str(" · ");
                present.push_str(pf_l10n::console::console_overlay_holding_keyframe());
            }
            hud.local = vec![present];
            let dt = last_frame.elapsed().as_secs_f64();
            last_frame = Instant::now();
            let result = app.display.frame(&mut |canvas| {
                if let Some(fonts) = &fonts {
                    hud.draw(canvas, fonts, scale);
                    if let Some(text) = notice.as_deref().filter(|_| started.elapsed() < hud::NOTICE_FOR) {
                        hud::draw_notice(canvas, fonts, text, (width, height), scale);
                    }
                    ring.render(canvas, &ui, width, height, scale, fonts, dt);
                    dial.seat = dial.keyboard.seat(dial.typing, dt);
                    if dial.seat > 0.0 {
                        let (w, bottom) = (f64::from(width), f64::from(height));
                        dial.keyboard
                            .render(canvas, &ui, fonts, w, bottom, dial.seat, f64::from(scale));
                    }
                }
            });
            match result {
                Ok(report) => perf.note(&report),
                Err(e) => {
                    tracing::error!("present: {e:#}");
                    break Next::End(StreamOutcome::ReturnToMenu(Some(
                        pf_l10n::client::ended_unexpected().into(),
                    )));
                }
            }
        }

        let elapsed = started.elapsed();
        if let Some(n) = test_run_step(app, last_elapsed, elapsed, &perf, connected) {
            break n;
        }
        last_elapsed = elapsed;
    };

    app.display.set_stream(false, Fit::Contain);
    connected.set_hud_enabled(false);
    app.pads = input.stop();
    if app.pads.is_none() {
        tracing::error!("the input thread panicked; pads are gone until relaunch");
    }
    next
}

/// The dial's state beside the ring itself.
#[derive(Default)]
struct Dial {
    /// The pad that opened the dial last: controller mouse and a system button's tap act on it.
    opener: Option<u8>,
    /// The controller keyboard is up, and its keys type into the host.
    typing: bool,
    keyboard: Keyboard,
    /// How far the keyboard's tray has risen, from its last frame.
    seat: f64,
}

impl Dial {
    /// The wire pads a dial command acts on: the pad that opened the dial, else every live one.
    fn target(&self, connected: &Connected) -> u16 {
        self.opener
            .map_or_else(|| connected.client().live_pads(), |pad| 1 << pad)
    }

    /// `base` with what the session holds right now, for the dial's faces.
    fn facts(&self, base: &RingFacts, connected: &Connected) -> RingFacts {
        let client = connected.client();
        let target = self.target(connected);
        RingFacts {
            pad_mouse_target: target,
            pad_mouse: client.pad_mouse_mode(target),
            pointer_granted: client.access_grants() & punktfunk_core::quic::GRANT_POINTER != 0,
            audio_mute: client.audio_mute(),
            invert_scroll: client.invert_scroll(),
            ..base.clone()
        }
    }

    /// One pad event on the controller keyboard. A key types into the host, X deletes, the
    /// Done key sends Return, and anything that closes the tray ends the typing.
    fn type_key(&mut self, ev: MenuEvent, connected: &Connected) {
        let done_key = matches!(ev, MenuEvent::Confirm);
        let (msg, _) = self.keyboard.menu(ev);
        let keys = connected.input();
        let event = |kind, code| InputEvent {
            kind,
            _pad: [0; 3],
            code,
            x: 0,
            y: 0,
            flags: 0,
        };
        let tap = |vk: u32| {
            keys.send(&event(InputKind::KeyDown, vk));
            keys.send(&event(InputKind::KeyUp, vk));
        };
        match msg {
            KeyMsg::Type(ch) if connected.client().host_caps & punktfunk_core::quic::HOST_CAP_TEXT_INPUT != 0 => {
                keys.send(&event(InputKind::TextInput, u32::from(ch)));
            }
            KeyMsg::Type(_) => tracing::info!("keyboard: this host takes no typed text"),
            KeyMsg::Backspace => tap(VK_BACK),
            KeyMsg::Done => {
                if done_key {
                    tap(VK_RETURN);
                }
                self.typing = false;
            }
            KeyMsg::None => {}
        }
    }
}

/// The input thread's hand-offs: the dial, and a shortcut's dial command.
fn take_routed(
    input: &InputThread,
    ring: &mut pf_console_ui::Ring,
    hud: &mut StatsOverlay,
    dial: &mut Dial,
    connected: &Connected,
    (width, height): (u32, u32),
) -> Option<Next> {
    for routed in input.from.try_iter() {
        match routed {
            // The chord that opens the dial closes the keyboard instead.
            Routed::ToggleDial(_) if dial.typing => dial.typing = false,
            Routed::ToggleDial(pad) => {
                dial.opener = Some(pad);
                ring.input(RingInput::Toggle {
                    x: width as f32 / 2.0,
                    y: height as f32 / 2.0,
                });
            }
            Routed::Dial(ev) if dial.typing => dial.type_key(ev, connected),
            Routed::Dial(ev) => {
                ring.menu(ev);
            }
            Routed::Command(cmd) => {
                tracing::info!("shortcut: {cmd:?}");
                if let Some(n) = run_command(cmd, hud, dial, connected, &input.to) {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn take_ring_commands(
    ring: &mut pf_console_ui::Ring,
    hud: &mut StatsOverlay,
    dial: &mut Dial,
    connected: &Connected,
    to: &Sender<ToInput>,
) -> Option<Next> {
    while let Some(cmd) = ring.take_command() {
        if let Some(n) = run_command(cmd, hud, dial, connected, to) {
            return Some(n);
        }
    }
    None
}

/// One dial command, picked on the dial or fired by a shortcut (`pads::command`).
fn run_command(
    cmd: RingCommand,
    hud: &mut StatsOverlay,
    dial: &mut Dial,
    connected: &Connected,
    to: &Sender<ToInput>,
) -> Option<Next> {
    let client = connected.client();
    match cmd {
        RingCommand::EndStream | RingCommand::DisconnectLinger => {
            return Some(Next::End(StreamOutcome::ReturnToMenu(None)));
        }
        RingCommand::CycleStats => hud.cycle(connected),
        RingCommand::Keyboard => dial.typing = !dial.typing,
        RingCommand::CyclePadMouse => {
            if let Err(e) = client.cycle_pad_mouse(dial.target(connected)) {
                tracing::warn!("dial: controller mouse refused: {e}");
            }
        }
        RingCommand::ToggleStreamMute => {
            let muted = client.audio_mute() & punktfunk_core::client::AUDIO_MUTE_LOCAL != 0;
            client.set_audio_muted(!muted);
        }
        RingCommand::ToggleScrollInvert => client.set_invert_scroll(!client.invert_scroll()),
        RingCommand::TapButton(bit) => {
            let pad = dial.opener.unwrap_or(0);
            let _ = to.send(ToInput::Tap { pad, bit });
        }
        other => tracing::info!("dial: {other:?} does nothing on this console"),
    }
    None
}

#[derive(Default)]
struct StreamPerf {
    frames: u64,
    missed: u64,
    pictures: u64,
    decode_to_present_us: Vec<u32>,
}

impl StreamPerf {
    fn note(&mut self, report: &crate::platform::FrameReport) {
        self.frames += 1;
        self.missed += u64::from(report.missed);
        self.pictures += u64::from(report.new_picture);
        if let Some(us) = report.decode_to_present_us {
            if self.decode_to_present_us.len() < 1 << 16 {
                self.decode_to_present_us.push(us);
            }
        }
    }

    fn json(&self, connected: &Connected) -> serde_json::Value {
        let mut sorted = self.decode_to_present_us.clone();
        sorted.sort_unstable();
        let pct = |p: usize| sorted.get(sorted.len() * p / 100).copied();
        serde_json::json!({
            "frames": self.frames,
            "missed_vblanks": self.missed,
            "new_pictures": self.pictures,
            "decode_to_present_us": { "p50": pct(50), "p95": pct(95), "p99": pct(99), "n": sorted.len() },
            "frames_dropped": connected.client().frames_dropped(),
            "rtt_us": connected.client().rtt_us(),
        })
    }
}

/// The stream scenario: captures on schedule, then the verdict and the exit.
fn test_run_step(
    app: &mut App,
    before: Duration,
    after: Duration,
    perf: &StreamPerf,
    connected: &Connected,
) -> Option<Next> {
    let run = app.test_run.as_ref().filter(|r| r.scenario == Scenario::Stream)?;
    if let Some(second) = run.capture_due(before, after) {
        match app.display.read_back() {
            Ok(Some(frame)) => {
                if let Err(e) = run.capture(&format!("{second}s"), frame) {
                    tracing::warn!("test run: {e:#}");
                }
            }
            Ok(None) => tracing::info!("test run: no read-back at {second}s — this output has no 8-bit pixels"),
            Err(e) => tracing::warn!("test run: read-back at {second}s: {e:#}"),
        }
    }
    if after < Duration::from_secs(run.seconds) {
        return None;
    }
    let run = app.test_run.take()?;
    if let Err(e) = run.write_json("perf.json", &perf.json(connected)) {
        tracing::warn!("test run: perf.json: {e:#}");
    }
    let ok = perf.pictures > 0;
    run.finish(app, ok, &format!("{} pictures over {}s", perf.pictures, run.seconds));
    Some(Next::End(StreamOutcome::Quit(None)))
}

/// A failed connect in a stream scenario ends the run with its reason.
fn end_test_run(app: &mut App, ok: bool, detail: &str) -> Option<StreamOutcome> {
    let run = app.test_run.take().filter(|r| r.scenario == Scenario::Stream)?;
    run.finish(app, ok, detail);
    Some(StreamOutcome::Quit(None))
}

fn lane_name(codec: u8) -> &'static str {
    if codec == punktfunk_core::quic::CODEC_PYROWAVE {
        "PyroWave (GPU)"
    } else {
        "Videodec2"
    }
}

fn fp_hex(fp: &[u8; 32]) -> String {
    use std::fmt::Write;
    fp.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
