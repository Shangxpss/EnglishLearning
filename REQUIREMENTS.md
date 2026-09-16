# Requirements: Sentence-Segmented Video Player (Rust-Only)

> Cross-platform desktop app that splits a video/audio file into sentence-level
> segments. The user clicks any sentence in the UI and the player jumps to
> (and optionally loops) that exact sentence. The entire application is
> **pure Rust**, built from one crate and compiled into a **single standalone
> executable** (`.exe` on Windows, a single binary on macOS/Linux). The UI is
> the user's default web browser, embedded as static assets and served by an
> in-process HTTP server that the binary starts and opens automatically.

***

## 1. Product Overview

### 1.1 Goal

A single compiled binary (e.g. `sentence-video.exe serve <file>`, or simply
double-clicking the executable) that:

1. Splits the source media into **sentence-by-sentence** segments with
   sample-accurate start/end timestamps.
2. Opens the user's **default browser** at the running URL so no native UI
   toolkit is required.
3. Renders a page listing every sentence; clicking a sentence plays the
   corresponding video/audio slice.

Everything — media decode/probe, resampling, segmentation, the HTTP server,
asset serving, and browser launching — runs **in one compiled Rust program**.

### 1.2 Why this design

* **Cross-platform without Electron/Tauri weight** — the OS browser is the UI,
  and the whole app is a single Rust binary with no Python runtime, no
  interpreter, and no user-installed FFmpeg.

* **Rust for the entire pipeline** — media I/O uses `ffmpeg-next` (FFmpeg
  bindings) in-process; there is no PyAV, no `ffprobe` subprocess, and no
  Python orchestrator. The crate is a normal executable, not a `.so` extension.

* **One artifact to ship** — a single compiled executable per platform, easy to
  bundle and run anywhere.

