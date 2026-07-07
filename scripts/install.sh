#!/usr/bin/env bash
# Install Minimal TTS as a desktop app: launcher, icon, and an always-warm user service.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UV="$(command -v uv || true)"
: "${UV:?uv not found on PATH — install it first: https://docs.astral.sh/uv/}"

APPS="$HOME/.local/share/applications"
UNITS="$HOME/.config/systemd/user"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"
mkdir -p "$APPS" "$UNITS" "$ICONS"

# Sync deps up front so the first service start is fast and offline.
( cd "$HERE" && "$UV" sync --frozen )

cat > "$ICONS/minimal-tts.svg" <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#171a21"/>
      <stop offset="1" stop-color="#0a0b0d"/>
    </linearGradient>
    <linearGradient id="bars" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0" stop-color="#818cf8"/>
      <stop offset=".5" stop-color="#c084fc"/>
      <stop offset="1" stop-color="#38bdf8"/>
    </linearGradient>
    <filter id="g" x="-30%" y="-30%" width="160%" height="160%">
      <feGaussianBlur stdDeviation="2.4" result="b"/>
      <feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
  </defs>
  <rect width="128" height="128" rx="28" fill="url(#bg)"/>
  <rect x=".5" y=".5" width="127" height="127" rx="27.5" fill="none" stroke="#fff" stroke-opacity=".07"/>
  <g fill="url(#bars)" filter="url(#g)">
    <rect x="22" y="46" width="12" height="36" rx="6"/>
    <rect x="46" y="30" width="12" height="68" rx="6"/>
    <rect x="70" y="40" width="12" height="48" rx="6"/>
    <rect x="94" y="52" width="12" height="24" rx="6"/>
  </g>
</svg>
SVG

cat > "$UNITS/minimal-tts.service" <<EOF
[Unit]
Description=Minimal TTS backend
After=default.target

[Service]
Type=simple
Environment=MINIMAL_TTS_NO_BROWSER=1
WorkingDirectory=$HERE
ExecStart=$UV run main.py
Restart=on-failure

[Install]
WantedBy=default.target
EOF

cat > "$APPS/minimal-tts.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Minimal TTS
Comment=Local read-aloud
Exec=$HERE/scripts/launch.sh
Icon=minimal-tts
Terminal=false
Categories=AudioVideo;Audio;Utility;
Keywords=tts;speech;read;kokoro;
StartupNotify=true
StartupWMClass=minimal-tts
EOF

chmod +x "$HERE/scripts/launch.sh"
systemctl --user daemon-reload
systemctl --user enable --now minimal-tts.service
update-desktop-database "$APPS" &>/dev/null || true
gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" &>/dev/null || true

echo "Installed. Launch “Minimal TTS” from your app menu."
echo "Prefer on-demand instead of always-on?  systemctl --user disable minimal-tts"
