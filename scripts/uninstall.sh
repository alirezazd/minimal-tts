#!/usr/bin/env bash
# Remove the Minimal TTS desktop app. Leaves the code and downloaded model weights alone.
set -euo pipefail

APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"

rm -f "$APPS/minimal-tts.desktop" "$ICONS/minimal-tts.svg"
update-desktop-database "$APPS" &>/dev/null || true

echo "Removed. (Run scripts/install.sh to reinstall.)"
