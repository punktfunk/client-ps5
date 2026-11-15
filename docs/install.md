# Install Punktfunk on a PS5

Punktfunk for PS5 is a folder you copy to the console. Your title loader puts it on the home
screen. This page covers installing, the first stream, updating and what to do when something
goes wrong.

> **Early release.** It has streamed on a tester's PS5 on firmware 13.42. Other firmwares, loaders
> and TVs are untested, so a report from yours helps: see [When something goes wrong](#when-something-goes-wrong).

## What you need

- **A PS5 that already runs homebrew**, with a title loader that registers folders in
  `/data/homebrew` ([ShadowMountPlus](https://github.com/drakmor/ShadowMountPlus) is the common
  one) and an FTP server. Punktfunk brings no exploit and no loader.
- **A Punktfunk host** on the same network: [install it](https://docs.punktfunk.unom.io/docs/install)
  on the PC you want to stream from.
- **A computer with an FTP client**, such as FileZilla, Cyberduck or WinSCP.
- **A network cable** for the console and the host if you can. Wi-Fi works, with less headroom.

## Install

1. Download `PPSA99724.zip` from the
   [latest release](https://github.com/punktfunk/client-ps5/releases). `PPSA99724.zip.sha256`
   beside it is its checksum, if you want to check the download.
2. Extract the zip. It holds the `PPSA99724` folder, and the license texts beside it.
3. Connect your FTP client to the console and upload the whole `PPSA99724` folder into
   `/data/homebrew/`. The result is `/data/homebrew/PPSA99724/eboot.bin`. Don't upload the zip
   itself or only some of the files.
4. Wait for the title loader to pick it up. **Punktfunk** appears on the home screen within a
   minute. If it doesn't, restart the loader or the console.

If you have this repository, [Task](https://taskfile.dev/installation/) and Python 3, two commands
do steps 2 and 3. Set `PS5_HOST` in `.env` first (copy `.env.example`).

```sh
task unpack -- ~/Downloads/PPSA99724.zip
task deploy
```

## The first stream

1. Launch **Punktfunk**. Hosts on your network are listed by themselves. If yours is missing,
   choose **Add host** and enter its IP address. Host names don't work on the console.
2. Pick the host and pair it. Either choose **Request access** and approve the console under
   **Devices** in the host's web console, or get a PIN there from **Pair a device** and type it in.
   The [pairing guide](https://docs.punktfunk.unom.io/docs/pairing) shows the host's side.
3. Choose the desktop or a game from the host's library.

| Press | To |
| --- | --- |
| Hold the touchpad click, press Cross | Open the quick menu in a stream |
| Hold L1 + R1 + Options + touchpad click for one second | End the stream |

The Create and PS buttons belong to the console and never reach the game. In the menus, the hint
bar at the bottom names each button. **Settings → Input → Shortcuts** changes the two shortcuts.

## What works

- 3840×2160 at 60 or 120 Hz: H.264 and HEVC on the console's decoder, PyroWave on its GPU.
- HDR10 when the TV shows HDR and the host sends it.
- DualSense rumble, adaptive triggers and light bar, one controller per signed-in user.
- Stereo sound.

Not yet: surround sound, DualSense haptics, keyboard and mouse, and VRR.

HEVC at 3840×2160 and 120 Hz keeps up to about 60 Mbit/s on the console's decoder. Automatic
bitrate stays under that by itself. PyroWave needs a fast wired link.

## Update

1. Close Punktfunk on the console. A copy under a running title leaves it half-replaced.
2. Upload the new `PPSA99724` folder over the old one, as in [Install](#install).

Your paired hosts and settings are kept outside that folder, in the title's own storage, so an
update keeps them.

Keep only one copy of `PPSA99724` where your loader looks. With a second copy, on a USB drive for
example, the loader may start the older one.

## Remove

Delete `/data/homebrew/PPSA99724` over FTP, then delete Punktfunk from the home screen as you
delete any title. To forget the console on a host, remove it under **Devices** in the host's web
console.

## When something goes wrong

### Punktfunk does not appear on the home screen

Check that `/data/homebrew/PPSA99724/eboot.bin` and `/data/homebrew/PPSA99724/sce_sys/param.json`
exist. Then restart your title loader and wait a minute.

### An old version starts after an update

The loader found a second copy of `PPSA99724`. Delete the extra one and restart the loader.

### My host is not listed

Choose **Add host** and enter the host's IP address. Check that the console and the host are on
the same network.

### The picture stutters

Use a cable for the console and the host. Under **Settings**, leave the bitrate on Automatic.

### Report a problem

[Open an issue](https://github.com/punktfunk/client-ps5/issues) or ask on
[Discord](https://discord.gg/wzEGg9y45z). Say what you did and when, and add:

- the console's firmware, your title loader and its version
- the TV and whether the console is on a cable or Wi-Fi
- Punktfunk's version on the console (**Settings → About**) and on the host
- the console's log: on the host's card, choose **Send logs to host**, then download it from the
  **Logs** page of the host's web console. Read it before you share it: it names your hosts and
  their addresses.

[`tester.md`](tester.md) has the scripted test runs the developer may ask for.
