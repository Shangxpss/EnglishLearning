# Rust Build Issues: ffmpeg-sys-next (sentence-video) — Problems & Solutions

Date: 2026-09-19
Scope: building `EnglishLearning/rust` (crate `sentence-video`) on Linux Mint 22.2 (Ubuntu noble base, no sudo available, unstable network that kills long TLS transfers).

## FINAL RESOLUTION (2026-09-19)

The machine now has proper system packages, so **all build-script workarounds are removed**:

```bash
sudo apt install -y nasm libclang1-14t64 libllvm14t64 libclang-common-14-dev
```

- `libclang1-14t64` + `libllvm14t64` + `libclang-common-14-dev` install libclang **and its builtin headers at the compiled-in path** `/usr/lib/llvm-14/lib/clang/14.0.6/include` — bindgen then works with zero environment variables
- `nasm` covers future FFmpeg rebuilds
- Removed afterwards: `LD_LIBRARY_PATH` from `package.json` scripts, and the whole `[env]` section from `rust/.cargo/config.toml` (now kept only as comments documenting the packages)
- `rust/target/` still caches the compiled FFmpeg (`build/ffmpeg-sys-next-*/out/dist/`), so even a build-script rerun does not touch the network
- `~/git-shim` + `~/ffmpeg-tarballs` kept as an offline fallback in case `rust/target` is ever deleted on this bad network

Verified: `cargo build` passes in a completely clean environment (no `LIBCLANG_PATH` / `BINDGEN_EXTRA_CLANG_ARGS` / `LD_LIBRARY_PATH`), including a forced build-script rerun.

Everything below documents how each problem was originally solved WITHOUT sudo — useful for machines where you can't install packages.

---

The dependency chain that caused every problem below:

```
ffmpeg-next (Rust)
└── ffmpeg-sys-next (Rust binding crate)
    ├── build step A: git clone FFmpeg 6.1 source, then configure + make  → needs git, network, nasm, gcc
    └── build step B: bindgen generates Rust bindings from FFmpeg headers → needs libclang (+ libLLVM, + clang builtin headers)
```

---

## Problem 1 — `git clone` of FFmpeg source dies mid-transfer

**Symptom** (from `ffmpeg-sys-next` build script):

```
Cloning into 'ffmpeg-6.1'...
error: RPC failed; curl 56 Recv failure: Connection timed out
fatal: early EOF / fetch-pack: unexpected disconnect
thread 'main' panicked at build.rs:656: "fetch failed"
```

