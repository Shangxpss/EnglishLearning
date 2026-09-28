# EnglishLearning — Project Catch-Up

> Purpose: a single document that reconstructs **what this project is, how it is
> built, what every stage of the pipeline does, and how each piece of work was
> arrived at** — including the wav2vec2 alignment stack and the reasoning behind
> its current shape. Written to be read cold, by someone (or some agent) with no
> prior context.
>
> Last updated: 2026-09-27.

---

## 1. What the project is

An **English-learning video player**. You give it a video; it produces a
sentence-segmented, word-timestamped transcript, and the player lets you click a
word to hear it, replay a sentence, slow it down, and (optionally) hear a
**synthesised replacement voice track** (dubbing) instead of the original audio.

The goal is a **single static executable** with **no end-user installs and no
runtime downloads**:

- FFmpeg is compiled from source and linked in (`ffmpeg-next` `build` feature).
- SQLite is compiled in (`rusqlite` `bundled`).
- whisper.cpp is compiled and linked in, and the ASR **model is embedded in the
  binary** (`include_bytes!`) — result: an ~86 MB single file.
- The only optional on-disk asset is the **wav2vec2 alignment model** (378 MB,
  far too large to embed), which is why alignment is a non-default feature.

### Two backends in one repo

| Backend                   | Where                     | Status                                                                                   |
| ------------------------- | ------------------------- | ---------------------------------------------------------------------------------------- |
| **Rust `sentence-video`** | `EnglishLearning/rust/`   | **Current.** Everything the nav links to is served by this.                              |
| Python / FastAPI          | `EnglishLearning/python/` | **Legacy.** Kept as reference; the frontend routes that call it are hidden from the nav. |

There is also `AI-Demo/` (the Bun/Python "agent-runtime" the Rust agent service
was ported from) and `word_timestamps_pipeline.md`, a design essay on the
word-timestamp problem and what of it this repo does/does not do.

---

## 2. Repository layout

```
EnglishLearning/
├── AGENTS.md                     # build/run rules + the FFmpeg/GitHub hazard
├── word_timestamps_pipeline.md   # design essay: ASR → align → punctuation
├── project-ollow-up.md           # ← this file
├── .trae/documents/              # approved plan docs (see §12)
│   ├── dub-audio-checkbox.md
│   └── resumable-processing-and-manage-page.md
└── EnglishLearning/
    ├── package.json              # pnpm scripts (dev / build) — routes cargo via the shim
    ├── build_issue.md            # the full FFmpeg/bindgen/nasm post-mortem
    ├── rust/                     # crate `sentence-video`
    │   ├── Cargo.toml            # features: asr-whisper (default), agent (default), asr-align (off)
    │   ├── build.rs              # embeds the ASR model via include_bytes!
    │   ├── scripts/
    │   │   ├── cargo-shim.sh     # puts ~/git-shim + ~/cmake/bin on PATH
    │   │   ├── fetch-asr-model.sh
    │   │   └── fetch-align-model.sh
    │   └── src/                  # see §4
    └── frontend/web/             # React + Vite + react-router
        └── src/                  # see §9
```

---

## 3. Build environment & the FFmpeg saga

The machine is Linux Mint 22.2 (Ubuntu noble base). The dependency chain that
caused every early problem:

```
ffmpeg-next
└── ffmpeg-sys-next
    ├── build step A: git clone FFmpeg 6.1 → configure + make   (needs git, network, nasm, gcc)
    └── build step B: bindgen generates Rust bindings           (needs libclang + libLLVM + clang builtins)
```

### 3.1 The five problems (full detail in `EnglishLearning/build_issue.md`)

1. **`git clone` of FFmpeg dies mid-transfer** — the network drops long TLS
   streams, so the ~60 MB packfile never finishes (GitHub _and_ the Gitee mirror
   both failed).
2. **FFmpeg `configure` fails: `nasm` not found.**
3. **bindgen: `Unable to find libclang`.**
4. **libclang found but `libLLVM-14.so.1` missing.**
5. **bindgen: `limits.h` not found** — libclang looks for its builtin headers at
   a path compiled into the library, which doesn't exist for a user-dir extract.

### 3.2 Final resolution (no workarounds remain in the build path)

```bash
sudo apt install -y nasm libclang1-14t64 libllvm14t64 libclang-common-14-dev
```

With those installed, `LIBCLANG_PATH`, `BINDGEN_EXTRA_CLANG_ARGS` and
`LD_LIBRARY_PATH` are all unnecessary, and the `[env]` block was removed from
`rust/.cargo/config.toml`.

### 3.3 The one hazard that remains: FFmpeg + GitHub

`ffmpeg-sys-next`'s build script runs
`git clone --depth=1 -b release/6.1 https://github.com/FFmpeg/FFmpeg`, and this
machine **cannot reach GitHub**, so the build appears to hang ~11 minutes and
then panics.

- It only re-runs when FFmpeg's build-script unit is rebuilt: after
  `rm -rf rust/target` / `cargo clean`, on the **first** build of a new profile,
  or when a dependency change alters the build-script hash.
- **Most crate additions do NOT trigger it.** Do not assume a new dependency
  caused a hang — check the process tree.

The offline fix already exists:

- `~/git-shim/git` answers exactly that one clone by extracting
  `~/ffmpeg-tarballs/ffmpeg-6.1.tar.xz` and passes everything else to real git.
- `rust/scripts/cargo-shim.sh` puts it on `PATH` automatically.
- `package.json` already routes `build:server` and `dev:server` through that
  script, so the normal commands are safe. For a raw cargo invocation:

```bash
PATH="$HOME/git-shim:$PATH" cargo build --release --manifest-path rust/Cargo.toml
```

