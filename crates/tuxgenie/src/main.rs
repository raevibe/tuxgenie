//! TuxGenie — a Rust take on RazerGenie: configure Razer devices on Linux.
//!
//! The GUI holds no device state of its own: every change goes to the `tuxgenied`
//! daemon, which writes it to the hardware, remembers it and restores it at login.

mod client;
mod system;

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Sense, Stroke, Vec2};
use tuxgenie_core::{
    toggle_label, Change, ColorMode, DeviceInfo, DpiStages, Effect, Rgb, ZoneInfo, APP_ID,
};

use client::{Cmd, Msg};

const ACCENT: Color32 = Color32::from_rgb(0x3c, 0xd0, 0x6e);
const DEFAULT_COLOR: Rgb = Rgb(0, 255, 0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Lighting,
    Custom,
    Performance,
    Power,
    Info,
}

impl Tab {
    fn from_env() -> Option<Tab> {
        Some(match std::env::var("TUXGENIE_TAB").ok()?.as_str() {
            "lighting" => Tab::Lighting,
            "custom" => Tab::Custom,
            "performance" => Tab::Performance,
            "power" => Tab::Power,
            "info" => Tab::Info,
            _ => return None,
        })
    }
}

struct App {
    tx: Sender<Cmd>,
    rx: Receiver<Msg>,
    devices: Vec<DeviceInfo>,
    selected: usize,
    tab: Tab,
    connected: Option<Result<(), String>>,
    status: Option<(String, Instant)>,
    /// Debounced changes, keyed by `(device id, Change::key())`.
    pending: BTreeMap<(String, String), Change>,
    last_flush: Instant,
    last_edit: Instant,
    /// Remembered colour per zone, so switching effects keeps your colour.
    zone_color: BTreeMap<(String, String), Rgb>,
    paint: Rgb,
    lock_xy: bool,
    daemon_at_login: Option<bool>,
    window_at_login: bool,
    screenshot: Option<(String, Instant, bool)>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.selection.bg_fill = ACCENT.linear_multiply(0.6);
        style.visuals.hyperlink_color = ACCENT;
        style.visuals.slider_trailing_fill = true;
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.slider_width = 260.0;
        cc.egui_ctx.set_style(style);

