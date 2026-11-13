#!/usr/bin/env bash
# Remove TuxGenie and give control back to openrazer-daemon. Keeps ~/.config/tuxgenie.
set -uo pipefail
BINDIR="${BINDIR:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
CONF="${XDG_CONFIG_HOME:-$HOME/.config}"
APP=io.github.raevibe.TuxGenie

systemctl --user disable --now tuxgenied.service 2>/dev/null
rm -f "$CONF/systemd/user/tuxgenied.service" \
      "$DATA/dbus-1/services/$APP.service" \
      "$DATA/applications/$APP.desktop" \
      "$DATA/icons/hicolor/scalable/apps/$APP.svg" \
      "$CONF/autostart/$APP.desktop" \
      "$BINDIR/tuxgenied" "$BINDIR/tuxgenie"
rm -rf "$DATA/doc/tuxgenie"
systemctl --user daemon-reload
if systemctl --user cat openrazer-daemon.service >/dev/null 2>&1; then
    systemctl --user unmask openrazer-daemon.service 2>/dev/null
    systemctl --user enable --now openrazer-daemon.service
    echo "openrazer-daemon re-enabled"
fi
echo "TuxGenie removed (settings kept in $CONF/tuxgenie)"
