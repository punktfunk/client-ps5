//! The stats overlay: core's own lines (`hud::format`) on a dark plate, top left, refreshed once
//! a second while it shows; and the line a stream owes about its screen at start, bottom centre.

use std::time::{Duration, Instant};

use pf_console_ui::theme::{Fonts, W};
use punktfunk_core::hud::{self, HudLine, Role, StatsVerbosity};
use punktfunk_core::quic::v2::hello::{RoleEcho, ScreenWish};
use skia_safe::{Canvas, Color4f, Paint, RRect, Rect};

use crate::session::Connected;

const REFRESH: Duration = Duration::from_secs(1);

pub(super) struct StatsOverlay {
    pub tier: StatsVerbosity,
    advanced: bool,
    lines: Vec<HudLine>,
    taken: Option<Instant>,
    /// Lines this client adds: the lane, pacing misses, the audio ring.
    pub local: Vec<String>,
}

impl StatsOverlay {
    pub fn new(tier: StatsVerbosity, advanced: bool) -> Self {
        Self {
            tier,
            advanced,
            lines: Vec::new(),
            taken: None,
            local: Vec::new(),
        }
    }

    /// Steps to the next tier, turning core's sampling on or off with it.
    pub fn cycle(&mut self, connected: &Connected) {
        self.tier = self.tier.next();
        connected.set_hud_enabled(self.tier != StatsVerbosity::Off);
        self.taken = None;
    }

    /// Takes a new window when one is due.
    pub fn refresh(&mut self, connected: &Connected, decoder: &str) {
        if self.tier == StatsVerbosity::Off || self.taken.is_some_and(|t| t.elapsed() < REFRESH) {
            return;
        }
        self.taken = Some(Instant::now());
        let snap = connected.hud_snapshot(decoder);
        self.lines = hud::format(&snap, self.tier, self.advanced);
    }

    pub fn draw(&self, canvas: &Canvas, fonts: &Fonts, scale: f32) {
        if self.tier == StatsVerbosity::Off {
            return;
        }
        let size = f64::from(13.0 * scale);
        let line_h = (size * 1.3) as f32;
        let pad = 10.0 * scale;
        let texts: Vec<(&str, Role)> = self
            .lines
            .iter()
            .map(|l| (l.text.as_str(), l.role))
            .chain(self.local.iter().map(|t| (t.as_str(), Role::Muted)))
            .collect();
        let width = texts
            .iter()
            .map(|(t, _)| fonts.measure(t, W::Medium, size))
            .fold(0.0f32, f32::max);
        let height = line_h * texts.len() as f32;
        let plate = Rect::from_xywh(pad, pad, width + pad * 2.0, height + pad * 2.0);
        let mut paint = Paint::new(Color4f::new(0.0, 0.0, 0.0, 0.62), None);
        paint.set_anti_alias(true);
        canvas.draw_rrect(RRect::new_rect_xy(plate, 8.0 * scale, 8.0 * scale), &paint);
        for (i, (text, role)) in texts.iter().enumerate() {
            let baseline = f64::from(pad * 2.0 + line_h * (i as f32 + 0.8));
            fonts.draw(
                canvas,
                text,
                f64::from(pad * 2.0),
                baseline,
                W::Medium,
                size,
                colour(*role),
            );
        }
    }
}

/// The one line a session owes about its screen at start: another device's screen, else why the
/// preset's wish was not met. One slot; the shared screen explains a lost wish too.
pub(super) fn screen_line(shared: bool, wish: Option<&ScreenWish>, echo: Option<RoleEcho>) -> Option<String> {
    if shared {
        return Some(pf_l10n::client::notice_shared_screen().into());
    }
    pf_client_core::host_text::screen_wish_line(wish, echo?.reason)
}

/// How long the start line stays up: the desktop's notice time.
pub(super) const NOTICE_FOR: Duration = Duration::from_secs(6);

/// One line on a pill, bottom centre, over the picture.
pub(super) fn draw_notice(canvas: &Canvas, fonts: &Fonts, text: &str, (width, height): (u32, u32), scale: f32) {
    let size = f64::from(15.0 * scale);
    let line_h = (size * 1.3) as f32;
    let (pad_x, pad_y) = (16.0 * scale, 9.0 * scale);
    let w = fonts.measure(text, W::Medium, size) + pad_x * 2.0;
    let h = line_h + pad_y * 2.0;
    let x = (width as f32 - w) / 2.0;
    let y = height as f32 - h - 48.0 * scale;
    let mut paint = Paint::new(Color4f::new(0.0, 0.0, 0.0, 0.62), None);
    paint.set_anti_alias(true);
    canvas.draw_rrect(
        RRect::new_rect_xy(Rect::from_xywh(x, y, w, h), h / 2.0, h / 2.0),
        &paint,
    );
    let baseline = f64::from(y + pad_y + line_h * 0.8);
    fonts.draw(
        canvas,
        text,
        f64::from(x + pad_x),
        baseline,
        W::Medium,
        size,
        colour(Role::Primary),
    );
}

fn colour(role: Role) -> Color4f {
    match role {
        Role::Warn => Color4f::new(1.0, 0.78, 0.3, 1.0),
        Role::Muted => Color4f::new(0.75, 0.75, 0.78, 1.0),
        _ => Color4f::new(1.0, 1.0, 1.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use punktfunk_core::quic::v2::hello::RoleReason;

    /// A shared screen wins the one slot; a wish met says nothing.
    #[test]
    fn the_start_line_says_one_thing_about_the_screen() {
        let echo = |reason| Some(RoleEcho { role: None, reason });
        let second = Some(&ScreenWish::Second);
        let lost = pf_client_core::host_text::screen_wish_line(second, RoleReason::NotAllowed);
        assert!(lost.is_some());
        assert_eq!(screen_line(false, second, echo(RoleReason::NotAllowed)), lost);
        assert_eq!(
            screen_line(true, second, echo(RoleReason::NotAllowed)).as_deref(),
            Some(pf_l10n::client::notice_shared_screen())
        );
        assert_eq!(screen_line(false, second, echo(RoleReason::AsAsked)), None);
        assert_eq!(screen_line(false, second, None), None, "a host that does not say");
    }
}
