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
#   2. Sets git's low-speed timeouts so any other stalled transfer aborts in
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

# Abort a stalled transfer quickly instead of hanging.
export GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1000}"
export GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-30}"

exec cargo "$@"
