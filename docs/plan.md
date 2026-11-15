# PS5 client — a Rust homebrew title on Vulkan

> **Status: code written through WP4 and linked for the console, 2026-10-16; nothing proven on a
> console yet.** Where the code departs from this plan, and what each work package still needs a
> console for: `NOTES.md`. Repository `punktfunk/client-ps5`.
>
> Supersedes the 2026-10-15 proposal draft: Vulkan (RADV) replaces GL/AGC as the one graphics path, PyroWave is in the first release rather than a later extension, 3840×2160 at
> 119.88 Hz is the output the client is built for, the console's own libraries are preferred over
> ports, and every work package is written for a developer who never holds the console. The
> sibling of record is `punktfunk/client-webos`: same shared crates, same layering, same build and
> telemetry shape; what differs is the platform crate underneath.

The question this answers: *the console can run Vulkan and decode HEVC, the shared crates draw the
console UI and speak the protocol — what has to be built, in which order, so that a tester with a
13.60 console and a developer without one can bring up a client that plays 4K120?*

## 0. Decisions

1. **Rust owns the application; C stays at three boundaries.** The payload SDK's startup objects
   and link stubs, RADV (Mesa's Vulkan driver, C) and PyroWave (C++) are linked in as archives.
   Nothing else is C. No SDL, no FFmpeg, no PipeWire, no bundled GL. `unsafe` lives in two
   crates — `pf-ps5-sys` (the console's C ABI) and `pf-ps5-vk` (the Vulkan plane) — and every
   other crate is `#![forbid(unsafe_code)]`.
2. **One Vulkan device for the process's life.** RADV from the Mesa PS5 port is linked into the
   title; the console UI (Skia's Vulkan backend), PyroWave decode, the HEVC/H.264 presentation
   shader and the stream overlays all run on that device. The swapchain is `VK_KHR_display` on
   VideoOut. There is no second graphics API in the binary and no handover between UI and stream.
3. **The console's libraries where the console has one.** VideoOut (reached through RADV's
   swapchain), AudioOut, Pad, UserService, SystemService, Sysmodule, the kernel's direct-memory
   calls and BSD sockets through libkernel, Videodec2 for H.264/HEVC. Third-party native code is
   limited to RADV (+ zlib/zstd), PyroWave (+ Granite), libc++, Skia and libopus.
4. **Two video lanes, one presenter.** PyroWave decodes on the GPU through the shared
   `pf-client-video` decoder. H.264/HEVC decode through Videodec2 into direct memory that the
   presenter imports as a Vulkan image (`VK_EXT_external_memory_host`, the same mechanism RADV's
   PS5 swapchain uses for its own buffers); a staged upload is the measured fallback. Both lanes
   end in the shared planar CSC shader. No AV1: nothing public shows Videodec2 decoding it, and
   RADV on the console is built without Vulkan Video.
5. **Output is the console's one mode.** 3840×2160, at 59.94 Hz or at 119.88 Hz when the title's
   `param.json` declares high-frame-rate output and the display takes it. A 1080p or 1440p stream
   is scaled by the presenter into that target, as every other client does with `video_fit`.
   SDR first: the Vulkan swapchain exposes `B8G8R8A8` sRGB only; HDR is §9.
6. **A title, not a payload.** The release is a folder `/data/homebrew/<TITLE_ID>/` with
   `eboot.bin`, `sce_sys/param.json` and `icon0.png`, launched from the home screen by the title
   loader the tester already runs. The client stays inside the title sandbox: identity, settings
   and logs live in `/download0` (`downloadDataSize` in `param.json`). No filesystem elevation —
   on 13.60 Videodec2 refuses to initialise once a process has left its sandbox. A bare ELF sent to
   the ELF loader on port 9021 stays the diagnostic form for runtime probes.
7. **Three implementations of every platform seam.** `ps5` (the console), `host` (a Linux or
   macOS developer box: Vulkan through the system loader, an offscreen or windowed surface,
   scripted pads, a WAV audio sink) and `fake` (tests). The seams are traits in the application
   crate; the platform crates implement them; Cargo enforces the direction. A feature is not
   "working" until a console test-run bundle (§5) says so.
8. **No new player-facing settings.** `Platform::Ps5` in `pf-console-ui` gates rows: the codec
   list includes PyroWave, window rows are off, pad glyphs on, HDR rows off until §9. Resolution
   and refresh are the existing `width`/`height`/`refresh_hz` rows with the values the console
   can scan out. Rules: `docs/settings.md` in the monorepo.
9. **Independence from the other PS5 streaming client.** ProsperoLight is read for platform
   facts — API names, struct layouts, limits, the 13.60 trap — and each fact is re-proven by a
   probe here before it is relied on. No fork, no translated application code, no copied interface
   or assets, no Moonlight protocol stack.
10. **Licensing: source `MIT OR Apache-2.0`, binaries GPL-3.0-or-later.** Decided 2026-10-15,
    binaries changed 2026-11-15. Three linked pieces are GPL-3.0-or-later: PS5_Vulkan's
    `app_crt.cpp` and `app_cpp_runtime.cpp` (entry point, C++ runtime glue), PS5_PayloadSDK's
    `libps5platform.a` (heap, thread-stack wraps, libc replacements) and the
    `sce_module/libc.prx` its generator writes. A binary that links them is GPL-3.0-or-later and
    ships with the license text and the pins of its sources. Replacing all three with our own
    permissive code would return binaries to `MIT OR Apache-2.0`. It buys named threads, a heap
    we tune and one link recipe, and no speed, for several console rounds on the entry point
    and the heap, so it waits for a need. Such code is written from the documented facts, never
    translated from the GPL sources. RADV and PyroWave are MIT, Skia BSD, libc++ Apache-2.0 with
    the LLVM exception;
    `THIRD-PARTY-NOTICES.txt` is generated by `build.rs` as the webOS client does.

## 1. What is already true (2026-10-15)

Facts the plan stands on, with where each comes from. None is measured by Punktfunk yet.

### 1.1 The console

| Fact | Source | Consequence |
|---|---|---|
| The public SDK compiles with `-target x86_64-sie-ps5`, FreeBSD 11.4 headers (`__FreeBSD_version 1104001`), `-femulated-tls -fno-plt -fno-stack-protector`, links `crt1.o`, a thin `libc.a`, and stubs for the console's modules (`libSceVideoOut`, `libSceAudioOut`, `libScePad`, `libSceUserService`, `libSceSystemService`, `libSceSysmodule`, `libSceAgc`, `libSceAgcDriver`, `libSceNet`, `libkernel`). SDK is GPL-3.0; `include/freebsd` is BSD. | `ps5-payload-dev/sdk` `host/bin/prospero-clang`, `libc/Makefile`, `sce_stubs/` | Rust targets a FreeBSD-11 ABI with emulated TLS and PIE. Videodec2 is **not** in the stub set: a hand-written link stub (symbol names only) is how it is linked; no Sony file is needed. |
| The SDK's `getaddrinfo` is a failure stub; sockets are BSD sockets through libkernel; `recvmmsg` exists in the SDK libc. | SDK `libc/netdb.c`, `libc/recvmmsg.c`; ProsperoLight TROUBLESHOOTING | Hostnames do not resolve through `std`. Manual host entry is by address; a resolver over `libSceNet` is optional later. |
| Mesa 26.2.0 with 28 patches is a pacbrew package: RADV with a PS5 winsys on AGC (no DRM), `VK_KHR_display` on VideoOut, zink/EGL on top, `-Dvideo-codecs=` empty, built as `libvulkan.so.1` since the console has no loader. The patches come from `mihawk-99/PS5_Mesa`. | `ps5-payload-dev/pacbrew-repo/mesa/PKGBUILD` and patches 0001–0028 (2026-10-08) | Vulkan is the graphics API. No Vulkan Video. |
| RADV reports Vulkan 1.4 on the console; the pinned 1.4 CTS ran there twice, the second run with 1,569,390 passes and 0 failures. Compute and transfer queue families are served from the graphics ring. Fifteen Vulkan samples hold 119.9 fps at 3840×2160. | `mihawk-99/PS5_Vulkan` README (2026-10-01) | Compute shaders, timeline semaphores, synchronization2 are available. Queue concurrency is not real; the presenter and decoder share one ring. |
| The VideoOut swapchain: one plane, 3840×2160 at 59.94 Hz, 119.88 Hz offered when the title's metadata declares HFR and the display refreshes at it; FIFO is the only present mode; 2–3 images; `B8G8R8A8_UNORM`/`_SRGB`, `SRGB_NONLINEAR` only. A flip thread submits at vblank. The swapchain requires `GetMemoryHostPointerPropertiesEXT`. | Mesa patch 0011 `wsi_common_videoout.c` | 4K120 SDR is the ceiling and the target. HDR needs a WSI change (§9). Host-pointer import works on this driver. |
| On a VRR display a frame presents when ready between 48 and 120 Hz. With the loader's kernel hooks active a system call costs about 20 µs; paused, 0.73 µs. | PS5_Vulkan README, vkQuake measurements | VRR is plausible but unverified for this client. Receive-path syscall cost is a first-week measurement. |
| A title cannot load a repository-built `.so` at run time; the driver is linked into the title. | PS5_Vulkan README | RADV, PyroWave and Skia are static archives in one ELF. |
| Videodec2 decodes H.264 and HEVC (Main, Main10) into GPU-visible memory; a 4K HEVC stream above roughly 80 Mbps at 120 fps exceeds its real-time budget. On 13.60 it fails its first compute query (`811d0111`) once the process has left the title sandbox. | ProsperoLight README, TROUBLESHOOTING, PYROWAVE.md | Decoder initialisation and the whole client stay in the sandbox. 4K120 HEVC is bitrate-capped; PyroWave is the high-bitrate lane. |
| PyroWave runs on RADV on the console in all seven profiles, including HDR10 scanout through a 10-bit BT.2020/PQ VideoOut format; 500–600 Mbps was stable on wired LAN. | ProsperoLight PYROWAVE.md | The GPU and driver carry 4K120 PyroWave. The limit is the network. |
| A title ends through the shell, never by returning from `main` or calling `exit` (the console records that as a crash). AGC is initialised once per process. Four signed-in users give four pads; a pad without a user is invisible. | PS5_VulkanTemplate `ps5/README.md`, ProsperoLight ARCHITECTURE/TROUBLESHOOTING | Lifecycle is a seam with one correct exit. Pad enumeration goes through UserService. |
| Deploy is FTP to `/data/homebrew/<TITLE_ID>/` (port 2121); klog is a TCP stream on port 3232; a scripted test run writes a file into the title folder, the next launch runs it and writes PNGs and a verdict the PC fetches back. | ProsperoLight DEPLOYMENT.md; PS5_VulkanTemplate `tools/run.sh` | The blind-development loop (§5) exists in the ecosystem; this repository adopts its shape. |
| The SDL2 PS5 port (zlib) uses `sceVideoOutOpen/SetBufferAttribute2/RegisterBuffers2/SubmitFlip`, `sceKernelAllocateMainDirectMemory/MapDirectMemory`, `sceAudioOutInit/Open/Output/Close`. | `ps5-payload-dev/SDL` `src/video/ps5`, `src/audio/ps5` | A permissively licensed reference for VideoOut, AudioOut and Pad call shapes. |
| No Rust toolchain or project targets the console. `rustc` has `x86_64-unknown-freebsd` (tier 2, std); the `libc` crate selects a FreeBSD ABI level by `--cfg libc_unstable_freebsd_version="11"`. | rust-lang target list; `libc` 0.2.190 `build.rs` | A custom target JSON derived from the FreeBSD spec, `-Zbuild-std` on a pinned nightly, FreeBSD 11 ABI pinned. §3.1. |

### 1.2 The shared crates (monorepo `0.43.1`, edition 2024, MSRV 1.85)

| Crate | What the PS5 client uses | What has to change upstream |
|---|---|---|
| `pf-console-ui` | `Console::new(ConsoleOptions, ConsoleEntry, &ConsoleHandles)`, `frame(canvas, &Viewport, pad, pad_pref, pads)`, `menu/ok/pointer/key/text`, `take_action() -> OverlayAction`, `ConsoleBus::drain() -> Vec<ConsoleCmd>`, `ConsoleShared`/`LibraryShared` setters, `SnapshotStore`, `Platform`. Fonts are embedded. Screens render to CPU raster in tests (`PF_CONSOLE_DUMP`). | `Platform::Ps5` and its match sites (`rows.rs`, `glyphs.rs`, `ring_editor.rs`, `controls.rs`, `players.rs`); the crate cfg list in `lib.rs` and the Cargo target tables gain the PS5 target; a skia-binaries archive for the target (`git.unom.io/unom/skia-binaries`). |
| `pf-client-core` (`default-features = false`) | `trust::{Settings, KnownHost, KnownHosts, load_or_create_identity, pair_with_host, request_access_to_host, probe_one}`, `presets`, `profiles`, `settings::effective_settings`, `menu_nav::{MenuNav, MenuSample, MenuEvent, PadInfo}`, `console::{OverlayAction, PointerInput}`, `library` types, `shortcuts`, `discovery` (mdns-sd). | `build.rs` `portable` alias and a Cargo target table gain the PS5 target; `discovery`'s OS list too; `shortcuts::Client::Ps5`. Library/art HTTPS fetch is desktop-only upstream; the client keeps its own pinned-TLS fetch as webOS does. |
| `punktfunk-core` (`quic`) | `client::NativeClient::{connect, pair, request_access, probe}`, `next_frame/next_audio/next_rumble/next_hidout/next_hdr_meta/next_host_timing`, `send_input/send_rich_input`, `request_keyframe/request_rfi/report_decode_us`, `reanchor::ReanchorGate`, `hud::Stats`, `phase::{CadenceClock, PanelGrid, pace_slot}`, caps bits (`CODEC_PYROWAVE 0x08`, `VIDEO_CAP_*`), `audio::JitterPolicy`. Media reads run on a native thread through `quinn_udp::UdpSocketState`. | Nothing by design; `cfg(unix)` covers the target. The transport's socket options on libkernel are a WP0 probe. |
| `pf-client-video` (`default-features = false, features = ["pyrowave"]`) | `video_vk::{VulkanDecodeDevice, QueueLock}`, `video_pyrowave::PyroWaveDecoder` → `PyroWavePlanarFrame` (three plane views on the presenter's device), `video_color::ColorDesc`, `video_csc_spv` (committed SPIR-V), `DecodedFrame/DecodedImage`. | Cargo target tables (today `linux | windows | android`) and `pyrowave-sys`'s bindgen gate gain the PS5 target. |
| `pyrowave-sys` | Vendored PyroWave + Granite via cmake; C API 0.6.0; device requirements: Vulkan ≥ 1.3, `shaderInt16`, `storageBuffer8BitAccess`, `timelineSemaphore`, `subgroupSizeControl`, `computeFullSubgroups`, `synchronization2`, subgroup `VOTE|BALLOT|ARITHMETIC|SHUFFLE|SHUFFLE_RELATIVE|BASIC`; `shaderFloat16` optional. | cmake toolchain file for the PS5 compiler; the gate. |
| `clients/android/native/src/pyro/{device,present}.rs` | The reference for a Vulkan presenter without SDL: own `ash` instance and device, exported as `VulkanDecodeDevice`, three planes sampled by the shared CSC on its own swapchain. `crates/client/pf-console-ui/src/skia_overlay.rs` is the reference for a Skia `DirectContext` over `ash` handles. | Read, not linked. |
| `clients/probe` | `--trajectory` drives `NativeClient` headlessly; `main.rs` is the wire-level teaching copy. | The headless console probe in WP0 is modelled on `trajectory.rs`. |

All three monorepo dependencies are pinned to one git **rev**, never a tag — the webOS lesson:
a tag and a rev build two copies and the types do not unify.

### 1.3 What the webOS client gives this one

Lifted as they are, renamed where `Tv` says `Ps5`: the `core::media` traits (`VideoSink`,
`AudioSink`, `AudioPlane`, `MediaClock`, `VideoSinkCaps`), `session/{stage,timeline,pump,audio,
stats,probe,pad_audio}` with their `FakeSink` tests (freeze-until-reanchor, one PTS mapping per
picture, frames counted only on `Presented`), `services/{atomic,store,paths,join,budget,art,
library,status,pinned_tls,discovery,wol}`, `logger/{sink,ring,level}` (file + TCP telemetry +
on-screen ring), the `console/model.rs` service loop over `ConsoleCmd`, `runtime`'s
`UiOutcome`/`StreamOutcome` alternation, `build.rs`'s notices generator, the go-task + Docker
shape, and the CI that lints with `-D warnings` on the cross target because a host `cargo check`
proves nothing about gated code.

Not lifted: anything under `platform/webos`, the NDL sink, the pointer UI (`app/` — the console
is pad-only), SDL2, the glibc shim, `luna`/`ls2`.

Fixed rather than copied: the webOS `session ↔ platform` edge runs both ways in practice
(`session::pipeline::load_player` names the NDL constructors; `platform::audio` reaches into
`session::audio`). Here the backend choice is injected by the binary and the platform crate does
not depend on the application crate at all (§2).

## 2. Architecture

```text
  pf-ps5 (bin)            the title: SDK entry glue, wires ps5 adapters into the app, never returns
      │
  pf-ps5-app              portable, no unsafe: core (domain, seams, fakes), session (on
      │                   punktfunk-core), ui (Console hosting), services (store, discovery,
      │                   library, art), runtime (menu ⇄ stream), logger
      ├── pf-console-ui · pf-client-core · punktfunk-core
      │
  pf-ps5-vk               portable, unsafe with proofs: the Vulkan device wrapper and QueueLock,
      │                   the presenter (two lanes → one CSC → swapchain), pacing, the Skia
      │                   DirectContext, the PyroWave lane, frame read-back
      ├── ash · skia-safe · pf-client-video
      │
  pf-ps5-platform         the console: device on RADV + VK_KHR_display, AudioOut sink,
      │                   Pad/UserService, Videodec2 lane, /download0 storage, lifecycle, klog
      └── pf-ps5-sys      hand-written FFI: VideoOut types, AudioOut, Pad, UserService,
                          SystemService, Sysmodule, Videodec2, direct memory, klog; layout tests

  pf-ps5-host             the developer box: device via the loader, offscreen/windowed surface,
                          WAV or null audio, scripted pads, a temp dir, process exit
```

Dependency direction, enforced by Cargo: `pf-ps5-sys` ← `pf-ps5-platform` ← `pf-ps5` →
`pf-ps5-app` → `pf-ps5-vk`, and `pf-ps5-host` → `pf-ps5-app`. The application crate defines the
seams and knows no platform; both platform crates depend on the application crate's seam module
only (a `pf-ps5-app::platform` module, or a small `pf-ps5-seams` crate if the app crate grows a
dependency a platform crate must not inherit) and on `pf-ps5-vk` for the device type they hand
back. `pf-ps5-platform` and `pf-ps5-sys` are `cfg(target_vendor = "sie")`; `pf-ps5-host` is
`cfg(not(target_vendor = "sie"))`. The binary picks one. Tests run the app against `fake`
implementations on every CI runner and against `host` on a runner with a GPU or lavapipe.

### 2.1 Threads

| Thread | Owns | Never |
|---|---|---|
| main | the Vulkan device, swapchain, Skia context, `Console`, overlays, present | blocks on the network or a decoder |
| pump | `NativeClient::next_frame`, loss accounting, `ReanchorGate`, decode submit (`VideoStage`) | touches the swapchain |
| audio | `next_audio` → Opus decode → `JitterPolicy` ring → AudioOut grains (256 frames) | allocates per grain |
| input | Pad sampling at 250 Hz, `MenuNav`, `InputEvent` send, feedback (rumble, triggers, lightbar) | draws |
| service | `ConsoleCmd` handling: discovery, pairing, library and art fetch, probes, log upload | holds a GPU resource |
| client | `punktfunk-core`'s own tokio runtime and media reader | — |

The pump hands the presenter a decoded image plus a timeline value; the presenter picks the frame
due at the next vblank with `punktfunk_core::phase` (`CadenceClock` on the VideoOut vblank
period, `pace_slot`) and the webOS `Pacing` map. No frame queue is unbounded; the PyroWave lane
keeps two frames and decodes the newest, the Videodec2 lane keeps the decoder's own pool.

### 2.2 The seams (traits in `pf-ps5-app::platform`)

```rust
pub trait Gpu {                       // one per process
    fn device(&self) -> &VulkanDevice;            // ash handles + the QueueLock pf-client-video shares
    fn acquire(&mut self) -> Result<Frame>;       // swapchain image + the vblank the present targets
    fn present(&mut self, frame: Frame) -> Result<()>;
    fn refresh(&self) -> Refresh;                 // 59.94 / 119.88 Hz, measured from vblanks
    fn read_back(&mut self, into: &mut [u8]) -> Result<Extent>;   // test runs only
}
pub trait AudioOut: Send + Sync {
    fn open(&mut self, channels: u8, rate: u32) -> Result<AudioPort>;
    fn write(&self, port: &AudioPort, pcm: &[i16]) -> Result<()>;  // blocks one grain
    fn depth_ms(&self, port: &AudioPort) -> Option<i64>;
}
pub trait Pads: Send {
    fn poll(&mut self, out: &mut Vec<PadSample>);  // all signed-in users, 250 Hz
    fn feedback(&mut self, pad: PadId, out: &HidOutput);
    fn identity(&self, pad: PadId) -> PadIdentity;  // DualSense / DualSense Edge / other
}
pub trait HardwareDecoder: Send {     // the Videodec2 lane; PyroWave is pf-client-video
    fn open(&mut self, codec: Codec, w: u32, h: u32, ten_bit: bool) -> Result<()>;
    fn decode(&mut self, au: &[u8], pts_ns: u64) -> Result<Option<HwFrame>>;  // HwFrame: VkImage + wait
    fn release(&mut self, frame: HwFrame);
    fn flush(&mut self) -> Result<()>;
}
pub trait Storage { fn data_dir(&self) -> &Path; fn write_atomic(&self, path, bytes) -> Result<()>; }
pub trait Lifecycle { fn hide_splash(&self); fn exit(&self) -> !; fn poll_events(&mut self) -> Vec<SystemEvent>; }
pub trait LogSink: Write + Send {}    // klog, file in data_dir, optional TCP telemetry
```

Signatures are indicative; the contract tests in `pf-ps5-app/tests/seams/` are the spec. Each
trait has a `fake` in `pf-ps5-app` used by those tests and by the app's own unit tests, so the
session, the store and the console service are fully tested without a GPU or a console.

## 3. The three hard problems

### 3.1 Rust on `x86_64-sie-ps5`

A custom target JSON, committed as `targets/x86_64-sie-ps5.json`, derived from
`rustc --print target-spec-json --target x86_64-unknown-freebsd`:

| Key | Value | Why |
|---|---|---|
| `llvm-target` | `x86_64-sie-ps5` | LLVM knows the PS5 as a platform; the SDK compiles everything else with this triple. Fallback if codegen differs in a way that matters: `x86_64-unknown-freebsd` with the SDK's clang as the linker. |
| `os` / `vendor` / `env` | `freebsd` / `sie` / `""` | `std`'s FreeBSD port is the one that matches the kernel and libc ABI; `target_vendor = "sie"` is the gate word in shared crates: `cfg(all(target_os = "freebsd", target_vendor = "sie"))`. |
| `has-thread-local` | `false` | The SDK compiles with `-femulated-tls`; `std` then keys thread locals through `pthread_key`. |
| `linker` / `linker-flavor` | `prospero-clang` / `gnu-lld-cc` | The SDK's wrapper adds the sysroot, `crt1.o`, `-lc -lSceLibcInternal -lSceNet -lkernel_web`. |
| `position-independent-executables` | `true` | The SDK builds position-independent ELFs. |
| `panic-strategy` | `abort` | No unwinding across the C boundaries; a panic writes the log and aborts (§5.4). |
| `cpu` | `znver2` | The console's CPU; Mesa's PS5 patches already assume AVX2 (patch 0027). |

Build: `cargo +<pinned nightly> build -Z build-std=std,panic_abort --target targets/x86_64-sie-ps5.json`
with `RUSTFLAGS='--cfg libc_unstable_freebsd_version="11" --cfg getrandom_backend="custom"'`.
The `getrandom` custom backend wraps `arc4random_buf` from the SDK libc (the kernel has no
`getrandom(2)`); it is implemented in `pf-ps5-sys`, as the webOS client sets `linux_raw`.

Known gaps between `std`'s FreeBSD port and the console, each a WP0 probe with a shim in
`pf-ps5-sys` if it fails: `__error`, `_umtx_op` (mutex and condvar), `kqueue`/`kevent` (mio under
tokio and mdns-sd), `sysctl` (`available_parallelism`), `elf_aux_info`, `getrandom`, `dl_iterate_phdr`
(backtraces; RADV's patch 0021 says the console lacks it), `pthread_getthreadid_np`,
`clock_gettime(CLOCK_MONOTONIC)`. The SDK's `libc/` directory lists what Sony's libc lacks and
the SDK supplies — the first place to look when a symbol is missing.

The dependency chain that must compile and run: `libc`, `socket2`, `mio`, `tokio`, `quinn`,
`quinn-udp`, `rustls` with **aws-lc-rs** (the workspace pins it; `ring` is not in core's graph),
`rcgen`, `mdns-sd`, `if-addrs`, `opus` (libopus via cmake or the pacbrew package), `ureq` with
`rustls-no-provider`, `image` (PNG/JPEG only), `serde_json`, `skia-safe`, `ash`, `pyrowave-sys`.
`aws-lc-sys` is the one with a cmake and assembler build; FreeBSD x86_64 is a supported aws-lc
platform, so the risk is the toolchain file, not the code. If it does not build in the first
week, the fallback is a `ring` provider, which needs the maintainer's yes in the monorepo.

### 3.2 Vulkan from Rust, statically

`ash` with the `linked`-style entry: RADV is a static archive, `vkGetInstanceProcAddr` is a
link-time symbol, `ash::Entry::from_static_fn` wraps it. The archive is built from the pacbrew
Mesa package's exact source and patches with `-Ddefault_library=static`, pinned by the patch set's
sha1 (the PKGBUILD computes a `radv-build-id` the same way; without it RADV finds no shader cache
and silently has no device). Meson's cross file comes from the SDK (`prospero.ini`). Device
bring-up mirrors `clients/android/native/src/pyro/device.rs`: `VK_KHR_surface` + `VK_KHR_display`
on the instance, `VK_KHR_swapchain` + `VK_EXT_external_memory_host` on the device, the exact
feature list PyroWave reconstructs from `VulkanDecodeDevice` (§1.2), one graphics queue behind
`QueueLock`.

Skia: `skia-safe` with `vulkan,textlayout`, built once from source by skia-bindings with the SDK
compilers (`FORCE_SKIA_BUILD=1`, GN `target_os="linux"` with `cc`/`cxx`/`ar` overridden,
`skia_use_fontconfig=false`, bundled FreeType, no X11, no system freetype), published as the
archive the Cargo build downloads — the webOS armv7 recipe. The console draws with Skia's Vulkan
backend on the shared device, `DirectContext` over `ash` handles as `skia_overlay.rs` does, into
an offscreen RGBA slot composited by the presenter. Fallback for bring-up and for a backend
defect: CPU raster into a staging buffer uploaded each frame; the kit already renders that way in
its tests.

Shader cache: RADV's on-disk cache (`MESA_DISK_CACHE_DATABASE`) under `/download0`, keyed by the
pinned Mesa revision; the release ships a warmed cache so the first launch compiles nothing.
Measured by PS5_Vulkan: 3.77 s to first present from an empty cache versus 2.44 s warm.

### 3.3 Decoded frames into Vulkan without a CPU copy

PyroWave is solved by construction: it decodes on the shared device into `PyroWavePlanarFrame`
and the presenter samples the planes. Videodec2 is the open question, in three measured steps:

1. The decoder's output buffers are allocated by the client in direct memory
   (`sceKernelAllocateMainDirectMemory` + `sceKernelMapDirectMemory`) with the alignment the
   decoder asks for; the same mapping is imported as `VkDeviceMemory` through
   `VK_EXT_external_memory_host` and bound to a `VkImage` (NV12 as two R8/R8G8 planes, P010 as
   R16/R16G16). The presenter's CSC shader samples it. Zero copies.
2. If the decoder's stride or tiling does not match what RADV accepts for a linear image: one GPU
   copy from an imported buffer into a device-local image. One copy, GPU-side.
3. If host-pointer import fails for decoder memory: a staging upload per frame. 3840×2160 NV12 is
   12.4 MiB; at 120 fps that is 1.5 GB/s over the CPU — recorded as a number, not promised away.

Each frame carries the decoder's completion signal; the presenter waits on it before sampling and
hands the buffer back only after both the decoder and the last present that used it are done
(`HwFrame::release`). The lane produces `DecodedImage`-shaped frames so the presenter does not
know which lane fed it.

## 4. Work packages and exit criteria

Each package names what the tester does and what is proven without the console. "Bundle" means a
test-run bundle (§5.2). A package is done when its exit line is true on the console and in CI.

### WP0 — Toolchain and runtime (the two-week investigation)

Build: the target JSON, the pinned nightly and SDK revision, `scripts/toolchain.sh` installing
both into a Docker image, the cmake/meson cross files, a `task build-probe`. The `pf-ps5-probe`
binary runs numbered checks and prints one `probe NN ok|fail <detail>` line per check to klog and
to its log file: allocation, threads, thread-locals, mutex/condvar under contention, monotonic and
wall clocks, `available_parallelism`, files in `/download0` (create, rename, fsync, reopen after
relaunch), `arc4random`, UDP send/recv with `quinn_udp::UdpSocketState::new` and every socket
option `punktfunk-core` sets, TCP connect, TLS handshake with rustls/aws-lc-rs against the host's
management port, mDNS browse, `recvmmsg` batch size and the per-datagram cost at 10k datagrams/s,
a tokio runtime with two workers, a `std::process::abort` after writing the log. Then the headless
client: `NativeClient::pair`, `NativeClient::connect` at 1080p60, 600 frames received, input
sent, identity saved and reused on relaunch — `clients/probe/src/trajectory.rs` as the model.

Both launch forms are tested: the ELF on port 9021 and the probe installed as a title.

Tester: deploys the probe once as a payload and once as a title, launches each, closes it, runs
`task fetch-bundle`. Records model, firmware, loader and its hooks setting, display and its modes,
network link, in the first bundle's `environment.json` (a form in the probe's launch screen).

Without the console: the probe runs on `host` in CI; the target compiles in CI with `-D warnings`;
the ABI layout tests in `pf-ps5-sys` pin every struct the client passes to the console against the
sizes read from the SDK's FreeBSD headers.

**Exit:** every probe line `ok` in both forms on 13.60; the headless client pairs, reconnects with
its saved identity, receives frames, sends input; the dependency patch inventory is written down
(crate, version, patch or shim, upstreamable or not). If `std`'s port needs more than shims — a
fork of `std` — the scope is reassessed before WP1.

### WP1 — Vulkan on the console from Rust

RADV linked statically; instance, device, `VK_KHR_display` surface, three-image FIFO swapchain;
a clear and a textured quad; the PyroWave feature probe (`pf-presenter`'s list) printed as one
line; present pacing with the measured vblank period; `param.json` declaring HFR; a read-back of
the last frame to `verdict/frame.png`. PyroWave decoder created and fed one shipped sample frame.

Tester: launch, 30 s, close, fetch.

Without the console: the same program on `host` renders the same frame; `scripts/compare.py`
accepts a mean difference under 4.5/255 at 480×270 (the PS5_VulkanTemplate tolerance).

**Exit:** 3,000 frames at 119.88 Hz with zero missed vblanks in the bundle's pacing record; the
feature line matches what PyroWave requires; the console frame matches the host frame; the
PyroWave sample decodes and its three planes match the host's decode.

### WP2 — The shared console on the console

Skia for the target (§3.2); `Console` on the Skia Vulkan backend; `scePad` → `MenuSample` →
`MenuNav`; `SnapshotStore` over `/download0` with the webOS `StateWriter`; the service loop over
`ConsoleCmd`: discovery, `Pair`, `RequestAccess`, `FetchLibrary` + `push_art` through a pinned-TLS
client, `Probe`, `SaveHost/UpdateHost/ForgetHost`, `SendLogs`, `LoadLicenses`; `Platform::Ps5`
rows. The About screen shows the build id, the Mesa pin and the notices.

Tester: add host by address → request access → approve on the PC → library with covers → open
Settings → close. A second launch shows the host still paired.

Without the console: `PF_CONSOLE_DUMP` on the desktop and the `host` platform render the same
screens; the shared parity vectors (`clients/shared/console-vectors.json`) run in the app's tests;
the store round-trips the shared `settings.json` document whole (unmapped fields untouched).

**Exit:** console screens on the PS5 match the desktop dump within tolerance; add → pair → browse
by pad; pairing survives relaunch; no PS5-only settings key outside `extra` under `ps5.`.

### WP3 — The video plane

Both lanes behind the presenter: `pf-client-video`'s PyroWave decoder on the shared device, and
the Videodec2 lane (§3.3) for H.264 and HEVC at 8 and 10 bit; the planar CSC SPIR-V; pacing on
the vblank clock; `ReanchorGate` and the webOS `VideoStage` (hold on loss, resume on anchor, one
PTS map per picture); `hud::Stats` with decode and present stamps; the stats overlay.

Tester: three scripted sessions against their host — 4K60 HEVC, 4K120 HEVC at 60 Mbps, 4K120
PyroWave at 400 Mbps — 60 s each, then fetch.

Without the console: the lanes run on `host` from recorded elementary streams (the probe's
`--out` files from a real host) with the presenter writing frames; the `VideoStage` tests; the
decoder plan tests (dimensions, stride, pool size, release order) are device-free as in
`pf-vkdecode`.

**Exit:** all three sessions play with p95 decode-to-present under one frame interval and no
growing queue; the Videodec2 import step actually taken (1, 2 or 3 of §3.3) is recorded with its
per-frame cost; a 4K HEVC stream above the decoder's budget degrades to a reported hold, not a
crash.

### WP4 — The first playable client

Session pump, audio (AudioOut, `JitterPolicy`, stereo), one DualSense with rumble and adaptive
triggers through `scePad`, the disconnect chord and hold-to-exit, connect/reconnect with the
shared messages, `SessionPhase` into the console, system events (home button, controller loss →
release held inputs; suspend → end the session cleanly), the one correct exit.

Tester: add host → pair → browse → launch a game → play ten minutes → return to the library →
launch the desktop → disconnect → quit → relaunch, still paired. Report input feel in one sentence
and anything that stuck.

Without the console: the runtime loop runs end-to-end on `host` against the containerised host
from the webOS `docker:deploy` recipe (synthetic source, software encoder), including reconnects.

**Exit:** the whole path works with the pad alone; restart preserves pairing; held inputs are
released on every exit path; no new player-facing setting.

### WP5 — Reliability and latency

Soaks and faults: two hours at 4K120 PyroWave and two at 4K60 HEVC without a crash, memory
growth, queue growth or audio drift; 30 launch/stop/relaunch cycles; host restart, cable pull, pad
off and on, wrong fingerprint, corrupted `settings.json`, interrupted save. Percentiles for
receive→decode and decode→present, queue depth, misses, underruns, in the bundle.

Tester: the soak is a scripted run they start and leave; the fault list is a numbered checklist
with one line to write per item. The camera test: a phone at 240 fps on the pad's light bar (set
by the client on a button press) and the TV; the developer counts frames from the clip.

**Exit:** both soaks clean; every fault returns to an actionable screen with no stuck input; p95
client processing within two 120 Hz frames on the wired baseline; the camera number recorded
beside the same host and display measured with another Punktfunk client where the tester has one.

### WP6 — Package and release

`task package` → `dist/<TITLE_ID>/` and a zip; checksums; `THIRD-PARTY-NOTICES.txt`; the pins
file (SDK, Mesa patch set, Skia, PyroWave, monorepo rev, nightly); install, update with pairing
kept, and rollback procedures tested by the tester; GitHub Releases from CI on a tag; the
monorepo's install page gains the PS5 entry (with the maintainer's yes). Then §9, one capability at
a time.

Optional, when D10's need appears: the three GPL pieces go, each in its own change behind the
probe and WP1's bundle: a Rust entry point with the init arrays; our own platform layer (heap on
MIT dlmalloc, stack wraps, the libc replacements RADV and std call); our own `libc.prx`
generator. **Exit for this step:** `nm` of the linked title and the `link-title.sh` inputs name no
GPL object, and the WP4 soak passes on the replacements.

## 5. Developing without the console

The tester's time is the scarce resource. Everything below exists to spend it on what only the
console can answer.

### 5.1 The platform matrix

| | `ps5` | `host` | `fake` |
|---|---|---|---|
| Builds | Docker, the pinned image | native, any box | native |
| Runs | the console | a dev box or a GPU/lavapipe CI runner | every CI runner |
| Proves | the console | the app against a real Vulkan device and real sockets | contracts, state machines, stores |
| Pixels | read-back PNG in the bundle | PNG from the same code path | none |

A change to `pf-ps5-app` is tested on `fake` and `host` before it is built for `ps5`. A change to
`pf-ps5-platform` or `pf-ps5-sys` ships with the probe check or bundle field that will show it
working, written before the console sees it.

### 5.2 Test runs and bundles

`task deploy` uploads what changed over FTP (temporary name, rename on completion, `param.json`
last). `task test-run <scenario>` writes `test-run.json` into the title folder; the next launch
reads and deletes it, runs the scenario on its own (connect to the named host with the saved
pairing, play N seconds, exit), and writes `/download0/verdict/<time>/`: `log.txt`, the klog
excerpt if a klog capture was running, `frame-*.png`, `perf.json` (percentiles, counters,
pacing trace), `environment.json`. `task fetch-bundle` pulls the folder over FTP. The developer
reads bundles; the tester runs tasks. `docs/tester.md` is the tester's whole manual: install
the task runner, set `PS5_HOST`, run the three tasks, what "closed" means (fully, including the
crash dialog), and where to paste the one-line result.

### 5.3 Logging

The webOS logger: `tracing` through a non-blocking writer into `/download0/logs/` rotated per
launch under the host's 1 MiB bundle limit, an optional TCP sink named at launch (`-p telemetry=
IP:PORT`, the tester runs `nc -lk`), a 32-line ring for the on-screen log overlay, one live level.
Operator-register lines; the first line is the build id and the pins. `SendLogs` uploads the file
to the host as every client does, so the tester can hand over logs through their own host console.

### 5.4 Crashes

`panic = "abort"`. The panic hook writes the message and the frame-pointer stack (built with
`force-frame-pointers=yes`) to the log before aborting; a `SIGSEGV`/`SIGBUS` handler does the
same with the faulting address. Every CI build keeps its unstripped ELF as an artifact and
`scripts/symbolize.py <log> <elf>` names the frames. A crash without a bundle is a bug in the
tooling first.

### 5.5 Rules

- Every claim about the hardware in `docs/NOTES.md` cites a bundle id. "Should work" is not a
  claim.
- A console run tests at most one unknown. Two unknowns in one run cost two runs anyway.
- The build id is on screen (About) and in the first log line. A report without it is re-run.
- Nothing is asked of the tester that CI or `host` could have answered.
- The probe gains a check for every platform assumption the app code makes. The app does not
  carry a workaround whose cause the probe has not isolated.

## 6. Repository layout

```text
client-ps5/
  Cargo.toml               workspace; one monorepo rev for all three shared crates
  rust-toolchain.toml      the pinned nightly
  targets/x86_64-sie-ps5.json
  crates/
    pf-ps5-sys/            FFI, link stubs, getrandom backend, shims; tests/layout.rs
    pf-ps5-vk/             device, presenter, CSC, pacing, Skia context, PyroWave lane, read-back
    pf-ps5-platform/       Gpu, AudioOut, Pads, HardwareDecoder, Storage, Lifecycle, LogSink for the console
    pf-ps5-host/           the same for a dev box
    pf-ps5-app/            core/ session/ ui/ services/ runtime/ logger/ platform/ (seams + fakes)
    pf-ps5-probe/          the WP0 probe (uses sys + platform directly)
  src/main.rs              the title binary
  packaging/               param.json, icon0.png, pic0/pic1, description
  scripts/                 toolchain.sh, build-radv.sh, build-skia.sh, deploy.py, fetch-bundle.py, symbolize.py, compare.py
  taskfiles/ Taskfile.yml  build, check, test, lint, package, deploy, test-run, fetch-bundle, docker:*
  docs/                    plan.md (this), NOTES.md (bundle-cited findings), tester.md, pins.md
  .github/workflows/       lint + test on host, cross build in the image, release on tag
```

Conventions: `pf-ps5-sys` is the only crate with `extern "C"`; `pf-ps5-vk` is the only other
crate that allows `unsafe`, and every block has a `// SAFETY:` proof; the rest is
`#![forbid(unsafe_code)]`; clippy pedantic set from the webOS
manifest, `-D warnings` on the cross target in CI; writing rules from the monorepo's
`docs/writing.md` (commits, comments, two error registers); `AGENTS.md` at the root with the
layering, the commands and the invariants, kept to one screen.

## 7. Upstream changes (monorepo), each its own PR

1. `pf-console-ui`: `Platform::Ps5`; the match sites; the `lib.rs` cfg list and the Cargo tables
   gain `all(target_os = "freebsd", target_vendor = "sie")`; `shortcuts::Client::Ps5` in
   `pf-client-core`.
2. `pf-client-core`: `build.rs` `portable` alias and a Cargo target table; `discovery`'s OS list.
3. `pf-client-video` and `pyrowave-sys`: Cargo tables and the bindgen gate; a cmake toolchain
   hook (`CMAKE_TOOLCHAIN_FILE` already honoured by cmake-rs).
4. `skia-binaries`: the PS5 archive, with the build recipe beside the sha256 table.
5. Docs: the install page entry and the support matrix row, at WP6.
6. Only if WP0 forces it: a `ring` provider feature for the client side of `punktfunk-core`.

Nothing on the wire, nothing in the settings catalog.

## 8. Risks

| # | Risk | Trigger | Fallback |
|---|---|---|---|
| R1 | `std`'s FreeBSD port needs symbols the console lacks | WP0 probe lines | Shims in `pf-ps5-sys` over libkernel, the SDK's `libc/` pattern. More than shims → reassess before WP1. |
| R2 | `aws-lc-sys` does not cross-build | WP0, first week | A `ring` provider for the client side of `punktfunk-core` (upstream yes needed). |
| R3 | Static RADV in a Rust ELF: symbol clashes (Mesa's c11 threads, libc++), link time, size | WP1 | Patch 0022 already renames Mesa's threads; `--gc-sections` + LTO; a 40 MiB title is acceptable. |
| R4 | Skia will not build for the target | WP2 | GN as `linux` with overridden compilers is the plan; if GN refuses, patch skia-bindings' build support for the triple; the UI path itself has no alternative and this risk is first in WP2. |
| R5 | Videodec2's ABI is community-reconstructed; the 13.60 sandbox trap; stride/tiling of its output | WP3 | PyroWave-only first release; HEVC lands when the lane passes. |
| R6 | 119.88 Hz needs the display and the metadata; VRR unverified; HDR absent from the WSI | WP1, §9 | Ship 4K60 if 120 is refused; VRR and HDR stay unadvertised until a bundle shows them. |
| R7 | Receive-path syscall cost with the loader's hooks (~20 µs each) at 13k datagrams/s | WP0 measurement | `recvmmsg` batching (already in `quinn_udp` where the libc has it), larger `SO_RCVBUF`, pump thread priority; document the loader setting the tester can change. |
| R8 | One tester; days of turnaround | always | §5. |
| R9 | The `libc.prx` layout the loader accepts is documented only beside GPL code | WP6 | Rebuild it from the probe's observations of a working title; keep the GPL generator for test builds until a replacement boots. |
| R10 | Memory: swapchain 3 × 32 MiB, decoder pool, PyroWave buffers, Skia cache, QUIC buffers | WP3 soak | Budgets in `perf.json`; `gpu_cache_bytes` down to the kit's 96 MiB floor. |
| R11 | The pacbrew Mesa package warns that its patches were machine-generated and unreviewed | ongoing | Pin by patch-set hash; the CTS result is the evidence; re-run WP1's bundle on every bump. |

## 9. After the baseline

In order, each gated by a bundle: HDR10 (a WSI patch for a 10-bit BT.2020/PQ surface format plus
VideoOut HDR metadata; ProsperoLight shows the path exists), 5.1 and 7.1 through the eight-channel
AudioOut port, two to four pads (one per signed-in user), DualSense haptics and speaker over the
pad-audio plane, keyboard and mouse (`libSceKeyboard`, `libSceMouse`), VRR verification on a
display that reports it, a `libSceNet` resolver for hostnames, the console's touchpad and motion
as `RichInput`. AV1 is not planned: the console has no decoder for it and the GPU driver has no
Vulkan Video.

## 10. Sequence and timebox

WP0 → WP1 → WP2 ∥ WP3 → WP4 → WP5 → WP6. WP2 and WP3 proceed in parallel once WP1's device is
up. The first two weeks are WP0 and WP1 as an investigation: their output is a feasibility report
— the probe bundle, the Vulkan frame, the PyroWave feature line, the dependency patch inventory —
and an estimate for the rest from measured blockers rather than from this plan. The timebox bounds
the investigation, not the port.

## 11. Open before WP0 starts

- **Tester's setup** (into the first `environment.json`): model, firmware, loader and whether
  its kernel hooks can be paused, display and its 4K120/VRR/HDR support, wired or wireless, FTP
  and klog services available.
- **Title ID**: a `PPSA99xxx` value not used by another homebrew title.
- **Maintainer's yes**: §7 items 1–4, and the `ring` fallback should R2 fire.
- **Where this document lives**: here, beside the code it plans, with a one-line pointer from the
  planning repository's index.
