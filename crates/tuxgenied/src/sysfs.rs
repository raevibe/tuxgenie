//! Direct access to the OpenRazer kernel driver's sysfs attributes.
//!
//! The kernel modules (razerkbd, razermouse, razeraccessory, razerkraken) bind to
//! Razer HID interfaces and expose one directory per interface under
//! `/sys/bus/hid/drivers/<driver>/`. The "control" interface is the one that has a
//! `device_type` attribute; that is the one we talk to. The OpenRazer udev rules make
//! these files writable by the `plugdev` group, so no root is needed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tuxgenie_core::{
    hw_lookup, Battery, ColorMode, DeviceInfo, DeviceSettings, DpiStages, Effect, Rgb, ZoneInfo,
    ZoneSettings, EFFECT_KINDS, TOGGLES, ZONES,
};

pub const DRIVERS: &[(&str, &str)] = &[
    ("razerkbd", "keyboard"),
    ("razermouse", "mouse"),
    ("razeraccessory", "accessory"),
    ("razerkraken", "headset"),
];

const SYSFS_HID: &str = "/sys/bus/hid/drivers";

#[derive(Debug, Clone)]
pub struct Zone {
    pub info: ZoneInfo,
    prefix: &'static str,
    brightness_attr: &'static str,
}

/// A bound Razer control interface.
#[derive(Debug, Clone)]
pub struct HwDevice {
    pub path: PathBuf,
    pub info: DeviceInfo,
    pub zones: Vec<Zone>,
    /// Has a battery, i.e. talks to us through a receiver and can be off/asleep.
    pub wireless: bool,
    /// The device answered with a real serial number when probed.
    pub awake: bool,
}

fn read_raw(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
}

fn read_str(path: &Path) -> Option<String> {
    let raw = read_raw(path).ok()?;
    let s = String::from_utf8_lossy(&raw);
    let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
    Some(s)
}

/// Enumerate Razer control interfaces bound to an OpenRazer driver.
///
/// This only looks at directory entries; it sends nothing to the hardware, so it is
/// cheap enough to run every second (unlike reading attributes, which are USB requests).
pub fn list() -> Vec<(&'static str, &'static str, PathBuf)> {
    let mut out = Vec::new();
    for (driver, kind) in DRIVERS {
        let Ok(entries) = fs::read_dir(Path::new(SYSFS_HID).join(driver)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("device_type").exists() {
                out.push((*driver, *kind, path));
            }
        }
    }
    out.sort_by(|a, b| a.2.cmp(&b.2));
    out
}

/// A serial number that a sleeping wireless device (or a broken one) wouldn't report.
pub fn usable_serial(serial: &str) -> bool {
    !serial.is_empty() && serial.chars().any(|c| c != '0')
}

/// Parse the USB product id from a HID directory name like `0003:1532:0257.000A`.
fn parse_pid(dir: &str) -> Option<u16> {
    let mut parts = dir.split(':');
    let _bus = parts.next()?;
    let vid = parts.next()?;
    if !vid.eq_ignore_ascii_case("1532") {
        return None;
    }
    let pid = parts.next()?.split('.').next()?;
    u16::from_str_radix(pid, 16).ok()
}