With the shim, FFmpeg compiles once per profile (~3 min) and everything after is
cached (sub-second builds).

**Diagnose in seconds** — if there is no `cc1`/`rustc` child and cargo has run
for minutes, it is a network stall, not compiling:

```bash
ps -eo pid,ppid,etime,args | grep -E '[c]argo|[g]it clone'
ss -tnp | grep cargo          # CLOSE-WAIT / SYN-SENT to :443 == network stall
```

### 3.4 Other build-machine prerequisites

- **cmake** — `whisper-rs-sys` compiles the vendored whisper.cpp via cmake. A
  user-local copy lives at `~/cmake/bin` and is added to `PATH` by
  `rust/scripts/cargo-shim.sh`.
- **The ASR model** (~60 MB, gitignored) —
  `rust/scripts/fetch-asr-model.sh` (default `ggml-base.en-q5_1.bin`).
  `build.rs` embeds it; `ASR_MODEL=<path>` overrides. Absent → build still
  succeeds, transcription then needs `--asr-model <path>` at runtime.
- **`cargo build --no-default-features`** skips speech-to-text entirely for a
  fast, small, model-free build.
- **Cargo registry note:** `rsproxy.cn` is currently unreachable, so its
  `replace-with` line in `~/.cargo/config.toml` is commented out (direct
  crates.io). Backup at `~/.cargo/config.toml.bak-dsh`.

---

## 4. Build & run

```bash
pnpm run build:server   # rust release binary (via the shim wrapper)
pnpm run build:web      # vite production build
pnpm run dev:server     # rust dev server on :8018 (cargo watch, hot reload)
pnpm run dev:web        # vite dev server on :5173 (proxies /api -> :8018)
pnpm run dev            # agent + server + web together
```

- `dev:server` is `cargo watch -x 'run --features asr-align -- --no-browser --port 8018'`.
- `dev` also starts `pnpm run agent` — the AG-UI service on **:4000**.
- Processed sessions persist in SQLite at
  `~/.local/share/sentence-video/sessions.db` (override `--data-dir <path>`);
  re-opening the same media reuses the cached session.

---

## 5. Rust server architecture

Crate `sentence-video` in `EnglishLearning/rust/`. **~9 000 lines** of Rust.

### 5.1 Cargo features

| Feature       | Default | Effect                                                                                                                                                                                              |
| ------------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `asr-whisper` | ✅      | `dep:whisper-rs` — whisper.cpp statically linked + embedded model.                                                                                                                                  |
| `agent`       | ✅      | `ag-ui`, `axum`, `tokio`, `tower-http`, `reqwest`, `futures-util`, `async-stream` — the CopilotKit/AG-UI service. Drop with `--no-default-features --features asr-whisper` for a player-only build. |
| `asr-align`   | ❌      | **Deliberately empty** — the whole wav2vec2 model is hand-written in `src/align/`, so enabling it adds _no dependencies_. Needs 378 MB of weights on disk at runtime.                               |

### 5.2 Profile overrides (`Cargo.toml`)

```toml
[profile.release]
opt-level = 3
lto = true
strip = true

# The dev server runs the debug profile; the hand-written alignment forward pass
# took >18 min there on a 60 s clip. Scoped to this package only, so no
# dependency (FFmpeg, whisper.cpp, SQLite) is recompiled and the whole align hot
# path is optimized.
[profile.dev.package.sentence-video]
opt-level = 3
```

Verified reaching rustc: the `--crate-name sentence_video` compile line carries
`-C opt-level=3 -C debuginfo=2 -C debug-assertions=on`, with only 1 crate
recompiled.

### 5.3 Module map

| File                     | Lines | Responsibility                                                                                                                                                                                 |
| ------------------------ | ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `main.rs`                | 844   | CLI + arg parsing. Subcommands: serve (default), `align`, `dub`, `agent`. Flags `--align`, `--asr-model`, `--asr-language`, `--dub-engine/-voice/-proxy`, `--silence`, `--data-dir`, `--port`. |
| `app.rs`                 | 1643  | The HTTP app: routing, session CRUD, media/audio streaming, `builds` registry, `dub_jobs` registry. The biggest file.                                                                          |
| `server.rs`              | 205   | Thin HTTP listener/serve loop (native, no framework).                                                                                                                                          |
| `db.rs`                  | 468   | SQLite persistence: sessions, cues (`cues.words` JSON column), progress. `SourceKey` includes `subtitle_mtime` so a subtitle edit invalidates the cache.                                       |
| `media.rs`               | 524   | In-process FFmpeg decode/resample to 16 kHz mono `f32`; H.264 transcode for browser-playable video.                                                                                            |
| `models.rs`              | 52    | `Word { text, start, end, score }`, `Cue { index, start, end, text, words[], source_path }`, `Session`, `Progress`.                                                                            |
| `picker.rs`              | 159   | Server-side native file dialog (`POST /api/pick-file`).                                                                                                                                        |
| `segmenter.rs`           | 42    | `is_sentence_end` / `finalize` — re-index, clamp times, fill words + source_path.                                                                                                              |
| `subtitle.rs`            | 214   | SRT/VTT parse + write (`write_srt`, tolerant of a read-only media dir).                                                                                                                        |
| `asr.rs`                 | 425   | whisper.cpp integration: embedded model, token timestamps, DTW, onset snapping.                                                                                                                |
| `pipeline/mod.rs`        | 134   | `DubConfig` + `init_dub`/`dub_config` (`OnceLock`), the CLI `dub` entry.                                                                                                                       |
| `pipeline/transcribe.rs` | 496   | The build orchestrator: decode → ASR → optional align → `build_cues`. `BuildStage`/`BuildSink` progress.                                                                                       |
| `pipeline/synthesize.rs` | 1323  | TTS backends, two-pass respeed fitting, `PlaceStitcher`, checkpoint + retry API.                                                                                                               |
| `pipeline/checkpoint.rs` | 188   | Always-compiled atomic write (temp+rename), recipe-tagged load, `texthash`, FNV-1a.                                                                                                            |
| `align/`                 | 2 284 | wav2vec2 alignment — see §7.                                                                                                                                                                   |

