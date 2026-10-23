# punktfunk for PlayStation 5

A homebrew client that streams games and desktops from a [punktfunk](https://docs.punktfunk.unom.io)
host to a PS5.

> **Experimental.** Nothing has run on a console yet. Builds go to testers, not to a release page.

## What it is built to do

- 3840×2160 at 59.94 or 119.88 Hz: H.264 and HEVC on the console's decoder, PyroWave on its GPU.
- HDR10 when the host sends HDR.
- The punktfunk console UI: hosts, pairing, game libraries and settings, driven by the controller.
- DualSense rumble, adaptive triggers and light bar, one controller per signed-in user.
- Stereo sound.

Not yet: surround sound, DualSense haptics, keyboard and mouse, VRR, and host names (enter an IP
address).

## Test it on a console

[`docs/tester.md`](docs/tester.md) is the whole manual: what the console needs, then one task per
step.

## Build it

The client builds, runs and tests on macOS and Linux. The console build runs in Docker.

You need [rustup](https://rustup.rs) (`rust-toolchain.toml` installs the pinned nightly),
[Task](https://taskfile.dev), cmake and clang. On Linux also the packages the `host` job in
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) installs.

```sh
task test    # every test this machine can run
task lint    # rustfmt and clippy -D warnings, as CI runs them
task run     # the client on this machine (PF_PS5_DATA, PF_PS5_SIZE, PF_PS5_PADS)
```

The console build, in the toolchain image. On an Apple Silicon Mac, first
`export DOCKER_DEFAULT_PLATFORM=linux/amd64`.

```sh
task docker:toolchain       # once per pin: SDK, RADV, title converter, nightly
task docker:skia            # once per skia-safe version
task docker:package         # dist/PPSA99724/ and its zip
task docker:package-probe   # the probe: dist/PPSA99725/ and its zip
```

CI builds both titles on every push to `main` and keeps them as the `title` artifact, which
`task unpack` lays out in `dist/`. `task --list` shows every task.

## Repository

| Path | What |
| --- | --- |
| `crates/pf-ps5-app` | The app: sessions, the console UI, services, and the platform seams with fakes |
| `crates/pf-ps5-platform` | The console's implementation of those seams |
| `crates/pf-ps5-host` | The same seams on a developer machine |
| `crates/pf-ps5-vk` | Vulkan device, presenter and PyroWave lane |
| `crates/pf-ps5-sys` | The console's C interface, with layout tests |
| `crates/pf-ps5-probe` | The probe's checks |
| `crates/pf-ps5-title` | The console's entry point |
| `scripts/` | Toolchain, cross build, link, and the deploy and test-run tools |
| `packaging/` | `param.json` and the icon |

Read [`AGENTS.md`](AGENTS.md) before changing code: layering, commands and invariants.

## Docs

- [`docs/tester.md`](docs/tester.md): testing on a console.
- [`docs/NOTES.md`](docs/NOTES.md): what is proven, what still needs a console, and the console
  facts the code relies on.
- [`docs/plan.md`](docs/plan.md): the plan of record.
- [`docs/pins.md`](docs/pins.md): every pinned dependency and how it moves.

## Licence

MIT OR Apache-2.0, at your option ([`LICENSE-MIT`](LICENSE-MIT),
[`LICENSE-APACHE`](LICENSE-APACHE)). Test builds also link three GPL-3.0-or-later pieces from
PS5_Vulkan and its payload SDK fork, so they are not published. They are replaced before the
first release ([`docs/pins.md`](docs/pins.md)). The build writes `THIRD-PARTY-NOTICES.txt`.
