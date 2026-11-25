//! Pads through `UserService` and `scePad`: one handle per signed-in user, re-scanned once a second.

use std::time::{Duration, Instant};

use pf_ps5_app::platform::{PadFeedback, PadSample, Pads};
use pf_ps5_sys::pad::*;
use pf_ps5_sys::user::*;

const RESCAN: Duration = Duration::from_secs(1);

struct Open {
    user: i32,
    handle: i32,
}

pub struct Ps5Pads {
    slots: [Option<Open>; 4],
    scanned: Option<Instant>,
}

impl Ps5Pads {
    pub fn new() -> anyhow::Result<Self> {
        // SAFETY: plain init calls with no arguments that outlive them.
        unsafe {
            let rc = sceUserServiceInitialize(std::ptr::null());
            if rc != 0 && rc != ERROR_ALREADY_INITIALIZED {
                anyhow::bail!("sceUserServiceInitialize: {}", pf_ps5_sys::sce_error(rc));
            }
            let rc = scePadInit();
            anyhow::ensure!(rc == 0, "scePadInit: {}", pf_ps5_sys::sce_error(rc));
        }
        Ok(Self {
            slots: Default::default(),
            scanned: None,
        })
    }

    /// Opens a pad for every signed-in user and closes those whose user left.
    fn rescan(&mut self) {
        let mut list = SceUserServiceLoginUserIdList::default();
        // SAFETY: `list` is a live 16-byte out-parameter.
        if unsafe { sceUserServiceGetLoginUserIdList(&mut list) } != 0 {
            return;
        }
        for (slot, &user) in self.slots.iter_mut().zip(list.user_id.iter()) {
            match (slot.as_ref(), user) {
                (Some(open), u) if open.user == u => {}
                (Some(open), _) => {
                    // SAFETY: closing a handle this struct opened.
                    unsafe { scePadClose(open.handle) };
                    tracing::info!("pad: user {:#x} left", open.user);
                    *slot = None;
                }
                (None, USER_ID_INVALID) => {}
                (None, u) => {
                    // SAFETY: a signed-in user id, the standard port, no param.
                    let handle = unsafe { scePadOpen(u, PORT_STANDARD, 0, std::ptr::null()) };
                    if handle < 0 {
                        // Right after launch scePadOpen can fail for a while; the next scan retries.
                        tracing::debug!("pad: open for user {u:#x}: {}", pf_ps5_sys::sce_error(handle));
                        continue;
                    }
                    // SAFETY: a handle just opened.
                    unsafe { scePadSetVibrationMode(handle, VIBRATION_MODE_RUMBLE) };
                    let motion = motion_sensor_on(handle).map_or_else(pf_ps5_sys::sce_error, pf_ps5_sys::sce_error);
                    tracing::info!("pad: user {u:#x} on handle {handle}, motion sensor {motion}");
                    *slot = Some(Open { user: u, handle });
                }
            }
        }
    }

    fn handle(&self, slot: u8) -> Option<i32> {
        self.slots.get(usize::from(slot))?.as_ref().map(|o| o.handle)
    }
}

impl Pads for Ps5Pads {
    fn poll(&mut self, out: &mut Vec<PadSample>) {
        if self.scanned.is_none_or(|t| t.elapsed() >= RESCAN) {
            self.scanned = Some(Instant::now());
            self.rescan();
        }
        for (i, open) in self.slots.iter().enumerate() {
            let Some(open) = open else { continue };
            let mut data = ScePadData::default();
            // SAFETY: a live handle and a 120-byte out-parameter.
            if unsafe { scePadReadState(open.handle, &mut data) } != 0 {
                continue;
            }
            if let Some(sample) = crate::pad_map::sample(i as u8, &data) {
                out.push(sample);
            }
        }
    }

    fn feedback(&mut self, slot: u8, feedback: &PadFeedback) {
        let Some(handle) = self.handle(slot) else { return };
        // SAFETY: a live handle; every parameter is a local that outlives its call.
        unsafe {
            match feedback {
                PadFeedback::Rumble { low, high } => {
                    let p = ScePadVibrationParam {
                        large_motor: crate::pad_map::motor(*low),
                        small_motor: crate::pad_map::motor(*high),
                    };
                    scePadSetVibration(handle, &p);
                }
                PadFeedback::Light { r, g, b } => {
                    let p = ScePadLightBarParam {
                        r: *r,
                        g: *g,
                        b: *b,
                        reserved: 255,
                    };
                    scePadSetLightBar(handle, &p);
                }
                PadFeedback::Trigger { which, effect } => {
                    if let Some(p) = crate::triggers::param(*which, effect) {
                        scePadSetTriggerEffect(handle, &p);
                    }
                }
            }
        }
    }

    fn release_all(&mut self) {
        let reset = crate::triggers::reset();
        let still = ScePadVibrationParam::default();
        for open in self.slots.iter().flatten() {
            // SAFETY: live handles; locals outlive the calls.
            unsafe {
                scePadSetVibration(open.handle, &still);
                scePadSetTriggerEffect(open.handle, &reset);
                scePadResetLightBar(open.handle);
            }
        }
    }
}
