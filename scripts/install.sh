#!/usr/bin/env bash
# Install Minimal TTS as a desktop app: a launcher icon that starts the server,
# opens the window, and tears the server down again when you close it.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UV="$(command -v uv || true)"
[ -n "$UV" ] || {
  echo "uv not found on PATH — install it first: https://docs.astral.sh/uv/" >&2
  exit 1
}

APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"
mkdir -p "$APPS" "$ICONS"

# Sync deps up front so the first launch is fast and offline.
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

cat > "$APPS/minimal-tts.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Minimal TTS
Comment=Local read-aloud
Exec=$UV run --directory $HERE main.py
Icon=minimal-tts
Terminal=false
Categories=AudioVideo;Audio;Utility;
Keywords=tts;speech;read;kokoro;
StartupNotify=true
StartupWMClass=minimal-tts
EOF

update-desktop-database "$APPS" &>/dev/null || true
gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" &>/dev/null || true

echo "Installed. Launch “Minimal TTS” from your app menu."
