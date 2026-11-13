#!/usr/bin/env bash
# Build TuxGenie and install it for the current user (no root needed).
#
# Installs:  ~/.local/bin/{tuxgenie,tuxgenied}
#            ~/.config/systemd/user/tuxgenied.service   (starts at login, restores settings)
#            ~/.local/share/dbus-1/services/...          (GUI auto-starts the daemon if needed)
#            ~/.local/share/applications/...desktop + icon
# and switches from the Python openrazer-daemon to tuxgenied (uninstall.sh switches back).
set -euo pipefail
cd "$(dirname "$0")"

BINDIR="${BINDIR:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
CONF="${XDG_CONFIG_HOME:-$HOME/.config}"
APP=io.github.raevibe.TuxGenie

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*"; }

# --- requirements --------------------------------------------------------------
if ! ls -d /sys/bus/hid/drivers/razer* >/dev/null 2>&1 && ! modinfo razerkbd >/dev/null 2>&1; then
    warn "the OpenRazer kernel driver is not installed. TuxGenie needs it:"
    echo "    sudo add-apt-repository ppa:openrazer/stable && sudo apt install openrazer-driver-dkms"
    echo "    (or your distro's openrazer-driver / openrazer-kernel-modules-dkms package)"
fi
if ! id -nG | tr ' ' '\n' | grep -qx plugdev; then
    warn "you are not in the 'plugdev' group, so you can't write to the device files:"
    echo "    sudo gpasswd -a $USER plugdev   (then log out and back in)"
fi
command -v cargo >/dev/null || { echo "cargo not found: install Rust from https://rustup.rs"; exit 1; }

# --- build -----------------------------------------------------------------------
say "building (release)"
cargo build --release --locked 2>/dev/null || cargo build --release

# --- install files -----------------------------------------------------------------
say "installing to $BINDIR"
install -Dm755 target/release/tuxgenied "$BINDIR/tuxgenied"
install -Dm755 target/release/tuxgenie  "$BINDIR/tuxgenie"

subst() { sed "s|@BINDIR@|$BINDIR|g" "$1"; }
mkdir -p "$CONF/systemd/user" "$DATA/dbus-1/services" "$DATA/applications" \
         "$DATA/icons/hicolor/scalable/apps" "$DATA/doc/tuxgenie"
subst packaging/tuxgenied.service             > "$CONF/systemd/user/tuxgenied.service"
subst packaging/$APP.dbus.service             > "$DATA/dbus-1/services/$APP.service"
subst packaging/$APP.desktop                  > "$DATA/applications/$APP.desktop"
install -m644 packaging/$APP.svg "$DATA/icons/hicolor/scalable/apps/$APP.svg"
install -m644 README.md "$DATA/doc/tuxgenie/README.md"
command -v update-desktop-database >/dev/null && update-desktop-database "$DATA/applications" 2>/dev/null || true

# --- services ------------------------------------------------------------------------
say "switching from openrazer-daemon to tuxgenied"
systemctl --user daemon-reload
if systemctl --user cat openrazer-daemon.service >/dev/null 2>&1; then
    systemctl --user disable --now openrazer-daemon.service 2>/dev/null || true
    # The Python daemon is also D-Bus activatable (e.g. by Polychromatic); mask it so it
    # can't come back and fight over the devices. uninstall.sh unmasks it.
    systemctl --user mask openrazer-daemon.service >/dev/null 2>&1 || true
fi
systemctl --user enable tuxgenied.service
systemctl --user restart tuxgenied.service

sleep 2
if systemctl --user is-active --quiet tuxgenied.service; then
    say "tuxgenied is running and will start at every login"
    "$BINDIR/tuxgenied" --list 2>/dev/null | grep '"name"' | sed 's/.*"name": "\(.*\)",/    found: \1/' || true
else
    warn "tuxgenied failed to start; see: journalctl --user -u tuxgenied"
fi
case ":$PATH:" in *":$BINDIR:"*) ;; *) warn "$BINDIR is not on your PATH";; esac
say "done. Launch \"TuxGenie\" from your app menu, or run: tuxgenie"
