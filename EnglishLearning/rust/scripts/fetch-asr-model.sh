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

# Hugging Face endpoint. Defaults to the China mirror, which is reachable here
# while huggingface.co often is not. Override with HF_ENDPOINT if you prefer the
# origin (or another mirror).
HF_ENDPOINT="${HF_ENDPOINT:-https://hf-mirror.com}"

model="${1:-ggml-base.en-q5_1.bin}"
repo="${ASR_MODEL_REPO:-ggerganov/whisper.cpp}"
subpath="${ASR_MODEL_PATH_IN_REPO:-}"
dest_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/assets/models"
dest="$dest_dir/$(basename "$model")"
url="$HF_ENDPOINT/$repo/resolve/main/${subpath:+$subpath/}$model"

mkdir -p "$dest_dir"
echo "fetching $model → $dest"
echo "  via $HF_ENDPOINT"

# Determine the exact byte size. Neither HEAD nor a ranged GET reports the real
# length through the mirrors (both answer with a redirect stub), which would let
# a truncated download look complete. The HF tree API returns real sizes,
# including for LFS files.
total=$( { curl -sSL "$HF_ENDPOINT/api/models/$repo/tree/main${subpath:+/$subpath}" 2>/dev/null || true; } \
  | WANT="$model" python3 -c '
import json, os, sys
want = os.environ["WANT"]
try:
    for entry in json.load(sys.stdin):
        if entry.get("path") == want:
            print(entry.get("size", 0))
            break
    else:
        print(0)
except Exception:
    print(0)
' 2>/dev/null )
total=${total:-0}
echo "  size: ${total:-unknown} bytes"

for attempt in $(seq 1 60); do
  size=$(stat -c%s "$dest" 2>/dev/null || echo 0)
  if [ "$total" -gt 0 ] && [ "$size" -eq "$total" ]; then
    echo "done: $model ($((size / 1000000)) MB)"
    exit 0
  fi
  # --continue-at - resumes from whatever we already have.
  timeout 120 curl -sSfL -C - -o "$dest" "$url" 2>/dev/null || true
  echo "  attempt $attempt: $(stat -c%s "$dest" 2>/dev/null || echo 0) / ${total:-?} bytes"
done

size=$(stat -c%s "$dest" 2>/dev/null || echo 0)
if [ "$total" -gt 0 ] && [ "$size" -eq "$total" ]; then
  echo "done: $model ($((size / 1000000)) MB)"
  exit 0
fi
echo "error: could not finish downloading $model ($size / $total bytes)" >&2
exit 1