`align/` is `#[cfg(feature = "asr-align")]`; `checkpoint.rs` is deliberately
_outside_ it so a default `cargo test` still covers the checkpoint logic.

---

## 6. HTTP API surface

Top level (`app.rs` dispatch):

| Method       | Path                                  | Purpose                                                                                                                                                                                    |
| ------------ | ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `GET`        | `/api/sessions`                       | List processed videos + their state (`dubbed`, dub state, progress).                                                                                                                       |
| `DELETE`     | `/api/sessions`                       | Delete all sessions (+ their checkpoint dirs and tracks).                                                                                                                                  |
| `GET`        | `/api/builds`                         | Builds **in flight** — the video isn't in the session list until the build ends, so this is what the Manage page polls to show "⏳ Processing". Sorted longest-first, with `elapsed_secs`. |
| `POST`       | `/api/session`                        | Create a session. Body: `dub`, `dub_engine`, `align`, `subtitle_path`, … `?force=1` bypasses the DB cache.                                                                                 |
| `POST`       | `/api/pick-file`                      | Native file dialog. `?kind=media\|subtitle`.                                                                                                                                               |
| `GET`        | `/api/session/{id}`                   | Session JSON + derived `dubbed`, `dubbing`, `video`. Opening a session starts the H.264 re-encode if needed.                                                                               |
| `DELETE`     | `/api/session/{id}`                   | Delete session + track + checkpoints.                                                                                                                                                      |
| `GET`        | `/api/session/{id}/cues`              | The cue array.                                                                                                                                                                             |
| `POST`/`GET` | `/api/session/{id}/dub`               | Start / poll dubbing (async job).                                                                                                                                                          |
| `POST`       | `/api/session/{id}/retranscode`       | Remove the cached `h264/{hash}.mp4` and re-open.                                                                                                                                           |
| `GET`        | `/api/session/{id}/media`             | Range-stream the (original or transcoded) video.                                                                                                                                           |
| `GET`        | `/api/session/{id}/dubbed-audio`      | Range-stream the generated voice WAV.                                                                                                                                                      |
| `GET`        | `/api/session/{id}/audio?start=&end=` | Exact decoded slice; serves the **dubbed** source when a track exists.                                                                                                                     |
| `GET`/`POST` | `/api/session/{id}/progress`          | `{ last_index, position }` — resume-where-you-left-off.                                                                                                                                    |

`GET {path}` for a non-`/api/` path with no extension serves the SPA.

### 6.1 Build progress model

`BuildStage` (in `pipeline/transcribe.rs`) is one of:

- `Decode` — decoding audio to 16 kHz mono.
- `Transcribe` — whisper over the whole buffer (no sub-progress possible).
- `Align { done, total }` — wav2vec2 window by window; the only stage with a
  countable unit, so the only one worth a progress bar.

Crucially, `align` reports `align 0/N` **before** the first window starts —
otherwise the frontend keeps showing "transcribing" for the whole of the first
window (minutes).

---

## 7. The alignment stack (wav2vec2 forced alignment)

This is the deepest piece of engineering in the repo, so it gets the most detail.

### 7.1 Why it exists

Whisper's own timestamps (even whisper.cpp's DTW token timestamps) are still
**~0.2 s late at phrase boundaries**. The audio-onset snapping in `asr.rs` only
partly fixes that. The standard fix — what WhisperX does — is: get the
_transcript_ from whisper, then determine _when_ each word was said with a CTC
acoustic model aligned by a Viterbi pass over the known transcript.

### 7.2 Why it is hand-written (no ONNX runtime)

Every off-the-shelf runtime was ruled out **on this machine**:

| Option               | Why rejected                                                                                                                                                                                               |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `tract-onnx`         | Cannot run either shipped ONNX graph: Xenova's int8 uses `ConvInteger` with mixed u8/i8 (tract refuses to unify); the fp16 graph dies on symbolic shape inference and tract 0.23 has no `concretize_dims`. |
| `ort` (ONNX Runtime) | 2.0.0-rc.13 has no `static` feature → would ship a shared library next to the binary; and `download-binaries` needs `cdn.pyke.io`, uncovered by any mirror here.                                           |
| `native-whisperx`    | 0.1.x, ~6% doc coverage, pulls candle + pyannote + FFmpeg; high churn / supply-chain risk.                                                                                                                 |
| `qwen-asr`           | Needs a system BLAS (OpenBLAS) and downloads ~1.3 GB — breaks the "no user-installed dependencies" property.                                                                                               |

So the encoder is hand-written in `src/align/`, which keeps the `Cargo.toml`
promise: **no new dependencies, no runtime downloads beyond the model file**.

### 7.3 Architecture (verified against `modeling_wav2vec2.py`)

`config.json` says `feat_extract_norm: "group"` and `do_stable_layer_norm:
false`, which selects specific code paths (both change real behaviour):

```text
input (16 kHz, normalised zero-mean unit-variance)
  └─ feature extractor, 7 convs, strides 5·2·2·2·2·2·2 = 320 (20 ms frames)
       conv0:    Conv1d(1→512, k=10, s=5, no bias) → GroupNorm(512 groups) → GELU
       conv1..6: Conv1d(k=3/3/3/3/2/2, s=2, no bias) → GELU   (no norm)
  └─ feature projection: LayerNorm(512) → Linear(512→768)
  └─ positional conv: grouped Conv1d(768, k=128, groups=16) + weight-norm
       hidden = hidden + pos_conv(hidden)      ← residual, then:
       hidden = LayerNorm(768)
  └─ 12 × post-norm transformer layer:
       r = h; h = r + attn(h); h = LayerNorm(h)
       h = h + ffn(h);         h = LayerNorm(h)
  └─ lm_head: Linear(768→32) → CTC emissions (blank = 0)
```

