#!/usr/bin/env bash
#
# Fetch the speech-to-text model that gets embedded into the executable.
#
# The model is ~60 MB and is deliberately NOT committed to git. Run this once
# before building; `build.rs` then bakes it into the binary so end users need
# no installs and no downloads.
#
# Resumable: this network kills long transfers, so a partial download is
# continued rather than restarted.
#
# Usage:
#   rust/scripts/fetch-asr-model.sh [model-name]
#
#   model-name  ggml-*.bin from ggerganov/whisper.cpp (default: base.en-q5_1)
#
# Choose a different size/accuracy trade-off, e.g.:
#   ggml-tiny.en-q5_1.bin   (~32 MB, fastest, least accurate)
#   ggml-base.en-q5_1.bin   (~60 MB, default)
#   ggml-small.en-q5_1.bin  (~190 MB, most accurate of the three, slower)

set -euo pipefail

model="${1:-ggml-base.en-q5_1.bin}"
repo="${ASR_MODEL_REPO:-ggerganov/whisper.cpp}"
dest_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/assets/models"
dest="$dest_dir/$model"
url="https://huggingface.co/$repo/resolve/main/$model"

mkdir -p "$dest_dir"
echo "fetching $model → $dest"

total=$(curl -sSIL "$url" 2>/dev/null | awk 'BEGIN{IGNORECASE=1} /^content-length:/{v=$2} END{gsub(/\r/,"",v); print v+0}')

for attempt in $(seq 1 60); do
  size=$(stat -c%s "$dest" 2>/dev/null || echo 0)
  if [ "$total" -gt 0 ] && [ "$size" -ge "$total" ]; then
    echo "done: $model ($((size / 1000000)) MB)"
    exit 0
  fi
  # --continue-at - resumes from whatever we already have.
  timeout 120 curl -sSfL -C - -o "$dest" "$url" 2>/dev/null || true
  echo "  attempt $attempt: $(stat -c%s "$dest" 2>/dev/null || echo 0) / ${total:-?} bytes"
done

size=$(stat -c%s "$dest" 2>/dev/null || echo 0)
if [ "$total" -gt 0 ] && [ "$size" -ge "$total" ]; then
  echo "done: $model ($((size / 1000000)) MB)"
  exit 0
fi
echo "error: could not finish downloading $model" >&2
exit 1
