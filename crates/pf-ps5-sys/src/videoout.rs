//! libSceVideoOut outside Mesa's WSI: the display's HDR capability, read before the WSI opens the
//! output for the process, and the HDR10 scan-out probe (buffers registered in the HDR10 pixel
//! format, flipped with solid PQ fills, so the 64 KiB tiling needs no swizzle).

use core::ffi::c_void;

use crate::kernel;

const USER_SYSTEM: i32 = 0xff;
/// Packed R10 G10 B10 A2, red in the low bits; the format tags the output BT.2020 / PQ.
pub const FORMAT_HDR10: u64 = 0x8100_0704_2200_0000;
const TILING_64KB_R_X: u32 = 0;
const FORMAT_SDR: u64 = 0x8000_0000_0000_0000;
const FLIP_VSYNC: u32 = 1;
const REQUEST_DEFAULT: u32 = 1;
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

fn fill(buffer: &mut [u32], [r, g, b]: [u32; 3]) {
    buffer.fill(r | (g << 10) | (b << 20) | (3 << 30));
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

fn flip_phases(handle: i32, log: &mut dyn FnMut(String)) -> Result<(), i32> {
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
    // SAFETY: maps the block just allocated; VideoOut may hold the buffers, so they're never unmapped.
    let rc = unsafe { kernel::sceKernelMapDirectMemory(&mut address, bytes, PROT_CPU_GPU_RW, 0, phys, ALIGN) };
    log(format!("map -> {rc:#010x}"));
    if rc != 0 {
        return Err(rc);
    }
    // SAFETY: the mapping is `bytes` long, writable, 2 MiB aligned, and nothing else refers to it.
    let words = unsafe { core::slice::from_raw_parts_mut(address.cast::<u32>(), bytes / 4) };
    // SAFETY: a plain setter on a live handle.
    log(format!("flip rate -> {:#010x}", unsafe {
        sceVideoOutSetFlipRate(handle, 0)
    }));
    let null_c = core::ptr::null();
    // SAFETY: the default output request takes no parameter blocks.
    let rc = unsafe { sceVideoOutConfigureOutput(handle, REQUEST_DEFAULT, null_c, null_c, null_c) };
    log(format!("configure output -> {rc:#010x}"));
    let base = address.cast::<u8>();
    let null = core::ptr::null_mut();
    let mut buffers = [0, 1].map(|i| Buffer {
        // SAFETY: both pointers are inside the mapping, one buffer apart.
        data: unsafe { base.add(i * BUFFER_BYTES) }.cast(),
        metadata: null,
        reserved: [null; 2],
    });
    let mut attr = [0_u8; 80];
    // SAFETY: `attr` is the 80-byte attribute block the call fills; `buffers` outlives the registration.
    let rc = unsafe {
        sceVideoOutSetBufferAttribute2(
            attr.as_mut_ptr().cast(),
            FORMAT_HDR10,
            TILING_64KB_R_X,
            WIDTH,
            HEIGHT,
            0,
            0,
            0,
        );
        sceVideoOutRegisterBuffers2(handle, 0, 0, buffers.as_mut_ptr(), 2, attr.as_mut_ptr().cast(), 0, null)
    };
    log(format!("register hdr10 {WIDTH}x{HEIGHT} -> {rc:#010x}"));
    if rc != 0 {
        // SAFETY: as above, with the SDR format: tells a refused format from a refused buffer.
        let sdr = unsafe {
            sceVideoOutSetBufferAttribute2(
                attr.as_mut_ptr().cast(),
                FORMAT_SDR,
                TILING_64KB_R_X,
                WIDTH,
                HEIGHT,
                0,
                0,
                0,
            );
            sceVideoOutRegisterBuffers2(handle, 0, 0, buffers.as_mut_ptr(), 2, attr.as_mut_ptr().cast(), 0, null)
        };
        log(format!("register sdr {WIDTH}x{HEIGHT} -> {sdr:#010x}"));
        return Err(rc);
    }
    let mut flip = 0_i64;
    for (n, phase) in PHASES.iter().enumerate() {
        fill(words, phase.rgb);
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
