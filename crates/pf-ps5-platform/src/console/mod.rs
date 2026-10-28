//! The console's implementations, assembled into one [`Platform`].

mod audio;
mod klog;
mod lifecycle;
mod pads;
mod videodec2;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use pf_ps5_app::core::caps::VideoCaps;
use pf_ps5_app::core::media::VideoSink;
use pf_ps5_app::platform::{DeviceInfo, Platform, Storage, StreamSpec, VideoDecoders};
use pf_ps5_vk::{GpuDevice, Presenter, PyroLane, Target, VideoSlot};
use punktfunk_core::quic;

/// Title-local writable storage; present when `param.json` sets `downloadDataSize`.
const DOWNLOAD0: &str = "/download0";
/// Where a payload launch (ELF loader, no title sandbox) keeps its files instead.
const PAYLOAD_DIR: &str = "/data/punktfunk-ps5";

/// The pins this build was made from, for the log and the About screen.
const MESA_PIN: &str = match option_env!("PF_MESA_PIN") {
    Some(p) => p,
    None => "unpinned",
};

struct Dir(PathBuf);

impl Storage for Dir {
    fn data_dir(&self) -> &Path {
        &self.0
    }
}

fn storage() -> Dir {
    if Path::new(DOWNLOAD0).is_dir() {
        Dir(PathBuf::from(DOWNLOAD0))
    } else {
        let _ = std::fs::create_dir_all(PAYLOAD_DIR);
        Dir(PathBuf::from(PAYLOAD_DIR))
    }
}

/// The statically linked RADV as an `ash::Entry`: no loader on the console.
fn entry() -> ash::Entry {
    unsafe extern "system" fn gipa(
        instance: ash::vk::Instance,
        name: *const std::ffi::c_char,
    ) -> ash::vk::PFN_vkVoidFunction {
        use ash::vk::Handle as _;
        // SAFETY: forwards Vulkan's own contract: `name` is NUL-terminated, `instance` null or live.
        unsafe { pf_ps5_sys::vk::vk_icdGetInstanceProcAddr(instance.as_raw() as *mut _, name) }
    }
    // SAFETY: `gipa` resolves every global and instance command, as a loader would.
    unsafe {
        ash::Entry::from_static_fn(ash::StaticFn {
            get_instance_proc_addr: gipa,
        })
    }
}

/// `PyroWave` on the GPU, H.264/HEVC through `Videodec2`.
struct Ps5Video {
    pyro: PyroLane,
    vd2: videodec2::Vd2Lane,
    /// What [`display_hdr`] read at launch.
    hdr: bool,
}

impl VideoDecoders for Ps5Video {
    fn caps(&self) -> VideoCaps {
        VideoCaps {
            pyrowave: self.pyro.available(),
            hdr: self.hdr,
            ..VideoCaps::PS5
        }
    }

    fn open(&self, spec: &StreamSpec) -> Result<Box<dyn VideoSink>> {
        if spec.codec == quic::CODEC_PYROWAVE {
            self.pyro.open(spec)
        } else {
            self.vd2.open(spec)
        }
    }
}

/// Narrows this thread, and so every thread it starts from here on, to the CPUs Videodec2's
/// workers are not given. `ProsperoLight` keeps every stream thread off them
/// (`src/moonlight_stream.cpp:4646-4659`) and decodes 4K120 HEVC at 56 Mbit/s in 6.9 ms a
/// picture; sharing them, this title took 7.4-8.7 ms at 4.5 Mbit/s. Runs before logging exists,
/// so it reports to the kernel log.
fn keep_off_decoder_cpus() {
    use pf_ps5_sys::kernel::{set_thread_affinity, thread_affinity};
    use std::io::Write as _;
    let decoder = pf_ps5_sys::videodec2::DEFAULT_AFFINITY;
    let line = match thread_affinity() {
        Ok(mask) if mask & !decoder == 0 => format!("CPUs {mask:#x} are all the decoder's; left as they are"),
        Ok(mask) => match set_thread_affinity(mask & !decoder) {
            Ok(()) => format!(
                "CPUs {:#x}, off the decoder's {decoder:#x} (was {mask:#x})",
                mask & !decoder
            ),
            Err(errno) => format!("CPUs {mask:#x} kept: narrowing them failed (errno {errno})"),
        },
        Err(errno) => format!("CPU mask unreadable (errno {errno})"),
    };
    let _ = writeln!(klog::Klog::default(), "punktfunk-ps5: threads: {line}");
}

/// Whether the display shows HDR10, so a session asks the host for PQ only then. Read once a
/// launch, before Mesa's WSI takes the output: an HDR setting changed while the title runs applies
/// on the next launch. An unreadable capability keeps HDR on; the presenter still falls back to
/// SDR when the output refuses HDR10.
fn display_hdr() -> bool {
    use pf_ps5_sys::videoout::{device_capability, CAPABILITY_BT2020_PQ};
    use std::io::Write as _;
    let (hdr, line) = match device_capability() {
        Ok(word) => {
            let hdr = word & CAPABILITY_BT2020_PQ != 0;
            (
                hdr,
                format!("capability {word:#x}, HDR10 {}", if hdr { "on" } else { "off" }),
            )
        }
        Err(code) => (
            true,
            format!("capability unreadable: {}; HDR10 kept on", pf_ps5_sys::sce_error(code)),
        ),
    };
    let _ = writeln!(klog::Klog::default(), "punktfunk-ps5: display: {line}");
    hdr
}

/// The data directory and the kernel log, for the probe, which needs nothing else.
pub fn probe_parts() -> (PathBuf, Box<dyn std::io::Write + Send>) {
    (storage().0, Box::new(klog::Klog::default()))
}

/// Builds the console platform. Order matters: Videodec2 loads before anything else touches the
/// process (the 13.60 trap), Mesa's environment is set before the instance exists, and the
/// display's HDR capability is read before Mesa's WSI opens the output.
pub fn platform() -> Result<Platform> {
    videodec2::warm_up();
    keep_off_decoder_cpus();
    let dir = storage();
    let cache = dir.0.join("radv-cache");
    let _ = std::fs::create_dir_all(&cache);
    std::env::set_var("MESA_SHADER_CACHE_DIR", &cache);
    let hdr = display_hdr();
    let dev = Arc::new(GpuDevice::new(entry(), Target::Display).context("bring up the GPU")?);
    let slot = Arc::new(VideoSlot::default());
    let presenter = Presenter::new(dev.clone(), slot.clone()).context("bring up the display")?;
    let device = DeviceInfo {
        platform: "ps5",
        model: "PlayStation 5".into(),
        firmware: pf_ps5_sys::kernel::system_version()
            .map_or_else(String::new, |v| format!("{:x}.{:02x}", v >> 24, (v >> 16) & 0xff)),
        gpu: format!("{} (mesa {MESA_PIN})", dev.name),
        ui: presenter.ui(),
    };
    Ok(Platform {
        display: Box::new(presenter),
        video: Arc::new(Ps5Video {
            pyro: PyroLane::new(dev.clone(), slot.clone()),
            vd2: videodec2::Vd2Lane::new(dev, slot),
            hdr,
        }),
        audio: Arc::new(audio::Ps5Audio::new()?),
        pads: Box::new(pads::Ps5Pads::new()?),
        storage: Arc::new(dir),
        lifecycle: Box::new(lifecycle::Ps5Lifecycle),
        log: Some(Box::new(klog::Klog::default())),
        device,
    })
}
