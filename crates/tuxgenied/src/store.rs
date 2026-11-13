//! Persistent settings: `~/.config/tuxgenie/settings.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tuxgenie_core::{ColorMode, DeviceSettings, Effect, Rgb, ZoneSettings};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct StoreFile {
    #[serde(default)]
    pub version: u32,
    /// Set once we've looked at OpenRazer's persistence.conf, so it is only imported once.
    #[serde(default)]
    pub imported_openrazer: bool,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceSettings>,
    /// "driver-pid" -> last real serial seen, so sleeping wireless devices keep their identity.
    #[serde(default)]
    pub known_serials: BTreeMap<String, String>,
}

pub struct Store {
    path: PathBuf,
    pub data: StoreFile,
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Store {
    pub fn load() -> Store {
        let path = config_dir().join("tuxgenie").join("settings.json");
        let data = match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                log::error!("{}: {e}; starting with empty settings (old file kept as .bad)", path.display());
                let _ = fs::copy(&path, path.with_extension("json.bad"));
                StoreFile::default()
            }),
            Err(_) => StoreFile::default(),
        };
        let mut store = Store { path, data };
        if !store.data.imported_openrazer {
            store.import_openrazer();
            store.data.imported_openrazer = true;
            store.save();
        }
        store
    }

    pub fn save(&mut self) {
        self.data.version = 1;
        let res = (|| -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            fs::write(&tmp, serde_json::to_vec_pretty(&self.data)?)?;
            fs::rename(&tmp, &self.path)
        })();
        if let Err(e) = res {
            log::error!("saving {}: {e}", self.path.display());
        }
    }

    /// One-time migration of the settings the Python openrazer-daemon last persisted.
    fn import_openrazer(&mut self) {
        let path = config_dir().join("openrazer").join("persistence.conf");
        let Ok(text) = fs::read_to_string(&path) else { return };
        let imported = parse_openrazer_persistence(&text);
        let n = imported.len();
        for (serial, settings) in imported {
            self.data.devices.entry(serial).or_insert(settings);
        }
        if n > 0 {
            log::info!("imported settings for {n} device(s) from {}", path.display());
        }
    }
}

fn parse_openrazer_persistence(text: &str) -> BTreeMap<String, DeviceSettings> {
    // Minimal INI parsing: [serial] then key = value.
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = Some(name.trim().to_string());
            sections.entry(name.trim().to_string()).or_default();
        } else if let (Some(sec), Some((k, v))) = (&current, line.split_once('=')) {
            sections.get_mut(sec).unwrap().insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    let mut out = BTreeMap::new();
    for (serial, kv) in sections {
        let mut s = DeviceSettings::default();
        let get = |k: &str| kv.get(k).map(String::as_str);
        let num = |k: &str| get(k).and_then(|v| v.parse::<f32>().ok());

        for zone in ["backlight", "logo", "scroll", "left", "right"] {
            let Some(effect_name) = get(&format!("{zone}_effect")) else { continue };
            let colors: Vec<u8> = get(&format!("{zone}_colors"))
                .unwrap_or("")
                .split_whitespace()
                .filter_map(|c| c.parse().ok())
                .collect();
            let c = |i: usize| {
                colors.get(i * 3..i * 3 + 3).map(|v| Rgb(v[0], v[1], v[2])).unwrap_or(Rgb(0, 255, 0))
            };
            let speed = num(&format!("{zone}_speed")).unwrap_or(1.0) as u8;
            let wave_dir = num(&format!("{zone}_wave_dir")).unwrap_or(1.0) as u8;
            let active = get(&format!("{zone}_active")).map(|v| v != "False").unwrap_or(true);
            let effect = if !active {
                Some(Effect::None)
            } else {
                match effect_name {
                    "none" => Some(Effect::None),
                    "static" => Some(Effect::Static { color: c(0) }),
                    "spectrum" => Some(Effect::Spectrum),
                    "wave" => Some(Effect::Wave { direction: wave_dir }),
                    "breathRandom" => Some(Effect::Breath { colors: ColorMode::Random }),
                    "breathSingle" | "breathMono" => Some(Effect::Breath { colors: ColorMode::Single { color: c(0) } }),
                    "breathDual" | "breathTriple" => {
                        Some(Effect::Breath { colors: ColorMode::Dual { a: c(0), b: c(1) } })
                    }
                    "reactive" => Some(Effect::Reactive { speed: speed.max(1), color: c(0) }),
                    "starlightRandom" => Some(Effect::Starlight { speed: speed.max(1), colors: ColorMode::Random }),
                    "starlightSingle" => {
                        Some(Effect::Starlight { speed: speed.max(1), colors: ColorMode::Single { color: c(0) } })
                    }
                    "starlightDual" => {
                        Some(Effect::Starlight { speed: speed.max(1), colors: ColorMode::Dual { a: c(0), b: c(1) } })
                    }
                    _ => None,
                }
            };
            let brightness = num(&format!("{zone}_brightness")).map(|b| b.round().clamp(0.0, 100.0) as u8);
            s.zones.insert(zone.to_string(), ZoneSettings { effect, brightness });
        }
        if let (Some(x), Some(y)) = (num("dpi_x"), num("dpi_y")) {
            s.dpi = Some((x as u16, y as u16));
        }
        s.poll_rate = num("poll_rate").map(|p| p as u16);
        out.insert(serial, s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openrazer_file() {
        let text = "[PM2127F45501893]\nbacklight_active = True\nbacklight_brightness = 100\nbacklight_effect = static\nbacklight_colors = 255 0 255 0 255 255 0 0 255\nbacklight_speed = 1\nbacklight_wave_dir = 1\n\n[632448H30203547]\ndpi_x = 800\ndpi_y = 800\npoll_rate = 1000\nscroll_active = True\nscroll_brightness = 100\nscroll_effect = spectrum\n";
        let m = parse_openrazer_persistence(text);
        let kb = &m["PM2127F45501893"];
        assert_eq!(kb.zones["backlight"].effect, Some(Effect::Static { color: Rgb(255, 0, 255) }));
        assert_eq!(kb.zones["backlight"].brightness, Some(100));
        let mouse = &m["632448H30203547"];
        assert_eq!(mouse.dpi, Some((800, 800)));
        assert_eq!(mouse.poll_rate, Some(1000));
        assert_eq!(mouse.zones["scroll"].effect, Some(Effect::Spectrum));
    }
}
