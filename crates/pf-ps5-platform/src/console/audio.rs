//! `AudioOut`: a MAIN port at 48 kHz, S16 (the format console titles have proven), one 256-frame
//! grain per blocking write. Stereo plays on the stereo port, 5.1 and 7.1 on the eight-channel
//! one (`audio_map`).

use anyhow::{bail, Result};
use pf_ps5_app::platform::{AudioOut, AudioPort};
use pf_ps5_sys::audio::*;
use pf_ps5_sys::user::*;

pub struct Ps5Audio;

impl Ps5Audio {
    pub fn new() -> Result<Self> {
        // SAFETY: no arguments.
        let rc = unsafe { sceAudioOutInit() };
        if rc != 0 && rc != ERROR_ALREADY_INIT {
            bail!("sceAudioOutInit: {}", pf_ps5_sys::sce_error(rc));
        }
        Ok(Self)
    }
}

impl AudioOut for Ps5Audio {
    fn open(&self, channels: u8, rate: u32) -> Result<Box<dyn AudioPort>> {
        let (format, width) = match crate::audio_map::port_channels(usize::from(channels)) {
            Some(2) => (FORMAT_S16_STEREO, 2),
            Some(width) => (FORMAT_S16_8CH, width),
            None => bail!("AudioOut plays 2, 6 or 8 channels, not {channels}"),
        };
        // SAFETY: plain integer arguments.
        let handle = unsafe { sceAudioOutOpen(USER_ID_SYSTEM, PORT_MAIN, 0, GRAIN_FRAMES, rate, format) };
        if handle < 1 {
            bail!("sceAudioOutOpen: {}", pf_ps5_sys::sce_error(handle));
        }
        tracing::info!("audio: port {handle}, {channels} ch on {width} S16 @ {rate} Hz, grain {GRAIN_FRAMES}");
        Ok(Box::new(Port {
            handle,
            channels: usize::from(channels),
            width,
            pcm: vec![0; GRAIN_FRAMES as usize * width],
        }))
    }

    fn haptics(&self) -> bool {
        true
    }

    /// The vibration port of the user in `slot`: the pads take their slots in the login list's
    /// order (`pads::Ps5Pads::rescan`), and the port is opened for the user, not the pad.
    fn open_haptics(&self, slot: u8) -> Result<Box<dyn AudioPort>> {
        let mut list = SceUserServiceLoginUserIdList::default();
        // SAFETY: `list` is a live 16-byte out-parameter.
        let rc = unsafe { sceUserServiceGetLoginUserIdList(&mut list) };
        let user = list.user_id.get(usize::from(slot)).copied().unwrap_or(USER_ID_INVALID);
        if rc != 0 || user == USER_ID_INVALID {
            bail!("no signed-in user in slot {slot}");
        }
        // SAFETY: plain integer arguments.
        let handle = unsafe { sceAudioOutOpen(user, PORT_VIBRATION, 0, GRAIN_FRAMES, 48_000, FORMAT_S16_STEREO) };
        if handle < 1 {
            bail!("sceAudioOutOpen: {}", pf_ps5_sys::sce_error(handle));
        }
        tracing::info!("haptics: port {handle} for user {user:#x}, stereo S16 @ 48000 Hz, grain {GRAIN_FRAMES}");
        Ok(Box::new(Port {
            handle,
            channels: 2,
            width: 2,
            pcm: vec![0; GRAIN_FRAMES as usize * 2],
        }))
    }
}

struct Port {
    handle: i32,
    /// The stream's channels, and the port's.
    channels: usize,
    width: usize,
    pcm: Vec<i16>,
}

impl AudioPort for Port {
    fn grain_frames(&self) -> usize {
        GRAIN_FRAMES as usize
    }

    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        let want = GRAIN_FRAMES as usize * self.channels;
        if pcm.len() != want {
            bail!("grain of {} samples, port takes {want}", pcm.len());
        }
        crate::audio_map::interleave(pcm, self.channels, &mut self.pcm, self.width);
        // SAFETY: `pcm` holds exactly one grain at the opened width and outlives the call.
        let rc = unsafe { sceAudioOutOutput(self.handle, self.pcm.as_ptr().cast()) };
        if rc < 0 {
            bail!("sceAudioOutOutput: {}", pf_ps5_sys::sce_error(rc));
        }
        Ok(())
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        // SAFETY: draining and closing a handle this port opened.
        unsafe {
            sceAudioOutOutput(self.handle, std::ptr::null());
            sceAudioOutClose(self.handle);
        }
    }
}
