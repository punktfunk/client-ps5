//! libSceVideoOut outside Mesa's WSI: the display's HDR capability, read before the WSI opens the
//! output for the process, and two probes. HDR10 scan-out registers buffers in the HDR10 pixel
//! format and flips solid PQ fills, so the 64 KiB tiling needs no swizzle. Output mode asks the
//! output for each colorimetry and for a variable refresh, under a grey SDR picture.

use core::ffi::{c_void, CStr};
use std::time::{Duration, Instant};

use crate::kernel;

const USER_SYSTEM: i32 = 0xff;
/// Packed R10 G10 B10 A2, red in the low bits; the format tags the output BT.2020 / PQ.
pub const FORMAT_HDR10: u64 = 0x8100_0704_2200_0000;
const TILING_64KB_R_X: u32 = 0;
const FORMAT_SDR: u64 = 0x8000_0000_0000_0000;
const FLIP_VSYNC: u32 = 1;
/// `sceVideoOutConfigureOutput` presets: the output's own mode, and 119.88 Hz.
const REQUEST_DEFAULT: u32 = 1;
const REQUEST_120_HZ: u32 = 15;
/// Opaque mid grey in the SDR format.
const GREY: u32 = 0xff80_8080;
/// The flip status record's words (Mesa's `wsi_common_videoout.c` reads the fourth).
const STATUS_WORDS: usize = 16;
const STATUS_PROCESS_TIME: usize = 1;
const STATUS_FLIP_ARG: usize = 3;
/// Colorimetry values asked for in turn. Firmware's log names the output's own state `0x5`
/// for 709 and `0xe` for BT.2020 PQ; shadPS4 has 12 for the PS4's BT.2020 PQ request.
const COLORIMETRIES: core::ops::RangeInclusive<u8> = 0..=15;
/// Flip rates paced under a variable refresh. 90 and 100 divide no 119.88 Hz grid.
const PACES: [u32; 4] = [60, 90, 100, 50];
const WIDTH: u32 = 3840;
const HEIGHT: u32 = 2160;
const ALIGN: usize = 2 << 20;
/// Four bytes a pixel, height rounded to 128-row tiles, rounded to the alignment.
const BUFFER_BYTES: usize = (4 * WIDTH as usize * HEIGHT.div_ceil(128) as usize * 128).div_ceil(ALIGN) * ALIGN;
const DIRECT_TYPE_CPU: i32 = 12;
const PROT_CPU_GPU_RW: i32 = 0x01 | 0x02 | 0x10 | 0x20;
/// The capability word's bit for a display that shows BT.2020 PQ (shadPS4 `video_out.h`).
pub const CAPABILITY_BT2020_PQ: u64 = 0x80;
/// Record sizes offered to the capability call, smallest first. shadPS4's record is one `u64`.
const CAPABILITY_SIZES: [usize; 3] = [8, 16, 32];

/// The output mode a title asks for (shadPS4 `video_out.cpp`): `0xff` in a field is "any".
#[repr(C)]
pub struct Mode {
    pub size: u32,
    pub encoding: u8,
    pub range: u8,
    pub colorimetry: u8,
    pub depth: u8,
    pub refresh_rate: u64,
    pub resolution: u64,
    pub reserved: [u8; 8],
}

const _: () = assert!(size_of::<Mode>() == 32);

type ModeSetAny = unsafe extern "C" fn(*mut Mode, u32);
type ConfigureOutputMode = unsafe extern "C" fn(i32, u32, *const Mode, *const c_void, u32, u32) -> i32;
type VrrUnpeg = unsafe extern "C" fn(i32) -> i32;
type VrrPeg = unsafe extern "C" fn(i32, u64, u64) -> i32;

#[repr(C)]
struct Buffer {
    data: *mut c_void,
    metadata: *mut c_void,
    reserved: [*mut c_void; 2],
}

