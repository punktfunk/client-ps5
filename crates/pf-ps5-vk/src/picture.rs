//! A decoded picture on its way from a decode lane to the presenter, and the slot between them.
//!
//! The slot holds one picture, newest wins: a lane that outruns the display overwrites, and the
//! presenter takes whatever is newest once an output image is free. A picture keeps its planes
//! alive through `hold` until the last frame that sampled it has finished on the GPU.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use ash::vk;
use pf_client_video::video_color::ColorDesc;

/// Plane views in the layout the lane leaves them (`layout`).
#[derive(Clone, Copy, Debug)]
pub enum Planes {
    /// Y, Cb, Cr: `PyroWave`.
    Three([vk::ImageView; 3]),
    /// Y and interleaved `CbCr`: NV12 or P010 from Videodec2.
    Two([vk::ImageView; 2]),
}

pub struct Picture {
    pub planes: Planes,
    pub layout: vk::ImageLayout,
    pub width: u32,
    pub height: u32,
    pub color: ColorDesc,
    /// 8 or 10.
    pub depth: u8,
    /// 10-bit codes in the MSBs of 16-bit samples.
    pub msb_packed: bool,
    /// Multiplies the CSC rows' sample terms: 64 for Videodec2's Main10, whose 10-bit codes sit
    /// in the LOW bits (rows built as MSB-packed, then scaled), else 1.
    pub sample_scale: f32,
    /// Visible fraction of the decoded surface (crop over padded pools); `[1.0, 1.0]` when exact.
    pub uv_scale: [f32; 2],
    pub decoded_at: Instant,
    /// Host capture PTS and the realtime decode stamp, for the overlay's display stage.
    pub pts_ns: u64,
    pub decoded_ns: u64,
    /// The present turn the lane offered (`QueueLock::offer_present_turn`), if any.
    pub turn: Option<u64>,
    /// The session's overlay figures, where the display stage is noted.
    pub hud: Option<Arc<punktfunk_core::hud::Stats>>,
    /// Keeps the planes' images and memory alive.
    pub hold: Box<dyn std::any::Any + Send + Sync>,
}

/// One picture between a lane and the presenter.
#[derive(Default)]
pub struct VideoSlot {
    newest: Mutex<Option<Arc<Picture>>>,
    arrived: Condvar,
}

impl VideoSlot {
    fn lock(&self) -> MutexGuard<'_, Option<Arc<Picture>>> {
        self.newest.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces whatever the presenter has not taken yet, and wakes it.
    pub fn publish(&self, picture: Picture) {
        *self.lock() = Some(Arc::new(picture));
        self.arrived.notify_one();
    }

    /// Whether a picture waits, after at most `timeout`.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.arrived
            .wait_timeout_while(self.lock(), timeout, |waiting| waiting.is_none())
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .is_some()
    }

    /// The waiting picture, left in the slot.
    pub fn peek(&self) -> Option<Arc<Picture>> {
        self.lock().clone()
    }

    /// The newest picture, if one arrived since the last take.
    pub fn take(&self) -> Option<Arc<Picture>> {
        self.lock().take()
    }

    /// Drops a waiting picture: the stream ended.
    pub fn clear(&self) {
        self.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Picture {
        Picture {
            planes: Planes::Two([vk::ImageView::null(); 2]),
            layout: vk::ImageLayout::UNDEFINED,
            width: 0,
            height: 0,
            color: ColorDesc {
                primaries: 1,
                transfer: 1,
                matrix: 1,
                full_range: false,
            },
            depth: 8,
            msb_packed: false,
            sample_scale: 1.0,
            uv_scale: [1.0, 1.0],
            decoded_at: Instant::now(),
            pts_ns: 0,
            decoded_ns: 0,
            turn: None,
            hud: None,
            hold: Box::new(()),
        }
    }

    #[test]
    fn a_wait_ends_when_a_picture_is_published() {
        let slot = Arc::new(VideoSlot::default());
        assert!(!slot.wait(Duration::ZERO));
        let lane = slot.clone();
        let publisher = std::thread::spawn(move || lane.publish(picture()));
        assert!(slot.wait(Duration::from_secs(10)));
        publisher.join().unwrap();
        assert!(slot.peek().is_some(), "a peek leaves the picture");
        assert!(slot.take().is_some());
        assert!(!slot.wait(Duration::ZERO), "taken, nothing waits");
    }
}
