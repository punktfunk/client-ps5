# Notes

What this client knows about the console, where each fact comes from, and what still needs a
console run to prove. Plan of record: `plan.md`. A claim proven on the console cites its bundle
id or probe log; the rest is **sourced, not proven**.

## Where the code stands (2026-10-17)

| WP | Written | Proven without a console | Needs a console run for |
| --- | --- | --- | --- |
| 0 | probe (16 checks), target JSON, toolchain + link scripts, FFI with layout tests | probe passes on macOS and Linux; layouts pinned; title and probe link in CI (artifact `title`) | `sockbuf` and `mdns`; the rest passed on 13.42 (round 1) |
| 1 | device, `VK_KHR_display` output, presenter, read-back, PyroWave lane | presenter renders the console on lavapipe (CI artifact) | swapchain at 119.88 Hz, pacing record, PyroWave feature line |
| 2 | console hosting, service loop, store, pairing, library, art | fakes and the host platform; menu test run produces a PNG | Skia's Vulkan backend on RADV (R4) |
| 3 | stage/timeline/pump (lifted), Videodec2 lane, PyroWave lane | stage tests incl. intra-only anchors | Videodec2 decode, the import step taken, 4K120 budgets |
| 4 | stream loop, input thread, dial, chord, rumble/light/triggers, AudioOut, reconnect | pad routing, trigger translation, audio ring | input feel, triggers, audio drift |

Not built yet: the rest of plan §9 (surround, haptics, keyboard/mouse, VRR), system-event
polling (layout unproven, below), a hostname resolver (addresses only).

## Deviations from the plan, and why

- **The app does not depend on `pf-ps5-vk`.** `pf-client-video` compiles to nothing on macOS, so
  an app built on the Vulkan crate could not be tested on the Mac. The `Display` and
  `VideoDecoders` seams carry what the app needs; the platform crates own the Vulkan plane.
- **The title is a static library linked by `scripts/link-title.sh`,** not a binary linked through
  the target JSON's `linker`. RADV titles link with a raw `prospero-lld` call (CRT objects,
  whole-archive RADV, wraps, defsyms, version scripts); the script reuses PS5_Vulkan's
  `radv_link_recipe` at a pinned revision instead of re-deriving it.
- **RADV comes from PS5_Vulkan's Mesa fork**, built by that repository's `build-radv.sh`, not from
  the pacbrew package: it is the tree the 1.4 CTS ran on, and it ships the link recipe that goes
  with it.
- **Videodec2 lands through a buffer import and one GPU copy** (plan §3.3 step 2), with a staging
  copy (step 3) where the import is refused. No repository imports decoder memory as an image.
- **The dial opens on Touchpad + Cross, and the escape chord is L1+R1+Options+Touchpad.** Create
  is system-owned on the console (bit 0x1, see below).
- **Pacing is newest-picture-at-vblank** under FIFO. `CadenceClock`/`pace_slot` phase pacing waits
  for the first pacing record to show whether it is needed.
- **Audio is S16** through AudioOut: float formats appear only in the SDL port's code.

## Upstream changes (plan §7)

