#!/usr/bin/env bash
# Build Minimal TTS as a self-contained Linux AppImage.
#
# Bundles the release binary, the fp32 model + voices, and espeak-ng (lib +
# data). Embeds GitHub-release zsync update info so Gear Lever / AppImageUpdate
# can auto-update. Usage: scripts/build-appimage.sh [output.AppImage]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")" # native/
cd "$root"

OUT="${1:-$root/dist/Minimal_TTS-x86_64.AppImage}"
mkdir -p "$(dirname "$OUT")"
WORK="$(mktemp -d)"
AD="$WORK/AppDir"
trap 'rm -rf "$WORK"' EXIT

# prerequisites
[ -x target/release/minimal-tts ] || cargo build --release
[ -f models/kokoro.onnx ] || ./scripts/get-models.sh
[ -f vendor/libespeak-ng.so ] || { echo "vendor/libespeak-ng.so missing" >&2; exit 1; }

# assemble AppDir with the exe-relative layout the app resolves
mkdir -p "$AD/usr/bin/vendor" "$AD/usr/bin/models" \
         "$AD/usr/share/icons/hicolor/scalable/apps" "$AD/usr/share/applications"
cp target/release/minimal-tts "$AD/usr/bin/"
cp -r vendor/. "$AD/usr/bin/vendor/"
cp models/kokoro.onnx models/voices-v1.0.bin "$AD/usr/bin/models/"
cp assets/minimal-tts.svg "$AD/minimal-tts.svg"
cp assets/minimal-tts.svg "$AD/usr/share/icons/hicolor/scalable/apps/minimal-tts.svg"
cp assets/minimal-tts.desktop "$AD/minimal-tts.desktop"
cp assets/minimal-tts.desktop "$AD/usr/share/applications/minimal-tts.desktop"
cat > "$AD/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export APPDIR="$HERE"
exec "$HERE/usr/bin/minimal-tts" "$@"
EOF
chmod +x "$AD/AppRun"

# appimagetool (reuse $APPIMAGETOOL if set, else fetch the continuous build)
TOOL="${APPIMAGETOOL:-$WORK/appimagetool}"
if [ ! -x "$TOOL" ]; then
  curl -L -f --retry 3 -o "$TOOL" \
    https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "$TOOL"
fi

UPD="gh-releases-zsync|alirezazd|minimal-tts|latest|Minimal_TTS-*-x86_64.AppImage.zsync"
VERSION="${VERSION:-dev}" ARCH=x86_64 "$TOOL" --appimage-extract-and-run -u "$UPD" "$AD" "$OUT"
echo "built: $OUT"