extern "C" {
    fn sceVideoOutOpen(user: i32, bus: i32, index: i32, param: *const c_void) -> i32;
    fn sceVideoOutClose(handle: i32) -> i32;
    fn sceVideoOutSetFlipRate(handle: i32, rate: i32) -> i32;
    fn sceVideoOutConfigureOutput(
        handle: i32,
        request: u32,
        a: *const c_void,
        b: *const c_void,
        c: *const c_void,
    ) -> i32;
    fn sceVideoOutSetBufferAttribute2(
        attr: *mut c_void,
        format: u64,
        tiling: u32,
        width: u32,
        height: u32,
        r1: u64,
        r2: u32,
        r3: u64,
    );
    fn sceVideoOutRegisterBuffers2(
        handle: i32,
        set: i32,
        start: i32,
        buffers: *mut Buffer,
        count: i32,
        attr: *mut c_void,
        r: i32,
        option: *mut c_void,
    ) -> i32;
    fn sceVideoOutSubmitFlip(handle: i32, index: i32, mode: u32, arg: i64) -> i32;
    fn sceVideoOutWaitVblank(handle: i32) -> i32;
    fn sceVideoOutGetFlipStatus(handle: i32, status: *mut [u64; STATUS_WORDS]) -> i32;
    /// Positive where the output takes `request`.
    fn sceVideoOutIsOutputSupported(
        handle: i32,
        request: u32,
        a: *const c_void,
        b: *const c_void,
        c: *const c_void,
    ) -> i32;
    /// NID `kGVLc3htQE8`. The trailing `_` is the sized form, like `sceVideoOutModeSetAny_`:
    /// `size` is the caller's record, and a size it refuses fails `0x80290001`.
    fn sceVideoOutGetDeviceCapabilityInfo_(handle: i32, info: *mut u64, size: usize) -> i32;
}

/// The display's capability word and the record size it answered at,
/// [`CAPABILITY_BT2020_PQ`] set when it shows HDR10. Opens and closes its own handle: Mesa's WSI
/// keeps the output once it opens it, so this runs first.
pub fn device_capability() -> Result<(u64, usize), i32> {
    // SAFETY: a null parameter block is the default output; the handle is closed below.
    let handle = unsafe { sceVideoOutOpen(USER_SYSTEM, 0, 0, core::ptr::null()) };
    if handle < 0 {
        return Err(handle);
    }
    // Sized for the widest record offered, in case this firmware's is wider than the one word.
    let mut info = [0_u64; 4];
    let mut answer = Err(0);
    for size in CAPABILITY_SIZES {
        // SAFETY: `info` is live, and 32 bytes long, the largest size offered.
        let rc = unsafe { sceVideoOutGetDeviceCapabilityInfo_(handle, info.as_mut_ptr(), size) };
        answer = if rc == 0 { Ok((info[0], size)) } else { Err(rc) };
        if rc == 0 {
            break;
        }
    }
    // SAFETY: `handle` came from sceVideoOutOpen and isn't used again.
    unsafe { sceVideoOutClose(handle) };
    answer
}

/// One solid fill: a name, its 10-bit PQ codes, and how long it shows.
pub struct Phase {
    pub name: &'static str,
    pub rgb: [u32; 3],
    pub seconds: u64,
}

/// Red 1000 nits (shows the channel order), grey 203 (SDR white), white 1000, PQ full scale.
pub const PHASES: [Phase; 4] = [
    Phase {
        name: "red 1000 nits",
        rgb: [769, 0, 0],
        seconds: 8,
    },
    Phase {
        name: "grey 203 nits",
        rgb: [592, 592, 592],
        seconds: 8,
    },
    Phase {
        name: "white 1000 nits",
        rgb: [769, 769, 769],
        seconds: 8,
    },
    Phase {
        name: "white pq 1023",
        rgb: [1023, 1023, 1023],
        seconds: 8,
    },
];

/// Fills the buffers with one pixel value and writes them through to memory for the scan-out.
fn fill(buffer: &mut [u32], pixel: u32) {
    buffer.fill(pixel);
    for at in (0..buffer.len()).step_by(16) {
        // SAFETY: `at` is inside `buffer`; clflush only writes the line back to memory.
        unsafe { core::arch::x86_64::_mm_clflush(buffer.as_ptr().add(at).cast()) };
    }
    // SAFETY: a fence has no memory operands.
    unsafe { core::arch::x86_64::_mm_mfence() };
}

/// Opens `VideoOut`, registers two 3840x2160 HDR10 buffers and flips each phase in turn, logging
/// every result. Returns the first failing call's code. The display shows the phases while it runs.
pub fn hdr_scanout_probe(log: &mut dyn FnMut(String)) -> Result<(), i32> {
    // SAFETY: a null parameter block is the default output; the handle is closed below.
    let handle = unsafe { sceVideoOutOpen(USER_SYSTEM, 0, 0, core::ptr::null()) };
    log(format!("open handle={handle:#010x}"));
    if handle < 0 {
        return Err(handle);
    }
    let result = flip_phases(handle, log);
    // SAFETY: `handle` came from sceVideoOutOpen and isn't used again.
    let closed = unsafe { sceVideoOutClose(handle) };
    log(format!("close -> {closed:#010x}"));
    result
}