Merged into monorepo main (#1966–#1968); every punktfunk pin from `770504dbc` on carries all three:

- #1966: `Platform::Ps5` and its rows; `shortcuts::Client::Ps5`.
- #1967: `pf-client-core` (portable half, discovery) and `pf-console-ui` build for
  `all(target_os = "freebsd", target_vendor = "sie")`.
- #1968: `pyrowave-sys` and `pf-client-video`'s PyroWave lane on that target.

Not started: the PS5 Skia archive on `skia-binaries` (§7.4, `task skia` builds it), the install
page entry (§7.5, at WP6). §7.6 (a `ring` provider) only if aws-lc-sys will not cross-build.

## Proven on the console (round 1, 2026-10-17)

A tester's PS5 on 13.42, wired to a Windows host. Evidence kept with the tester's report: probe
logs `probe-20261017-015745.log` and `probe-20261017-031128.log`, menu bundle
`PPSA99724/1792206947`, kernel logs `klog-20261016*.txt` and `klog-20261017-*.txt`.

- **Dead imports:** `pthread_setname_np`, `gai_strerror` and `mkstemp` resolve only to
  libScePosixForWebKit, `readlink` only to libkernel_sys, and a title's import from either is
  null. Each has a shim; `link-title.sh` refuses a title that imports another.
- **Entropy:** `kern.rng_pseudo` hands out 64-byte blocks and refuses a smaller buffer (ENOMEM).
- **Socket flags:** on a socket, `ioctl(FIONBIO)` and `fcntl(F_SETFL)` fail with EACCES and
  `F_GETFD`/`F_SETFD` with EINVAL; `SO_NBIO` (0x1200) works, and so do sockets created with
  `SOCK_NONBLOCK|SOCK_CLOEXEC`. The `fcntl`/`ioctl` wraps map one onto the other.
- **No duplicates:** `F_DUPFD_CLOEXEC` (EINVAL) and `dup` (EPERM) are refused on sockets and
  kqueues; `--wrap=close` shares the descriptor instead. kqueue itself works for mio and tokio:
  `EVFILT_USER`, read and write filters, UDP readiness. quinn-udp works through the wraps.
- **Execute-only code:** reading `.text` faults (`SYSTEM_XO_VIOLATION`). aws-lc builds without
  assembly: s2n-bignum keeps its tables there.
- **Videodec2:** HEVC needs the coded height in 32-row blocks (2176 for 2160); at 2160 every
  picture fails `0x811d0302`. H.264 decodes at 2160, 4K60 at ~13.5 ms a frame.
- **Socket buffers** stayed at FreeBSD's defaults, 9 KiB send and 41 KiB receive: core's 32 MiB
  ask was refused whole. Probe `sockbuf` records the console's answers.
- **UI layer:** the menu ran on the half-size CPU raster, ~200 ms a frame. Skia on Vulkan
  refused the device's 1.4 version (fixed; lavapipe reproduced it), and the startup line now
  names the path.
- **Open:** `mdns` found no host in 4 s on any run, while the host answered over TCP.

## Proven on the console (round 2, 2026-10-17)

Same console and host, build 4a8d225. Evidence: probe log `probe-20261017-201422.log`, menu
bundle `PPSA99724/1792279851`, kernel log `klog-20261017-round2.txt`.

- **UI layer:** Skia on Vulkan draws the menu at ~100 fps, under 1 ms of CPU a frame.
- **Socket buffers:** a 32 MiB `SO_RCVBUF` fails with ENOBUFS, 4 MiB and below are accepted, and
  core's 32 MiB ask through the step-down wrap reads back 512 KiB both ways.
  `kern.ipc.maxsockbuf` is not readable from a title.
- **Videodec2:** H.264 at 4K120 decodes in 9-17 ms against an 8.3 ms period; the queue it
  leaves is flushed every 2 s and core reads each flush as congestion, down to 2 Mbit/s. HEVC
  at 4K60 decodes in 12-15 ms at 15-20 Mbit/s, with no plane-set misses.
- **PyroWave** at 4K120 arrives at 499 Mbit/s with no loss; that rate was the client's own
  ceiling (200 Mbit/s, under PyroWave's 0.5 bit/pixel floor), not the console's.
- **mDNS:** binding UDP 5353 fails with EACCES, so mdns-sd's daemon has no socket.

## Proven on the console (round 3, 2026-10-17)

Same console and host, branch `fix/console-round-3`. Evidence: probe logs
`probe-20261017-204235.log`, `-210124.log`, `-214202.log`, kernel log `klog-20261017-round2.txt`
(sessions from 23:40 UTC on), ProsperoLight's `performance-last.json` from the same console.

- **Socket buffers:** every accepted `SO_RCVBUF` reads back at most 512 KiB (4 MiB asks read
  524288); 8 MiB and up fail with ENOBUFS.
- **mDNS:** a PTR query from an ephemeral port with the unicast-response bit is answered by the
  host's mdns-sd and finds it, while binding 5353 still fails.
- **CPU masks:** a title thread starts on `0x1fff`; an eight-byte `cpuset_setaffinity` to
  `0x1fc0` (off Videodec2's `0x3f`) takes and reads back.
- **Videodec2 HEVC** with 4 slices and the title's threads off its CPUs: 6.1-8.1 ms a picture at
  4K120 for 45-60 Mbit/s, ~11.5 ms at ~110 Mbit/s (it then keeps up with ~85 of 120 pictures);
  ~13.5 ms at 4K60 for ~90 Mbit/s and ~17 ms at ~110. One slice and shared CPUs cost ~2x: 7.6 ms
  at 4.5 Mbit/s and 12-17 ms at 13-29 Mbit/s.
- **Plane copy** into device-local planes costs 0.1 ms once it is not waited inside the decode.
- **PyroWave** at 4K120 tops out at ~790 Mbit/s on the wire (~637 of video): core paces at its
  800 Mbit/s link floor, because this console's port facts are unknown to core.

## Proven on the console (round 7, 2026-10-23)

Same console, an HDR TV, a Windows host; build `2ec950e` with Mesa `c90ea0aab`. Evidence:
bundles `PPSA99724/1792798151` (HEVC HDR, 4K60, 70 s) and `PPSA99724/1792803716` (PyroWave
HDR, 4K120, 40 s, over Wi-Fi), with the host logs and the tester's report.

- **119.88 Hz:** both sessions open a `3840x2160 @ 119880 mHz` HDR10 swapchain, within 25 ms of
  the first picture. Title start to swapchain is 3.2-3.8 s, of which ~2.9 s is the host sending
  nothing while its virtual display and encoder come up.
- **HEVC HDR10:** Main10 PQ at 4K60 decodes in 9.48 ms mean, 15.3 ms max, with no decoder
  errors and no flushes.
- **Console HDR Off on an HDR TV:** Mesa still opens and flips the HDR10 swapchain; the TV shows
  no HDR badge.
- **AudioOut under a full link:** PyroWave at a fixed 611 Mbit/s over Wi-Fi (Automatic bitrate
  off) logged 25 underruns in 40 s while the ring grew to 65 ms, 24 of them in the first 15 s.
  The HEVC run at ~50 Mbit/s logged none.

## Console facts the code relies on (sourced, not yet proven)

Sources: ProsperoLight (PL), PS5_Vulkan (VK), PS5_VulkanTemplate (TPL), the payload SDK (SDK),
OpenNOW-PS5 (ONW). Section numbers refer to the extraction kept with the plan's research.

- **Exit:** `exit()` and returning from `main` are recorded as crashes. The only clean end is
  `sceSystemServiceLoadExec("exit", NULL)` then sleeping until the shell ends the process
  (TPL platform-contracts, PL main.cpp, VK DEPLOYMENT).
- **Splash:** `sceSystemServiceHideSplashScreen` before the first present (TPL, VK).
- **RADV entry:** no loader; `vk_icdGetInstanceProcAddr` from the archive (VK radv_smoke.c).
  One display "PS5 VideoOut", modes largest first with each 119.88 Hz mode before its 59.94 Hz
  one; 119.88 only when `param.json` `attribute3` declares HFR (0x40; VK/TPL ship 0x80040).
- **HDR10 scan-out:** VideoOut registers 3840x2160 buffers in format `0x8100070422000000`
  (R10G10B10A2, red low, BT.2020 PQ) only when `param.json` `attribute` carries `0x20000000`;
  without it `sceVideoOutRegisterBuffers2` returns `80290003` where the SDR format registers the
  same buffers, and `0x02000000` or `0x40000000` alone don't lift it. With it, 3839 vsync flips
  at 120 Hz, none failed; the TV shows its HDR badge and the fills in order (red first, so red
  sits in the low bits). Title process only: a payload gets no direct memory (`80020023`).
  (PPSA99725 probe-20261022-235054, -235719, -235800, -235850, -235909.) Through the Mesa fork
  (`KrisEnigma/PS5_Mesa` `hdr10`): a Windows host's 10-bit BT.2020 PQ PyroWave at 1440p120
  switches the swapchain to HDR10 on its first picture, 8941 pictures in 90 s, and the TV shows
  the desktop in HDR at its usual colours (bundle 1792728081). An HDR game's in-game calibration
  reads correct through it (Kris, 2026-10-23, by eye); 300 s of play, 25165 pictures, decode to
  present p50 4.1 ms, none dropped (bundle 1792728541).
- **Display HDR:** `sceVideoOutGetDeviceCapabilityInfo_` (NID `kGVLc3htQE8`) takes the record's
  size as a third argument and fills a `u64` whose `0x80` bit is BT.2020 PQ (shadPS4
  `video_out.h`, where the bit needs the display's HDR and the title's HDR attribute). Without
  the size it fails `0x80290001`, HDR On and Off alike (round 8). The size that answers and
  whether the console's HDR setting clears the bit are what probe `display-hdr` shows; the client
  asks for HDR only with the bit set, or when the call fails. The firmware's own view is in the
  kernel log: `v-request auto attr=…: hdmi:vrr hdr` / `sdr`, and `video: port:HDMI …` carries
  `YCC2100_PQ` / `709`. With HDR Off the output stays 709 under an HDR10 swapchain and the
  picture looks normal, so a bit that never moves costs nothing visible.
- **Videodec2:** title process only (payload contexts fail), `sceSysmoduleLoadModule(207)` first,
  memory type 12 in 16 KiB pages, a DPB at least the stream's references plus one (a six-deep
  one failed every picture `811d0302` against Intel VAAPI's nine references at 1440p; sized from
  the level it decodes, 13 deep, bundle 1792307155) and at most 16 (17 fails the memory query
  `811d0209`, bundle 1792307478; 16 at 1080p decodes, bundle 1792308114), H.264 needs `Flush`
  after each `Decode` at depth 1. Output is LINEAR NV12, or two-plane 16-bit with the 10 bits
  LOW-aligned for Main10 (not P010). On 13.60 it fails `811d0111` once a process leaves its
  sandbox (PL TROUBLESHOOTING). 4K120 HEVC holds to about 80 Mbit/s (PL PERFORMANCE_GUIDE).
- **Pads:** one handle per signed-in user, 120-byte state; Create (0x1) and PS (0x10000) are the
  system's; bit 0x80000000 marks a sample the shell intercepted. Trigger effects take Sony's
  high-level modes, 120-byte parameter (PL ps5_dualsense, duaLib).
- **AudioOut:** user 0xFF, MAIN port, 48 kHz, grains of 256..2048 frames in steps of 256,
  `sceAudioOutOutput` blocks one grain; S16 stereo and 8-channel are hardware-verified.
- **klog:** `sceKernelDebugOutText(0, line)`; a title's stderr does not reach it.
- **libc traps for std:** `arc4random*` resolve to nothing in a title (bound to the SDK fork's
  `ps5_` versions by the link recipe); `pthread_set_name_np` from inside the new thread hangs it
  (bound to a no-op here); malloc is wrapped onto the platform heap; TLS is emulated
  (`has-thread-local: false` in the target); `kern.rng_pseudo` is the proven entropy sysctl.
- **Packaging:** `/data/homebrew/<TITLE_ID>/` with `eboot.bin` (fake-signed SELF from
  `ps5-native-tool`), `sce_module/libc.prx`, `sce_sys/param.json`, `sce_sys/icon0.png` (512²).
  `/download0` exists when `downloadDataSize` > 0. Files FTP must read are 0666/0777.
- **Ids in use elsewhere:** PPSA99002 (ProsperoLight), 99008, 99010, 99014, 99015, 99130, 99169,
  99988, 99996–99999. This client is PPSA99724 and its probe PPSA99725: no public repository
  used either on 2026-10-15.

## Open questions the probe answers first

Round 1 answered BSD sockets, `_umtx_op`, kqueue, getrandom 0.2 and recvmmsg: all work in a
title, through the wraps above (`udp-rate`: 10 000 datagrams, ~4.2 µs each).

6. **`dlopen` at link time.** Granite references `dlopen` even when handed a
   `GetInstanceProcAddr`; if the console's libkernel stub lacks it, the title link needs a
   `--defsym` to a stub returning null (WP1).
7. **System events.** `sceSystemServiceGetStatus`/`ReceiveEvent` layouts are emulator-derived;
   `Lifecycle::poll_events` returns nothing until a probe line proves them. Focus loss shows up
   as intercepted pad samples, which release held input.