pub fn probe(driver: &str, kind: &str, path: &Path) -> Option<HwDevice> {
    let dir = path.file_name()?.to_string_lossy().to_string();
    let pid = parse_pid(&dir)?;
    let name = read_str(&path.join("device_type")).unwrap_or_else(|| format!("Razer device {pid:04X}"));
    let serial = read_str(&path.join("device_serial")).unwrap_or_default();
    let firmware = read_str(&path.join("firmware_version")).unwrap_or_default();
    let hw = hw_lookup(driver, pid);
    let has = |attr: &str| path.join(attr).exists();

    let matrix = hw
        .filter(|h| h.0 > 0 && h.1 > 0 && has("matrix_custom_frame") && has("matrix_effect_custom"))
        .map(|h| (h.0, h.1));

    let mut zones: Vec<Zone> = Vec::new();
    for (prefix, id, label, bright) in ZONES {
        if zones.iter().any(|z| z.info.id == *id) {
            continue;
        }
        let effects: Vec<String> = EFFECT_KINDS
            .iter()
            .filter(|k| {
                if **k == "custom" {
                    prefix.is_empty() && matrix.is_some()
                } else {
                    has(&format!("{prefix}matrix_effect_{k}"))
                }
            })
            .map(|k| k.to_string())
            .collect();
        if effects.is_empty() {
            continue;
        }
        zones.push(Zone {
            info: ZoneInfo { id: id.to_string(), label: label.to_string(), effects, has_brightness: has(bright) },
            prefix,
            brightness_attr: bright,
        });
    }

    // Sleeping wireless devices report an empty serial; the daemon maps those back to the
    // last serial seen for this driver+pid.
    let awake = usable_serial(&serial);
    let id = if awake { serial.clone() } else { fallback_id(driver, pid) };
    let wireless = has("charge_level");

    let poll_rates = if has("poll_rate") {
        match hw.map(|h| h.3) {
            Some(r) if !r.is_empty() => r.to_vec(),
            _ => vec![125, 500, 1000],
        }
    } else {
        vec![]
    };

    let info = DeviceInfo {
        id,
        name,
        kind: kind.to_string(),
        serial,
        firmware,
        driver: driver.to_string(),
        usb_pid: pid,
        sysfs_path: path.to_string_lossy().to_string(),
        zones: zones.iter().map(|z| z.info.clone()).collect(),
        matrix,
        has_dpi: has("dpi"),
        dpi_max: hw.map(|h| h.2).filter(|d| *d > 0).unwrap_or(16000),
        has_dpi_stages: has("dpi_stages"),
        poll_rates,
        has_idle_time: has("device_idle_time"),
        has_low_battery: has("charge_low_threshold"),
        battery: None,
        toggles: TOGGLES.iter().filter(|(t, _)| has(t)).map(|(t, _)| t.to_string()).collect(),
        settings: DeviceSettings::default(),
        asleep: wireless && !awake,
        error: None,
    };
    Some(HwDevice { path: path.to_path_buf(), info, zones, wireless, awake })
}

pub fn fallback_id(driver: &str, pid: u16) -> String {
    format!("{driver}-{pid:04x}")
}

fn pct_to_byte(p: u8) -> u8 {
    ((p.min(100) as f32) * 2.55).round() as u8
}

fn byte_to_pct(b: u8) -> u8 {
    ((b as f32) / 2.55).round().min(100.0) as u8
}

impl HwDevice {
    fn write(&self, attr: &str, data: &[u8]) -> Result<(), String> {
        fs::write(self.path.join(attr), data).map_err(|e| format!("{attr}: {e}"))
    }

    fn read(&self, attr: &str) -> Option<String> {
        read_str(&self.path.join(attr))
    }

    fn zone(&self, id: &str) -> Result<&Zone, String> {
        self.zones.iter().find(|z| z.info.id == id).ok_or_else(|| format!("no lighting zone '{id}'"))
    }

    /// One USB request: does the device answer with its serial right now?
    pub fn read_serial(&self) -> String {
        self.read("device_serial").unwrap_or_default()
    }

    pub fn battery(&self) -> Option<Battery> {
        if !self.awake {
            return None;
        }
        let level: u8 = self.read("charge_level")?.parse().ok()?;
        let charging = self.read("charge_status").map(|s| s == "1").unwrap_or(false);
        Some(Battery { percent: byte_to_pct(level), charging })
    }

    pub fn apply_effect(&self, zone: &str, effect: &Effect, frame: Option<&Vec<Vec<Rgb>>>) -> Result<(), String> {
        let z = self.zone(zone)?;
        if !z.info.effects.iter().any(|k| k == effect.kind()) {
            return Err(format!("{} does not support {} on {}", self.info.name, effect.kind(), zone));
        }
        let attr = format!("{}matrix_effect_{}", z.prefix, effect.kind());
        let colors = |m: &ColorMode| -> Vec<u8> {
            match m {
                ColorMode::Random => vec![],
                ColorMode::Single { color } => color.bytes().to_vec(),
                ColorMode::Dual { a, b } => [a.bytes(), b.bytes()].concat(),
            }
        };
        let data: Vec<u8> = match effect {
            Effect::None | Effect::Spectrum => b"1".to_vec(),
            Effect::Static { color } => color.bytes().to_vec(),
            Effect::Wave { direction } => direction.clamp(&1, &2).to_string().into_bytes(),
            Effect::Breath { colors: m } => match m {
                ColorMode::Random => vec![1],
                _ => colors(m),
            },
            Effect::Reactive { speed, color } => {
                let mut v = vec![(*speed).clamp(1, 4)];
                v.extend(color.bytes());
                v
            }
            Effect::Starlight { speed, colors: m } => {
                let mut v = vec![(*speed).clamp(1, 3)];
                v.extend(colors(m));
                v
            }
            Effect::Custom => {
                let frame = frame.ok_or("no custom frame stored")?;
                self.apply_custom_frame(frame)?;
                b"1".to_vec()
            }
        };
        self.write(&attr, &data)
    }

