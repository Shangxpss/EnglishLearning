# Resumable processing + Manage page

## Context

Two problems.

**1. Nothing is resumable.** The expensive per-video work is all-or-nothing:

- Dubbing (`synthesize_track`) synthesizes every cue **in memory**. If sentence 17 of 23 fails, the failure is silently swallowed (its slot is left silent) and there is no way to retry just that sentence — let alone the ones that never ran because the process died. A killed run re-does all 23 TTS calls.
- Transcription + alignment re-run from zero on any interruption. Alignment is windowed (`align::plan_windows`, 30 s windows) and is the CPU bottleneck, yet the windows are recomputed every time.

The Python project already solves this. Its `ParallelSynthesizer.synthesize(cues, tts_dir)` writes one WAV per sentence (`sent_{ci:04d}.wav`), and at [parallel_synth.py:187-203](file:///home/shang/Desktop/project/EnglishLearning/EnglishLearning/python/backend/app/services/sync/parallel_synth.py#L187-L203) an existing file is **validated and reused rather than re-synthesized**; a `for attempt in range(max_retries + 1)` loop then re-runs only the still-failing sentences. At [dubbing.py:261-282](file:///home/shang/Desktop/project/EnglishLearning/EnglishLearning/python/backend/app/api/dubbing.py#L261-L282) the video-level check skips a video whose output exists — *"the filesystem IS the checkpoint"* — and the aligner is handed a `checkpoint_dir`.

Goal: port that mechanism to the Rust server (per-sentence TTS checkpoints, per-window alignment checkpoints, retry-N-then-continue), and add a **Manage** page for CRUD over processed videos and their processing state.

**Sentence splitting is not touched.** `pipeline/transcribe.rs::build_cues` and `segmenter.rs` stay exactly as they are — the user confirmed the current split is correct.

Decisions taken from the user:

- Resumable scope: **dubbing (TTS) + transcription/alignment**.
- On failure: **retry each sentence N times, then continue**; record which failed; a resume retries only those.
- Manage page is **separate from the Player** (Player = watch). Manage = CRUD + add video + watch the process.
- Adding a video uses the **existing server-side file dialog** (`POST /api/pick-file`), not a browser upload.

## Design

### Checkpoints

All under `data_dir` (never next to the media — that directory may be read-only; `subtitle::write_srt` already tolerates a failure there).

| Artifact | Path | Purpose |
| --- | --- | --- |
| Raw whisper words | `data_dir/work/{fnv1a(media_path\|len\|mtime)}/words.json` | skip whisper on a re-run |
| Alignment windows | `data_dir/work/{fnv1a(media_path\|len\|mtime)}/align/w{first}-{last}.json` | skip a 30 s alignment window |
| Sentence audio | `data_dir/dubbed/{session_id}.tts/sent_{idx:04d}_{texthash:08x}.wav` | skip a TTS call |

Three corrections that matter:

- **The TTS dir is keyed by session id, not by the media hash.** The media hash cannot see a subtitle edit: editing `<stem>_full_sentence.srt` does not change the media mtime, and `db.rs::SourceKey` includes `subtitle_mtime` precisely because such an edit *must* invalidate. This mirrors the existing reasoning in `AppState::dubbed_track_path` ("keyed by session id … the track can never go stale").
- **`texthash`** is a hash of the cue's text, in the filename, so re-indexing or a subtitle edit can only invalidate the sentences that actually changed.
- **No rate suffix.** Python deletes the pass-1 clip before pass 2 and reuses one name per cue; one file per cue is idempotent because on load we measure the cached clip and re-run `speedup_for` — a pass-2 clip already fits its slot, so it is never respeeded twice.

Also record the recipe (voice + engine + respeed constants) in the TTS dir; a `--dub-voice` change must not silently reuse another voice's audio. Same idea as the existing `PLAYABLE_RECIPE` constant in `app.rs`.

**Honesty about ASR:** `asr::transcribe` runs whisper.cpp over the whole buffer in one call, so a *partially transcribed* file cannot be resumed. A raw-words checkpoint written right after ASR buys exactly one boundary — "ASR done, alignment/dubbing interrupted". That is worth having, but the plan does not claim resumable transcription; retry-N applies per alignment window and per sentence.

### Lifecycle

- On **success** write `dubbed/{session_id}.wav` and delete only the `.tts/` dir (mirrors Python's `rmtree(tts_dir)`). Keep `work/*/align/` — it is cheap and mirrors Python keeping `out_dir`.
- On **failure** keep everything, record the failures, and leave the state retryable.

### Dubbing becomes asynchronous

`ensure_dubbed_audio` currently runs **synchronously inside `POST /api/session`**, blocking the request for ~40 s. That has to change for a progress-reporting Manage page, and Python already does it (background thread + polled `job_id`).

Add `dub_jobs: Arc<Mutex<HashMap<String, DubProgress>>>` next to the existing `playable_jobs`, surfaced in the session JSON as `"dubbing"`. Unlike `playable_jobs`, a failed dub **must be re-runnable** — guard on the job *state*, not on key presence, because `playable_jobs` blocks on presence forever.

Probe `build_backend` **synchronously before** spawning, so a missing engine still returns `422` exactly as today.

### Retry loop

Keep `synthesize_cues(backend, cues) -> Vec<(usize, Vec<f32>)>` as a thin wrapper (no checkpoint, swallow failures) so all existing tests pass untouched. Add a checkpointed variant:

```rust
pub struct SynthProgress { pub done: usize, pub total: usize }
pub struct SynthFailures { pub failed: Vec<(usize, String)>, pub synthesized: usize }

pub fn synthesize_cues_checkpointed(
    backend: &dyn TtsBackend,
    cues: &[Cue],
    cp: Option<&Path>,
    max_retries: usize,
    on_progress: impl FnMut(SynthProgress),
) -> (Vec<(usize, Vec<f32>)>, SynthFailures)
```

Pass 1 jobs = cues with no valid cached clip → `run_concurrent`; then `for _ in 0..=max_retries` re-running only still-failing jobs. Then the existing overflow decision (`available_room` / `speedup_for`) selects pass-2 jobs, which are also cache-checked, retried, and written over the pass-1 file. `MAX_SYNTH_RETRIES = 2` (three attempts, as Python).

Port Python's `validate_audio` as `validate_sentence(samples, sr, text) -> Result<(), String>` (0.3–60 s, peak ≥ 0.001, a plausible chars/sec rate) and apply it to **fresh** output too, not only to cache — that is what makes a retry meaningful. The chars/sec ceiling must be scaled by the requested speedup, or a 2× respeeded sentence false-positives.

## Files

**New**

- `rust/src/pipeline/checkpoint.rs` — always-compiled atomic write (temp + rename), recipe-tagged load, `texthash`. Deliberately *not* inside `align/mod.rs`, which is `#[cfg(feature = "asr-align")]` and therefore untested by a default `cargo test`.
- `frontend/web/src/lib/server-api.ts` — `readError` + `pickFileOnServer`, extracted from `features/player/index.tsx` where they are currently module-private, so Manage can reuse them.
- `frontend/web/src/features/manage/index.tsx` — the Manage page.

**Changed**

- `rust/src/pipeline/synthesize.rs` — validation, retry, the checkpointed API, the thin wrapper.
- `rust/src/align/mod.rs` — `refine_words(aligner, samples, words, checkpoint: Option<&Path>)`. Read before spawning the window work and write inside the existing `std::thread::scope` worker loop (temp + rename), because that is where window identity and failure are both known.
- `rust/src/pipeline/transcribe.rs` — thread `checkpoint` through `refine_timings` (**both** cfg variants), `transcribe_words_with`, `transcribe_and_save`; write the raw-words JSON atomically right after ASR. `transcribe_words` passes `None` (CLI path).
- `rust/src/app.rs` — `dub_jobs`, `spawn_dub_async`, new routes, `"dubbing"` in the session JSON, dub state in the `/api/sessions` payload, checkpoint cleanup on delete.
- `frontend/web/src/features/player/index.tsx` — poll while `dubbing.state` is running (same 3 s interval pattern as the existing transcode poll).
- `frontend/web/src/router/routes.tsx` + `router/navigation.tsx` — mount `manage`, add one nav item.

## HTTP

| Route | Behaviour |
| --- | --- |
| `POST /api/session/{id}/dub` | body `{engine?, retry_failed?}` → `200 {state,done,total}`. Probes the engine first (`422` if missing), then synthesizes in the background. No-op while running; allowed from `failed`. |
| `GET /api/session/{id}/dub` | `{state:idle\|running\|done\|failed, stage:synth\|stitch, done, total, failed:[{index,text,error}], error}` |
| `GET /api/session/{id}` | gains `"dubbing"` beside the existing `dubbed` / `video`. `dubbed` stays a boolean — the player reads it. |
| `GET /api/sessions` | each summary enriched with `dubbed` and dub state. Done in `app.rs` serialization, **not** in `db.rs`; `Db` must stay free of job state. |
| `DELETE /api/session/{id}` and `/api/sessions` | also remove the checkpoint dirs, or they leak forever. |

No resume-transcribe route in v1: a session only exists after transcription succeeded, so the alignment checkpoints simply make `POST /api/session?force=1` fast.

## Manage page

A table over `GET /api/sessions`: duration, cue count, dubbed?, dub progress, browser-playable?, progress-through-the-video. Actions: **add video** (`POST /api/pick-file` → `POST /api/session`), **start/resume dubbing**, **re-transcode** (remove the cached `h264/{hash}.mp4`, then re-open the session to trigger `ensure_playable_async`), **open in player**, **delete**.

## Tests

New/extended inline `#[cfg(test)]` modules:

- `checkpoint.rs`: round-trip; a recipe mismatch is ignored; a half-written temp file is not mistaken for a checkpoint.
- `synthesize.rs`: with a fake backend and a temp dir — a cache hit makes zero backend calls; a flaky cue succeeds within three attempts; a permanently failing cue is *listed* but the track is still produced; a near-silent cached clip is re-synthesized; respeed overwrites in place.
- `app.rs`: the progress JSON shape and the retry-from-`failed` guard.
- `align/mod.rs`: `plan_windows` / `merge_window` tests need no change — the signature gains an optional argument only.

## Verification

1. `bash rust/scripts/cargo-shim.sh check --manifest-path rust/Cargo.toml --features asr-align` — clean; then `cargo test --features asr-align`.
2. **Resume proof (the point of the work):** dub the Kafka lecture, then delete a *later* checkpoint by hand — e.g. `rm ~/.local/share/sentence-video/dubbed/*.tts/sent_0018_*.wav` — and press Resume. The log must resynthesize that one sentence and *reuse* the rest, with the other `sent_*` files' mtimes unchanged. Previously this re-ran all 23.
3. **Failure proof:** point `--dub-engine` at a broken command so every call fails; confirm the failure list is reported, the state is `failed`, and a later run with a working engine completes.
4. **Invalidation proof:** edit `<stem>_full_sentence.srt`, rebuild the session, and confirm the changed sentences are re-synthesized while unchanged ones are still cached.
5. **Alignment:** with `asr-align`, interrupt a long file mid-alignment, re-run, and confirm the completed `align/w*.json` windows are not recomputed.
6. Frontend: `npx tsc -b` (only the 5 documented pre-existing errors), then load `/manage` and walk add → dub → progress → resume → delete. Delete must leave no `dubbed/*.tts` or `work/*` debris behind.

## Out of scope

- Any change to sentence splitting / `build_cues` / `segmenter.rs`.
- Browser multipart upload (server-side dialog instead, per the user's choice).
- Batch folder jobs (Python's Dubbing Studio `folder → _dubbed/`); the Rust server has no batch concept and this plan does not add one.
- Cancel/abort of an in-flight dub (Python has no cancel either).
- Editing `AGENTS.md` — not needed unless the new routes should be documented there.