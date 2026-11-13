# TuxGenie

A Rust take on [RazerGenie](https://github.com/z3ntu/RazerGenie) for Linux: configure
lighting, DPI, polling rate and power settings of Razer keyboards, mice and accessories.
Your settings are saved and restored at every login.

## How it fits together

```
 ┌──────────────┐  D-Bus (session)   ┌──────────────┐   sysfs writes   ┌───────────────────────┐
 │  tuxgenie    │ ─────────────────▶ │  tuxgenied   │ ───────────────▶ │ OpenRazer kernel      │
 │  (egui GUI)  │ ◀── DevicesChanged │ (user daemon)│                  │ driver (razerkbd,     │
 └──────────────┘                    └──────┬───────┘                  │ razermouse, …)        │
                                            │ ~/.config/tuxgenie/      └───────────────────────┘
                                            ▼ settings.json
```

| Component | What it is | Who provides it |
|---|---|---|
| `tuxgenie` | The GUI. It keeps no device state: every change goes to the daemon. | this repo (Rust) |
| `tuxgenied` | A user daemon, started at login by systemd. It finds devices, writes settings to the hardware, saves them, and restores them at login, on hotplug, when a wireless device wakes up, and after suspend. | this repo (Rust) |
| OpenRazer kernel driver | The kernel modules that expose each Razer device as files under `/sys/bus/hid/drivers/razer*/`. TuxGenie can't work without them. | `openrazer-driver-dkms` package |

`tuxgenied` **replaces the Python `openrazer-daemon`**. Both daemons write the same files,
so only one should run. `install.sh` disables and masks `openrazer-daemon`, and
`uninstall.sh` turns it back on. That means Polychromatic and RazerGenie, which need
`openrazer-daemon`, won't work while TuxGenie is installed.

The first time it starts, `tuxgenied` imports what `openrazer-daemon` last saved
(`~/.config/openrazer/persistence.conf`), so your current lighting carries over.

## Install

You need:

* the OpenRazer kernel driver: `sudo apt install openrazer-driver-dkms` (from the
  [OpenRazer PPA](https://openrazer.github.io/#download) on Ubuntu/Pop!_OS)
* your user in the `plugdev` group: `sudo gpasswd -a $USER plugdev`, then log out and back in
* Rust (`cargo`)

Then run:

```bash
./install.sh
```

This installs everything into your home directory, so it doesn't need root:

| File | Purpose |
|---|---|
| `~/.local/bin/tuxgenie`, `~/.local/bin/tuxgenied` | the GUI and the daemon |
| `~/.config/systemd/user/tuxgenied.service` | starts the daemon at login (`WantedBy=default.target`) |
| `~/.local/share/dbus-1/services/io.github.raevibe.TuxGenie.service` | the GUI starts the daemon on demand if it isn't running |
| `~/.local/share/applications/io.github.raevibe.TuxGenie.desktop` | app-menu entry |

The sidebar has two login options: **Restore settings at login** turns the daemon's
systemd service on or off. **Open this window at login** adds an XDG autostart entry.

To remove TuxGenie, run `./uninstall.sh`. It keeps `~/.config/tuxgenie`.

## Features

* **Lighting**, for every zone the driver exposes (backlight, logo, scroll wheel, left and
  right strips, and dock charging states): off, static, spectrum, wave, breathing
  (random, one colour or two colours), reactive, starlight, and brightness.
* **Custom per-key lighting** on keyboards with a known LED matrix. You paint the keys on a
  grid. The matrix sizes for about 200 devices were generated from OpenRazer's device
  definitions (`crates/tuxgenie-core/src/hw_table.rs`).
* **Performance**: DPI (X/Y locked or separate), DPI stages, polling rate.
* **Power**: battery level and charging state, the idle time before sleep, the low-battery
  warning threshold.
* **Options**: game mode, macro LED, scroll-wheel modes and similar on/off settings, when
  the device has them.

## Daemon details

* It checks for new or removed devices once a second by listing sysfs directories, which
  sends no USB traffic. Device attributes are read only when a device appears.
* Wireless devices are checked every 5 s to see if they're awake. While a mouse sleeps, its
  receiver answers with zeros. The daemon ignores those zeros instead of saving them, and
  re-applies your settings when the mouse wakes up.
* It listens for logind's `PrepareForSleep` signal and re-applies settings 3 s after resume.
* If writes fail, which happens when a device has only just been plugged in, it retries
  with exponential backoff.
* Other commands: `tuxgenied --list` prints the detected devices and their settings as
  JSON. `tuxgenied --once` applies the saved settings and exits.
* Logs: `journalctl --user -u tuxgenied -f`

### D-Bus API

Bus name `io.github.raevibe.TuxGenie`, object `/io/github/raevibe/TuxGenie`,
interface `io.github.raevibe.TuxGenie1`:

| Member | Description |
|---|---|
| `ListDevices() → s` | a JSON array of `DeviceInfo` |
| `Apply(id: s, change: s)` | applies a JSON `Change` and saves it, e.g. `{"change":"dpi","x":1600,"y":1600}` |
| `Reapply()` | writes every saved setting to the hardware again |
| `DevicesChanged` (signal) | emitted on hotplug, sleep/wake, errors and changes |

```bash
busctl --user call io.github.raevibe.TuxGenie /io/github/raevibe/TuxGenie \
  io.github.raevibe.TuxGenie1 Apply ss PM2127F45501893 \
  '{"change":"effect","zone":"backlight","effect":{"effect":"spectrum"}}'
```

## Development

```bash
cargo test
cargo run -p tuxgenied -- --list
TUXGENIE_SCREENSHOT=shot.png TUXGENIE_TAB=custom cargo run -p tuxgenie
```

## License

GPL-2.0-or-later. The device table is derived from OpenRazer, which is GPL-2.0.