        let (tx, rx) = client::spawn(cc.egui_ctx.clone());
        App {
            tx,
            rx,
            devices: vec![],
            selected: std::env::var("TUXGENIE_DEVICE").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
            tab: Tab::from_env().unwrap_or(Tab::Lighting),
            connected: None,
            status: None,
            pending: BTreeMap::new(),
            last_flush: Instant::now(),
            last_edit: Instant::now() - Duration::from_secs(10),
            zone_color: BTreeMap::new(),
            paint: Rgb(0, 255, 0),
            lock_xy: true,
            daemon_at_login: system::daemon_enabled(),
            window_at_login: system::window_autostart_enabled(),
            screenshot: std::env::var("TUXGENIE_SCREENSHOT").ok().map(|p| (p, Instant::now(), false)),
        }
    }

    fn queue(&mut self, id: &str, change: Change) {
        self.pending.insert((id.to_string(), change.key()), change);
        self.last_edit = Instant::now();
    }

    fn flush(&mut self, force: bool) {
        if self.pending.is_empty() || (!force && self.last_flush.elapsed() < Duration::from_millis(90)) {
            return;
        }
        for ((id, _), change) in std::mem::take(&mut self.pending) {
            let _ = self.tx.send(Cmd::Apply(id, change));
        }
        self.last_flush = Instant::now();
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Devices(mut devs) => {
                    self.connected = Some(Ok(()));
                    // Don't let a refresh fight with the slider the user is dragging.
                    if self.last_edit.elapsed() < Duration::from_millis(800) || !self.pending.is_empty() {
                        for d in devs.iter_mut() {
                            if let Some(old) = self.devices.iter().find(|o| o.id == d.id) {
                                d.settings = old.settings.clone();
                            }
                        }
                    }
                    let selected_id = self.devices.get(self.selected).map(|d| d.id.clone());
                    self.devices = devs;
                    if let Some(id) = selected_id {
                        if let Some(i) = self.devices.iter().position(|d| d.id == id) {
                            self.selected = i;
                        }
                    }
                    if self.selected >= self.devices.len() {
                        self.selected = 0;
                    }
                }
                Msg::Disconnected(e) => {
                    self.connected = Some(Err(e));
                    self.devices.clear();
                }
                Msg::Error(e) => self.status = Some((e, Instant::now())),
            }
        }
    }

    fn color_for(&self, dev: &str, zone: &str, effect: Option<&Effect>) -> Rgb {
        effect
            .and_then(Effect::primary_color)
            .or_else(|| self.zone_color.get(&(dev.to_string(), zone.to_string())).copied())
            .unwrap_or(DEFAULT_COLOR)
    }

    // ---------------------------------------------------------------- panels

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("devices").resizable(false).exact_width(250.0).show(ctx, |ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("TuxGenie").size(24.0).strong().color(ACCENT));
            ui.label(RichText::new("Razer device settings").weak());
            ui.add_space(8.0);
            ui.separator();

            for (i, d) in self.devices.iter().enumerate() {
                let icon = match d.kind.as_str() {
                    "keyboard" => "⌨",
                    "mouse" => "🖱",
                    "headset" => "🎧",
                    _ => "◆",
                };
                let sub = if d.asleep {
                    "asleep".to_string()
                } else if let Some(b) = &d.battery {
                    format!("{}% battery{}", b.percent, if b.charging { ", charging" } else { "" })
                } else {
                    d.kind.clone()
                };
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
                let selected = self.selected == i;
                let bg = if selected {
                    Color32::from_rgb(0x1f, 0x4d, 0x2e)
                } else if resp.hovered() {
                    Color32::from_gray(40)
                } else {
                    Color32::TRANSPARENT
                };
                let p = ui.painter();
                p.rect_filled(rect, 6.0, bg);
                let fg = if d.asleep { Color32::from_gray(130) } else { Color32::from_gray(235) };
                let left = rect.left_center() + Vec2::new(12.0, 0.0);
                p.text(left, egui::Align2::LEFT_CENTER, icon, egui::FontId::proportional(20.0), fg);
                let name = d.name.strip_prefix("Razer ").unwrap_or(&d.name);
                p.text(left + Vec2::new(30.0, -8.0), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(14.0), fg);
                p.text(
                    left + Vec2::new(30.0, 10.0),
                    egui::Align2::LEFT_CENTER,
                    sub,
                    egui::FontId::proportional(12.0),
                    Color32::from_gray(140),
                );
                if resp.clicked() {
                    self.selected = i;
                }
            }
            if self.devices.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("No devices").weak());
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.add_space(8.0);
                let (text, color) = match &self.connected {
                    Some(Ok(())) => ("daemon running", ACCENT),
                    Some(Err(_)) => ("daemon not running", Color32::LIGHT_RED),
                    None => ("connecting…", Color32::GRAY),
                };
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, color);
                    ui.label(RichText::new(text).color(color).small());
                });
                let mut win = self.window_at_login;
                if ui.checkbox(&mut win, "Open this window at login").changed() {
                    match system::set_window_autostart(win) {
                        Ok(()) => self.window_at_login = win,
                        Err(e) => self.status = Some((e, Instant::now())),
                    }
                }
                if let Some(mut on) = self.daemon_at_login {
                    if ui
                        .checkbox(&mut on, "Restore settings at login")
                        .on_hover_text("Enables the tuxgenied systemd user service")
                        .changed()
                    {
                        match system::set_daemon_enabled(on) {
                            Ok(()) => self.daemon_at_login = Some(on),
                            Err(e) => self.status = Some((e, Instant::now())),
                        }
                    }
                }
                ui.separator();
            });
        });
    }

    fn no_daemon(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            match &self.connected {
                Some(Err(e)) => {
                    ui.heading("Can't reach tuxgenied");
                    ui.label(RichText::new(e).weak());
                    ui.add_space(12.0);
                    if ui.button("Start daemon").clicked() {
                        if let Err(e) = system::start_daemon() {
                            self.status = Some((e, Instant::now()));
                        }
                    }
                }
                _ => {
                    ui.heading("No Razer devices found");
                    ui.label(
                        RichText::new(
                            "TuxGenie needs the OpenRazer kernel driver (openrazer-driver-dkms)\n\
                             and your user in the plugdev group.",
                        )
                        .weak(),
                    );
                }
            }
        });
    }

    fn device_page(&mut self, ui: &mut egui::Ui) {
        let dev = self.devices[self.selected].clone();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&dev.name).size(22.0).strong());
            if dev.asleep {
                ui.label(RichText::new("asleep — move it to wake it up").color(Color32::GOLD));
            }
        });
        if let Some(err) = &dev.error {
            ui.label(RichText::new(format!("Error: {err}")).color(Color32::LIGHT_RED));
        }
        ui.add_space(4.0);

        let mut tabs = vec![(Tab::Lighting, "Lighting")];
        if dev.matrix.is_some() {
            tabs.push((Tab::Custom, "Custom"));
        }
        if dev.has_dpi || !dev.poll_rates.is_empty() {
            tabs.push((Tab::Performance, "Performance"));
        }
        if dev.battery.is_some() || dev.has_idle_time || dev.has_low_battery {
            tabs.push((Tab::Power, "Power"));
        }
        tabs.push((Tab::Info, "Info"));
        if !tabs.iter().any(|(t, _)| *t == self.tab) {
            self.tab = Tab::Lighting;
        }
        ui.horizontal(|ui| {
            for (t, label) in &tabs {
                let text = RichText::new(*label).size(15.0);
                ui.selectable_value(&mut self.tab, *t, text);
            }
        });
        ui.separator();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_enabled_ui(!dev.asleep, |ui| match self.tab {
                Tab::Lighting => self.lighting_tab(ui, &dev),
                Tab::Custom => self.custom_tab(ui, &dev),
                Tab::Performance => self.performance_tab(ui, &dev),
                Tab::Power => self.power_tab(ui, &dev),
                Tab::Info => self.info_tab(ui, &dev),
            });
        });
    }

    // ---------------------------------------------------------------- tabs

    fn lighting_tab(&mut self, ui: &mut egui::Ui, dev: &DeviceInfo) {
        if dev.zones.is_empty() {
            ui.label(RichText::new("This device has no controllable lighting.").weak());
        }
        for zone in &dev.zones {
            card(ui, &zone.label, |ui| self.zone_controls(ui, dev, zone));
        }
        if !dev.toggles.is_empty() {
            card(ui, "Options", |ui| {
                for t in &dev.toggles {
                    let mut on = dev.settings.toggles.get(t).copied().unwrap_or(false);
                    if ui.checkbox(&mut on, toggle_label(t)).changed() {
                        self.devices[self.selected].settings.toggles.insert(t.clone(), on);
                        self.queue(&dev.id, Change::Toggle { name: t.clone(), on });
                    }
                }
            });
        }
    }

    fn zone_controls(&mut self, ui: &mut egui::Ui, dev: &DeviceInfo, zone: &ZoneInfo) {
        let stored = dev.settings.zones.get(&zone.id).cloned().unwrap_or_default();
        let current = stored.effect.clone();
        let base_color = self.color_for(&dev.id, &zone.id, current.as_ref());
        let mut new_effect: Option<Effect> = None;

        egui::Grid::new(("zone", &dev.id, &zone.id)).num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Effect");
            let kind = current.as_ref().map(Effect::kind).unwrap_or("");
            let selected_text = if kind.is_empty() { "Not set" } else { Effect::label(kind) };
            egui::ComboBox::from_id_salt(("effect", &dev.id, &zone.id))
                .width(180.0)
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for k in &zone.effects {
                        if ui.selectable_label(k == kind, Effect::label(k)).clicked() && k != kind {
                            new_effect = Effect::default_for(k, base_color);
                        }
                    }
                });
            ui.end_row();

            if let Some(effect) = &current {
                if let Some(e) = effect_params(ui, effect) {
                    new_effect = Some(e);
                }
            }

            if zone.has_brightness {
                ui.label("Brightness");
                let mut b = stored.brightness.unwrap_or(100);
                if ui.add(egui::Slider::new(&mut b, 0..=100).suffix(" %")).changed() {
                    self.devices[self.selected].settings.zones.entry(zone.id.clone()).or_default().brightness =
                        Some(b);
                    self.queue(&dev.id, Change::Brightness { zone: zone.id.clone(), percent: b });
                }
                ui.end_row();
            }
        });

        if current == Some(Effect::Custom) {
            if ui.link("Edit the per-key colours in the Custom tab →").clicked() {
                self.tab = Tab::Custom;
            }
        }

        if let Some(effect) = new_effect {
            if let Some(c) = effect.primary_color() {
                self.zone_color.insert((dev.id.clone(), zone.id.clone()), c);
            }
            self.devices[self.selected].settings.zones.entry(zone.id.clone()).or_default().effect = Some(effect.clone());
            self.queue(&dev.id, Change::Effect { zone: zone.id.clone(), effect });
        }
    }

    fn custom_tab(&mut self, ui: &mut egui::Ui, dev: &DeviceInfo) {
        let Some((rows, cols)) = dev.matrix else { return };
        let (rows, cols) = (rows as usize, cols as usize);
        let mut frame = dev.settings.custom_frame.clone().unwrap_or_else(|| vec![vec![DEFAULT_COLOR; cols]; rows]);
        frame.resize(rows, vec![]);
        for row in frame.iter_mut() {
            row.resize(cols, Rgb::BLACK);
        }
        let mut changed = false;

        card(ui, "Per-key lighting", |ui| {
            ui.horizontal(|ui| {
                ui.label("Paint colour");
                let mut c = self.paint.bytes();
                if egui::color_picker::color_edit_button_srgb(ui, &mut c).changed() {
                    self.paint = Rgb(c[0], c[1], c[2]);
                }
                for preset in [
                    Rgb(255, 0, 0),
                    Rgb(255, 128, 0),
                    Rgb(255, 255, 0),
                    Rgb(0, 255, 0),
                    Rgb(0, 255, 255),
                    Rgb(0, 0, 255),
                    Rgb(255, 0, 255),
                    Rgb(255, 255, 255),
                    Rgb(0, 0, 0),
                ] {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                    ui.painter().rect_filled(rect, 4.0, to_c32(preset));
                    if self.paint == preset {
                        ui.painter().rect_stroke(rect, 4.0, Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Outside);
                    }
                    if resp.clicked() {
                        self.paint = preset;
                    }
                }
                ui.separator();
                if ui.button("Fill all").clicked() {
                    frame.iter_mut().flatten().for_each(|k| *k = self.paint);
                    changed = true;
                }
                if ui.button("Clear").clicked() {
                    frame.iter_mut().flatten().for_each(|k| *k = Rgb::BLACK);
                    changed = true;
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Click or drag across the keys to paint them. Right-click picks a key's colour.").weak());
            ui.add_space(6.0);

            let gap = 5.0;
            let key = ((ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32).clamp(22.0, 52.0);
            let size = Vec2::new(cols as f32 * (key + gap) - gap, rows as f32 * (key + gap) - gap);
            let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
            let cell_at = |pos: egui::Pos2| {
                let rel = pos - rect.min;
                let (c, r) = ((rel.x / (key + gap)) as usize, (rel.y / (key + gap)) as usize);
                (r < rows && c < cols && rel.x >= 0.0 && rel.y >= 0.0).then_some((r, c))
            };
            if let Some(pos) = resp.interact_pointer_pos() {
                if let Some((r, c)) = cell_at(pos) {
                    if resp.secondary_clicked() {
                        self.paint = frame[r][c];
                    } else if (resp.dragged_by(egui::PointerButton::Primary) || resp.clicked()) && frame[r][c] != self.paint
                    {
                        frame[r][c] = self.paint;
                        changed = true;
                    }
                }
            }
            let hover = resp.hover_pos().and_then(cell_at);
            let painter = ui.painter_at(rect);
            for (r, row) in frame.iter().enumerate() {
                for (c, rgb) in row.iter().enumerate() {
                    let min = rect.min + Vec2::new(c as f32 * (key + gap), r as f32 * (key + gap));
                    let cell = egui::Rect::from_min_size(min, Vec2::splat(key));
                    painter.rect_filled(cell, 5.0, Color32::from_gray(28));
                    painter.rect_filled(cell.shrink(3.0), 3.0, to_c32(*rgb));
                    let stroke = if hover == Some((r, c)) {
                        Stroke::new(2.0, Color32::WHITE)
                    } else {
                        Stroke::new(1.0, Color32::from_gray(60))
                    };
                    painter.rect_stroke(cell, 5.0, stroke, egui::StrokeKind::Inside);
                }
            }
            if let Some((r, c)) = hover {
                resp.on_hover_text(format!("row {}, column {}", r + 1, c + 1));
            }
        });

        let active = dev.settings.zones.get("backlight").and_then(|z| z.effect.clone()) == Some(Effect::Custom);
        if !active {
            ui.horizontal(|ui| {
                ui.label(RichText::new("The backlight isn't using the custom layout.").weak());
                if ui.button("Use it now").clicked() {
                    changed = true;
                }
            });
        }

        if changed {
            let s = &mut self.devices[self.selected].settings;
            s.custom_frame = Some(frame.clone());
            s.zones.entry("backlight".into()).or_default().effect = Some(Effect::Custom);
            self.queue(&dev.id, Change::CustomFrame { frame });
        }
    }

    fn performance_tab(&mut self, ui: &mut egui::Ui, dev: &DeviceInfo) {
        let s = dev.settings.clone();
        if dev.has_dpi {
            card(ui, "Sensitivity (DPI)", |ui| {
                let (mut x, mut y) = s.dpi.unwrap_or((800, 800));
                let range = 100..=dev.dpi_max;
                let mut changed = false;
                ui.checkbox(&mut self.lock_xy, "Same DPI for X and Y");
                if self.lock_xy {
                    changed |= ui.add(egui::Slider::new(&mut x, range.clone()).step_by(50.0).text("DPI")).changed();
                    y = x;
                } else {
                    changed |= ui.add(egui::Slider::new(&mut x, range.clone()).step_by(50.0).text("X")).changed();
                    changed |= ui.add(egui::Slider::new(&mut y, range).step_by(50.0).text("Y")).changed();
                }
                if changed {
                    self.devices[self.selected].settings.dpi = Some((x, y));
                    self.queue(&dev.id, Change::Dpi { x, y });
                }
            });
        }

        if dev.has_dpi_stages {
            card(ui, "DPI stages", |ui| {
                ui.label(RichText::new("The DPI button on the mouse cycles through these. The selected stage is active.").weak());
                let mut st = s.dpi_stages.clone().unwrap_or(DpiStages {
                    active: 1,
                    stages: vec![(400, 400), (800, 800), (1600, 1600), (3200, 3200), (6400, 6400)],
                });
                let mut changed = false;
                let mut remove = None;
                egui::Grid::new("stages").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                    let n = st.stages.len();
                    for (i, (x, y)) in st.stages.iter_mut().enumerate() {
                        changed |= ui.radio_value(&mut st.active, (i + 1) as u8, format!("Stage {}", i + 1)).changed();
                        let mut v = *x;
                        if ui.add(egui::Slider::new(&mut v, 100..=dev.dpi_max).step_by(50.0).suffix(" DPI")).changed() {
                            (*x, *y) = (v, v);
                            changed = true;
                        }
                        if n > 1 && ui.small_button("Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
                if let Some(i) = remove {
                    st.stages.remove(i);
                    st.active = st.active.min(st.stages.len() as u8).max(1);
                    changed = true;
                }
                if st.stages.len() < 5 && ui.button("+ Add stage").clicked() {
                    let last = st.stages.last().map(|s| s.0).unwrap_or(800);
                    let v = (last.saturating_mul(2)).min(dev.dpi_max);
                    st.stages.push((v, v));
                    changed = true;
                }
                if changed {
                    let d = &mut self.devices[self.selected].settings;
                    d.dpi = st.stages.get(st.active as usize - 1).copied().or(d.dpi);
                    d.dpi_stages = Some(st.clone());
                    self.queue(&dev.id, Change::DpiStages { stages: st });
                }
            });
        }

        if !dev.poll_rates.is_empty() {
            card(ui, "Polling rate", |ui| {
                let mut hz = s.poll_rate.unwrap_or(1000);
                let before = hz;
                ui.horizontal(|ui| {
                    for r in &dev.poll_rates {
                        ui.selectable_value(&mut hz, *r, format!("{r} Hz"));
                    }
                });
                if hz != before {
                    self.devices[self.selected].settings.poll_rate = Some(hz);
                    self.queue(&dev.id, Change::PollRate { hz });
                }
            });
        }
    }

    fn power_tab(&mut self, ui: &mut egui::Ui, dev: &DeviceInfo) {
        if let Some(b) = &dev.battery {
            card(ui, "Battery", |ui| {
                let fill = if b.percent <= 15 { Color32::LIGHT_RED } else { ACCENT };
                ui.add(
                    egui::ProgressBar::new(b.percent as f32 / 100.0)
                        .desired_width(300.0)
                        .fill(fill)
                        .text(format!("{}%{}", b.percent, if b.charging { "  (charging)" } else { "" })),
                );
            });
        }
        if dev.has_idle_time {
            card(ui, "Sleep", |ui| {
                let mut t = dev.settings.idle_time.unwrap_or(300);
                let resp = ui.add(
                    egui::Slider::new(&mut t, 60..=900)
                        .step_by(30.0)
                        .custom_formatter(|v, _| format!("{:.1} min", v / 60.0))
                        .text("Go to sleep after idle"),
                );
                if resp.changed() {
                    self.devices[self.selected].settings.idle_time = Some(t);
                    self.queue(&dev.id, Change::IdleTime { seconds: t });
                }
            });
        }
        if dev.has_low_battery {
            card(ui, "Low battery warning", |ui| {
                let mut p = dev.settings.low_battery.unwrap_or(10);
                if ui.add(egui::Slider::new(&mut p, 1..=100).suffix(" %").text("Warn below")).changed() {
                    self.devices[self.selected].settings.low_battery = Some(p);
                    self.queue(&dev.id, Change::LowBattery { percent: p });
                }
            });
        }
    }

    fn info_tab(&mut self, ui: &mut egui::Ui, dev: &DeviceInfo) {
        card(ui, "Device", |ui| {
            egui::Grid::new("info").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                let rows = [
                    ("Name", dev.name.clone()),
                    ("Type", dev.kind.clone()),
                    ("Serial", if dev.serial.is_empty() { "—".into() } else { dev.serial.clone() }),
                    ("Firmware", if dev.firmware.is_empty() { "—".into() } else { dev.firmware.clone() }),
                    ("USB ID", format!("1532:{:04x}", dev.usb_pid)),
                    ("Driver", dev.driver.clone()),
                    ("sysfs", dev.sysfs_path.clone()),
                ];
                for (k, v) in rows {
                    ui.label(RichText::new(k).weak());
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
        card(ui, "Settings", |ui| {
            ui.label(
                RichText::new(
                    "Settings are saved in ~/.config/tuxgenie/settings.json and restored by tuxgenied \
                     at login, when a device is plugged in or wakes up, and after suspend.",
                )
                .weak(),
            );
            if ui.button("Re-apply saved settings now").clicked() {
                let _ = self.tx.send(Cmd::Reapply);
            }
        });
    }

    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, started, requested)) = self.screenshot.clone() else { return };
        if !requested && started.elapsed() > Duration::from_secs(3) {
            self.screenshot = Some((path.clone(), started, true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.request_repaint();
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            if let Some(buf) = ::image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                let _ = buf.save(&path);
            }
            self.screenshot = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();

        self.sidebar(ctx);

        if let Some((msg, at)) = &self.status {
            if at.elapsed() > Duration::from_secs(8) {
                self.status = None;
            } else {
                let msg = msg.clone();
                egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
                    ui.label(RichText::new(format!("Error: {msg}")).color(Color32::LIGHT_RED));
                });
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.devices.is_empty() {
                self.no_daemon(ui);
            } else {
                self.device_page(ui);
            }
        });

        self.flush(false);
        if !self.pending.is_empty() || self.status.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.screenshot(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush(true);
        // Give the worker a moment to deliver the last change.
        std::thread::sleep(Duration::from_millis(150));
    }
}

// -------------------------------------------------------------------- helpers

fn to_c32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(Color32::from_gray(32))
        .stroke(Stroke::new(1.0, Color32::from_gray(48)))
        .corner_radius(8.0)
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(16.0).strong());
            ui.add_space(4.0);
            body(ui);
        });
    ui.add_space(10.0);
}

fn color_button(ui: &mut egui::Ui, c: &mut Rgb) -> bool {
    let mut b = c.bytes();
    let changed = egui::color_picker::color_edit_button_srgb(ui, &mut b).changed();
    *c = Rgb(b[0], b[1], b[2]);
    changed
}

/// Colour-mode editor (random / one / two colours). Returns true if changed.
fn color_mode(ui: &mut egui::Ui, m: &mut ColorMode) -> bool {
    let mut changed = false;
    let fallback = match m {
        ColorMode::Single { color } | ColorMode::Dual { a: color, .. } => *color,
        ColorMode::Random => DEFAULT_COLOR,
    };
    ui.horizontal(|ui| {
        let mut sel = match m {
            ColorMode::Random => 0,
            ColorMode::Single { .. } => 1,
            ColorMode::Dual { .. } => 2,
        };
        let before = sel;
        ui.selectable_value(&mut sel, 0, "Random");
        ui.selectable_value(&mut sel, 1, "One colour");
        ui.selectable_value(&mut sel, 2, "Two colours");
        if sel != before {
            *m = match sel {
                0 => ColorMode::Random,
                1 => ColorMode::Single { color: fallback },
                _ => ColorMode::Dual { a: fallback, b: Rgb(0, 0, 255) },
            };
            changed = true;
        }
        ui.add_space(8.0);
        match m {
            ColorMode::Random => {}
            ColorMode::Single { color } => changed |= color_button(ui, color),
            ColorMode::Dual { a, b } => {
                changed |= color_button(ui, a);
                changed |= color_button(ui, b);
            }
        }
    });
    changed
}

/// Parameter rows for the current effect. Returns the edited effect if anything changed.
fn effect_params(ui: &mut egui::Ui, effect: &Effect) -> Option<Effect> {
    let mut e = effect.clone();
    let mut changed = false;
    match &mut e {
        Effect::Static { color } => {
            ui.label("Colour");
            changed |= color_button(ui, color);
            ui.end_row();
        }
        Effect::Wave { direction } => {
            ui.label("Direction");
            ui.horizontal(|ui| {
                changed |= ui.selectable_value(direction, 1, "← Left").changed();
                changed |= ui.selectable_value(direction, 2, "Right →").changed();
            });
            ui.end_row();
        }
        Effect::Breath { colors } => {
            ui.label("Colours");
            changed |= color_mode(ui, colors);
            ui.end_row();
        }
        Effect::Reactive { speed, color } => {
            ui.label("Colour");
            changed |= color_button(ui, color);
            ui.end_row();
            ui.label("Fade time");
            ui.horizontal(|ui| {
                for (v, l) in [(1, "Short"), (2, "Medium"), (3, "Long"), (4, "Very long")] {
                    changed |= ui.selectable_value(speed, v, l).changed();
                }
            });
            ui.end_row();
        }
        Effect::Starlight { speed, colors } => {
            ui.label("Colours");
            changed |= color_mode(ui, colors);
            ui.end_row();
            ui.label("Speed");
            ui.horizontal(|ui| {
                for (v, l) in [(1, "Fast"), (2, "Medium"), (3, "Slow")] {
                    changed |= ui.selectable_value(speed, v, l).changed();
                }
            });
            ui.end_row();
        }
        Effect::None | Effect::Spectrum | Effect::Custom => {}
    }
    changed.then_some(e)
}

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID)
            .with_title("TuxGenie")
            .with_inner_size([980.0, 660.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("TuxGenie", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
