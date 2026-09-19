# AGENTS.md

Guidance for AI agents working in this repository.

## ⚠️ This machine cannot reach GitHub — the FFmpeg build will hang

**Symptom**

`pnpm run build:server` (or any raw `cargo build` of `EnglishLearning/rust`) appears to
hang for ~11 minutes, then fails:

```
Cloning into 'ffmpeg-6.1'...
fatal: unable to access 'https://github.com/FFmpeg/FFmpeg/':
  Failed to connect to github.com port 443 after 135587 ms: Couldn't connect to server
thread 'main' panicked at ffmpeg-sys-next-6.1.0/build.rs:656:
  called `Result::unwrap()` on an `Err` value: ... "fetch failed"
```

**Cause**

`ffmpeg-sys-next` (a dependency of `rust/`) is used with the `build` feature, so its build
script fetches FFmpeg with:

```
git clone --depth=1 -b release/6.1 https://github.com/FFmpeg/FFmpeg
```

GitHub is unreachable from this machine, so cargo blocks on the socket until it times out.

This only runs when FFmpeg's build-script unit is rebuilt:

- after `rm -rf rust/target` / `cargo clean`;
- the **first** build of a profile that has no cached FFmpeg (e.g. release when only debug
  has been built);
- when a dependency change alters FFmpeg's build-script hash. Observed with `rusqlite`,
  because `libsqlite3-sys` also depends on `cc`, which changed `cc`'s unified features.

**Most crate additions do NOT trigger it** — adding a plain crate (e.g. `itoa`) left
FFmpeg `Fresh`. So do not assume a new dependency caused a hang; check the process tree.

**Do NOT**

- Do not just wait: each attempt dies after ~2 minutes and the build fails.
- Do not re-run the same command hoping it gets further.
- `cargo build --offline` does **not** help — the clone is spawned by the build script,
  not by cargo, so `--offline` has no effect on it.
- Do not `rm -rf rust/target` to "fix" it; that guarantees a rebuild.

**DO — the fix**

An offline shim already exists: `~/git-shim/git` answers exactly that one clone by
extracting `~/ffmpeg-tarballs/ffmpeg-6.1.tar.xz`, and passes everything else to real git.

`rust/scripts/cargo-shim.sh` puts it on `PATH` automatically and adds git low-speed
timeouts. **`package.json` already routes `build:server` and `dev:server` through it**, so
the normal commands are safe. For a raw cargo invocation, use:

```bash
PATH="$HOME/git-shim:$PATH" cargo build --release --manifest-path rust/Cargo.toml
```

With the shim, FFmpeg compiles once per profile (~3 min) and then everything is cached
(sub-second builds).

**Diagnose in seconds**

```bash
# Is a git clone to GitHub stuck under cargo?
ps -eo pid,ppid,etime,args | grep -E '[c]argo|[g]it clone'
ss -tnp | grep cargo          # CLOSE-WAIT / SYN-SENT to :443 == network stall, not compiling
```

If there is no `cc1`/`rustc` child and cargo has been running for minutes, it is a network
stall — kill it and use the shim.

**If the shim or tarball is missing**

Both `~/git-shim/git` and `~/ffmpeg-tarballs/ffmpeg-6.1.tar.xz` must exist. Re-download the
tarball with a resumable loop (details in `EnglishLearning/build_issue.md`, "Problem 1"):

```bash
curl -L -C - -o ~/ffmpeg-tarballs/ffmpeg-6.1.tar.xz https://ffmpeg.org/releases/ffmpeg-6.1.tar.xz
```

`EnglishLearning/build_issue.md` documents the full FFmpeg/bindgen history and why the shim
was dropped from `PATH` in the first place.

## Build & run

```bash
pnpm run build:server   # rust release binary (via the shim wrapper)
pnpm run build:web      # vite production build
pnpm run dev:server     # rust dev server on :8018
pnpm run dev:web        # vite dev server on :5173 (proxies /api -> :8018)
pnpm run dev            # both
```

Processed sessions are persisted in SQLite at `~/.local/share/sentence-video/sessions.db`
(override with `--data-dir <path>`); re-opening the same media reuses the cached session.

## Known pre-existing type errors (do not chase these)

`pnpm run build:web` first runs `tsc -b`, which currently reports 5 errors that predate
recent work:

- `src/features/assistant/index.tsx` — CopilotKit `labels.title` is not in the type
- `src/features/reading/index.tsx` — unused `e` parameter
- `src/features/subtitle/index.tsx` — unused `saving` / `setSaving`
- `src/providers/auth-context.tsx` — unused `REFRESH_KEY`

`vite build` on its own succeeds, and the dev server does not typecheck, so these do not
block running the app. Do not report them as new regressions.