/// Direct memory for two 3840x2160 buffers, as their pixels. Never unmapped: `VideoOut` may
/// hold the buffers.
fn two_buffers(log: &mut dyn FnMut(String)) -> Result<&'static mut [u32], i32> {
    let bytes = 2 * BUFFER_BYTES;
    let mut phys = -1_i64;
    // SAFETY: the out-pointer is a live i64; the range is the whole direct-memory space.
    let rc = unsafe {
        kernel::sceKernelAllocateDirectMemory(
            0,
            kernel::sceKernelGetDirectMemorySize(),
            bytes,
            ALIGN,
            DIRECT_TYPE_CPU,
            &mut phys,
        )
    };
    log(format!("alloc {bytes} bytes -> {rc:#010x}"));
    if rc != 0 {
        return Err(rc);
    }
    let mut address: *mut c_void = core::ptr::null_mut();
    // SAFETY: maps the block just allocated.
    let rc = unsafe { kernel::sceKernelMapDirectMemory(&mut address, bytes, PROT_CPU_GPU_RW, 0, phys, ALIGN) };
    log(format!("map -> {rc:#010x}"));
    if rc != 0 {
        return Err(rc);
    }
    // SAFETY: the mapping is `bytes` long, writable, 2 MiB aligned, and nothing else refers to it.
    Ok(unsafe { core::slice::from_raw_parts_mut(address.cast::<u32>(), bytes / 4) })
}

/// Registers the two buffers [`two_buffers`] mapped as indices 0 and 1, in `format`.
fn register(handle: i32, words: &mut [u32], format: u64) -> i32 {
    let base = words.as_mut_ptr().cast::<u8>();
    let null = core::ptr::null_mut();
    let mut buffers = [0, 1].map(|i| Buffer {
        // SAFETY: both pointers are inside the mapping, one buffer apart.
        data: unsafe { base.add(i * BUFFER_BYTES) }.cast(),
        metadata: null,
        reserved: [null; 2],
    });
    let mut attr = [0_u8; 80];
    // SAFETY: `attr` is the 80-byte attribute block the call fills; the mapping outlives the
    // registration.
    unsafe {
        sceVideoOutSetBufferAttribute2(
            attr.as_mut_ptr().cast(),
            format,
            TILING_64KB_R_X,
            WIDTH,
            HEIGHT,
            0,
            0,
            0,
        );
        sceVideoOutRegisterBuffers2(handle, 0, 0, buffers.as_mut_ptr(), 2, attr.as_mut_ptr().cast(), 0, null)
    }
}

fn flip_phases(handle: i32, log: &mut dyn FnMut(String)) -> Result<(), i32> {
    let words = two_buffers(log)?;
    // SAFETY: a plain setter on a live handle.
    log(format!("flip rate -> {:#010x}", unsafe {
        sceVideoOutSetFlipRate(handle, 0)
    }));
    let null_c = core::ptr::null();
    // SAFETY: the default output request takes no parameter blocks.
    let rc = unsafe { sceVideoOutConfigureOutput(handle, REQUEST_DEFAULT, null_c, null_c, null_c) };
    log(format!("configure output -> {rc:#010x}"));
    let rc = register(handle, words, FORMAT_HDR10);
    log(format!("register hdr10 {WIDTH}x{HEIGHT} -> {rc:#010x}"));
    if rc != 0 {
        // The SDR format tells a refused format from a refused buffer.
        let sdr = register(handle, words, FORMAT_SDR);
        log(format!("register sdr {WIDTH}x{HEIGHT} -> {sdr:#010x}"));
        return Err(rc);
    }
    let mut flip = 0_i64;
    for (n, phase) in PHASES.iter().enumerate() {
        let [r, g, b] = phase.rgb;
        fill(words, r | (g << 10) | (b << 20) | (3 << 30));
        log(format!("phase {n} {} for {} s", phase.name, phase.seconds));
        let (mut failed, end) = (
            0_u32,
            std::time::Instant::now() + std::time::Duration::from_secs(phase.seconds),
        );
        while std::time::Instant::now() < end {
            // SAFETY: buffer indices 0 and 1 are the two registered above.
            let rc = unsafe { sceVideoOutSubmitFlip(handle, (flip & 1) as i32, FLIP_VSYNC, flip) };
            if rc != 0 {
                failed += 1;
                if failed <= 3 {
                    log(format!("flip {flip} -> {rc:#010x}"));
                }
            }
            flip += 1;
            // SAFETY: a wait on a live handle.
            unsafe { sceVideoOutWaitVblank(handle) };
        }
        log(format!("phase {n} done flips={flip} failed={failed}"));
    }
    Ok(())
}