    pub fn apply_custom_frame(&self, frame: &[Vec<Rgb>]) -> Result<(), String> {
        let (rows, cols) = self.info.matrix.ok_or("device has no per-key matrix")?;
        let mut buf = Vec::with_capacity(rows as usize * (3 + cols as usize * 3));
        for r in 0..rows {
            buf.extend([r, 0, cols - 1]);
            for c in 0..cols {
                let rgb = frame.get(r as usize).and_then(|row| row.get(c as usize)).copied().unwrap_or(Rgb::BLACK);
                buf.extend(rgb.bytes());
            }
        }
        self.write("matrix_custom_frame", &buf)
    }

    pub fn apply_brightness(&self, zone: &str, percent: u8) -> Result<(), String> {
        let z = self.zone(zone)?;
        if !z.info.has_brightness {
            return Err(format!("zone {zone} has no brightness control"));
        }
        self.write(z.brightness_attr, pct_to_byte(percent).to_string().as_bytes())
    }

    pub fn apply_dpi(&self, x: u16, y: u16) -> Result<(), String> {
        let max = self.info.dpi_max;
        let (x, y) = (x.clamp(100, max), y.clamp(100, max));
        self.write("dpi", &[x.to_be_bytes(), y.to_be_bytes()].concat())
    }

    pub fn apply_dpi_stages(&self, s: &DpiStages) -> Result<(), String> {
        if s.stages.is_empty() || s.active < 1 || s.active as usize > s.stages.len() {
            return Err("invalid DPI stages".into());
        }
        let max = self.info.dpi_max;
        let mut buf = vec![s.active];
        for (x, y) in s.stages.iter().take(5) {
            buf.extend(x.clamp(&100, &max).to_be_bytes());
            buf.extend(y.clamp(&100, &max).to_be_bytes());
        }
        self.write("dpi_stages", &buf)
    }

    pub fn apply_poll_rate(&self, hz: u16) -> Result<(), String> {
        self.write("poll_rate", hz.to_string().as_bytes())
    }

    pub fn apply_idle_time(&self, seconds: u16) -> Result<(), String> {
        self.write("device_idle_time", seconds.clamp(60, 900).to_string().as_bytes())
    }

    pub fn apply_low_battery(&self, percent: u8) -> Result<(), String> {
        let raw = ((percent.min(100) as f32 / 100.0) * 255.0).floor() as u8;
        self.write("charge_low_threshold", raw.to_string().as_bytes())
    }

    pub fn apply_toggle(&self, name: &str, on: bool) -> Result<(), String> {
        if !self.info.toggles.iter().any(|t| t == name) {
            return Err(format!("unknown toggle {name}"));
        }
        self.write(name, if on { b"1" } else { b"0" })
    }

