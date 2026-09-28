#!/usr/bin/env bash
#
# Fetch the wav2vec2 forced-alignment weights used by `sentence-video align`.
#
# Unlike the speech-to-text model (`fetch-asr-model.sh`), which `build.rs` embeds
# into the executable, these weights stay on disk and are loaded at runtime when
# the `asr-align` feature is compiled in. They are ~378 MB, so they are not
# committed: `rust/.gitignore` ignores all of `assets/models/`. Run this once on
# any machine that needs the aligner.
#
# Resumable: this network kills long transfers, so a partial download is
# continued rather than restarted.
#
# Usage:
#   rust/scripts/fetch-align-model.sh
#
# Result: assets/models/wav2vec2-base-960h.safetensors — the first place
# `align::default_weights_path` looks. Point elsewhere with `--align-model
# <path>` or SENTENCE_VIDEO_ALIGN_MODEL.

set -euo pipefail

# Hugging Face endpoint. Defaults to the China mirror, which is reachable here
# while huggingface.co often is not. Override with HF_ENDPOINT if you prefer the
# origin (or another mirror).
HF_ENDPOINT="${HF_ENDPOINT:-https://hf-mirror.com}"

repo="${ALIGN_MODEL_REPO:-facebook/wav2vec2-base-960h}"
# The file in the repo is `model.safetensors`; the name on disk below is what
# `align::WEIGHTS_FILE` expects.
remote="${ALIGN_MODEL_PATH_IN_REPO:-model.safetensors}"
dest_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/assets/models"
dest="$dest_dir/wav2vec2-base-960h.safetensors"
url="$HF_ENDPOINT/$repo/resolve/main/$remote"

mkdir -p "$dest_dir"
echo "fetching $repo/$remote → $dest"
echo "  via $HF_ENDPOINT"

# Determine the exact byte size. Neither HEAD nor a ranged GET reports the real
# length through the mirrors (both answer with a redirect stub), which would let
# a truncated download look complete. The HF tree API returns real sizes,
# including for LFS files.
total=$( { curl -sSL "$HF_ENDPOINT/api/models/$repo/tree/main" 2>/dev/null || true; } \
  | WANT="$remote" python3 -c '
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
    echo "done: $(basename "$dest") ($((size / 1000000)) MB)"
    exit 0
  fi
  # --continue-at - resumes from whatever we already have.
  timeout 120 curl -sSfL -C - -o "$dest" "$url" 2>/dev/null || true
  echo "  attempt $attempt: $(stat -c%s "$dest" 2>/dev/null || echo 0) / ${total:-?} bytes"
done

size=$(stat -c%s "$dest" 2>/dev/null || echo 0)
if [ "$total" -gt 0 ] && [ "$size" -eq "$total" ]; then
  echo "done: $(basename "$dest") ($((size / 1000000)) MB)"
  exit 0
fi
echo "error: could not finish downloading $(basename "$dest") ($size / $total bytes)" >&2
exit 1