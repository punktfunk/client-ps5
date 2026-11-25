//! libSceNet: the console's socket API. Used by the probe to compare against BSD sockets; the
//! client reaches the network through std until a probe says otherwise.

use core::ffi::{c_char, c_void};

/// `setsockopt(SOL_SOCKET, …)`: non-blocking I/O on an sceNet socket.
pub const SO_NBIO: i32 = 0x1200;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SceNetEpollEvent {
    pub events: u32,
    pub reserved: u32,
    pub ident: u64,
    pub data: u64,
}

extern "C" {
    /// `type_` is the bare type: no `SOCK_NONBLOCK` / `SOCK_CLOEXEC`.
    pub fn sceNetSocket(name: *const c_char, domain: i32, type_: i32, protocol: i32) -> i32;
    pub fn sceNetSocketClose(fd: i32) -> i32;
    pub fn sceNetBind(fd: i32, addr: *const c_void, len: u32) -> i32;
    pub fn sceNetSendto(
        fd: i32,
        buf: *const c_void,
        len: usize,
        flags: i32,
        addr: *const c_void,
        addr_len: u32,
    ) -> isize;
    pub fn sceNetRecvfrom(
        fd: i32,
        buf: *mut c_void,
        len: usize,
        flags: i32,
        addr: *mut c_void,
        addr_len: *mut u32,
    ) -> isize;
    pub fn sceNetErrnoLoc() -> *mut i32;
}

#[cfg(target_vendor = "sie")]
type Create = unsafe extern "C" fn(*const c_char, i32, i32) -> i32;
#[cfg(target_vendor = "sie")]
type Destroy = unsafe extern "C" fn(i32) -> i32;
#[cfg(target_vendor = "sie")]
type Ntoa = unsafe extern "C" fn(i32, *const c_char, *mut u32, i32, i32, i32) -> i32;

/// The resolver's memory pool, in bytes.
#[cfg(target_vendor = "sie")]
const POOL_BYTES: i32 = 16 << 10;

/// `name` through libSceNet's resolver, each step's answer to `log`: a pool, a resolver on it,
/// one lookup with the resolver's own timeout. The address comes back as its four bytes.
#[cfg(target_vendor = "sie")]
pub fn resolve_probe(name: &core::ffi::CStr, log: &mut dyn FnMut(String)) -> Option<[u8; 4]> {
    use crate::kernel::{export, module};
    let module = match module("libSceNet.sprx") {
        Ok(module) => module,
        Err(code) => {
            log(format!("libSceNet module -> {code:#010x}"));
            return None;
        }
    };
    let find = |symbol: &core::ffi::CStr, log: &mut dyn FnMut(String)| {
        export(module, symbol)
            .map_err(|code| log(format!("{} -> {code:#010x}", symbol.to_string_lossy())))
            .ok()
    };
    let pool_create = find(c"sceNetPoolCreate", log)?;
    let pool_destroy = find(c"sceNetPoolDestroy", log)?;
    let create = find(c"sceNetResolverCreate", log)?;
    let destroy = find(c"sceNetResolverDestroy", log)?;
    let ntoa = find(c"sceNetResolverStartNtoa", log)?;
    // SAFETY: each is the module's export under its own name, with the PS4 signature the
    // name carries.
    let (pool_create, pool_destroy, create, destroy, ntoa) = unsafe {
        (
            core::mem::transmute::<*mut c_void, Create>(pool_create),
            core::mem::transmute::<*mut c_void, Destroy>(pool_destroy),
            core::mem::transmute::<*mut c_void, Create>(create),
            core::mem::transmute::<*mut c_void, Destroy>(destroy),
            core::mem::transmute::<*mut c_void, Ntoa>(ntoa),
        )
    };
    let label = c"pf-probe".as_ptr();
    // SAFETY: a NUL-terminated label and plain integers.
    let pool = unsafe { pool_create(label, POOL_BYTES, 0) };
    log(format!("pool -> {pool:#010x}"));
    if pool < 0 {
        return None;
    }
    // SAFETY: as above, on the pool just made.
    let resolver = unsafe { create(label, pool, 0) };
    log(format!("resolver -> {resolver:#010x}"));
    let mut address = 0_u32;
    let answer = (resolver >= 0).then(|| {
        // SAFETY: a NUL-terminated name and a live out-word; the resolver is destroyed after.
        let rc = unsafe { ntoa(resolver, name.as_ptr(), &mut address, 0, 0, 0) };
        // SAFETY: a resolver this function made.
        unsafe { destroy(resolver) };
        rc
    });
    // SAFETY: a pool this function made, with its resolver gone.
    unsafe { pool_destroy(pool) };
    let rc = answer?;
    log(format!("lookup -> {rc:#010x}"));
    (rc == 0).then(|| address.to_ne_bytes())
}