Key constants: `HIDDEN=768`, `HEADS=12`, `HEAD_DIM=64`, `N_LAYERS=12`,
`INTERMEDIATE=3072`, `CONV_DIM=512`, `POS_KERNEL=128`, `POS_GROUPS=16`,
`VOCAB=32`, `ATTENTION_SCALE = 64^-0.5 = 0.125`,
`LAYER_NORM_EPS = GROUP_NORM_EPS = 1e-5`, `SAMPLES_PER_FRAME = 320`.

**Frame rate: 50 fps** (16 000 Hz / 320 hop = 20 ms). `FRAME_SECS = 0.02`.

### 7.4 Files

| File                   | Lines | Role                                                                                                                                                 |
| ---------------------- | ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `align/mod.rs`         | 459   | `Aligner`, `plan_windows`, `refine_words` (window scheduler + checkpoints), `RefineStats`.                                                           |
| `align/wav2vec2.rs`    | 572   | The forward pass; weight loading from safetensors; `attention()` (row-split).                                                                        |
| `align/tensor.rs`      | 572   | The `f32` kernels: `linear`, `conv1d`, `layer_norm`, `gelu`/`erf`, `softmax`, `group_norm_per_channel`, `reconstruct_weight_norm`, `rows_split_mut`. |
| `align/safetensors.rs` | 256   | Minimal safetensors reader (`st.f32(name)`).                                                                                                         |
| `align/ctc.rs`         | 425   | `encode_words`, `forced_align` (Viterbi), `greedy_decode`, `frame_confidence`.                                                                       |

### 7.5 Windowing

Self-attention is **quadratic in frames** (a 30 s window is ~1500 frames; a
10-minute file would be 30 000 frames and need ~3.6 GB for one layer's score
matrix). So audio is aligned window by window:

- `WINDOW_SECS = 30.0` — measured ~5.7 s of CPU per second of audio at 15–20 s
  windows, only ~12% more at 30 s; 30 s keeps overhead small with few windows.
- `WINDOW_PAD_SECS = 0.4` — audio kept on either side so the window's first and
  last word keep their onset and release (whisper runs ~0.2 s late, hence more
  than a frame).
- `plan_windows` groups **greedily at word boundaries**: start from `words[first].start`,
  extend while `words[last+1].end - start <= WINDOW_SECS`. Windows are
  **contiguous and non-overlapping**; bounds are
  `start = (start - PAD).max(0.0)`, `end = words[last].end + PAD`.
- Each window carries the words whose whisper timings fall inside it, because
  the transcript handed to the encoder must cover **exactly** the audio given.
- A per-window transcript is essential: feeding the whole transcript would
  distort timing with out-of-window words.

### 7.6 Window scheduling (the correction to an earlier oversimplification)

An earlier description — "6 windows × 1 core, then the last 2" — was **wrong**.
The truth in `refine_words`:

```rust
let cores   = std::thread::available_parallelism()…;
let threads = cores.min(windows.len());
let budget  = (cores / threads).max(1);   // each window thread's share of cores

std::thread::scope(|scope| {
    for _ in 0..threads {
        scope.spawn(|| loop {
            let i = next.fetch_add(1, Ordering::Relaxed);   // pull queue
            if i >= windows.len() { break; }
            tensor::set_thread_budget(budget);
            /* … align window i … */
        });
    }
});
```

- It is a **pull queue over an `AtomicUsize`**, not a static split — window
  costs vary with how much speech they contain.
- `set_thread_budget` is a **thread-local** `Cell<usize>` (0 = unset → use all
  cores); the kernels' fan-out is `budget.min(rows)`.
- **The design gap:** with ≥ 6 windows, `budget = 1`, so `rows_split_mut` never
  fans out _inside_ a window. The internal row parallelism is entirely inert for
  multi-window videos — which is exactly why a 4-minute video (≈8 windows on 6
  workers) scaled so poorly before the conv fix (§7.9).

### 7.7 The CTC alignment itself (`align/ctc.rs`)

- `encode_words` maps the transcript to vocabulary tokens and records
  `word_ranges`, so each word's span in the token sequence is known.
- `forced_align` runs a Viterbi pass over the frame×token lattice (blank = 0) —
  this is what turns "the word was said somewhere in here" into frame-accurate
  boundaries.
- A word spans its first symbol's first frame to its last symbol's last frame;
  `align()` reports `start = first * FRAME_SECS`, `end = (last + 1) * FRAME_SECS`
  (`last` inclusive).
- `score` = mean per-frame CTC confidence over the word's span
  (`frame_confidence`).
- Words with no vocabulary characters (digits, dashes) are **omitted** rather
  than given a fabricated time.

### 7.8 Correctness discipline: bit-identity

The forward pass is validated stage-by-stage against reference dumps, so **every
kernel optimization must preserve the exact accumulation order** (a single
accumulator, index `i` ascending). Both parallelism (`rows_split_mut`) and
blocking therefore leave the result **bit-identical**, and the test suite asserts
it with `assert_eq!` on `Vec<f32>`:

- `split_rows_are_bit_identical_to_the_sequential_kernel` (budget 1 vs 64)
- `blocking_rows_is_bit_identical_to_the_row_at_a_time_kernel`
  (t over `[1, 7, ROW_BLOCK, ROW_BLOCK+1, 64, 257]`)
