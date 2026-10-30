# Pins

`scripts/pins.env` is the source; this says what each pin is for and how it moves.

| Pin | Value | What it fixes | Moves when |
| --- | --- | --- | --- |
| Rust nightly | `nightly-2026-08-11` | `-Z build-std` for the custom target | a std change is needed; re-run the probe |
| punktfunk | `277b11d94` | `punktfunk-core`, `pf-client-core`, `pf-console-ui`, `pf-client-video`, one rev for all | the shared crates change; one bump, all four |
| PS5_Vulkan | `f410e67a0` | link recipe, title CRT, `ps5-native-tool`, `libc.prx` | RADV or the recipe changes upstream |
| PS5_Mesa | `c90ea0aab` | RADV with the PS5 winsys: PS5_Vulkan's `1ba9874f4` plus HDR10 swapchains and 119.88 Hz under VRR | with PS5_Vulkan; re-run WP1's bundle (R11) |
| PS5_PayloadSDK | `b83202be7` | payload SDK + platform layer (`libps5platform.a`) | with PS5_Vulkan |
| skia-safe | `0.153.3` | the console UI's Skia, matching `pf-console-ui` | with the punktfunk pin |
| skia-bindings hash | `b7f043e0b1e2a850e702` | the key a build looks its Skia archive up by | with skia-safe |

A bump changes `scripts/pins.env` (and `rust-toolchain.toml` / `Cargo.toml` for the first two),
then runs the probe and WP1's bundle before anything else ships. The About screen and the first log
line carry the build id with the core and Mesa pins.

Releases ship under `MIT OR Apache-2.0` (plan D10). Until WP6 replaces them, test builds link
three GPL-3.0-or-later pieces from the two rows above: `app_crt.cpp` / `app_cpp_runtime.cpp`,
`libps5platform.a` and the generated `libc.prx`. Such a build is never published. RADV and PyroWave
are MIT, Skia BSD-3-Clause, libc++ Apache-2.0 with the LLVM exception.
