//! tuxgenied — the TuxGenie user daemon.
//!
//! * finds Razer devices bound to the OpenRazer kernel driver (sysfs polling, 1 s),
//! * re-applies the last settings on login, on hotplug/reconnect and after resume,
//! * persists every change to `~/.config/tuxgenie/settings.json`,
//! * serves the GUI on the session bus as `io.github.raevibe.TuxGenie`.
//!
//! It replaces the Python `openrazer-daemon`; only the OpenRazer *kernel* driver is needed.

mod store;
mod sysfs;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tuxgenie_core::{dbus, Change, DeviceInfo, Effect, Rgb};
use zbus::object_server::SignalEmitter;

use store::Store;
use sysfs::HwDevice;

struct Live {
    hw: HwDevice,
    /// Settings have been merged with what the hardware reports (done once it's awake).
    initialised: bool,
    error: Option<String>,
    attempts: u32,
    next_try: Option<Instant>,
    /// Wireless devices: when to next check whether they're awake/asleep.
    next_presence_check: Instant,
}

struct State {
    store: Store,
    live: BTreeMap<PathBuf, Live>,
    reapply_at: Option<Instant>,
}

const PRESENCE_INTERVAL: Duration = Duration::from_secs(5);

impl State {
    /// Give sleeping wireless devices the id (serial) they had when last seen awake.
    fn resolve_id(&mut self, hw: &mut HwDevice) {
        let key = sysfs::fallback_id(&hw.info.driver, hw.info.usb_pid);
        if hw.awake {
            if self.store.data.known_serials.get(&key) != Some(&hw.info.id) {
                self.store.data.known_serials.insert(key, hw.info.id.clone());
                self.store.save();
            }
        } else if let Some(serial) = self.store.data.known_serials.get(&key) {
            hw.info.id = serial.clone();
        }
    }

    /// Merge stored settings with what an awake device reports, then schedule an apply.
    fn initialise(&mut self, path: &PathBuf, now: Instant) {
        let Some(l) = self.live.get_mut(path) else { return };
        let current = l.hw.read_current();
        let entry = self.store.data.devices.entry(l.hw.info.id.clone()).or_default();
        sysfs::fill_missing(entry, current);
        l.initialised = true;
        l.attempts = 0;
        l.next_try = Some(now);
        self.store.save();
    }

    /// Rescan sysfs, track sleep/wake, (re)apply settings where needed.
    /// Returns true if anything the GUI shows changed.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        let listed = sysfs::list();
        let mut changed = false;

        let present: HashSet<&PathBuf> = listed.iter().map(|(_, _, p)| p).collect();
        self.live.retain(|path, l| {
            let keep = present.contains(path);
            if !keep {
                log::info!("removed: {} ({})", l.hw.info.name, l.hw.info.id);
                changed = true;
            }
            keep
        });

        for (driver, kind, path) in &listed {
            if self.live.contains_key(path) {
                continue;
            }
            let Some(mut hw) = sysfs::probe(driver, kind, path) else { continue };
            self.resolve_id(&mut hw);
            log::info!(
                "found: {} ({}){} at {}",
                hw.info.name,
                hw.info.id,
                if hw.awake { "" } else { " [asleep]" },
                path.display()
            );
            let awake = hw.awake;
            self.live.insert(
                path.clone(),
                Live {
                    hw,
                    initialised: false,
                    error: None,
                    attempts: 0,
                    next_try: None,
                    next_presence_check: now + PRESENCE_INTERVAL,
                },
            );
            if awake {
                self.initialise(path, now);
            }
            changed = true;
        }

        // Wireless devices: notice when they wake up (re-apply) or fall asleep.
        let wireless: Vec<PathBuf> = self
            .live
            .iter()
            .filter(|(_, l)| l.hw.wireless && now >= l.next_presence_check)
            .map(|(p, _)| p.clone())
            .collect();
        for path in wireless {
            let l = self.live.get_mut(&path).unwrap();
            l.next_presence_check = now + PRESENCE_INTERVAL;
            let awake_now = sysfs::usable_serial(&l.hw.read_serial());
            if awake_now == l.hw.awake {
                continue;
            }
            changed = true;
            if awake_now {
                // Re-probe so firmware/serial are real, then re-apply everything.
                let (driver, kind) = (l.hw.info.driver.clone(), l.hw.info.kind.clone());
                if let Some(mut hw) = sysfs::probe(&driver, &kind, &path) {
                    self.resolve_id(&mut hw);
                    log::info!("{} ({}) woke up", hw.info.name, hw.info.id);
                    self.live.get_mut(&path).unwrap().hw = hw;
                    self.initialise(&path, now);
                }
            } else {
                log::info!("{} ({}) went to sleep", l.hw.info.name, l.hw.info.id);
                l.hw.awake = false;
                l.hw.info.asleep = true;
                l.next_try = None;
            }
        }

