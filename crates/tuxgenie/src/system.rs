//! Login integration: the systemd user service (daemon) and an XDG autostart entry (window).

use std::path::PathBuf;
use std::process::Command;

const SERVICE: &str = "tuxgenied.service";

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
}

fn autostart_file() -> PathBuf {
    config_dir().join("autostart").join(format!("{}.desktop", tuxgenie_core::APP_ID))
}

fn systemctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!("systemctl --user {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// None when the service isn't installed at all.
pub fn daemon_enabled() -> Option<bool> {
    let out = Command::new("systemctl").args(["--user", "is-enabled", SERVICE]).output().ok()?;
    match String::from_utf8_lossy(&out.stdout).trim() {
        "enabled" | "enabled-runtime" | "linked" => Some(true),
        "disabled" | "masked" => Some(false),
        _ => None,
    }
}

pub fn set_daemon_enabled(on: bool) -> Result<(), String> {
    systemctl(&[if on { "enable" } else { "disable" }, SERVICE]).map(|_| ())
}

pub fn start_daemon() -> Result<(), String> {
    systemctl(&["start", SERVICE]).map(|_| ())
}

pub fn window_autostart_enabled() -> bool {
    autostart_file().exists()
}

pub fn set_window_autostart(on: bool) -> Result<(), String> {
    let path = autostart_file();
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=TuxGenie\nComment=Razer device settings\nExec={}\nIcon={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display(),
        tuxgenie_core::APP_ID
    );
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, entry).map_err(|e| format!("{}: {e}", path.display()))
}
