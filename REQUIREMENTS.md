# Requirements: Sentence-Segmented Video Player

> Cross-platform desktop app that splits a video/audio file into sentence-level
> segments. The user clicks any sentence in the UI and the player jumps to
> (and optionally loops) that exact sentence. Backend is Python + Rust;
> the frontend is the user's default web browser, launched automatically
> when the server starts.

***

## 1. Product Overview

### 1.1 Goal

A single command — e.g. `sentence-video serve <file>` — starts a local server
that:

1. Splits the source media into **sentence-by-sentence** segments with
   sample-accurate start/end timestamps.
2. Opens the user's **default browser** at the running URL so no native UI
   toolkit is required.
3. Renders a page listing every sentence; clicking a sentence plays the
   corresponding video/audio slice.

### 1.2 Why this design

* **Cross-platform without Electron/Tauri weight** — the OS browser is the UI.

* **Python for the AI/media pipeline** that already exists in this repo;
  **Rust for the hot paths** (segment extraction, range serving, alignment
  pre/post-processing) where Python is the bottleneck.

* **Reuses existing work** — the `EnglishLearning/backend` already implements
  sentence segmentation, word alignment, and a subtitle→video pipeline.

***

## 2. Current Code Baseline (what exists and can be reused)

| Capability                                                   | Existing location                                                                                                | Reuse plan                                                                         |
| ------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| Word-level forced alignment (faster-whisper / stable-ts)     | [word\_aligner.py](file:///workspace/EnglishLearning/backend/app/services/sync/word_aligner.py)                  | Drive alignment from the new app; output `List[Word]`.                             |
| Group words into sentence cues (`[start, end, text, words]`) | [cue\_builder.py](file:///workspace/EnglishLearning/backend/app/services/sync/cue_builder.py)                    | Primary segmentation logic. Returns `AlignedCue` per sentence.                     |
| Split long cues + merge tiny fragments                       | [subtitle\_writer.py](file:///workspace/EnglishLearning/backend/app/services/sync/subtitle_writer.py)            | Post-process cues so each segment is a natural sentence.                           |
| PyAV media decode / duration / resample (no system ffprobe)  | [av\_utils.py](file:///workspace/EnglishLearning/backend/app/services/sync/av_utils.py)                          | Get duration, decode audio for alignment.                                          |
| FastAPI app skeleton + CORS + file serving                   | [main.py](file:///workspace/EnglishLearning/backend/app/main.py)                                                 | Template for the new thin server; reuse `/subtitle-to-video/download` pattern.     |
| Frontend page that lists cues with start/end/text            | [subtitle-video/index.tsx](file:///workspace/EnglishLearning/frontend/web/src/features/subtitle-video/index.tsx) | UI starting point — extend the cue `<li>` to be clickable, add a synced `<video>`. |
| Subtitle analyzer upload UI                                  | [subtitle/index.tsx](file:///workspace/EnglishLearning/frontend/web/src/features/subtitle/index.tsx)             | Upload pattern reference.                                                          |
| Sentence/word data models                                    | [models.py](file:///workspace/EnglishLearning/backend/app/services/sync/models.py) (`Word`, `AlignedCue`)        | Shared contract between Python and Rust.                                           |
| uv-based Python packaging                                    | [pyproject.toml](file:///workspace/EnglishLearning/backend/pyproject.toml)                                       | Add the new app as a uv project.                                                   |

### 2.1 Gaps in the current code (new work required)

1. **No Rust component exists yet** — Rust mentions today are only in dependency
   metadata. A Rust crate (exposed via PyO3/maturin) must be created.
2. **No "click sentence → play that slice" interaction** — the cue list in
   [subtitle-video/index.tsx](file:///workspace/EnglishLearning/frontend/web/src/features/subtitle-video/index.tsx)
   is read-only; it has no `<video currentTime>` wiring.
3. **No per-segment media serving** — the backend serves whole files, not
   byte-range slices of a sentence.
4. **No launcher that opens the default browser** — [server.py](file:///workspace/EnglishLearning/backend/server.py)
   just prints a URL.
5. **No cross-platform packaging** (single-binary / installer).

***

## 3. Functional Requirements

### FR-1 — Start & launch

* FR-1.1 The user runs one command from a terminal: `sentence-video serve <media-file>` (or double-clicks the packaged binary, which then runs the command).

* FR-1.2 The server starts on an **OS-assigned free port** (avoid hard-coding 8000 to prevent clashes).

* FR-1.3 On startup the server **opens the user's default browser** at `http://localhost:<port>/`. (Windows: `start`; macOS: `open`; Linux: `xdg-open`.)

* FR-1.4 The terminal prints the URL too, so a user whose browser fails to open can navigate manually.

* FR-1.5 A `--no-browser` flag must exist for headless / CI usage.

### FR-2 — Ingest & segment

* FR-2.1 Accept a local media file path (`.mp4`, `.mkv`, `.mov`, `.mp3`, `.m4a`, `.wav`, `.aac`) or a `.srt`/`.vtt` subtitle file alongside the media.

* FR-2.2 If a subtitle file is supplied, parse it directly into cues (no transcription).

* FR-2.3 If no subtitle is supplied, run word alignment (existing [word\_aligner.py](file:///workspace/EnglishLearning/backend/app/services/sync/word_aligner.py)) then sentence grouping (existing [cue\_builder.py](file:///workspace/EnglishLearning/backend/app/services/sync/cue_builder.py)).

* FR-2.4 Every segment exposes: `index`, `start`, `end`, `text`, optional `words[]` (word-level timings), and a `source_path`/`source_id` so the client can request that slice.

* FR-2.5 Segmentation must be **idempotent and cacheable** — re-running on the same file reuses a persisted alignment result (json/parquet next to the media or in a `.sentence-video/` cache dir).

### FR-3 — Sentence UI

* FR-3.1 The page shows the media player (`<video>` or `<audio>`) at the top and a **scrollable sentence list** below.

* FR-3.2 Each sentence row shows: index, timestamp range (`mm:ss.xxx`), and the sentence text.

* FR-3.3 Clicking a sentence seeks the player to `start` and, by default, **plays only until** **`end`** then pauses (single-segment playback mode).

* FR-3.4 A "Loop this sentence" toggle replays the current segment until toggled off.

* FR-3.5 A "Play through" mode plays continuously across sentences while keeping the list **auto-highlighting** the active sentence.

* FR-3.6 The active sentence is visually highlighted and auto-scrolled into view.

* FR-3.7 Keyboard shortcuts: `↑/↓` prev/next sentence, `Space` play/pause, `L` toggle loop, `Enter` jump to selected.

### FR-4 — Segment media delivery

* FR-4.1 The browser player uses the **original source file via HTTP byte-range** so no slicing is required on disk.

* FR-4.2 For formats the browser cannot play natively (e.g. some `.mkv`/`.m4a`), the backend **transcodes on the fly** to HLS or a streamable `.mp4`/`.mp3`. This is the primary candidate for the **Rust** fast path.

* FR-4.3 A `/segments/<id>/audio` endpoint returns the per-sentence audio clip (decoded once, cached) — used for per-sentence "audio only" playback and waveform preview.

### FR-5 — Rust ↔ Python split

* FR-5.1 Rust extension (PyO3) named e.g. `sentence_video_native` must expose:

  * `extract_range(path, start, end, out_path)` — cut a media slice with sample accuracy.

  * `probe(path) -> {duration, streams, codec}` — replace ffprobe calls.

  * `transcode_stream(path, fmt) -> path` — fast transcode to a browser-friendly container.

  * `resample_wav(path, sample_rate)` — for the alignment pipeline.

* FR-5.2 Python remains the orchestrator: it calls Rust for media I/O and runs the Whisper/alignment stack in Python.

* FR-5.3 If the Rust extension fails to import (e.g. platform miss), the app **falls back** to the existing PyAV/ffmpeg Python path and logs a warning — never hard-crash.

### FR-6 — Cross-platform

* FR-6.1 Supported: Windows 10/11, macOS 11+, Ubuntu 20.04+ (and glibc-equivalent distros).

* FR-6.2 The launcher detects the OS and picks the right "open browser" command.

* FR-6.3 Pre-built wheels for the Rust extension via `maturin` (or `cargo build --release` + bundled `.so`/`.dll`/`.dylib`).

* FR-6.4 No user-installed ffmpeg/ffprobe requirement — bundled or shipped via PyAV + Rust demuxers.

***

## 4. Non-Functional Requirements

| ID               | Requirement                                                                                                                                                                                           |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| NFR-1 (Perf)     | Alignment re-runs on a 30-min file must complete within the existing pipeline's budget; segment click-to-play latency < 300 ms when cached.                                                           |
| NFR-2 (Perf)     | Rust hot paths (range probe, transcode) must outperform the pure-Python path by ≥ 2× on large files.                                                                                                  |
| NFR-3 (Robust)   | Missing optional deps must degrade gracefully (the existing [main.py](file:///workspace/EnglishLearning/backend/app/main.py) `try/except ImportError` pattern must be preserved).                     |
| NFR-4 (Security) | Server binds to `127.0.0.1` only by default (not `0.0.0.0`); a `--host 0.0.0.0` opt-in exists for LAN sharing.                                                                                        |
| NFR-5 (Security) | File-serving endpoints must restrict access to the working media dir (reuse the path-prefix check in [main.py](file:///workspace/EnglishLearning/backend/app/main.py) `/subtitle-to-video/download`). |
| NFR-6 (UX)       | First paint of the sentence list must happen before transcription finishes for the subtitle-supplied path.                                                                                            |
| NFR-7 (Maint)    | Keep the existing monorepo layout; new app lives alongside `EnglishLearning/` and `AI-Demo/`.                                                                                                         |
| NFR-8 (Compat)   | Python 3.11+; Rust stable 1.74+; PyO3 0.21+ (match the ABI constraints noted in [pyproject.toml](file:///workspace/AI-Demo/sdk-python/pyproject.toml)).                                               |

***

## 5. Proposed Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│  sentence-video (new top-level package, uv + maturin workspace)   │
│                                                                  │
│  ┌──────────────────────┐        ┌──────────────────────────────┐ │
│  │  Python orchestrator│        │  Rust crate (PyO3)          │ │
│  │  - FastAPI server    │ call → │  - extract_range            │ │
│  │  - alignment reuse   │        │  - probe / transcode_stream │ │
│  │  - cue_builder reuse │        │  - resample_wav              │ │
│  │  - browser launcher  │        │  - segment cache            │ │
│  └──────────┬───────────┘        └──────────────┬─────────────┘ │
│             │                                    │               │
│             └────────── HTTP (127.0.0.1:<port>) ─┘               │
│                                  │                               │
│                  ┌───────────────▼───────────────┐               │
│                  │  Default browser (UI)         │               │
│                  │  - <video>/<audio>            │               │
│                  │  - sentence list (clickable)   │               │
│                  │  - loop / play-through modes   │               │
│                  └───────────────────────────────┘               │
└──────────────────────────────────────────────────────────────────┘
```

### 5.1 New repo/package layout

```
sentence-video/
  pyproject.toml              # uv project, depends on the EnglishLearning sync pkg
  Cargo.toml                 # maturin hybrid project
  src/
    sentence_video/
      __init__.py
      server.py              # FastAPI app (thin)
      launcher.py            # pick port + open default browser
      segmenter.py           # wraps cue_builder + word_aligner
      cache.py               # idempotent alignment cache
      _native.pyi            # type stubs for the Rust extension
    rust/
      lib.rs                 # PyO3 #[pyfunction] exports
      probe.rs
      transcode.rs
      range.rs
  frontend/
    index.html               # single-page, served by FastAPI static
    app.tsx                  # ports subtitle-video/index.tsx cue list
```

***

## 6. API Surface (new endpoints)

| Method | Path                                     | Purpose                                                                              |
| ------ | ---------------------------------------- | ------------------------------------------------------------------------------------ |
| `POST` | `/api/session`                           | Accept a media path (or upload), run/cache segmentation, return `session_id` + cues. |
| `GET`  | `/api/session/<id>/cues`                 | Return the sentence list (`index, start, end, text, words?`).                        |
| `GET`  | `/api/session/<id>/media`                | Stream the source file with `Range` support (for `<video>`/`<audio>`).               |
| `GET`  | `/api/session/<id>/segments/<idx>/audio` | Per-sentence audio clip (Rust-extracted, cached).                                    |
| `GET`  | `/health`                                | Liveness probe.                                                                      |

***

## 7. Out of Scope (v1)

* Multi-user / remote server hardening (v1 is local single-user).

* Editing/translating subtitles (already covered by `EnglishLearning` dubbing studio).

* Mobile native apps (the browser UI is responsive but no native wrapper).

* AI dubbing/TTS (handled by the existing `video_dubber`; this app only *plays* slices).

***

## 8. Acceptance Criteria

* AC-1 Running `sentence-video serve ./talk.mp4` opens the browser and shows a sentence list within alignment time.

* AC-2 Clicking any sentence seeks the player to that sentence's start and stops at its end.

* AC-3 Loop mode replays the same sentence until disabled.

* AC-4 Re-running on the same file uses the cache and shows the list instantly.

* AC-5 The Rust extension is used for range extraction; disabling it (env var) falls back to PyAV without breaking.

* AC-6 Works on Windows, macOS, and Linux without the user installing ffmpeg.