    /// Re-apply everything we have stored. Returns the collected errors, if any.
    pub fn apply_all(&self, s: &DeviceSettings) -> Result<(), String> {
        let mut errors = Vec::new();
        let mut run = |r: Result<(), String>| {
            if let Err(e) = r {
                errors.push(e);
            }
        };
        for z in &self.zones {
            if let Some(zs) = s.zones.get(&z.info.id) {
                if let Some(effect) = &zs.effect {
                    run(self.apply_effect(&z.info.id, effect, s.custom_frame.as_ref()));
                }
                if let (Some(b), true) = (zs.brightness, z.info.has_brightness) {
                    run(self.apply_brightness(&z.info.id, b));
                }
            }
        }
        if self.info.has_dpi_stages {
            if let Some(st) = &s.dpi_stages {
                run(self.apply_dpi_stages(st));
            }
        }
        if self.info.has_dpi {
            if let Some((x, y)) = s.dpi {
                run(self.apply_dpi(x, y));
            }
        }
        if !self.info.poll_rates.is_empty() {
            if let Some(hz) = s.poll_rate {
                run(self.apply_poll_rate(hz));
            }
        }
        if self.info.has_idle_time {
            if let Some(t) = s.idle_time {
                run(self.apply_idle_time(t));
            }
        }
        if self.info.has_low_battery {
            if let Some(p) = s.low_battery {
                run(self.apply_low_battery(p));
            }
        }
        for (name, on) in &s.toggles {
            if self.info.toggles.contains(name) {
                run(self.apply_toggle(name, *on));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Read whatever the hardware lets us read back (effects are write-only).
    pub fn read_current(&self) -> DeviceSettings {
        let mut s = DeviceSettings::default();
        for z in &self.zones {
            let brightness = if z.info.has_brightness {
                self.read(z.brightness_attr).and_then(|v| v.parse::<u8>().ok()).map(byte_to_pct)
            } else {
                None
            };
            s.zones.insert(z.info.id.clone(), ZoneSettings { effect: None, brightness });
        }
        if self.info.has_dpi {
            s.dpi = self.read("dpi").and_then(|v| {
                let mut it = v.split(':').map(|p| p.trim().parse::<u16>().ok());
                let x = it.next()??;
                let y = it.next().flatten().unwrap_or(x);
                (x >= 100 && y >= 100).then_some((x, y))
            });
        }
        if self.info.has_dpi_stages {
            if let Ok(raw) = read_raw(&self.path.join("dpi_stages")) {
                if raw.len() >= 5 && raw[0] >= 1 && raw[1..5].iter().any(|b| *b != 0) {
                    let stages = raw[1..]
                        .chunks_exact(4)
                        .map(|c| (u16::from_be_bytes([c[0], c[1]]), u16::from_be_bytes([c[2], c[3]])))
                        .collect::<Vec<_>>();
                    let active = raw[0].clamp(1, stages.len() as u8);
                    s.dpi_stages = Some(DpiStages { active, stages });
                }
            }
        }
        if !self.info.poll_rates.is_empty() {
            s.poll_rate = self.read("poll_rate").and_then(|v| v.parse().ok()).filter(|hz| *hz > 0);
        }
        if self.info.has_idle_time {
            s.idle_time = self.read("device_idle_time").and_then(|v| v.parse().ok()).filter(|t| *t >= 60);
        }
        if self.info.has_low_battery {
            s.low_battery = self
                .read("charge_low_threshold")
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|b| *b > 0)
                .map(|b| ((b as f32 / 255.0) * 100.0).round() as u8);
        }
        for t in &self.info.toggles {
            if let Some(v) = self.read(t).and_then(|v| v.parse::<u8>().ok()) {
                s.toggles.insert(t.clone(), v != 0);
            }
        }
        s
    }
}

/// Fill every field the stored settings don't have yet from what the hardware reports.
pub fn fill_missing(stored: &mut DeviceSettings, current: DeviceSettings) {
    for (zone, cur) in current.zones {
        let z = stored.zones.entry(zone).or_default();
        if z.brightness.is_none() {
            z.brightness = cur.brightness;
        }
    }
    if stored.dpi.is_none() {
        stored.dpi = current.dpi;
    }
    if stored.dpi_stages.is_none() {
        stored.dpi_stages = current.dpi_stages;
    }
    if stored.poll_rate.is_none() {
        stored.poll_rate = current.poll_rate;
    }
    if stored.idle_time.is_none() {
        stored.idle_time = current.idle_time;
    }
    if stored.low_battery.is_none() {
        stored.low_battery = current.low_battery;
    }
    for (t, v) in current.toggles {
        stored.toggles.entry(t).or_insert(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_parse() {
        assert_eq!(parse_pid("0003:1532:0257.000A"), Some(0x0257));
        assert_eq!(parse_pid("0003:046D:C52B.0001"), None);
    }

    #[test]
    fn pct_roundtrip() {
        for p in 0..=100u8 {
            assert_eq!(byte_to_pct(pct_to_byte(p)), p);
        }
    }
}