The network here drops long-running TLS streams (git's sideband packfile transfer), regardless of host — GitHub **and** the gitee mirror both failed. Small requests (`ls-remote`) worked; the ~60 MB pack never finished.

**Solution used** (no sudo, resumable HTTP instead of git transport):

1. Download the source tarball with curl resume (`-C -` in a retry loop — each attempt makes progress even though connections drop):
   ```bash
   curl -L -C - -o ~/ffmpeg-tarballs/ffmpeg-6.1.tar.xz https://ffmpeg.org/releases/ffmpeg-6.1.tar.xz
   ```
2. Create a **git shim** that fakes only the FFmpeg clone by extracting the tarball, and passes everything else to real git (`~/git-shim/git`, chmod +x):
   ```bash
   #!/bin/bash
   url_found=0
   for arg in "$@"; do
     case "$arg" in
       https://github.com/FFmpeg/FFmpeg) url_found=1 ;;
     esac
   done
   if [ "$url_found" = "1" ]; then
     dest="${@: -1}"
     rm -rf "$dest"; mkdir -p "$dest"
     tar -xJf "$HOME/ffmpeg-tarballs/ffmpeg-6.1.tar.xz" -C "$dest" --strip-components=1
     exit 0
   fi
   exec /usr/bin/git "$@"
   ```
3. Run the build with the shim first in `PATH`:
   ```bash
   PATH=~/git-shim:$PATH cargo build
   ```
   The build script clones "from github" but actually gets the extracted tarball, then proceeds to configure/make normally. (A `url.<mirror>.insteadOf` git rewrite via `GIT_CONFIG_*` env vars was tried first; it did not help because the failure was transport-level, not DNS/routing.)

**Would good network fix this?** YES — completely. This problem is purely network. With normal connectivity `cargo build` just works. The shim/tarball workaround is only for hostile networks.

**Generic technique**: any build script that shells out to `git clone <url>` can be satisfied by a PATH shim (or by an `url.X.insteadOf` rewrite pointing at a local mirror). The build script only checks the clone's exit status.

---

## Problem 2 — FFmpeg `configure` fails: `nasm` not found

**Symptom**: FFmpeg's configure aborts because the assembler (x86 SIMD) is missing.

**Solution used** (no sudo): download the distro `nasm` deb, unpack to a user dir, prepend to PATH:

```bash
apt download nasm && dpkg -x nasm_*.deb ~/nasm-local
PATH=~/nasm-local/usr/bin:$PATH cargo build
```

**Would good network fix this?** NO — the package is simply not installed; this happens on any network. With sudo it is one command: `sudo apt install nasm`. (nasm is only needed because FFmpeg is compiled from source.)

---

## Problem 3 — bindgen: `Unable to find libclang`

**Symptom**:

```
thread 'main' panicked at bindgen-0.64.0/lib.rs:2393:
Unable to find libclang: "couldn't find any valid shared libraries matching:
['libclang.so', 'libclang-*.so', ...], set the LIBCLANG_PATH environment variable"
```

Any crate using `bindgen` in its build script (all `*-sys` crates with C headers) hits this if clang is not installed.

**Solution used** (no sudo — extract the debs to a user tree):

```bash
apt download libclang1-14t64          # tar.gz bundle containing the debs
tar xzf libclang1-14t64.tar.gz        # extracts a SUBDIRECTORY with debs inside
dpkg -x libclang1-14t64/*.deb ~/clang-local
# bindgen looks for names like 'libclang.so' / 'libclang-*.so':
ln -s <found>/libclang-14.so.1 <found>/libclang.so
export LIBCLANG_PATH=~/clang-local/usr/lib/llvm-14/lib
```

Note: on Ubuntu noble the package is renamed by the 64-bit-time_t transition — `libclang1-14t64`, and it pulls `libllvm14t64` (30 MB bundle).

**Would good network fix this?** NO — missing system package. With sudo: `sudo apt install libclang-dev` (or `clang`), then nothing else is needed.

---

## Problem 4 — libclang found, but its own dependency `libLLVM-14.so.1` missing

**Symptom**:

```
Unable to find libclang: "the `libclang` shared library at ~/clang-local/.../libclang-14.so.1
could not be opened: libLLVM-14.so.1: cannot open shared object file"
```

**Solution used**: export `LD_LIBRARY_PATH` pointing at the dir holding the extracted `libLLVM-14.so.1`:

```bash
export LD_LIBRARY_PATH=$HOME/clang-local/usr/lib/x86_64-linux-gnu
```

**Important cargo gotcha**: this CANNOT be put in `.cargo/config.toml`'s `[env]` — cargo overrides `LD_LIBRARY_PATH` for build-script processes. `LIBCLANG_PATH` and `BINDGEN_EXTRA_CLANG_ARGS` do work via `[env]`, `LD_LIBRARY_PATH` must be exported by the shell (we put it in the pnpm scripts).

**Would good network fix this?** NO — consequence of the user-dir extraction in Problem 3. With a system install (`sudo apt install libclang-dev`) the loader finds libLLVM automatically.

---

## Problem 5 — bindgen: `/usr/include/limits.h:124: fatal error: 'limits.h' file not found`

**Symptom**: libclang loads, but parsing headers fails on `#include_next <limits.h>` inside glibc's `/usr/include/limits.h`.

**Root cause**: libclang locates its _builtin_ headers (clang resource dir, e.g. `clang/14.0.6/include/limits.h`) via a path **compiled into the library** — here `/usr/lib/llvm-14/lib/clang/14.0.6`, which doesn't exist for an extracted user tree. Without the builtin `limits.h`, the `_GCC_LIMITS_H_` guard never gets defined and glibc's `#include_next` falls off the end of the search path. (Installing `libclang-common-14-dev` alone into the user tree did NOT fix it, because the compiled-in absolute path still doesn't exist.)

**Solution used**: inject the extracted builtin-include dir through bindgen's official escape hatch:

```bash
export BINDGEN_EXTRA_CLANG_ARGS="-I$HOME/clang-local/usr/lib/llvm-14/lib/clang/14.0.6/include"
```

This makes clang's `limits.h` the first one found; it defines `_GCC_LIMITS_H_`, so glibc's wrapper skips its `include_next` and parsing succeeds. Could not use a symlink at the compiled-in path because `/usr/lib/llvm-14` needs root (sudo asked for a password).

**Would good network fix this?** NO — same class as Problems 3/4: a consequence of no-sudo extraction. With a system clang install the compiled-in resource path exists and this never appears.

---

## Final working configuration (kept in the repo / home dir)

| File                                   | Purpose                                                                                             |
| -------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `rust/.cargo/config.toml` `[env]`      | `LIBCLANG_PATH`, `BINDGEN_EXTRA_CLANG_ARGS` — applies to cargo builds AND rust-analyzer in the IDE  |
| `package.json` scripts                 | `LD_LIBRARY_PATH=$HOME/clang-local/usr/lib/x86_64-linux-gnu` exported before every cargo invocation |
| `~/git-shim/git`, `~/ffmpeg-tarballs/` | only needed for the FIRST FFmpeg source build                                                       |
| `~/nasm-local/`                        | only needed for the FIRST FFmpeg source build                                                       |

After one successful build everything is cached under `rust/target/`:

- `build/ffmpeg-sys-next-*/out/dist/lib/*.a` — compiled FFmpeg static libs; the build script skips clone+make entirely once `libavutil.a` exists (so later builds are fully offline)
- bindgen output and all Rust artifacts — incremental

## TL;DR for other Rust packages

Crates named `<lib>-sys` typically: (1) download/build the C library in a build script, (2) generate bindings with bindgen. The same playbook applies:

1. **Network failure during C-library download** → replace the download: tarball + PATH shim, or `url.insteadOf` rewrite to a local mirror. Purely a network problem.
2. **Missing build tools** (`nasm`, `make`, `cmake`, ...) → `apt install` if you have sudo; otherwise `apt download` + `dpkg -x` to a user dir and prepend to `PATH`. Not a network problem.
3. **bindgen/libclang** → needs `libclang` + `libLLVM` + clang builtin headers. With sudo: `sudo apt install libclang-dev` and done. Without sudo: extract debs to a user dir, symlink `libclang.so`, set `LIBCLANG_PATH` + `LD_LIBRARY_PATH` + `BINDGEN_EXTRA_CLANG_ARGS=-I<clang-resource-include>` (put the first and third in `.cargo/config.toml [env]`; the second must be a shell export).
4. Once built, `target/` caches everything — subsequent builds are offline and fast.
