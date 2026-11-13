//! Types shared by the TuxGenie daemon (`tuxgenied`) and the GUI (`tuxgenie`).
//!
//! Everything that crosses D-Bus is JSON-encoded with these types, so the
//! D-Bus interface itself stays tiny (see [`dbus`]).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod hw_table;
pub use hw_table::HW_TABLE;

pub mod dbus {
    pub const BUS_NAME: &str = "io.github.raevibe.TuxGenie";
    pub const OBJECT_PATH: &str = "/io/github/raevibe/TuxGenie";
    pub const INTERFACE: &str = "io.github.raevibe.TuxGenie1";
}

pub const APP_ID: &str = "io.github.raevibe.TuxGenie";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0, 0, 0);
    pub fn bytes(self) -> [u8; 3] {
        [self.0, self.1, self.2]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ColorMode {
    Random,
    Single { color: Rgb },
    Dual { a: Rgb, b: Rgb },
}

/// A lighting effect, as understood by the OpenRazer kernel driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    None,
    Static { color: Rgb },
    Spectrum,
    /// direction: 1 or 2
    Wave { direction: u8 },
    Breath { colors: ColorMode },
    /// speed: 1 (short) .. 4 (long)
    Reactive { speed: u8, color: Rgb },
    /// speed: 1 (fast) .. 3 (slow)
    Starlight { speed: u8, colors: ColorMode },
    /// Per-key colours from [`DeviceSettings::custom_frame`].
    Custom,
}

impl Effect {
    /// The sysfs file suffix (`<zone>matrix_effect_<kind>`).
    pub fn kind(&self) -> &'static str {
        match self {
            Effect::None => "none",
            Effect::Static { .. } => "static",
            Effect::Spectrum => "spectrum",
            Effect::Wave { .. } => "wave",
            Effect::Breath { .. } => "breath",
            Effect::Reactive { .. } => "reactive",
            Effect::Starlight { .. } => "starlight",
            Effect::Custom => "custom",
        }
    }

    pub fn label(kind: &str) -> &'static str {
        match kind {
            "none" => "Off",
            "static" => "Static",
            "spectrum" => "Spectrum",
            "wave" => "Wave",
            "breath" => "Breathing",
            "reactive" => "Reactive",
            "starlight" => "Starlight",
            "custom" => "Custom",
            _ => "Unknown",
        }
    }

    /// A reasonable default for an effect kind, used when the user switches effects in the GUI.
    pub fn default_for(kind: &str, color: Rgb) -> Option<Effect> {
        Some(match kind {
            "none" => Effect::None,
            "static" => Effect::Static { color },
            "spectrum" => Effect::Spectrum,
            "wave" => Effect::Wave { direction: 1 },
            "breath" => Effect::Breath { colors: ColorMode::Single { color } },
            "reactive" => Effect::Reactive { speed: 2, color },
            "starlight" => Effect::Starlight { speed: 2, colors: ColorMode::Single { color } },
            "custom" => Effect::Custom,
            _ => return None,
        })
    }

    pub fn primary_color(&self) -> Option<Rgb> {
        match self {
            Effect::Static { color } | Effect::Reactive { color, .. } => Some(*color),
            Effect::Breath { colors } | Effect::Starlight { colors, .. } => match colors {
                ColorMode::Single { color } => Some(*color),
                ColorMode::Dual { a, .. } => Some(*a),
                ColorMode::Random => None,
            },
            _ => None,
        }
    }
}

/// Stored state of one lighting zone. `None` means "never set, leave the hardware alone".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ZoneSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<Effect>,
    /// 0..=100 %
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpiStages {
    /// 1-based index into `stages`
    pub active: u8,
    pub stages: Vec<(u16, u16)>,
}

/// Everything TuxGenie remembers for a device and re-applies on login, hotplug and resume.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSettings {
    #[serde(default)]
    pub zones: BTreeMap<String, ZoneSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<(u16, u16)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi_stages: Option<DpiStages>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_rate: Option<u16>,
    /// seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_time: Option<u16>,
    /// percent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_battery: Option<u8>,
    #[serde(default)]
    pub toggles: BTreeMap<String, bool>,
    /// rows x cols, only for devices with a custom-frame capable matrix
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_frame: Option<Vec<Vec<Rgb>>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneInfo {
    pub id: String,
    pub label: String,
    /// effect kinds, see [`Effect::kind`]
    pub effects: Vec<String>,
    pub has_brightness: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Stable id: the serial number when the device reports one.
    pub id: String,
    pub name: String,
    pub kind: String,
    pub serial: String,
    pub firmware: String,
    pub driver: String,
    pub usb_pid: u16,
    pub sysfs_path: String,
    pub zones: Vec<ZoneInfo>,
    /// (rows, cols) when per-key custom lighting is supported
    pub matrix: Option<(u8, u8)>,
    pub has_dpi: bool,
    pub dpi_max: u16,
    pub has_dpi_stages: bool,
    pub poll_rates: Vec<u16>,
    pub has_idle_time: bool,
    pub has_low_battery: bool,
    pub battery: Option<Battery>,
    /// sysfs on/off attributes this device exposes, see [`toggle_label`]
    pub toggles: Vec<String>,
    pub settings: DeviceSettings,
    /// Wireless device whose receiver is connected but the device itself is off/asleep.
    pub asleep: bool,
    /// Last error while applying settings, if any.
    pub error: Option<String>,
}

