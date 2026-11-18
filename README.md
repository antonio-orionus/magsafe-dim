<div align="center">
  <img src="assets/icon.png" width="160" alt="MagSafe Dim icon: a black MagSafe 3 plug with a green LED">
  <h1>MagSafe Dim</h1>
  <p>Keeps your MagSafe charging light off.<br><br>
  <a href="https://github.com/antonio-orionus/magsafe-dim/releases/latest/download/MagSafe-Dim.dmg"><b>Download for macOS</b></a><br>
  <sub>MacBooks with MagSafe 3 · macOS 13 or later</sub></p>
</div>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/window-dark.png">
    <img src="assets/screenshots/window-light.png" width="512" alt="The MagSafe Dim window with the switches 'Keep the light off' and 'Allow light control' turned on, and a 'Use with AI' button">
  </picture>
</p>

MagSafe Dim is a free, open-source macOS app that turns off the charging LED on a
MacBook's MagSafe 3 connector and turns it off again after every plug-in and wake.
One switch: no menu bar icon, no charge limit, nothing else to set up. Want the
light by day? Turn on **Only at night** and it goes off from sunset to sunrise.
Scripts and AI agents can set the light too: green, amber or blinking.

## Install

1. Download [MagSafe-Dim.dmg](https://github.com/antonio-orionus/magsafe-dim/releases/latest/download/MagSafe-Dim.dmg),
   open it and drag MagSafe Dim to Applications.
2. Open MagSafe Dim from Applications and turn on **Keep the light off**.
3. Allow it once when macOS asks: click its notification, or turn on MagSafe Dim in
   System Settings › General › Login Items, then enter an administrator password.

Works on MacBooks with a MagSafe 3 port: MacBook Pro 14-inch and 16-inch (2021 and
later) and MacBook Air (M2 and later).

## Use with scripts and AI

Once macOS has allowed it, the app shows a second switch, **Allow light control**.
While it's on, a script can set the light, with no `sudo`, for example green when
your tests pass:

```sh
"/Applications/MagSafe Dim.app/Contents/MacOS/magsafe-dim" set green   # or amber, blink, off, system
```

**Use with AI** in the app gives a prompt to paste into an AI agent that can run
commands on your Mac, such as Claude Code or Codex. The agent suggests uses and sets
up the ones you pick. Turning **Keep the light off** on brings the light back to off.

## Updates

From 1.2.0, MagSafe Dim checks for a new version when you open it, at most once a
day and not on its very first launch, and offers to install it; **Check for
Updates…** in the app menu checks now. It can't check while closed, since it has no menu bar icon. Updating
from 1.1.0 or earlier: download the new version once as in [Install](#install).

## Privacy

Once a day the helper sends one anonymous request to the author's own
[Counterscale](https://github.com/benvinegar/counterscale) server, which counts how
many Macs use MagSafe Dim. It carries the app version, the macOS version (`macos-27`),
the Mac's model identifier (`Mac17,8`) and how long ago the app was installed
(`new`, `1-7d`, `8-30d`, `31-90d`, `over-90d`). No ID, serial number or name; the
server keeps the country, not the IP address. Builds from source never send it.
Turn **Send a daily heartbeat** off in the About window, or see exactly what would
go out:

```sh
"/Applications/MagSafe Dim.app/Contents/MacOS/magsafe-dim" heartbeat show   # or: heartbeat off
```

The update check downloads `appcast.xml` from GitHub, which hosts the updates;
the request names the app and its version.

## Uninstall

Turn **Allow light control** off, then move the app to the Trash. (Trashing it while it's
on also gives the light back within a few minutes, but leaves an entry in
Login Items.)

## Good to know

- The light comes back while the Mac sleeps, and goes off again when it wakes.
- **Only at night** takes sunset and sunrise from macOS's Night Shift schedule,
  which needs Location Services. Without a location, night is 20:00 to 07:00.
- **Updating from 1.0.0:** turn **Allow light control** off and on once after
  installing the new version. The 1.0.0 helper doesn't restart itself on updates.

## License

MIT. See [LICENSE](LICENSE).

<details>
<summary><b>Technical details</b></summary>

### Build from source

Needs Rust and the Xcode Command Line Tools.

```sh
git clone https://github.com/antonio-orionus/magsafe-dim
cd magsafe-dim
scripts/build-app.sh
open "target/MagSafe Dim.app"      # it offers to move itself to Applications
```

`scripts/build-app.sh` signs the app ad-hoc, which is enough on your own Mac. With a
Developer ID, set `SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"`, so
Login Items lists it under your name. Add `NOTARY_PROFILE=<profile>`, a profile saved with
`xcrun notarytool store-credentials`, to notarize and staple the app, then build, sign,
notarize and staple `target/MagSafe-Dim.dmg` for download (with
[dmgbuild](https://github.com/dmgbuild/dmgbuild), run through `uvx`). `OUT_DIR=target/dev`
puts the app elsewhere. Run it as yourself, not with `sudo`, so `codesign` can use the
key in your login keychain.

### How the app is put together

```
MagSafe Dim.app/Contents/
  MacOS/MagSafe Dim                 the window (SwiftUI, app/)
  MacOS/magsafe-dim                 the helper (Rust, src/), runs as `magsafe-dim run`
  Frameworks/Sparkle.framework      updates (app/Updater.swift)
  Library/LaunchDaemons/dev.orionus.magsafe-dim.daemon.plist
  Resources/AppIcon.icns
```

**Allow light control** (which **Keep the light off** also turns on) registers or
unregisters the helper with Apple's `SMAppService`, which runs it from inside the app
as a launchd daemon. The window shows the light by running
`magsafe-dim led`, which reads it without root. From a terminal:

```sh
"/Applications/MagSafe Dim.app/Contents/MacOS/magsafe-dim" status   # light and helper state
sudo "/Applications/MagSafe Dim.app/Contents/MacOS/magsafe-dim" run # foreground (turn Allow light control off first); Ctrl-C gives the light back
log show --last 1h --predicate 'process == "magsafe-dim"'
```

### Other light states, for scripts

While **Allow light control** is on, `magsafe-dim set` makes the helper keep another state
instead of off, with no `sudo` (administrator accounts only). The prompt behind **Use
with AI** only points the agent at `magsafe-dim --help`, which documents all of this.

```sh
alias magsafe-dim="/Applications/MagSafe Dim.app/Contents/MacOS/magsafe-dim"  # scripts use the full path
magsafe-dim set amber       # system, off, green, amber, blink, blink-once, night
magsafe-dim target          # the state the helper keeps
magsafe-dim led             # the state the light shows now
magsafe-dim sun             # night or day now, and the night's sunset and sunrise
```

The helper sets it again after every plug-in and wake, as it does with off;
`system` and `blink-once` are set once and then left alone. `night` is off from
sunset to sunrise and `system` by day. The choice lasts through restarts until
the next `set`. In the app, **Keep the light off** switches between `off` and
`system`, **Only at night** between `off` and `night`, and turning **Allow light
control** off ends any choice.
Blinking amber is how a Mac reports a charging problem, so don't leave it on and
forget it.

The choice is one word in `/Library/Application Support/MagSafe Dim/led`, and the
heartbeat switch one word (`on`, `off`) in `heartbeat` next to it: the folder
belongs to root, the files can be changed by the admin group, and the helper accepts
nothing but those words. A file needs no connection into the root helper, and
the settings survive restarts and updates.

### What it touches

- **One SMC key, `ACLC`** (the LED), and nothing else. Before every read or write
  the helper checks that the key exists and is a one-byte `ui8 ` key, and refuses
  otherwise.
- **Known values only:** off while it runs (or the state chosen with
  `magsafe-dim set`), `0x00` (macOS decides) when it stops. The write function
  takes an enum of the states in [LED values](#led-values), so nothing else can
  reach the SMC.
- **One folder,** `/Library/Application Support/MagSafe Dim`, for the state chosen
  with `magsafe-dim set`, the heartbeat switch, and two files only root writes:
  the install's first day and last heartbeat (`heartbeat-days`), and day or night
  as the helper keeps it while it keeps `night` (`sun`, which `magsafe-dim sun` and
  the app show). The helper removes it when the app
  is deleted while light control is allowed; after deleting the app with it off,
  remove it with `sudo rm -r "/Library/Application Support/MagSafe Dim"`.
- **Two programs it runs:** `/usr/libexec/corebrightnessdiag sunschedule` for
  sunset and sunrise, only while it keeps the light off at night (killed after 10 s),
  and `/usr/bin/curl` for the [heartbeat](#privacy): one HTTPS GET a day to
  `stats.orionus.dev`, tried again 10 minutes later if it got no answer, at most six
  times a day. Only builds signed with the author's Developer ID send it.
- **Writes only on a mismatch.** Most events cost one read.
- **Never delays sleep** and never keeps the Mac awake. It changes no charging
  behaviour, power setting or file.
- **Dependencies:** `objc2-io-kit` and `objc2-core-foundation` (bindings to Apple's
  frameworks) and `libc` for the helper; Apple's SwiftUI and ServiceManagement and
  [Sparkle](https://sparkle-project.org) 2.10.1 for the window, downloaded by
  `scripts/fetch-sparkle.sh` and checked against a pinned SHA-256. Nothing else.

### How it works

macOS and the SMC rewrite `ACLC` on power events, so the daemon waits for them on a
CoreFoundation run loop and sets the LED back to the chosen state (off, unless a
script chose another):

| Event | Source | Check |
|---|---|---|
| plug-in, unplug | `IOPSCreateLimitedPowerNotification` | every 250 ms for 5 s |
| charge state, battery level, dark wakes | `IOPSNotificationCreateRunLoopSource` | once |
| wake from sleep | `IORegisterForSystemPower` | every 250 ms for 5 s |
| missed event; sunset or sunrise; app updated or deleted; heartbeat due | timer | once a minute |

The 5-second checks are needed because after a plug-in or wake the SMC resets the
LED in short bursts and ignores writes meanwhile. If an SMC call fails (the
connection can go stale across sleep), it reopens the connection and tries once more.
`SIGTERM` (when **Allow light control** turns it off), `SIGINT` and `SIGHUP` write `0x00` and exit.

Once a minute the helper also checks its own file. When the app was updated with a copy
signed like the running one (same Developer ID), it exits and launchd starts the new copy
at once. A copy signed any other way is left alone until the next start, so the root
helper never hurries into running a file someone else put there. When the app has been
missing for three minutes in a row, it gives the light back to macOS and stops.

### Limits

- **The LED comes back on while the Mac sleeps.** A few seconds into sleep the Mac
  renegotiates USB-C power with the charger (20 V → 5 V), and the SMC treats that
  like a fresh plug-in. Nothing can run during sleep to undo it. The daemon turns
  the LED off again on each wake, including the short maintenance wakes.
- **A short green flash on plug-in**, while the SMC ignores writes.
- **`ACLC` is undocumented.** A macOS or firmware update could change or lock it.
  The daemon then logs one error and does nothing, and the LED behaves as stock.

If you need it dark during sleep too, charge through a USB-C port with a plain
USB-C cable (no LED at all), or cover the LED.

Tested on: MacBook Pro M5 Pro (Mac17,8), macOS 27.2.1, firmware 20457.41.3.

### Other tools (surveyed November 2026)

Every tool found writes the same `ACLC` key, and none keeps the LED off during sleep.

| Tool | License | What it is |
|---|---|---|
| [batt](https://github.com/charlie0129/batt) | GPL-2.0 | Charge limiter with `magsafe-led always-off`; sets an 80 % limit by default |
| [AlDente Pro](https://apphousekitchen.com/) | proprietary | Paid charge limiter with an LED setting |
| [markschlosser/magled](https://github.com/markschlosser/magled) | MIT | C daemon, events only; the closest to this one |
| [bulava92/magsafe-dark](https://github.com/bulava92/magsafe-dark) | MIT | Menu bar app and root daemon; polls every 15 s |
| [great-andy/MagSafeLed](https://github.com/great-andy/MagSafeLed) | GPL-3.0 | ObjC++ daemon; suggests preventing sleep on power for a dark LED in sleep |
| [realAbitbol/MagSleep](https://github.com/realAbitbol/MagSleep) | MIT | Swift menu bar app and daemon; off always, only while the Mac or its display sleeps, or sunset to sunrise; 3 s timer |
| [gbretas/glowless](https://github.com/gbretas/glowless) | MIT | C daemon; polls every 2 s |
| [rolandsuu/ChargeKeeper](https://github.com/rolandsuu/ChargeKeeper) | MIT | Charge limiter with LED control |
| [shaoya828/magled](https://github.com/shaoya828/magled) | MIT | Swift app and helper; [a fork](https://github.com/zjhenrynsyh/magled) has macOS 27 fixes |
| [mpopv/magsafe-cli](https://github.com/mpopv/magsafe-cli) | MIT | Also dims the cable's own LED through its firmware (one cable firmware tested) |
| [maxim-kich/MagDot](https://github.com/maxim-kich/MagDot) | MIT | Turns the light into a status light for T3 Code agents |
| [kamenlevi/MagHue](https://github.com/kamenlevi/MagHue) | GPL-3.0 | Menu bar app that picks the charge level at which the light turns green |

### LED values

| `ACLC` | Name | LED |
|---|---|---|
| `00` | `system` | macOS decides (reads back as its current choice) |
| `01` | `off` | off |
| `03` | `green` | green |
| `04` | `amber` | amber |
| `05` | `blink-once` | amber, three quick blinks, then dark |
| `06` | `blink` | amber, blinking: 0.5 s on, 1 s off |
| `07` | | the same as `06` (batt calls them slow and fast blinking, but filmed on a MacBook Pro M5 Pro they blink identically) |

The values come from batt and magled; the descriptions are what a MacBook Pro M5 Pro
shows, filmed and measured. `02` is accepted but changes nothing (magled), and values
above `07` keep only their low three bits (Battery-Toolkit). The SMC user-client struct and selectors match
smcFanControl's `smc.c` and batt's gosmc.

### Development

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
scripts/preview-ui.sh     # the window in every state, dark and light: target/ui-preview/*.png
scripts/build-app.sh      # the whole app, ad-hoc signed
```

`preview-ui.sh` draws the window offscreen, so design changes need no signing, no
install and no running helper. `build-app.sh` runs `scripts/fetch-sparkle.sh`,
which downloads Sparkle into `target/sparkle` once.

A release uploads three files from `target/`: `MagSafe-Dim.dmg` (the download),
`MagSafe-Dim.zip` (what Sparkle installs) and `appcast.xml` (Sparkle's feed, read
from the latest release). The build signs the zip and the feed with the EdDSA key
in the login keychain (`generate_keys --account magsafe-dim`); the app accepts
neither without it, so a lost key means users can't update. To test the first-run approval, use a fresh macOS VM:
macOS remembers approvals, so a Mac that has allowed MagSafe Dim before won't ask again.

The icon is drawn in [`assets/icon.svg`](assets/icon.svg) and the disk image window's
background in [`assets/dmg-background.svg`](assets/dmg-background.svg); its layout is in
[`scripts/dmg-settings.py`](scripts/dmg-settings.py). After editing an SVG, run
`assets/build-icon.sh`, which renders `AppIcon.icns`, `icon.png` and
`dmg-background.tiff` with macOS's own SVG support, and commit the SVG with its output.

</details>
