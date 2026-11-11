//! libkernel: direct memory (Videodec2's buffers), flexible memory, the kernel log, the sandbox.

use core::ffi::{c_char, c_void};

/// The direct-memory type every console source uses: CPU-cached, coherent with the GPU.
pub const MEMORY_TYPE: i32 = 12;
/// Direct memory is allocated and mapped in 16 KiB pages.
pub const PAGE: usize = 0x4000;
pub const PROT_CPU_READ: i32 = 0x01;
pub const PROT_CPU_WRITE: i32 = 0x02;
pub const PROT_GPU_READ: i32 = 0x10;
pub const PROT_GPU_WRITE: i32 = 0x20;
/// CPU read/write, GPU read/write.
pub const PROT_CPU_GPU_RW: i32 = 0x33;
/// CPU write, GPU read/write: decoder input and frame pools.
pub const PROT_CPU_W_GPU_RW: i32 = 0x32;
pub const PROT_CPU_RW: i32 = 0x03;

/// The system software version from `kern.sdk_version` (`0x13420000` is 13.42), or `None`
/// where the sysctl is refused.
#[cfg(target_os = "freebsd")]
pub fn system_version() -> Option<u32> {
    let (mut version, mut len) = (0u32, size_of::<u32>());
    // SAFETY: the name is NUL-terminated; `version` is writable for `len` bytes.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.sdk_version".as_ptr(),
            (&raw mut version).cast(),
            &mut len,
            core::ptr::null_mut::<c_void>() as _,
            0,
        )
    };
    (rc == 0 && version != 0).then_some(version)
}

/// `cpuset_*affinity`'s level and id kind for "the calling thread".
#[cfg(target_vendor = "sie")]
const CPU_LEVEL_WHICH: core::ffi::c_int = 3;
#[cfg(target_vendor = "sie")]
const CPU_WHICH_TID: core::ffi::c_int = 1;

#[cfg(target_vendor = "sie")]
extern "C" {
    fn cpuset_getaffinity(
        level: core::ffi::c_int,
        which: core::ffi::c_int,
        id: i64,
        size: usize,
        mask: *mut u64,
    ) -> core::ffi::c_int;
    fn cpuset_setaffinity(
        level: core::ffi::c_int,
        which: core::ffi::c_int,
        id: i64,
        size: usize,
        mask: *const u64,
    ) -> core::ffi::c_int;
}

/// The calling thread's CPU mask, or the errno. The console takes an eight-byte set and
/// refuses `sizeof(cpuset_t)` with ERANGE (`ProsperoLight` `platform/ps5/ps5_thread_placement.c:28-32`).
#[cfg(target_vendor = "sie")]
pub fn thread_affinity() -> Result<u64, i32> {
    let mut mask = 0u64;
    // SAFETY: `mask` is writable for the eight bytes passed.
    if unsafe { cpuset_getaffinity(CPU_LEVEL_WHICH, CPU_WHICH_TID, -1, size_of::<u64>(), &mut mask) } != 0 {
        // SAFETY: errno is this thread's.
        return Err(unsafe { *libc::__error() });
    }
    Ok(mask)
}

/// Sets the calling thread's CPU mask, which threads it starts afterwards inherit. Read back:
/// a call that succeeds is no proof the mask took. `Err` is the errno, or ERANGE for a read-back
/// that differs.
#[cfg(target_vendor = "sie")]
pub fn set_thread_affinity(mask: u64) -> Result<(), i32> {
    // SAFETY: `mask` is readable for the eight bytes passed.
    if unsafe { cpuset_setaffinity(CPU_LEVEL_WHICH, CPU_WHICH_TID, -1, size_of::<u64>(), &mask) } != 0 {
        // SAFETY: errno is this thread's.
        return Err(unsafe { *libc::__error() });
    }
    match thread_affinity()? {
        got if got == mask => Ok(()),
        _ => Err(libc::ERANGE),
    }
}

/// The directory a title's sandbox mounts the system's shared files under (`/<word>/common`),
/// or `None` where the call names none.
#[cfg(target_vendor = "sie")]
pub fn sandbox_word() -> Option<String> {
    // SAFETY: takes nothing; returns null or a NUL-terminated string libkernel owns for the
    // process's life.
    let word = unsafe { sceKernelGetFsSandboxRandomWord() };
    if word.is_null() {
        return None;
    }
    // SAFETY: non-null, and NUL-terminated by the contract above.
    let word = unsafe { core::ffi::CStr::from_ptr(word) };
    Some(word.to_string_lossy().into_owned())
}

/// Rounds `len` up to [`PAGE`].
pub const fn page_align(len: usize) -> usize {
    (len + PAGE - 1) & !(PAGE - 1)
}

extern "C" {
    pub fn sceKernelGetDirectMemorySize() -> i64;
    pub fn sceKernelAllocateDirectMemory(
        search_start: i64,
        search_end: i64,
        len: usize,
        alignment: usize,
        memory_type: i32,
        phys_start: *mut i64,
    ) -> i32;
    pub fn sceKernelMapDirectMemory(
        addr: *mut *mut c_void,
        len: usize,
        prot: i32,
        flags: i32,
        phys_start: i64,
        alignment: usize,
    ) -> i32;
    pub fn sceKernelReleaseDirectMemory(phys_start: i64, len: usize) -> i32;
    pub fn sceKernelMunmap(addr: *mut c_void, len: usize) -> i32;
    pub fn sceKernelMapNamedFlexibleMemory(
        addr: *mut *mut c_void,
        len: usize,
        prot: i32,
        flags: i32,
        name: *const c_char,
    ) -> i32;
    pub fn sceKernelReleaseFlexibleMemory(addr: *mut c_void, len: usize) -> i32;
    /// One line to the kernel log (klog, TCP 3232). Append the newline yourself.
    pub fn sceKernelDebugOutText(channel: i32, text: *const c_char) -> i32;
    pub fn sceKernelUsleep(micros: u32) -> i32;
    /// The sandbox's ten-character directory name; see [`sandbox_word`].
    pub fn sceKernelGetFsSandboxRandomWord() -> *const c_char;
}