/// One line to the kernel log, where firmware's own lines about the output sit beside it.
fn klog(line: &str) {
    if let Ok(text) = std::ffi::CString::new(format!("punktfunk-ps5: output-mode: {line}\n")) {
        // SAFETY: a NUL-terminated string that outlives the call.
        unsafe { kernel::sceKernelDebugOutText(0, text.as_ptr()) };
    }
}

/// `libSceVideoOut`'s handle, for [`export`]. The module is loaded already: the title imports it.
fn module() -> Result<i32, i32> {
    let mut paths = vec![
        "libSceVideoOut.sprx".to_owned(),
        "/system/common/lib/libSceVideoOut.sprx".to_owned(),
    ];
    paths.extend(kernel::sandbox_word().map(|word| format!("/{word}/common/lib/libSceVideoOut.sprx")));
    let mut last = -1;
    for path in paths {
        let Ok(path) = std::ffi::CString::new(path) else {
            continue;
        };
        let null = core::ptr::null();
        // SAFETY: a NUL-terminated path, no arguments, no option block, no result wanted.
        last = unsafe { kernel::sceKernelLoadStartModule(path.as_ptr(), 0, null, 0, null, core::ptr::null_mut()) };
        if last > 0 {
            return Ok(last);
        }
    }
    Err(last)
}

/// An export the SDK's link stub may lack, looked up by name: a missing one is a code here,
/// where a missing import is a null call.
fn export(module: i32, name: &CStr) -> Result<*mut c_void, i32> {
    let mut address = core::ptr::null_mut();
    // SAFETY: a NUL-terminated name and a live out-pointer.
    let rc = unsafe { kernel::sceKernelDlsym(module, name.as_ptr(), &mut address) };
    if rc == 0 && !address.is_null() {
        Ok(address)
    } else {
        Err(rc)
    }
}

/// Flips the two buffers at `fps` for `seconds`. Returns the spacing between the flips the
/// output showed, by its own stamps: least, median and most.
fn paced(handle: i32, fps: u32, seconds: u32, flip: &mut i64) -> Option<[u64; 3]> {
    let period = Duration::from_nanos(1_000_000_000 / u64::from(fps));
    let started = Instant::now();
    let (mut shown, mut stamps) = (0, Vec::new());
    for n in 0..fps * seconds {
        if let Some(rest) = (period * n).checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
        *flip += 1;
        let mut status = [0_u64; STATUS_WORDS];
        // SAFETY: buffer indices 0 and 1 are registered; `status` is the record's full size.
        let read = unsafe {
            sceVideoOutSubmitFlip(handle, (*flip & 1) as i32, FLIP_VSYNC, *flip);
            sceVideoOutGetFlipStatus(handle, &mut status)
        };
        if read == 0 && status[STATUS_FLIP_ARG] != shown {
            shown = status[STATUS_FLIP_ARG];
            stamps.push(status[STATUS_PROCESS_TIME]);
        }
    }
    let mut gaps: Vec<u64> = stamps.windows(2).map(|pair| pair[1].wrapping_sub(pair[0])).collect();
    gaps.sort_unstable();
    Some([*gaps.first()?, gaps[gaps.len() / 2], *gaps.last()?])
}

/// Opens `VideoOut` under a grey SDR picture, asks it for each colorimetry in turn, then
/// unpegs the refresh and flips at rates a fixed 119.88 Hz can't show evenly. Each step goes
/// to the kernel log as it happens: firmware's `video mode(…)`, `v-request` and
/// `video: port:HDMI …` lines beside it say what the output did. Returns the first failing
/// call's code.
pub fn output_mode_probe(log: &mut dyn FnMut(String)) -> Result<(), i32> {
    let mut say = |line: String| {
        klog(&line);
        log(line);
    };
    // SAFETY: a null parameter block is the default output; the handle is closed below.
    let handle = unsafe { sceVideoOutOpen(USER_SYSTEM, 0, 0, core::ptr::null()) };
    say(format!("open handle={handle:#010x}"));
    if handle < 0 {
        return Err(handle);
    }
    let result = output_modes(handle, &mut say);
    let null = core::ptr::null();
    // SAFETY: the default request takes no parameter blocks; the handle isn't used after the close.
    let (restored, closed) = unsafe {
        (
            sceVideoOutConfigureOutput(handle, REQUEST_DEFAULT, null, null, null),
            sceVideoOutClose(handle),
        )
    };
    say(format!("restore -> {restored:#010x}; close -> {closed:#010x}"));
    result
}