- `split_conv_channels_is_bit_identical_to_the_sequential_one`

End-to-end proof on real audio: **all 19 word timings for a 14 s clip are
byte-identical** to the old naive-kernel `--release` binary.

### 7.9 Performance engineering (the alignment optimization log)

**Starting point.** The dev server ran the debug profile: a 60 s clip's two 30 s
windows took ~3 min in release and were still running after **>18 minutes** in
debug — alignment was effectively unusable in `pnpm run dev`.

**Step 1 — `opt-level = 3` for this package** (§5.2). One crate recompiles;
dependencies stay cached.

**Step 2 — measure before theorising.** `taskset -c N` limits CPU affinity, and
`std::thread::available_parallelism()` honours affinity, so core count could be
forced to 1/2/3/6 **with no code change**:

| Cores | Wall time (14 s clip) | Speedup   |
| ----- | --------------------- | --------- |
| 1     | 99.2 s                | —         |
| 2     | 73.7 s                | 1.35×     |
| 3     | 64.8 s                | 1.53×     |
| 6     | 61.8 s                | **1.61×** |

A **concurrency trace** (sampling `utime+stime` from `/proc/<pid>/stat` every
2 s) showed **1.00× for the first ~40 s**, then 3.8–4.5×. Model load is only
~0.4 s. So ~40 s genuinely ran on one core. Two wrong explanations were
discarded along the way (see §7.10).

**Root cause.** The feature-extractor **conv stack is serial**:
~34.4 × 10⁹ MACs, versus ~68.6 × 10⁹ for the 12 transformer layers — and only
the transformer was parallel. So a 14 s window spent its first ~40 s on one core
of six.

**Step 3 — tile the matmul (`linear`).** The naive order (one output row at a
time) re-streams the whole weight matrix per row; the FFN weight is
3072 × 768 × 4 B = 9.4 MB, which does not fit the 4 MB shared L3, so traffic is
~6.6 GB per FFN linear. Added `const ROW_BLOCK: usize = 8` — a weight row is
loaded once and reused for 8 outputs.

The _real_ win was **codegen**, isolated with a standalone micro-benchmark:
the identical inner loop with a **runtime** trip count compiled to
**0.93 GMAC/s**; with a **constant** `ROW_BLOCK = 8` bound it compiled to
**1.88 GMAC/s** (2.08 with `-C target-cpu=native`), with an **unchanged
checksum** — proving no reassociation. So 8 independent accumulators become real
wide instructions instead of an index loop LLVM cannot unroll.

> `-C target-cpu=native` was **tried and rejected**: it made the dynamic variant
> slower (1.36 → 0.98 GMAC/s). No build-flag change was made.

**Step 4 — split the conv over output channels.** This was a **self-correction**:
the conv had been recommended for _blocking_ on a memory-bound premise, but the
MACs arithmetic (~34.4 × 10⁹ at ~0.86 GMAC/s ≈ 43 s, matching the measured ~40 s
serial phase) shows it is **compute-bound** — blocking would not have helped.
Wrapping the per-`oc` body in `rows_split_mut(&mut out, t_out, …)` did:
channels are independent, so a single output value's accumulation is untouched.
Added an early return for `t_out == 0` (a `row_len = 0` split would panic).

**Cumulative result:**

|         | Before | After      | Change      |
| ------- | ------ | ---------- | ----------- |
| 1 core  | 99.2 s | **64.9 s** | −35%        |
| 6 cores | 61.8 s | **23.1 s** | −63% (2.7×) |

Post-fix concurrency trace: **3.7–4.6× from the first sample**. The user's
original complaint (>18 min unoptimized debug) is now ~23 s.

**Held throughout:** 122 tests pass (including the three bit-identity tests);
end-to-end word timings byte-identical to the naive kernel.

**Remaining structural inefficiency:** the 4-minute video has ~8 windows over 6
workers, so when fewer than `threads` windows remain, cores idle. Not addressed.

### 7.10 Two wrong turns, recorded so they are not repeated

1. **"The 1.61× ceiling is DRAM bandwidth."** Asserted without measuring, then
   disproved by the concurrency trace (~40 s of a 59 s run at 1.00×). The
   primary cause was the serial conv, not the memory bus.
2. **"Block/optimize the conv."** Recommended on memory-bound reasoning; the
   conv is compute-bound (§7.9 step 4). Parallelizing it — not blocking it — is
   what produced the win.

### 7.11 Alignment CLI

```bash
sentence-video align <media> [--transcript <text|file>] [--align-model <path>] [--start <seconds>]
```

`--start` is the window offset when aligning a slice; reporting stays on the
absolute timeline. Weights are auto-detected via `$SENTENCE_VIDEO_ALIGN_MODEL`,
next to the executable, or `assets/models/wav2vec2-base-960h.safetensors`
(`NO_WEIGHTS_HELP` explains otherwise).

---

## 8. Session build: media → cues

### 8.1 Which subtitle a session uses

`build_session` resolves the cue source in this order (`locate_subtitle`):

1. an explicit `subtitle_path` from the API/CLI;
2. **`<stem>_full_sentence.srt`** next to the media
   (e.g. `lecture.mp4` → `lecture_full_sentence.srt`);
3. otherwise **transcribe** — and the result is _saved as (2)_, so the next run
   reuses the file.

A legacy adjacent `<stem>.srt` / `<stem>.vtt` is **deliberately ignored**: the
canonical file is the single source of truth, so a missing
`_full_sentence.srt` always triggers transcription rather than silently adopting
an unrelated subtitle. A hand-made subtitle must be passed explicitly.

Consequences:

