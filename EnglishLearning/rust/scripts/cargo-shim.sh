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

# Fail fast, and treat an unset variable or a broken pipe as an error: this
# wrapper must never swallow a setup mistake and then hand cargo a half-built
# environment.
set -euo pipefail

# Where the offline git shim lives, and the tarball it serves the one
# unreachable `git clone .../FFmpeg` from. Both are overridable, so a machine
# that keeps them elsewhere (or a test) needs no edit here.
shim_dir="${FFMPEG_GIT_SHIM:-$HOME/git-shim}"
tarball="${FFMPEG_TARBALL:-$HOME/ffmpeg-tarballs/ffmpeg-6.1.tar.xz}"

# Arm the shim only when BOTH halves are present: a shim with no tarball behind
# it would break the clone rather than pass it through to real git.
if [ -x "$shim_dir/git" ] && [ -f "$tarball" ]; then
  # First on PATH, so the FFmpeg build script's `git clone` resolves to the
  # shim. Every other URL still reaches real git — the shim `exec`s it.
  export PATH="$shim_dir:$PATH"
else
  # Not fatal: a cached FFmpeg under target/ means no clone happens at all. But
  # if cargo later stalls on the clone, this warning is the explanation.
  echo "cargo-shim: WARNING: offline FFmpeg shim unavailable." >&2
  # Name which half is missing, so the fix is unambiguous.
  echo "  shim:    $shim_dir/git $([ -x "$shim_dir/git" ] && echo '(ok)' || echo '(MISSING)')" >&2
  echo "  tarball: $tarball $([ -f "$tarball" ] && echo '(ok)' || echo '(MISSING)')" >&2
  echo "  If cargo stalls on 'git clone .../FFmpeg', see AGENTS.md." >&2
fi

# The default build embeds speech-to-text (whisper.cpp), whose build script
# drives cmake. Prefer a user-local cmake so no sudo install is needed.
if [ -d "$HOME/cmake/bin" ]; then
  # Ahead of any system cmake, so the vendored whisper.cpp build uses the
  # version this machine was set up with.
  export PATH="$HOME/cmake/bin:$PATH"
elif ! command -v cmake >/dev/null 2>&1; then
  # No user-local copy AND none on PATH: the whisper build will fail, so report
  # it here rather than letting the error surface from inside cmake.
  echo "cargo-shim: WARNING: cmake not found — whisper.cpp cannot build." >&2
  echo "  Extract a user-local cmake into ~/cmake, or build without speech-to-text:" >&2
  echo "      cargo build --no-default-features" >&2
fi

# Stalled-transfer guard: git aborts a download once throughput stays under
# GIT_HTTP_LOW_SPEED_LIMIT bytes/s for GIT_HTTP_LOW_SPEED_TIME seconds. Without
# it, any *other* unreachable host hangs for minutes instead of ~30 s. Both are
# overridable if a slower link needs higher values.
export GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1000}"
export GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-30}"

# Hand over to the real cargo, replacing this shell: the exit code and signals
# are then cargo's own, with no extra process left in the chain.
exec cargo "$@"