fn output_modes(handle: i32, say: &mut dyn FnMut(String)) -> Result<(), i32> {
    let words = two_buffers(say)?;
    let null = core::ptr::null();
    // SAFETY: plain calls on a live handle; the 119.88 Hz request takes no parameter blocks.
    let (supported, configured) = unsafe {
        sceVideoOutSetFlipRate(handle, 0);
        (
            sceVideoOutIsOutputSupported(handle, REQUEST_120_HZ, null, null, null),
            sceVideoOutConfigureOutput(handle, REQUEST_120_HZ, null, null, null),
        )
    };
    say(format!(
        "119.88 Hz supported {supported}, configure -> {configured:#010x}"
    ));
    let rc = register(handle, words, FORMAT_SDR);
    say(format!("register sdr {WIDTH}x{HEIGHT} -> {rc:#010x}"));
    if rc != 0 {
        return Err(rc);
    }
    fill(words, GREY);
    let mut flip = 0_i64;
    paced(handle, 60, 2, &mut flip);

    let module = module();
    say(match module {
        Ok(handle) => format!("libSceVideoOut module {handle}"),
        Err(code) => format!("libSceVideoOut module -> {code:#010x}"),
    });
    let Ok(module) = module else { return Ok(()) };

    match (
        export(module, c"sceVideoOutModeSetAny_"),
        export(module, c"sceVideoOutConfigureOutputMode_"),
    ) {
        (Ok(any), Ok(configure)) => {
            // SAFETY: both are the module's exports under their own names, with the PS4
            // signatures the names carry.
            let (any, configure) = unsafe {
                (
                    core::mem::transmute::<*mut c_void, ModeSetAny>(any),
                    core::mem::transmute::<*mut c_void, ConfigureOutputMode>(configure),
                )
            };
            let size = size_of::<Mode>() as u32;
            // SAFETY: `mode` is written by the first call for `size` bytes and read by the second.
            let ask = |colorimetry: Option<u8>| unsafe {
                let mut mode = core::mem::zeroed::<Mode>();
                any(&mut mode, size);
                if let Some(colorimetry) = colorimetry {
                    mode.colorimetry = colorimetry;
                }
                configure(handle, 0, &mode, null, size, 0)
            };
            for colorimetry in COLORIMETRIES {
                let rc = ask(Some(colorimetry));
                say(format!("colorimetry {colorimetry:#x} -> {rc:#010x}"));
                if rc == 0 {
                    paced(handle, 60, 2, &mut flip);
                }
            }
            say(format!("colorimetry any -> {:#010x}", ask(None)));
        }
        (any, configure) => say(format!(
            "mode request not exported: set-any {:#010x}, configure {:#010x}",
            any.err().unwrap_or(0),
            configure.err().unwrap_or(0)
        )),
    }

    match (
        export(module, c"sceVideoOutVrrUnpegFromFixedRate"),
        export(module, c"sceVideoOutVrrPegToFixedRate"),
    ) {
        (Ok(unpeg), Ok(peg)) => {
            // SAFETY: both are the module's exports under their own names: a handle, and a
            // handle with two reserved words.
            let (unpeg, peg) = unsafe {
                (
                    core::mem::transmute::<*mut c_void, VrrUnpeg>(unpeg),
                    core::mem::transmute::<*mut c_void, VrrPeg>(peg),
                )
            };
            // SAFETY: plain calls on a live handle.
            let (configured, unpegged) = unsafe {
                (
                    sceVideoOutConfigureOutput(handle, REQUEST_120_HZ, null, null, null),
                    unpeg(handle),
                )
            };
            say(format!("119.88 Hz -> {configured:#010x}, unpeg -> {unpegged:#010x}"));
            for fps in PACES {
                say(format!(
                    "unpegged {fps} fps: spacing {:?} us",
                    paced(handle, fps, 3, &mut flip)
                ));
            }
            // SAFETY: as above.
            let pegged = unsafe { peg(handle, 0, 0) };
            say(format!("peg -> {pegged:#010x}"));
            say(format!(
                "pegged 90 fps: spacing {:?} us",
                paced(handle, 90, 3, &mut flip)
            ));
        }
        (unpeg, peg) => say(format!(
            "vrr peg not exported: unpeg {:#010x}, peg {:#010x}",
            unpeg.err().unwrap_or(0),
            peg.err().unwrap_or(0)
        )),
    }
    Ok(())
}