- The first run on a subtitle-free video pays the transcription cost and writes
  an inspectable, editable `.srt` next to it; later runs just parse it (no model
  load).
- Editing the generated `.srt` is honoured: the SQLite cache key includes the
  subtitle's **mtime**, so a changed file rebuilds the session.
- The DB cache is checked _before_ transcribing, so re-selecting an unchanged
  video is instant. `POST /api/session?force=1` bypasses it.
- Sessions record `source_format = "asr"` when transcribed and `"srt"`/`"vtt"`
  when parsed; the written file means a re-parse reports `"srt"`.

### 8.2 Speech-to-text (whisper.cpp, embedded)

A media file with **no subtitle** is segmented by transcribing its audio:

- whisper.cpp is **statically linked** and the model is **embedded in the
  executable** (`include_bytes!`) so end users need no installs/downloads.
- Artifact: **~86 MB** (was ~25 MB without the model).
- Runtime flags: `--asr-model <path>`, `--asr-language <code>` (default `en`).
- `asr::transcribe` runs whisper over the **whole buffer in one call** — so a
  _partially transcribed_ file cannot be resumed (see §10).

### 8.3 Word/cue timing accuracy (whisper side)

- Timings come from whisper.cpp's **DTW** token timestamps (`dtw = 1` in the
  startup log), preset derived from the model file name.
- DTW is still ~0.2 s late at phrase boundaries, so two audio-driven corrections
  run over the samples already decoded
  (`asr::snap_word_starts_to_onsets`, 20 ms RMS envelope, adaptive thresholds;
  `SENTENCE_VIDEO_ASR_DEBUG=1` reports counts):
  - **Starts** snap to the nearest sharp energy rise (a real onset — a breath
    ramps gently, so it is not mistaken for speech).
  - **Ends** are trimmed to the **speech offset** when a pause follows, rather
    than kept flush with the next word's start (ending exactly on the next onset
    put the boundary on the first phoneme of the following sentence).
- Measured on a 4-minute video: cue ends that immediately voice the next word
  dropped **22/22 → 7/22**; the remaining 7 have _no_ pause (audio genuinely
  continuous). The player polls the media clock every **20 ms** so it stops
  inside the pause (a 60 ms timer allowed ~40 ms of overshoot).
- With `asr-align` on, the refinement in §7 replaces these timings.

### 8.4 Sentence segmentation

`build_cues` splits **only** at sentence-final punctuation
(`segmenter::is_sentence_end` → `.` `!` `?`). There is **no duration or word
cap** — an earlier 12 s / 40-word cap sliced sentences mid-thought, so cues are
always complete sentences. `segmenter::finalize` then re-indexes, clamps times,
and fills `words` + `source_path`.

The split is considered correct and is not to be changed (`build_cues` /
`segmenter.rs` are explicitly out of scope in the resumable-processing plan).

---

## 9. Dubbing (TTS) pipeline

A **Dub audio** checkbox beside _Align timings_ makes session creation also
generate a replacement voice track, which playback uses instead of the video's
audio. **No new video file** — the session stays on the original media, and a
voice WAV is generated server-side.

### 9.1 Decisions

| Question    | Decision                                                                                                                                                                                                              |
| ----------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TTS engine  | **Both selectable**: `espeak-ng` (offline default) and `edge-tts` (neural). Neither adds a cargo dependency.                                                                                                          |
| Timing fit  | **Re-synthesise faster, never truncate** — the v4.1 `RespeedSynthesizer` + `PlaceStitcher` behaviour.                                                                                                                 |
| Granularity | **Sentence level** (one call per cue): _align at word level, synthesise at sentence level, place at word level_. Per-chunk synthesis was v2 and is a failed design (46% vocoder clicks, flat prosody, omitted words). |
| Output      | A generated voice **audio track**; no muxing into a video.                                                                                                                                                            |

### 9.2 Placement rules (ported)

Per cue: `slot = cue.end - cue.start`, `pause_after = next.start - cue.end`
(0 for the last cue), `available_room = slot + pause_after`.

- Synthesise the whole sentence at the natural rate and measure it.
- Re-synthesise once at `speedup = clamp(tts / available_room, 1.0, 2.0)` **iff**
  `tts > available_room + 0.1 s` (espeak → `-s wpm × speedup`, edge-tts →
  `--rate=+N%`). Audio within 100 ms of the room counts as fitting; **never
  speed up a sentence that fits**.
- `is_last` only removes `pause_after` from the room — it does **not** exempt the
  sentence from re-synthesis.
- Shorter than the room → place it, leave natural silence (compress-only, never
  expand, nothing time-stretched).
- Still overflowing after the 2× cap → place **as is** and overlap the next
  (`PlaceStitcher` overwrites; no boundary truncation, no zero-crossing cut —
  that was v4's `GentleStitcher`, and v2's hard truncation is the design being
  avoided).
- Quality: fade-in 5 ms always; fade-out 5 ms **only** when the audio reaches its
  own slot end (an unconditional fade-out attenuated final consonants);
  per-sentence RMS normalisation toward −20 dBFS with a 10× gain cap +
  `tanh` soft-clip; peak-normalise the mix to 0.95 only if it clips.
- Synthesised concurrently (`std::thread::scope`, ~8 at a time) — the reference
  measured 45 s → 8 s for 23 sentences on the network-bound engine.
- A missing engine is a clear **422**
  (`edge-tts not found — install it, or pick espeak-ng`), never a silently
  un-dubbed session.

### 9.3 Playback

The session stays on the original media and reports `"dubbed": true`. Playback
mutes the existing `<video>` (original file, HTTP range → seeking still works)
and plays the track from a second element, mirroring play/pause/seek/rate/volume
with drift corrected in the same **20 ms** media-clock poll.
`/api/session/{id}/audio` serves the **dubbed** slice when a track exists, so
"play one sentence" uses the new voice too.

