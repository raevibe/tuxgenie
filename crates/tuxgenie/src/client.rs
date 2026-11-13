//! Talks to tuxgenied over the session bus on a background thread, so a slow
//! device write never freezes the UI.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use tuxgenie_core::{Change, DeviceInfo};

#[zbus::proxy(
    interface = "io.github.raevibe.TuxGenie1",
    default_service = "io.github.raevibe.TuxGenie",
    default_path = "/io/github/raevibe/TuxGenie"
)]
trait TuxGenie {
    fn list_devices(&self) -> zbus::Result<String>;
    fn apply(&self, id: &str, change: &str) -> zbus::Result<()>;
    fn reapply(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn devices_changed(&self) -> zbus::Result<()>;
}

pub enum Cmd {
    Refresh,
    Apply(String, Change),
    Reapply,
}

pub enum Msg {
    Devices(Vec<DeviceInfo>),
    Disconnected(String),
    Error(String),
}

pub fn spawn(ctx: eframe::egui::Context) -> (Sender<Cmd>, Receiver<Msg>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
    let signal_tx = cmd_tx.clone();

    std::thread::Builder::new()
        .name("tuxgenie-dbus".into())
        .spawn(move || {
            let send = |m: Msg| {
                let _ = msg_tx.send(m);
                ctx.request_repaint();
            };
            loop {
                let proxy = match zbus::blocking::Connection::session()
                    .and_then(|c| TuxGenieProxyBlocking::new(&c))
                {
                    Ok(p) => p,
                    Err(e) => {
                        send(Msg::Disconnected(e.to_string()));
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                };

                // Wake up whenever the daemon says something changed.
                if let Ok(signals) = proxy.receive_devices_changed() {
                    let tx = signal_tx.clone();
                    std::thread::spawn(move || {
                        for _ in signals {
                            if tx.send(Cmd::Refresh).is_err() {
                                break;
                            }
                        }
                    });
                }

                let mut pending = vec![Cmd::Refresh];
                'session: loop {
                    // Coalesce everything that queued up meanwhile.
                    while let Ok(c) = cmd_rx.try_recv() {
                        pending.push(c);
                    }
                    let mut refresh = false;
                    for cmd in pending.drain(..) {
                        match cmd {
                            Cmd::Refresh => refresh = true,
                            Cmd::Apply(id, change) => {
                                let json = serde_json::to_string(&change).unwrap();
                                if let Err(e) = proxy.apply(&id, &json) {
                                    send(Msg::Error(dbus_message(&e)));
                                    refresh = true;
                                }
                            }
                            Cmd::Reapply => {
                                if let Err(e) = proxy.reapply() {
                                    send(Msg::Error(dbus_message(&e)));
                                }
                            }
                        }
                    }
                    if refresh {
                        match proxy.list_devices() {
                            Ok(json) => match serde_json::from_str(&json) {
                                Ok(devs) => send(Msg::Devices(devs)),
                                Err(e) => send(Msg::Error(format!("bad reply from daemon: {e}"))),
                            },
                            Err(e) => {
                                send(Msg::Disconnected(dbus_message(&e)));
                                std::thread::sleep(Duration::from_secs(2));
                                break 'session;
                            }
                        }
                    }
                    // Poll every few seconds anyway, for battery levels.
                    match cmd_rx.recv_timeout(Duration::from_secs(5)) {
                        Ok(c) => pending.push(c),
                        Err(RecvTimeoutError::Timeout) => pending.push(Cmd::Refresh),
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        })
        .expect("spawn dbus thread");

    (cmd_tx, msg_rx)
}

fn dbus_message(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(msg), _) => msg.clone(),
        other => other.to_string(),
    }
}