        if self.reapply_at.is_some_and(|t| now >= t) {
            log::info!("re-applying settings to all devices");
            self.reapply_at = None;
            for l in self.live.values_mut() {
                l.attempts = 0;
                l.next_try = Some(now);
            }
        }

        for l in self.live.values_mut() {
            if !l.next_try.is_some_and(|t| now >= t) {
                continue;
            }
            if !l.hw.awake || !l.initialised {
                l.next_try = None;
                continue;
            }
            let settings = self.store.data.devices.get(&l.hw.info.id).cloned().unwrap_or_default();
            let before = l.error.clone();
            match l.hw.apply_all(&settings) {
                Ok(()) => {
                    log::info!("{}: settings applied", l.hw.info.name);
                    l.error = None;
                    l.next_try = None;
                    l.attempts = 0;
                }
                Err(e) => {
                    l.attempts += 1;
                    // Freshly bound devices often refuse the first writes (udev permissions lag).
                    l.next_try = (l.attempts < 8).then(|| now + Duration::from_secs(1 << l.attempts.min(6)));
                    log::warn!("{}: apply failed (attempt {}): {e}", l.hw.info.name, l.attempts);
                    l.error = Some(e);
                }
            }
            changed |= before != l.error;
        }
        changed
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        let mut seen = HashSet::new();
        self.live
            .values()
            .filter(|l| seen.insert(l.hw.info.id.clone()))
            .map(|l| {
                let mut info = l.hw.info.clone();
                info.settings = self.store.data.devices.get(&info.id).cloned().unwrap_or_default();
                info.battery = l.hw.battery();
                info.error = l.error.clone();
                info
            })
            .collect()
    }

    fn apply(&mut self, id: &str, change: Change) -> Result<(), String> {
        let hws: Vec<HwDevice> = self.live.values().filter(|l| l.hw.info.id == id).map(|l| l.hw.clone()).collect();
        let Some(name) = hws.first().map(|h| h.info.name.clone()) else {
            return Err(format!("device {id} is not connected"));
        };
        if hws.iter().all(|h| !h.awake) {
            return Err(format!("{name} is asleep or switched off - move it to wake it up"));
        }
        let hws: Vec<HwDevice> = hws.into_iter().filter(|h| h.awake).collect();
        let mut s = self.store.data.devices.get(id).cloned().unwrap_or_default();

        let blank_frame = |hw: &HwDevice| {
            hw.info.matrix.map(|(r, c)| vec![vec![Rgb(0, 255, 0); c as usize]; r as usize])
        };

        let result: Result<(), String> = hws.iter().try_for_each(|hw| match &change {
            Change::Effect { zone, effect } => {
                if *effect == Effect::Custom && s.custom_frame.is_none() {
                    s.custom_frame = blank_frame(hw);
                }
                hw.apply_effect(zone, effect, s.custom_frame.as_ref())
            }
            Change::Brightness { zone, percent } => hw.apply_brightness(zone, *percent),
            Change::Dpi { x, y } => hw.apply_dpi(*x, *y),
            Change::DpiStages { stages } => {
                hw.apply_dpi_stages(stages)?;
                if let Some(&(x, y)) = stages.stages.get(stages.active as usize - 1) {
                    hw.apply_dpi(x, y)?;
                }
                Ok(())
            }
            Change::PollRate { hz } => hw.apply_poll_rate(*hz),
            Change::IdleTime { seconds } => hw.apply_idle_time(*seconds),
            Change::LowBattery { percent } => hw.apply_low_battery(*percent),
            Change::Toggle { name, on } => hw.apply_toggle(name, *on),
            Change::CustomFrame { frame } => {
                hw.apply_custom_frame(frame)?;
                hw.apply_effect("backlight", &Effect::Custom, Some(frame))
            }
        });
        result.map_err(|e| format!("{name}: {e}"))?;

        match change {
            Change::Effect { zone, effect } => s.zones.entry(zone).or_default().effect = Some(effect),
            Change::Brightness { zone, percent } => s.zones.entry(zone).or_default().brightness = Some(percent),
            Change::Dpi { x, y } => s.dpi = Some((x, y)),
            Change::DpiStages { stages } => {
                s.dpi = stages.stages.get(stages.active as usize - 1).copied().or(s.dpi);
                s.dpi_stages = Some(stages);
            }
            Change::PollRate { hz } => s.poll_rate = Some(hz),
            Change::IdleTime { seconds } => s.idle_time = Some(seconds),
            Change::LowBattery { percent } => s.low_battery = Some(percent),
            Change::Toggle { name, on } => {
                s.toggles.insert(name, on);
            }
            Change::CustomFrame { frame } => {
                s.custom_frame = Some(frame);
                s.zones.entry("backlight".into()).or_default().effect = Some(Effect::Custom);
            }
        }
        self.store.data.devices.insert(id.to_string(), s);
        self.store.save();
        Ok(())
    }
}