### 9.4 `edge-tts` detail

Externally installed CLI used only when selected (installed user-locally here via
`uv tool install edge-tts`). The proxy must be passed explicitly
(`--proxy`) — edge-tts's WebSocket **ignores `https_proxy`**.

---

## 10. Resumable processing + the Manage page

### 10.1 The problem

Everything expensive was all-or-nothing:

- Dubbing synthesised every cue **in memory**; a mid-run failure was silently
  swallowed and a killed run re-did every TTS call.
- Transcription + alignment re-ran from zero on interruption, even though
  alignment is windowed and is the CPU bottleneck.

The Python project already solved this ("the filesystem **is** the
checkpoint"), so the mechanism was ported to Rust.

### 10.2 Checkpoints

All under `data_dir` (never next to the media — that dir may be read-only):

| Artifact          | Path                                                                 | Purpose                      |
| ----------------- | -------------------------------------------------------------------- | ---------------------------- |
| Raw whisper words | `data_dir/work/{fnv1a(media_path\|len\|mtime)}/words.json`           | skip whisper on a re-run     |
| Alignment windows | `…/align/w{first}-{last}.json`                                       | skip a 30 s alignment window |
| Sentence audio    | `data_dir/dubbed/{session_id}.tts/sent_{idx:04d}_{texthash:08x}.wav` | skip a TTS call              |

Three details that matter:

- **The TTS dir is keyed by session id, not the media hash** — the media hash
  cannot see a subtitle edit (`db.rs::SourceKey` includes `subtitle_mtime`
  precisely because such an edit _must_ invalidate). Mirrors the existing
  reasoning for `AppState::dubbed_track_path`.
- **`texthash`** in the filename means re-indexing or a subtitle edit can only
  invalidate the sentences that actually changed.
- **No rate suffix** — one file per cue is idempotent because on load the cached
  clip is measured and `speedup_for` re-run; a pass-2 clip already fits, so it is
  never respeeded twice.

The recipe (voice + engine + respeed constants) is recorded in the TTS dir, so a
`--dub-voice` change cannot silently reuse another voice's audio.

**Honesty about ASR:** `asr::transcribe` runs whisper over the whole buffer in
one call, so a _partially transcribed_ file cannot be resumed. The raw-words
checkpoint buys exactly one boundary — "ASR done, alignment/dubbing
interrupted". Resumable transcription is **not** claimed; retry-N applies per
alignment window and per sentence.

### 10.3 Lifecycle

- **Success** → write `dubbed/{session_id}.wav`, delete only the `.tts/` dir;
  keep `work/*/align/` (cheap).
- **Failure** → keep everything, record the failures, leave the state retryable.
- `DELETE` removes the checkpoint dirs too, or they leak forever.

### 10.4 Dubbing becomes asynchronous

`ensure_dubbed_audio` used to run **synchronously inside `POST /api/session`**,
blocking the request ~40 s. It now runs as a background job
(`dub_jobs: Arc<Mutex<HashMap<String, DubProgress>>>`, next to the existing
`playable_jobs`), surfaced as `"dubbing"`. Unlike `playable_jobs`, a failed dub
**must be re-runnable** — the guard is on the job _state_, not on key presence.
`build_backend` is probed **synchronously before** spawning, so a missing engine
still returns `422`.

Retry shape: `synthesize_cues(backend, cues)` stays as a thin wrapper (no
checkpoint, swallow failures) so existing tests pass untouched; a checkpointed
variant `synthesize_cues_checkpointed(backend, cues, cp, max_retries, on_progress)`
does pass 1 (cache-checked) → `for _ in 0..=max_retries` re-running only the
still-failing jobs → the overflow/`speedup_for` decision for pass 2 (also
cache-checked and retried, written over pass 1). **`MAX_SYNTH_RETRIES = 2`** —
three attempts, as Python. Python's `validate_audio` is ported as
`validate_sentence(samples, sr, text)` (0.3–60 s, peak ≥ 0.001, plausible
chars/sec rate) and applied to **fresh** output too — that is what makes a retry
meaningful. The chars/sec ceiling is scaled by the requested speedup, or a 2×
respeeded sentence false-positives.

### 10.5 Manage page

A table over `GET /api/sessions`: duration, cue count, dubbed?, dub progress,
browser-playable?, progress-through-the-video — plus a **"⏳ Processing" panel**
fed by `GET /api/builds` (so a video being built is visible before it is in the
session list). Actions: **add video** (`POST /api/pick-file` → `POST
/api/session`), **start/resume dubbing**, **re-transcode**, **open in player**,
**delete**.

Design split: **Player = watch; Manage = CRUD + add video + watch the process.**
Adding a video uses the **existing server-side file dialog**, not a browser
upload.

Because a build is synchronous (the session row only reaches the DB at the end),
`BuildProgress` is what stops a long build from looking like a hang — see §6.1.

---

## 11. Frontend

React + Vite + TypeScript, in `EnglishLearning/frontend/web/`. Dev server on
**:5173**, proxying `/api` → **:8018**.

### 11.1 Routes (`router/routes.tsx`)

- Backed by the Rust server (linked in nav): `/` (Home), `/player`, `/manage`.
- **Legacy Python/FastAPI routes, hidden from the nav** but reachable by URL:
  `/subtitle`, `/subtitle-video`, `/video-dub`, `/dubbing-studio`, `/progress`,
  `/reading`, `/assistant`.
- `/login`, `/signup` (outside the app shell).

Nav (`router/navigation.tsx`): Home, Player, Manage — only Rust-backed routes.

### 11.2 Feature directories

`features/`: `home`, `player`, `manage`, `auth`, `subtitle`, `subtitle-video`,
`video-dub`, `dubbing-studio`, `progress`, `reading`, `assistant`.

`lib/server-api.ts` is the shared API layer: `readError`, `pickFileOnServer`,
`listSessions`, `listBuilds`, `createSession`, `getSession`, `deleteSession`,
`getDub`/`startDub`, …

### 11.3 Player page

State for `align` / `dub` + `dubEngine`; the body carries `dub`/`dub_engine`
**only when ticked** (an unticked box must never override a server default).
Polls `dubbing.state` while running (same 3 s interval pattern as the transcode
poll).

### 11.4 Known pre-existing type errors (do NOT chase these)

`pnpm run build:web` runs `tsc -b` first, which reports **5 errors** predating
recent work. `vite build` alone succeeds, and the dev server does not typecheck,
so these do not block running the app:

- `src/features/assistant/index.tsx` — CopilotKit `labels.title` not in the type
- `src/features/reading/index.tsx` — unused `e` parameter
- `src/features/subtitle/index.tsx` — unused `saving` / `setSaving`
- `src/providers/auth-context.tsx` — unused `REFRESH_KEY`

---

## 12. The agent service (`rust/src/agent/`)

The Rust port of the Bun `AI-Demo/agent-runtime` and Python `AI-Demo/backend`,
behind the default-on `agent` feature. It is the CopilotKit / **AG-UI** service,
served on **:4000** by `sentence-video agent`, and it is why one binary serves
both the media player and the agent — matching the single-executable goal.

- `ag-ui` owns the event vocabulary, SSE framing, the `Agent` trait and the axum
  mount; this crate supplies only the model client, the tool runtime and the
  A2UI payloads.
- Submodules: `config.rs`, `engine/{mod,prompt,state}.rs`, `llm/{mod,openai}.rs`
  (OpenAI-compatible client), `protocol/a2ui.rs`, `runtime/middleware.rs`,
  `tools/{builtin,math}.rs`.
- Git history for it: `316898e` → `7bfba15` → `5b410d8`.

---

## 13. Chronology (how the project got here)

| Phase | What was built                                                                   | Evidence                                                  |
| ----- | -------------------------------------------------------------------------------- | --------------------------------------------------------- |
| 1     | Monorepo init; Python/FastAPI backend + React web; speckit scaffolding           | `63cdbcf`, `.specify/`                                    |
| 2     | Rust backend foundations: SQLite persistence, native file picker, backend routes | `641d070`                                                 |
| 3     | Full speech-to-text pipeline with whisper.cpp (embedded model)                   | `d4ed1f6`                                                 |
| 4     | Build-environment battles: FFmpeg clone, nasm, libclang — then system packages   | `EnglishLearning/build_issue.md`, `AGENTS.md`             |
| 5     | Agent service: AG-UI protocol SDK, engine/runtime rework                         | `316898e`, `7bfba15`, `5b410d8`                           |
| 6     | Dubbing (TTS): engines, respeed fitting, `PlaceStitcher`, player checkbox        | `.trae/documents/dub-audio-checkbox.md`                   |
| 7     | Resumable processing (checkpoints, retry-N) + Manage page                        | `.trae/documents/resumable-processing-and-manage-page.md` |
| 8     | wav2vec2 forced alignment (hand-written)                                         | `rust/src/align/`                                         |
| 9     | Alignment performance: `opt-level=3`, `ROW_BLOCK=8`, conv channel split          | §7.9                                                      |

Note: the working tree is **dirty** — a large set of modified files plus new
`.trae/documents/`, `features/manage/`, `lib/server-api.ts`,
`scripts/fetch-align-model.sh` are uncommitted on `master` (last commit
`5b410d8`).

---

## 14. Where to look for what

| Question                                                               | File                                                      |
| ---------------------------------------------------------------------- | --------------------------------------------------------- |
| How do I build / why does FFmpeg hang?                                 | `AGENTS.md`, `EnglishLearning/build_issue.md`             |
| The word-timestamp architecture (and what of it this repo does not do) | `word_timestamps_pipeline.md`                             |
| Why dubbing works the way it does                                      | `.trae/documents/dub-audio-checkbox.md`                   |
| Why checkpoints exist and where they live                              | `.trae/documents/resumable-processing-and-manage-page.md` |
| The alignment forward pass                                             | `rust/src/align/wav2vec2.rs`                              |
| The alignment hot kernels                                              | `rust/src/align/tensor.rs`                                |
| Windowing + scheduling                                                 | `rust/src/align/mod.rs`                                   |
| CTC Viterbi alignment                                                  | `rust/src/align/ctc.rs`                                   |
| The build orchestrator + stages                                        | `rust/src/pipeline/transcribe.rs`                         |
| HTTP routes                                                            | `rust/src/app.rs`                                         |
| TTS + placement rules                                                  | `rust/src/pipeline/synthesize.rs`                         |

---

## 15. Open items / known limitations

- **Multi-window tail.** When fewer windows remain than workers, cores idle
  (§7.9). Not addressed.
- **The conv's arithmetic.** It is compute-bound; `im2col + GEMM` is the only
  remaining way to make it _arithmetically_ faster (stride-2 gathers currently
  block vectorization). Not attempted.
- **ASR is not resumable** — whisper runs over the whole buffer in one call
  (§10.2).
- **5 pre-existing `tsc -b` errors** (§11.4) — not regressions.
- **`rsproxy.cn` is unreachable**, so crates.io is used directly; restore
  `~/.cargo/config.toml.bak-dsh` when the mirror recovers.
- **Dev servers must be restarted** to pick up the new binary after an
  alignment/build change.
- **`WINDOW_SECS` was left at 30 s** deliberately; changing it was never
  authorised.