/// A single settings change sent from the GUI to the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    Effect { zone: String, effect: Effect },
    Brightness { zone: String, percent: u8 },
    Dpi { x: u16, y: u16 },
    DpiStages { stages: DpiStages },
    PollRate { hz: u16 },
    IdleTime { seconds: u16 },
    LowBattery { percent: u8 },
    Toggle { name: String, on: bool },
    CustomFrame { frame: Vec<Vec<Rgb>> },
}

impl Change {
    /// Changes with equal keys supersede each other (used for GUI debouncing).
    pub fn key(&self) -> String {
        match self {
            Change::Effect { zone, .. } => format!("effect:{zone}"),
            Change::Brightness { zone, .. } => format!("brightness:{zone}"),
            Change::Toggle { name, .. } => format!("toggle:{name}"),
            Change::Dpi { .. } => "dpi".into(),
            Change::DpiStages { .. } => "dpi_stages".into(),
            Change::PollRate { .. } => "poll_rate".into(),
            Change::IdleTime { .. } => "idle_time".into(),
            Change::LowBattery { .. } => "low_battery".into(),
            Change::CustomFrame { .. } => "custom_frame".into(),
        }
    }
}

/// On/off sysfs attributes we expose generically.
pub const TOGGLES: &[(&str, &str)] = &[
    ("game_led_state", "Game mode"),
    ("macro_led_state", "Macro LED"),
    ("keyswitch_optimization", "Keyswitch optimization"),
    ("scroll_mode", "Free-spin scroll wheel"),
    ("scroll_acceleration", "Scroll acceleration"),
    ("scroll_smart_reel", "Smart reel"),
    ("fn_toggle", "Function keys as primary"),
];

pub fn toggle_label(name: &str) -> &str {
    TOGGLES.iter().find(|(n, _)| *n == name).map(|(_, l)| *l).unwrap_or(name)
}

/// Lighting zones: (sysfs prefix, zone id, label, brightness file).
pub const ZONES: &[(&str, &str, &str, &str)] = &[
    ("", "backlight", "Backlight", "matrix_brightness"),
    ("backlight_", "backlight", "Backlight", "backlight_led_brightness"),
    ("logo_", "logo", "Logo", "logo_led_brightness"),
    ("scroll_", "scroll", "Scroll wheel", "scroll_led_brightness"),
    ("left_", "left", "Left side", "left_led_brightness"),
    ("right_", "right", "Right side", "right_led_brightness"),
    ("charging_", "charging", "Charging", "charging_led_brightness"),
    ("fast_charging_", "fast_charging", "Fast charging", "fast_charging_led_brightness"),
    ("fully_charged_", "fully_charged", "Fully charged", "fully_charged_led_brightness"),
];

pub const EFFECT_KINDS: &[&str] = &["none", "static", "spectrum", "wave", "breath", "reactive", "starlight", "custom"];

/// Look up generated OpenRazer hardware data: (rows, cols, dpi_max, poll_rates).
pub fn hw_lookup(driver: &str, pid: u16) -> Option<(u8, u8, u16, &'static [u16])> {
    HW_TABLE
        .iter()
        .find(|r| r.0 == driver && r.1 == pid)
        .map(|r| (r.2, r.3, r.4, r.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_roundtrip() {
        let c = Change::Effect {
            zone: "backlight".into(),
            effect: Effect::Breath { colors: ColorMode::Dual { a: Rgb(1, 2, 3), b: Rgb(4, 5, 6) } },
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Change>(&s).unwrap(), c);
    }

    #[test]
    fn table_has_my_devices() {
        assert_eq!(hw_lookup("razerkbd", 0x0257).map(|r| (r.0, r.1)), Some((5, 15)));
        assert_eq!(hw_lookup("razermouse", 0x00B9).map(|r| r.2), Some(18000));
    }
}