struct Service {
    state: Arc<Mutex<State>>,
}

#[zbus::interface(name = "io.github.raevibe.TuxGenie1")]
impl Service {
    /// JSON array of `tuxgenie_core::DeviceInfo`.
    fn list_devices(&self) -> String {
        serde_json::to_string(&self.state.lock().unwrap().devices()).unwrap_or_else(|_| "[]".into())
    }

    /// Apply a JSON-encoded `tuxgenie_core::Change` to a device and remember it.
    async fn apply(
        &self,
        id: &str,
        change: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let change: Change =
            serde_json::from_str(change).map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let res = self.state.lock().unwrap().apply(id, change);
        res.map_err(zbus::fdo::Error::Failed)?;
        let _ = Self::devices_changed(&emitter).await;
        Ok(())
    }

    /// Re-write all stored settings to every connected device.
    fn reapply(&self) {
        self.state.lock().unwrap().reapply_at = Some(Instant::now());
    }

    #[zbus(property)]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    #[zbus(signal)]
    async fn devices_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Devices lose their lighting state across suspend; re-apply shortly after resume.
fn watch_resume(state: Arc<Mutex<State>>) {
    std::thread::spawn(move || {
        let run = || -> zbus::Result<()> {
            let conn = zbus::blocking::Connection::system()?;
            let proxy = Login1ManagerProxyBlocking::new(&conn)?;
            for sig in proxy.receive_prepare_for_sleep()? {
                if !sig.args()?.start {
                    log::info!("resumed from sleep");
                    state.lock().unwrap().reapply_at = Some(Instant::now() + Duration::from_secs(3));
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            log::warn!("not watching suspend/resume: {e}");
        }
    });
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,zbus=warn,tracing=warn"))
        .format_timestamp(None)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut state = State { store: Store::load(), live: BTreeMap::new(), reapply_at: None };

    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("tuxgenied {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--list") => {
            for (driver, kind, path) in sysfs::list() {
                if let Some(mut hw) = sysfs::probe(driver, kind, &path) {
                    state.resolve_id(&mut hw);
                    let live = Live {
                        hw,
                        initialised: false,
                        error: None,
                        attempts: 0,
                        next_try: None,
                        next_presence_check: Instant::now(),
                    };
                    state.live.insert(path, live);
                }
            }
            println!("{}", serde_json::to_string_pretty(&state.devices())?);
            return Ok(());
        }
        Some("--once") => {
            state.tick();
            let failed: Vec<_> = state.live.values().filter_map(|l| l.error.as_ref()).collect();
            if !failed.is_empty() {
                anyhow::bail!("some settings failed to apply");
            }
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!("tuxgenied [--once | --list | --version]\n\nWithout arguments: run the daemon on the session bus.");
            return Ok(());
        }
        Some(other) => anyhow::bail!("unknown argument {other}"),
        None => {}
    }

    log::info!("tuxgenied {} starting", env!("CARGO_PKG_VERSION"));
    let state = Arc::new(Mutex::new(state));
    let conn = zbus::blocking::connection::Builder::session()?
        .serve_at(dbus::OBJECT_PATH, Service { state: state.clone() })?
        .name(dbus::BUS_NAME)?
        .build()
        .map_err(|e| anyhow::anyhow!("cannot claim {} on the session bus (already running?): {e}", dbus::BUS_NAME))?;

    let iface = conn.object_server().interface::<_, Service>(dbus::OBJECT_PATH)?;
    watch_resume(state.clone());

    loop {
        let changed = state.lock().unwrap().tick();
        if changed {
            let _ = zbus::block_on(Service::devices_changed(iface.signal_emitter()));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
