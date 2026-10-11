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

## Speech-to-text is built into the binary

A media file with **no subtitle** is segmented by transcribing its audio. The goal is that
end users need no installs and no downloads: whisper.cpp is statically linked and the model
is embedded in the executable (`include_bytes!`), so one file does everything.

Build-machine prerequisites (NOT end-user requirements):

1. **cmake** — `whisper-rs-sys` compiles the vendored whisper.cpp via cmake. A user-local
   copy lives at `~/cmake/bin` and is added to `PATH` by `rust/scripts/cargo-shim.sh`
   (extract the official `cmake-*-linux-x86_64.tar.gz` there; no sudo needed).
2. **The model** (~60 MB, not committed — see `rust/.gitignore`):
   ```bash
   rust/scripts/fetch-asr-model.sh                 # ggml-base.en-q5_1.bin (default)
   rust/scripts/fetch-asr-model.sh ggml-tiny.en-q5_1.bin   # smaller/faster
   ```
   `build.rs` embeds `rust/assets/models/ggml-base.en-q5_1.bin`; override with
   `ASR_MODEL=<path>`. If absent, the build still succeeds and transcription then requires
   `--asr-model <path>` at runtime.

Resulting artifact: **~86 MB** (was ~25 MB without the model). `cargo build
--no-default-features` skips speech-to-text entirely for a fast, small, model-free build.

Runtime flags: `--asr-model <path>`, `--asr-language <code>` (default `en`).

### Word / cue timing accuracy

Word timings come from whisper.cpp's **DTW** token timestamps (`dtw = 1` in the startup
log), with the preset derived from the model file name (`ggml-base.en-*.bin` → `BaseEn`).
DTW alone is still ~0.2 s late at phrase boundaries, so two audio-driven corrections run
over the samples we already decoded (`asr::snap_word_starts_to_onsets`, 20 ms RMS envelope,
adaptive thresholds; `SENTENCE_VIDEO_ASR_DEBUG=1` reports the counts):

- **Starts** snap to the nearest sharp energy rise — a real onset. A breath ramps gently,
  so it is not mistaken for speech.
- **Ends** are trimmed to the **speech offset** when a pause follows, rather than being kept
  flush with the next word's start. Ending exactly on the next onset put the boundary on the
  first phoneme of the following sentence, so any playback overshoot voiced half that word.

Measured on a 4-minute video, cue ends that immediately voice the next word dropped from
**22/22 to 7/22**; the remaining 7 are boundaries with *no* pause, where the audio is
genuinely continuous. The player polls the media clock every 20 ms so it stops inside the
pause (a 60 ms timer allowed ~40 ms of overshoot).

`build_cues` has **no word-count cap** — cues split at sentence-final punctuation, so a
normal sentence stays whole (an earlier 12 s / 40-word cap sliced sentences mid-thought).
Whisper punctuates a long monologue with commas alone, though, so a "sentence" can run for
minutes — which neither the dubber (it rejects a clip past 60 s) nor a reader can use.
`segmenter::finalize` therefore cuts any cue past **`MAX_CUE_SECS` (20 s)** at the nearest
natural boundary (the same boundary machinery the platform exporter uses). Because both the
transcription path and the cached-`_full_sentence.srt` parse run through `finalize`, an
already-segmented video is fixed on re-open without re-transcribing.

### Which subtitle a session uses

`build_session` resolves the cue source in this order (see
`pipeline::transcribe::locate_subtitle`):

1. an explicit `subtitle_path` from the API/CLI,
2. **`<stem>_full_sentence.srt`** next to the media — e.g. `lecture.mp4` →
   `lecture_full_sentence.srt`,
3. otherwise **transcribe** — and the result is *saved as (2)*, so the next run reuses the
   file.

A legacy adjacent `<stem>.srt` / `<stem>.vtt` is deliberately **ignored**: the canonical
file is the single source of truth, so a missing `_full_sentence.srt` always triggers
transcription rather than silently adopting an unrelated subtitle. To use a hand-made
subtitle, pass it explicitly as `subtitle_path` (API) or `--subtitle` (CLI).

Consequences worth knowing:

- The first run on a subtitle-free video pays the transcription cost and writes an
  inspectable, editable `.srt` next to it; later runs just parse that file (no model load).
- Editing the generated `.srt` is honoured: the SQLite cache key includes the subtitle's
  mtime, so a changed file rebuilds the session.
- The DB cache is checked *before* transcribing, so re-selecting an unchanged video is
  instant. `POST /api/session?force=1` bypasses it.
- Sessions record `source_format = "asr"` when transcribed and `"srt"`/`"vtt"` when parsed
  from a file; the written file means a re-parse reports `"srt"`.

### Cargo registry note (temporary)

`rsproxy.cn` (the configured crates.io mirror in `~/.cargo/config.toml`) is unreachable, so
its `replace-with` line is currently commented out to use direct crates.io. A backup of the
original file is at `~/.cargo/config.toml.bak-dsh`. Restore it once the mirror recovers.

## Known pre-existing type errors (do not chase these)

`pnpm run build:web` first runs `tsc -b`, which currently reports 5 errors that predate
recent work:

- `src/features/assistant/index.tsx` — CopilotKit `labels.title` is not in the type
- `src/features/reading/index.tsx` — unused `e` parameter
- `src/features/subtitle/index.tsx` — unused `saving` / `setSaving`
- `src/providers/auth-context.tsx` — unused `REFRESH_KEY`

`vite build` on its own succeeds, and the dev server does not typecheck, so these do not
block running the app. Do not report them as new regressions.
