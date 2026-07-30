#!/usr/bin/env bash
# Download the Kokoro model + voice vectors (timestamped ONNX export).
# Fetches the fp32 model (~325 MB). Smaller variants were tried and rejected:
# fp16 NaNs out on blended style vectors (silent audio), q8f16 segfaults,
# int8 runs 4x slower than fp32 on CPU.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/models"
mkdir -p "$DIR"
REPO="https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX-timestamped/resolve/main/onnx"

fetch() {
  local url="$1" out="$2"
  if [ -s "$out" ]; then
    echo "already present: $(basename "$out")"
    return
  fi
  echo "downloading $(basename "$out") …"
  curl -L -f --retry 3 --progress-bar -o "$out" "$url"
}

fetch "$REPO/model.onnx" "$DIR/kokoro.onnx"
fetch "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin" \
      "$DIR/voices-v1.0.bin"

echo "done: $(du -sh "$DIR" | cut -f1) in $DIR"
