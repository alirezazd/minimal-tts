#!/usr/bin/env bash
# Remove the Minimal TTS desktop app. Leaves the code and downloaded model weights alone.
set -euo pipefail

APPS="$HOME/.local/share/applications"
UNITS="$HOME/.config/systemd/user"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"

systemctl --user disable --now minimal-tts.service &>/dev/null || true
rm -f "$UNITS/minimal-tts.service" "$APPS/minimal-tts.desktop" "$ICONS/minimal-tts.svg"
systemctl --user daemon-reload
update-desktop-database "$APPS" &>/dev/null || true

echo "Removed. (Run scripts/install.sh to reinstall.)"
