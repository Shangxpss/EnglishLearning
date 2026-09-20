#!/usr/bin/env bash
#
# Cargo wrapper that keeps builds working on this machine's flaky network.
#
# WHY THIS EXISTS
#   `ffmpeg-sys-next`'s build script downloads FFmpeg with:
#       git clone --depth=1 -b release/6.1 https://github.com/FFmpeg/FFmpeg
#   GitHub is unreachable from here, so cargo blocks on the socket for ~2
#   minutes per attempt and then fails with "fetch failed". Because the build
#   script re-runs whenever FFmpeg's unit hash changes (e.g. after adding a
#   crate that shares a build-dependency such as `cc`), this can come back at
#   any time -- so it must be handled automatically, not manually.
#
# WHAT IT DOES
#   1. Puts the offline git shim first on PATH when the FFmpeg source tarball
#      is available, so that ONE clone is served from the local tarball.
#   2. Puts a user-local cmake (~/cmake) on PATH: whisper.cpp is compiled by
#      `whisper-rs-sys` via cmake, and cmake is not installed system-wide here.
#   3. Sets git's low-speed timeouts so any other stalled transfer aborts in
#      ~30s instead of hanging for minutes.
#
# NOTE: `cargo build --offline` does NOT prevent this, because the clone is
# spawned by the build script, not by cargo itself.
#
# Usage (see package.json): scripts/cargo-shim.sh <cargo args...>

set -euo pipefail

shim_dir="${FFMPEG_GIT_SHIM:-$HOME/git-shim}"
tarball="${FFMPEG_TARBALL:-$HOME/ffmpeg-tarballs/ffmpeg-6.1.tar.xz}"

if [ -x "$shim_dir/git" ] && [ -f "$tarball" ]; then
  export PATH="$shim_dir:$PATH"
else
  echo "cargo-shim: WARNING: offline FFmpeg shim unavailable." >&2
  echo "  shim:    $shim_dir/git $([ -x "$shim_dir/git" ] && echo '(ok)' || echo '(MISSING)')" >&2
  echo "  tarball: $tarball $([ -f "$tarball" ] && echo '(ok)' || echo '(MISSING)')" >&2
  echo "  If cargo stalls on 'git clone .../FFmpeg', see AGENTS.md." >&2
fi

# The default build embeds speech-to-text (whisper.cpp), whose build script
# drives cmake. Prefer a user-local cmake so no sudo install is needed.
if [ -d "$HOME/cmake/bin" ]; then
  export PATH="$HOME/cmake/bin:$PATH"
elif ! command -v cmake >/dev/null 2>&1; then
  echo "cargo-shim: WARNING: cmake not found — whisper.cpp cannot build." >&2
  echo "  Extract a user-local cmake into ~/cmake, or build without speech-to-text:" >&2
  echo "      cargo build --no-default-features" >&2
fi

# Abort a stalled transfer quickly instead of hanging.
export GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1000}"
export GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-30}"

exec cargo "$@"
