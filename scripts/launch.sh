#!/usr/bin/env bash
# Open Minimal TTS as a chromeless Chrome app window, starting the backend if needed.
set -euo pipefail

PORT="${MINIMAL_TTS_PORT:-8765}"
URL="http://127.0.0.1:${PORT}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="$HERE/.chrome-profile"

port_open() { (echo >"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; }

# Bring up the backend: the user service if installed, else a detached process.
if systemctl --user cat minimal-tts.service &>/dev/null; then
  systemctl --user start minimal-tts.service
elif ! port_open; then
  setsid bash -c "cd '$HERE' && MINIMAL_TTS_NO_BROWSER=1 exec uv run main.py" \
    >/dev/null 2>&1 </dev/null &
fi

# Wait up to ~20s for the port to accept connections.
for _ in $(seq 1 100); do port_open && break; sleep 0.2; done

for b in google-chrome google-chrome-stable chromium chromium-browser brave-browser microsoft-edge; do
  if command -v "$b" &>/dev/null; then CHROME="$(command -v "$b")"; break; fi
done
: "${CHROME:?No Chrome/Chromium found on PATH}"

exec "$CHROME" --app="$URL" --user-data-dir="$PROFILE" \
  --class=minimal-tts --no-first-run --no-default-browser-check --password-store=basic
