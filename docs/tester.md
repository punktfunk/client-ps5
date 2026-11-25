# Testing Punktfunk on a PS5

To install and use Punktfunk, read [`install.md`](install.md). This page is for test rounds with
the developer: it takes you from a fresh clone to the logs the developer needs. You run a few
tasks on your computer; the developer reads what they bring back. A failure with its log is a
good result.

## What you need

- **The console:** a PS5 that runs homebrew (13.42 is proven; 13.60 is what the sources
  describe), with an FTP server on port 2121
  and a title loader that launches folders in `/data/homebrew`. A klog server on port 3232 is
  optional and helps with crashes.
- **A computer** on the same network with [Python 3](https://www.python.org/downloads/),
  [Task](https://taskfile.dev/installation/) and this repository. The live log and `task klog`
  need `nc`, which macOS and Linux have.
- **A Punktfunk host** on the same network ([install it](https://docs.punktfunk.unom.io/docs/install))
  and its IP address. Use a cable for the console and the host in the stream rounds.
- **The build:** a release's `PPSA99724.zip`, which holds **Punktfunk**, or `title.zip` from
  the developer, which also holds **Punktfunk Probe** (PPSA99725).

## Set up once

1. Clone this repository and open a terminal in it.
2. Copy `.env.example` to `.env` and set `PS5_HOST` to the console's IP address. Leave the title
   ids as they are.
3. Unpack the build. Do this again with every new build:

   ```sh
   task unpack -- ~/Downloads/title.zip
   ```

> **Close a title before you deploy it.** Closed means gone from the home screen, including any
> crash dialog: close the dialog first. A deploy under a running title leaves it half-replaced.

## Round 1: the probe

The probe checks what the client needs from the console, one line per check, then closes
itself. It runs twice: the second launch checks that its files and its pairing survived.

1. Upload it. A new title takes up to 45 seconds to appear on the home screen.

   ```sh
   task probe-deploy
   ```

2. In the host's web console, open **Devices**, click **Pair a device** and **Show PIN**. The PIN
   lasts two minutes.
3. Name the host and the PIN:

   ```sh
   task probe-host -- 192.168.1.20 1234
   ```

4. Launch **Punktfunk Probe** from the home screen and leave it alone until it closes. Near the
   end it shows four colour fills for half a minute, then a grey picture for about a minute
   while the television changes mode several times. During the grey picture, note when the
   television shows its HDR badge and what refresh rate its information bar reads. Hold the
   controller for the last check: it buzzes on one side, then the other, then rumbles. Note
   which side came first.
5. Fetch its log. It prints one line: `ok: …`, `FAILED (…): …` or `STOPPED in …`.

   ```sh
   task probe-fetch
   ```

6. Launch **Punktfunk Probe** again, wait until it closes, and run `task probe-fetch` again.

Send both printed lines and the files in `bundles/PPSA99725/`. With this first report, also
tell the developer your console model, firmware, title loader and its version, the display and
its highest refresh rate, and whether the console is on a cable or Wi-Fi.

## Round 2: the menu

1. Upload the client and queue the menu scenario:

   ```sh
   task deploy
   task test-run -- menu
   ```

2. Launch **Punktfunk** and leave the controller alone. It draws its menu, saves pictures of it
   and closes itself.
3. Fetch the result. Send the line it prints and the folder it names.

   ```sh
   task fetch-bundle
   ```

## Round 3: pair and stream

1. Launch **Punktfunk**. If your host is not listed, choose **Add host** and enter its IP
   address. Host names don't work on the console.
2. Pick the host and pair it. Either choose **Request access — approve on the host** and click
   **Approve** under **Devices** in the host's web console, or type a PIN from **Pair a device**
   into **PIN** and choose **Pair**. The [pairing guide](https://docs.punktfunk.unom.io/docs/pairing)
   shows the host's side.
3. Go back to the home screen and choose **Quit**. The hint bar names the button.
4. Queue a stream to your host for 60 seconds, launch **Punktfunk** and leave the controller
   alone until it closes:

   ```sh
   task test-run -- stream 192.168.1.20 60
   ```

5. Run `task fetch-bundle` and send the line and the folder.

## Playing

After a stream round passes, play as you would on any client.

| Press | To |
| --- | --- |
| Hold the touchpad click, press Cross | Open the quick menu in a stream |
| Hold L1 + R1 + Options + touchpad click for one second | End the stream |
| Hold the touchpad click, press L3 | Send Select (Back) to the host |
| Hold the touchpad click, press R3 | Send Guide to the host |

The Create and PS buttons belong to the console and never reach the game; the last two rows stand in
for them.

When something goes wrong, note the time and what you did. A live log tells the developer the
rest. Before launching, start a listener on your computer, then deploy with your computer's IP
address:

```sh
nc -lk 9999 | tee live.log
TELEMETRY=192.168.1.10:9999 task deploy
```

Send `live.log`. A plain `task deploy` turns the live log off again.

## Troubleshooting

### `Couldn't reach the console at …`

The console's FTP server is not running, or `PS5_HOST` is wrong. Start the FTP server and check
the address in `.env`.

### The title does not appear on the home screen

A new title takes up to 45 seconds. If it is still missing, start your title loader and wait
again.

### `No bundle on the console yet` or `No probe.log on the console yet`

The title did not finish its run. Queue the run again, launch, and wait for the title to close by
itself. If it crashes, run `task klog > klog.txt` in a second terminal before the next launch and
send `klog.txt`.

### `STOPPED in check …`

The probe ended inside that check, usually by crashing. Close the crash dialog, then send the log
with a `task klog` capture of the same launch.

### The PIN is refused

It lasts two minutes. Show a new one, run `task probe-host` again with it, then launch.