* **Reuses existing Rust work** — the media layer already exists as the
  `english_media_native` crate (see
  [`native/rust`](file:///workspace/EnglishLearning/backend/native/rust)). Its
  `duration`, `decode_audio`, and `mux_video_audio` functions are the core of
  this app's media I/O and are promoted/kept and reused by the binary build.

***

## 2. Current Code Baseline (what exists and can be reused)

| Capability                                      | Existing location                                                                                  | Reuse plan in the Rust binary                                                                        |
| ----------------------------------------------- | -------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Rust media layer (ffmpeg-next)                  | [`native/rust/src/lib.rs`](file:///workspace/EnglishLearning/backend/native/rust/src/lib.rs)       | Canonical media I/O: `duration`, `decode_audio`, `mux_video_audio`. Used by the binary directly.     |
| Rust media deps (`ffmpeg-next`, PyO3)           | [`native/rust/Cargo.toml`](file:///workspace/EnglishLearning/backend/native/rust/Cargo.toml)        | Re-frame as a library (`lib` + `bin`) so the same code builds as both extension and executable.      |
| Python wrapper over the Rust extension          | [`av_utils.py`](file:///workspace/EnglishLearning/backend/app/services/sync/av_utils.py)           | Python path is **frozen/retired**; the logic moves natively into Rust. Used only as reference.       |
| Sentence/word data models                       | [`models.py`](file:///workspace/EnglishLearning/backend/app/services/sync/models.py)               | Port the data shapes (`Cue { index, start, end, text, words[] }`) to Rust `struct`s.                 |
| Sentence grouping logic (reference)             | [`cue_builder.py`](file:///workspace/EnglishLearning/backend/app/services/sync/cue_builder.py)     | Port the split/merge heuristics to Rust.                                                             |
| Subtitle pipeline (reference)                   | [`subtitle_writer.py`](file:///workspace/EnglishLearning/backend/app/services/sync/subtitle_writer.py) | Port post-processing to Rust so `.srt`/`.vtt` input needs no Python.                                |
| FastAPI/dubbing orchestrator API (reference)    | [`dubbing.py`](file:///workspace/EnglishLearning/backend/app/api/dubbing.py)                        | HTTP endpoint patterns to mirror in the Rust server (`axum`/`hyper`).                                |
| Sentence-list frontend                          | [`subtitle-video/index.tsx`](file:///workspace/EnglishLearning/frontend/web/src/features/subtitle-video/index.tsx) | Build/embed as static assets served by the Rust binary.                                              |
| Word alignment / transcription (currently Python, e.g. faster-whisper/stable-ts) | [`word_aligner.py`](file:///workspace/EnglishLearning/backend/app/services/sync/word_aligner.py) | Replaced by a **Rust-native** aligner (whisper.cpp bindings) for the transcription path.             |

### 2.1 Gaps to close for a Rust-only single-`.exe` build

1. **No Rust `bin` target yet** — the current Rust crate is a PyO3 `cdylib`.
   It must be restructured so the same crate ships a native `bin` (the
   standalone app) as well as the optional Python extension.
2. **No Rust-native alignment/transcription** — when no subtitle file is
   supplied, alignment currently needs Python Whisper. This must move to an
   in-process Rust model (e.g. bindings to `whisper.cpp`).
3. **No embedded HTTP server + embedded web assets** in Rust — needs an
   `axum`/`hyper` server with the frontend bundled (via `include_str!` /
   `rust-embed`) rather than served by Python/FastAPI.
4. **No "click sentence → play that slice" wiring** — the cue list is static;
   it needs a synced `<video currentTime>` interaction in the embedded UI.
5. **No cross-platform single-binary packaging / installer.**

***

## 3. Functional Requirements

### FR-1 — Start & launch (single executable)

* FR-1.1 The user runs one command — `sentence-video serve <media-file>` — from
  a terminal, or double-clicks the packaged `.exe` (which then runs that flow).
* FR-1.2 The server starts on an **OS-assigned free port** (avoid hard-coding 8000).
* FR-1.3 On startup the binary **opens the user's default browser** at
  `http://localhost:<port>/` (Windows `start`, macOS `open`, Linux `xdg-open`)
  **without shelling out to a Python launcher**.
* FR-1.4 The terminal prints the URL too, for when the browser cannot auto-open.
* FR-1.5 A `--no-browser` flag exists for headless / CI usage.

### FR-2 — Ingest & segment (all in Rust)

* FR-2.1 Accept a local media file (`.mp4`, `.mkv`, `.mov`, `.mp3`, `.m4a`, `.wav`, `.aac`)
  with optional `.srt`/`.vtt` subtitle alongside it.
* FR-2.2 If a subtitle file is supplied, parse it natively in Rust (pure Rust
  `.srt`/`.vtt` parser) into cues — no transcription.
* FR-2.3 If no subtitle is supplied, run **in-process** Rust transcription +
  alignment (whisper.cpp bindings) then the Rust sentence grouper.
* FR-2.4 Every segment exposes: `index`, `start`, `end`, `text`, optional
  `words[]`, and a `source_path`/`source_id`.
* FR-2.5 Segmentation is **idempotent and cacheable** — re-running on the same
  file reuses a persisted result (next to the media or in a `.sentence-video/`
  cache dir).

### FR-3 — Sentence UI

* FR-3.1 The page shows the media player (`<video>`/`<audio>`) at the top and a
  **scrollable sentence list** below (assets served by the Rust binary).
* FR-3.2 Each row shows: index, timestamp range (`mm:ss.xxx`), sentence text.
* FR-3.3 Clicking a sentence seeks to `start`, plays until `end`, then pauses
  (single-segment playback).
* FR-3.4 A "Loop this sentence" toggle replays the segment until disabled.
* FR-3.5 A "Play through" mode plays across sentences while **auto-highlighting**
  the active one.
* FR-3.6 The active sentence is visually highlighted and auto-scrolled.
* FR-3.7 Keyboard shortcuts: `↑/↓` prev/next, `Space` play/pause, `L` loop,
  `Enter` jump.

### FR-4 — Segment media delivery (Rust fast path)

* FR-4.1 The browser player uses the **original source file via HTTP byte-range**
  (Rust `Range` handling) so no on-disk slicing is required.
* FR-4.2 For formats the browser can't play natively (e.g. some `.mkv`), the
  **Rust** code transcodes on the fly (reusing the AAC `/` `.mp4` path proven in
  `mux_video_audio`) to a streamable container.
* FR-4.3 A `/segments/<id>/audio` endpoint returns the per-sentence audio clip
  (decoded once via `decode_audio`, cached) for "audio only" playback/waveform.

### FR-5 — Pure-Rust architecture (no split process)

* FR-5.1 The application is **one Rust crate** compiled to a single executable.
  Media I/O uses `ffmpeg-next` in-process. Required public functions, already
  proven in [`lib.rs`](file:///workspace/EnglishLearning/backend/native/rust/src/lib.rs):
  * `probe(path) -> duration` (media duration, no `ffprobe`).
  * `decode_audio(path, sr, mono, max_seconds)` (decode + resample).
  * `mux_video_audio(video, audio, out, bitrate, shortest)` (remux + AAC).
  * `extract_range(path, start, end, out_path)` (sample-accurate slice).
* FR-5.2 Subtitle parsing, segmentation, and the HTTP server are implemented in
  Rust; no Python runtime, no PyAV, no system `ffmpeg`/`ffprobe` dependency.
* FR-5.3 For the transcription path, bind a Rust-native model (whisper.cpp)
  rather than invoking Python.

### FR-6 — Cross-platform single `.exe`

* FR-6.1 Supported: Windows 10/11, macOS 11+, Ubuntu 20.04+.
* FR-6.2 The binary detects the OS for the "open browser" command and default
  paths.
* FR-6.3 Build a **single executable per platform**: `cargo build --release`
  (Windows `.exe`, macOS/Linux binary), with cross-compile toolchain documented.
* FR-6.4 **No user-installed ffmpeg/ffprobe** — FFmpeg is linked/bundled with the
  binary (static or vendored `ffmpeg-next` features).

***

## 4. Non-Functional Requirements

| ID            | Requirement                                                                                                                                                  |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| NFR-1 (Perf)  | Alignment re-runs on a 30-min file meet the pipeline budget; click-to-play latency < 300 ms when cached.                                                      |
| NFR-2 (Perf)  | Rust media hot paths must not regress vs the previous Python path (probe/decode are in-process, no subprocess overhead).                                      |
| NFR-3 (Robust)| Missing optional deps degrade gracefully; the native UI server must still start and show an inline error message.                                             |
| NFR-4 (Security) | Server binds to `127.0.0.1` only by default; a `--host 0.0.0.0` opt-in exists for LAN sharing.                                                              |
| NFR-5 (Security) | File-serving endpoints must restrict access to the working media dir (path-prefix check in Rust).                                                           |
| NFR-6 (UX)    | First paint of the sentence list happens before transcription finishes for the subtitle-supplied path.                                                        |
| NFR-7 (Maint) | Keep the media core shareable: the same crate remains buildable as a library (`lib`) with optional PyO3 extension for the legacy Python backend, plus the new `bin`. |
| NFR-8 (Compat) | Rust stable 1.74+; `ffmpeg-next` 7.x; a single executable per OS (static-linked where feasible).                                                            |

***

## 5. Proposed Architecture (single Rust binary)

```
┌──────────────────────────────────────────────────────────────────────┐
│  sentence-video  —  one Rust crate, one executable                    │
│                                                                      │
│  ┌───────────────────────────────┐                                   │
│  │  lib: media core (ffmpeg-next)│                                   │
│  │   - probe / decode_audio      │                                   │
│  │   - mux_video_audio           │                                   │
│  │   - extract_range             │   (shared with legacy Python ext) │
│  └───────────────────────────────┘                                   │
│              │                                                       │
│  ┌───────────▼───────────────┐        ┌─────────────────────────────┐│
│  │  bin: the desktop app      │        │  embedded web assets       ││
│  │   - HTTP server (axum)     │ ─────► │  - player + sentence list  ││
│  │   - subtitle parser (srt)  │ serve  └─────────────────────────────┘│
│  │   - segmentation (Rust)    │                                       │
│  │   - whisper.cpp alignment  │                                       │
│  │   - browser launcher       │                                       │
│  │   - Range/segment delivery │                                       │
│  └────────────────────────────┘                                       │
│              │                                                        │
│              └──────────── HTTP 127.0.0.1:<port> ──► default browser  │
└──────────────────────────────────────────────────────────────────────┘
```

### 5.1 Crate layout

```
sentence-video/
  Cargo.toml               # one crate; optional [features] pyo3
  build.rs                 # (optional) link static FFmpeg
  src/
    lib.rs                 # media core + shared models (re-usable)
    media/
      probe.rs             # duration / stream info   (from lib.rs probe)
      decode.rs            # decode + resample f32    (from decode_audio)
      mux.rs               # remux video + AAC audio  (from mux_video_audio)
      range.rs             # sample-accurate slice
    app/
      server.rs            # axum HTTP server
      launcher.rs          # free port + open default browser
      segmenter.rs         # cue split/merge (ported from cue_builder)
      srt_parser.rs        # .srt/.vtt → Cue
      transcribe.rs        # whisper.cpp bindings (optional feature)
      cache.rs             # idempotent alignment cache
  assets/
    index.html             # built/embedded via rust-embed (or include_str!)
    app.ts/js              # port of subtitle-video cue list + <video>
```

***

## 6. API Surface (served by the Rust binary)

| Method | Path                                     | Purpose                                                                          |
| ------ | ---------------------------------------- | -------------------------------------------------------------------------------- |
| `POST` | `/api/session`                           | Accept a media path (or upload), run/cache segmentation, return `session_id` + cues. |
| `GET`  | `/api/session/<id>/cues`                 | Return the sentence list (`index, start, end, text, words?`).                    |
| `GET`  | `/api/session/<id>/media`                | Stream the source file with `Range` support (for `<video>`/`<audio>`).           |
| `GET`  | `/api/session/<id>/segments/<idx>/audio` | Per-sentence audio clip (Rust-extracted, cached).                                |
| `GET`  | `/health`                                | Liveness probe.                                                                  |

***

## 7. Out of Scope (v1)

* Multi-user / remote server hardening (v1 is local single-user).
* Editing/translating subtitles (covered by the `EnglishLearning` dubbing studio).
* Mobile native apps (the embedded browser UI is responsive but no wrapper).
* A native (non-browser) desktop GUI — v1 keeps the browser as the UI.
* AI dubbing/TTS — this app only *plays* slices; dubbing stays in
  `EnglishLearning` (whose Python backend can keep using the same Rust core
  via the optional PyO3 extension).

***

## 8. Acceptance Criteria

* AC-1 Running `sentence-video.exe serve ./talk.mp4` (or double-clicking the
  binary) opens the browser and shows a sentence list within alignment time.
* AC-2 Clicking any sentence seeks the player to `start` and stops at `end`.
* AC-3 Loop mode replays the same sentence until disabled.
* AC-4 Re-running on the same file uses the cache and shows the list instantly.
* AC-5 The binary is a single Rust executable; **no Python, no system ffmpeg/
  ffprobe** is required to run it.
* AC-6 Works on Windows (`.exe`), macOS, and Linux from the one compiled binary.