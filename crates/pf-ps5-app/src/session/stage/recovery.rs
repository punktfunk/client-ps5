//! What the stage does when a picture cannot be trusted: the freeze-until-reanchor hold, the
//! keyframe-request throttle, and the play-error policy.
//!
//! Separated from the feed path so the ~20 lines that actually stamp and submit a picture can be
//! read against the reference implementation without paging through recovery.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::media::{NotReady, VideoSink, VideoSinkCaps};
use crate::session::timeline::ms;
use crate::session::StreamStats;

use super::{FrameFlags, SinkResult};

/// Minimum spacing between [`SinkResult::NeedKeyframe`] results, so a burst of skipped frames
/// cannot storm the control stream. A resume frame that played ends it early: the ask was
/// answered, and the next loss is a new one.
const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// What [`Recovery::gate`] decided about this frame.
pub(super) enum HoldGate {
    /// Feed it — not holding.
    Feed,
    /// This is the frame that lifts the hold. The timeline just jumped, so the caller re-anchors
    /// its mapping before feeding.
    Resume,
    /// Still frozen — skip it, and report this instead.
    Skip(SinkResult),
}

/// Hold state and the keyframe throttle.
pub(super) struct Recovery {
    stats: Arc<StreamStats>,
    /// Freeze-until-reanchor: while holding, frames are skipped rather than fed — the
    /// punch-through plane keeps the last good picture. Resumes on an IDR or an LTR-RFI
    /// recovery anchor only. `Some` for exactly as long as the hold lasts.
    hold_started: Option<Instant>,
    last_keyframe_request: Option<Instant>,
    /// Holds BEGUN and total time held, both session totals the heartbeat differences. The live
    /// `holding` flag is a snapshot and cannot see a hold that started and ended between two
    /// heartbeats — which is exactly the shape a stutter report is about.
    holds: u64,
    held: Duration,
}

impl Recovery {
    pub(super) fn new(stats: Arc<StreamStats>) -> Self {
        Self {
            stats,
            hold_started: None,
            last_keyframe_request: None,
            holds: 0,
            held: Duration::ZERO,
        }
    }

    pub(super) fn holding(&self) -> bool {
        self.hold_started.is_some()
    }

    pub(super) fn begin_hold(&mut self) {
        self.stats.holding.store(true, Ordering::Relaxed);
        if self.hold_started.is_none() {
            self.hold_started = Some(Instant::now());
            self.holds += 1;
        }
    }

    pub(super) fn hold_totals(&self) -> (u64, Duration) {
        // A hold still running counts its time so far, or a long one would read as zero until it
        // lifted.
        let running = self.hold_started.map_or(Duration::ZERO, |t| t.elapsed());
        (self.holds, self.held + running)
    }

    /// Stamps this keyframe request as sent, if throttle allows.
    pub(super) fn take_keyframe_slot(&mut self) -> bool {
        let ready = self
            .last_keyframe_request
            .is_none_or(|t| t.elapsed() >= KEYFRAME_REQUEST_MIN_INTERVAL);
        if ready {
            self.last_keyframe_request = Some(Instant::now());
        }
        ready
    }

    /// The frame that lifted the hold played: the ask behind it was answered, so the next
    /// request goes at once. A refused resume frame leaves the slot the hold's.
    pub(super) fn repair_played(&mut self) {
        self.last_keyframe_request = None;
    }

    /// Run before every feed.
    pub(super) fn gate(&mut self, flags: &FrameFlags) -> HoldGate {
        if flags.loss {
            let newly_holding = !self.holding();
            self.begin_hold();
            if newly_holding {
                tracing::warn!("loss (frame {}) — freezing", flags.index);
            }
            // No flush on the loss hold: loss is a network event and the decoder's queue holds
            // good frames. The decode-error path below flushes, because there the decoder erred.
        }
        let Some(started) = self.hold_started else {
            return HoldGate::Feed;
        };
        // An IDR or an RFI anchor predicts from nothing the decoder lacks. An intra-refresh wave
        // heals only a decoder that saw every frame of it, and the hold skipped those.
        if !flags.reanchor {
            // The slot is taken only while frames are still skipped. The frame that lifts the hold
            // restarts decoding by itself, and reporting a request on it made `submit` read the
            // feed as refused — abandoning the open AU, i.e. truncating the very keyframe that
            // resumed the picture on a slice-progressive session, and re-arming the hold.
            return HoldGate::Skip(if self.take_keyframe_slot() {
                SinkResult::NeedKeyframe
            } else {
                SinkResult::Held
            });
        }
        let held = started.elapsed();
        tracing::info!(
            "resuming after {:.0}ms (frame {})",
            held.as_secs_f32() * 1000.0,
            flags.index,
        );
        self.stats.holding.store(false, Ordering::Relaxed);
        self.held += held;
        self.hold_started = None;
        HoldGate::Resume
    }

    pub(super) fn on_play_error(
        &mut self,
        e: &anyhow::Error,
        flags: &FrameFlags,
        base_ns: u64,
        sink: &dyn VideoSink,
        caps: VideoSinkCaps,
    ) -> bool {
        tracing::warn!(
            "{} error (frame {}, pts {:.2}ms): {e:#}",
            sink.name(),
            flags.index,
            ms(base_ns),
        );
        // A decoder still opening (NotReady) is not an error: no flush, no hold, just a keyframe
        // request; at frame 0 there is no picture to freeze on.
        // A real decode error holds whether or not the throttle lets a request out: the throttle
        // spaces requests, never the hold, so P-frames never reach a decoder missing their reference.
        if e.downcast_ref::<NotReady>().is_none() {
            if caps.flush {
                let _ = sink.flush();
            }
            self.begin_hold();
        }
        self.take_keyframe_slot()
    }
}
