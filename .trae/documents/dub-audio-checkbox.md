# Dub audio: player checkbox + server-generated voice track (no video rewrite)

## Context

The player page (`frontend/web/src/features/player/index.tsx`) can create a session from a video,
transcribe it with whisper, and optionally refine word timings with wav2vec2 (`Align timings` → `align:
true`).

The legacy Python backend has a mature dubbing pipeline whose design is documented in
`python/backend/docs/TTS_SYNC_MECHANISM.md`. The Rust rewrite only half covers it: `pipeline::dub` +
`synthesize.rs` exist and are reachable from the CLI (`sentence-video dub …`), but nothing exposes dubbing
over HTTP, the fitting rules are missing, and the UI cannot ask for it.

Goal: a **Dub audio** checkbox beside *Align timings* that makes session creation also generate a
replacement voice track, which playback uses instead of the video's own audio (no new video file).

## Decisions

| Question | Decision |
| --- | --- |
| TTS engine | **Both selectable**: `espeak-ng` (offline default) and `edge-tts` (neural voices). A `<select>` beside the checkbox picks the engine. |
| Timing fit | **Re-synthesise at a faster rate, never truncate** — the v4.1 `RespeedSynthesizer` + `PlaceStitcher` behaviour, not v2's stretch-and-truncate. |
| Synthesis granularity | **Sentence level** (one call per cue), matching TTS_SYNC_MECHANISM Lesson 1: *align at word level, synthesise at sentence level, place at word level*. Per-chunk synthesis was v2 and is explicitly a failed design (46% vocoder clicks, flat prosody, omitted words). |
| Output / playback | **No video file.** The session stays on the original media; a voice **audio track** is generated server-side and the player streams it instead of the video's audio. |

`edge-tts` is an externally installed CLI used only when selected, so the default keeps the offline
single-binary story. Neither engine adds a **cargo** dependency.

## Behaviour (ported rules)

1. Tick **Dub audio**, pick an engine, choose a video → `POST /api/session` gains `"dub": true,
   "dub_engine": "espeak-ng" | "edge-tts"`.
2. Cues are resolved exactly as today (transcribe → `<stem>_full_sentence.srt`, `align` applied first if
   that box is ticked, since word-accurate times drive placement).
3. Per cue:
   - `slot = cue.end - cue.start`; `pause_after = next_cue.start - cue.end` (0 for the last cue);
     `available_room = slot + pause_after`.
   - Synthesise the **whole sentence** at the natural rate and measure the audio it returned.
   - Re-synthesise once at `speedup = clamp(tts / available_room, 1.0, 2.0)` **iff** `tts > available_room +
     0.1 s` (espeak → `-s wpm × speedup`, edge-tts → `--rate=+N%`) — a genuine collision with the next
     sentence. Audio within 100 ms of the room counts as fitting. Never speed up a sentence that fits.
   - The `is_last` flag only removes `pause_after` from the room — it does **not** exempt the sentence from
     re-synthesis. The last cue has *less* room (its own slot only), so it can perfectly well be respeeds;
     it just has no following sentence to overflow into.
   - Shorter than the room → place it, leave natural silence (compress-only, never expand, nothing is
     time-stretched).
   - A sentence still overflowing after the 2× cap is placed **as is** and overlaps the next one — v4.1's
     `PlaceStitcher` simply overwrites, so there is no boundary truncation and no zero-crossing cut
     (that was v4's separate `GentleStitcher`; v2's hard `audio[:target_samples]` truncation is the
     design being avoided).
4. Quality steps taken from the reference: fade-in 5 ms always, fade-out 5 ms **only** when the audio
   reaches its own slot end (v3.1a — an unconditional fade-out attenuated final consonants), and
   per-sentence RMS normalisation towards −20 dBFS with a 10× gain cap + `tanh` soft-clip so the dub
   sounds like one consistent speaker. The mix is peak-normalised to 0.95 only if it clips.
5. Sentences are synthesised concurrently (bounded, ~8 at a time) with `std::thread::scope` — the reference
   measured 45 s → 8 s for 23 sentences on the network-bound engine.
6. The track is written to the **app data dir** as a WAV (≈20 MB per 4 minutes) — not next to the user's
   media and **not** muxed into a video.
7. The session is created on the **original** media and reports `"dubbed": true`. Playback mutes the
   existing `<video>` (original file, HTTP range support → seeking still works) and plays the track from a
   second element, mirroring play/pause/seek/rate/volume with drift corrected in the existing 20 ms
   media-clock poll.
8. `/api/session/{id}/audio?start=…&end=…` (the per-sentence clip route) serves the **dubbed** slice when
   a track exists, so "play one sentence" uses the new voice too.
9. A missing engine is a clear `422` (`edge-tts not found — install it, or pick espeak-ng`), never a
   silently un-dubbed session.

## Changes

**Rust — `rust/src/pipeline/synthesize.rs`** (core work; extends the existing trait/helpers)
- `TtsBackend::synthesize` gains a rate ratio: `fn synthesize(&self, text: &str, speedup: f64) ->
  Result<Vec<f32>, String>`. `CommandTts` → `-s <wpm × speedup>` (clamp 80–450); `SilenceTts` scales its
  beep length by `speedup` so pass 2 is observable with no engine; both temp-file paths become unique per
  call (`temp_path`) because synthesis runs on several threads and a per-process name let two workers
  clobber each other's file.
- New `EdgeTts` backend: shells out to the `edge-tts` CLI (`--voice`, `--rate=+N%`, `--proxy`, temp
  `--write-media`), decoding through the existing `media::decode_to_f32`. Same error shape as `CommandTts`.
  The proxy must be passed explicitly (TTS_SYNC_MECHANISM Lesson 5: edge-tts's WebSocket ignores
  `https_proxy`).
- `synthesize_cues` = the two-pass rule (concurrent pass 1 → `respeed` list → pass 2, keeping pass-1 audio
  if a re-synthesis fails); `stitch_timeline` = v4.1 `PlaceStitcher` (RMS normalise, conditional fades,
  overwrite placement); `synthesize_track` signature unchanged, so the CLI `dub` subcommand and the server
  share one implementation.
- `build_backend(engine, voice, rate, proxy) -> Result<Box<dyn TtsBackend>, String>` as the server's seam;
  it probes the executable up front so a missing engine is an actionable error.

**Rust — `rust/src/pipeline/mod.rs`**
- New `DubConfig { engine, voice, rate_words_per_min, proxy }` + `init_dub`/`dub_config` (`OnceLock`),
  mirroring `asr.rs`. `dub(...)` (CLI: synthesises **and** muxes an mp4) keeps its signature and now
  inherits the new placement rules through `synthesize_track`.

**Rust — `rust/src/app.rs`**
- `AppState` gains `data_dir` (so the track has somewhere to live) and `ensure_dubbed_audio`, which builds
  the backend from `DubConfig`, synthesises the track and returns `422` on a missing engine.
- `create_session` parses `dub` / `dub_engine` and returns `"dubbed"` in the JSON.
- New `GET /api/session/{id}/dubbed-audio`: streams the track with the same range-support loop as
  `/media`. The path is **derived from the session id** (`data_dir/dubbed/{id}.wav`), so no DB migration
  and no staleness — a different media/subtitle/align setting yields a different session id and therefore
  a different track. `/audio` prefers the track; the session JSON reports `dubbed` from file existence.
  Deleting a session deletes its track.

**Rust — `rust/src/main.rs`**
- Play mode gains `--dub-engine <espeak-ng|edge-tts>`, `--dub-voice <name>`, `--dub-proxy <url>` plus
  `SENTENCE_VIDEO_DUB_ENGINE` / `_VOICE` / `_PROXY`, mirroring the `--align` pattern; usage text updated.
  (`--silence` remains the no-engine test path.)

**Frontend — `frontend/web/src/features/player/index.tsx`**
- State `dub` + `dubEngine`; body gains `dub` / `dub_engine` (only when ticked, so an unticked box never
  overrides a server default).
- **Dub audio** checkbox beside *Align timings* + engine `<select>`, reusing the existing checkbox markup;
  help text and the header route doc updated.
- Playback: when the session is dubbed, mute the video and drive the track in sync (mirror
  play/pause/seek/rate/volume; correct drift in the 20 ms poll).

## Verification

`espeak-ng` is absent here; `edge-tts` was installed user-locally (`uv tool install edge-tts`), so the
real-voice path could be checked end-to-end.

1. **No engine needed** — `--silence` (`SilenceTts`) exercises placement, fades, normalisation, track
   serving, and playback wiring; `cargo test` (98 passing with `asr-align`) covers the rules: a sentence
   inside its slot is never re-synthesised, one colliding with the next cue is re-synthesised, a cue with a
   long pause after it is *not*, the last cue gets no pause room, a short sentence is not stretched, the
   fade-out is skipped when it does not reach the slot end, and concurrent calls never share a temp file.
2. `cargo check` (both feature sets) + `cargo test`; `tsc -b` (only the 5 pre-existing errors) + `vite
   build`; browser click-through: request body carries `dub`/`dub_engine`, `/dubbed-audio` is requested,
   the video element is muted, and the track's `currentTime` tracks it after a seek.
3. **Real voice, measured** — 10 s fixture (`test.mp4` + `test_full_sentence.srt`, cues 0.5–3.5, 3.5–5.0,
   6.0–9.5) POSTed with `dub_engine: "edge-tts"`:

   - cue #2 (words `this slot is far too short for this whole sentence`) renders 3.79 s naturally against
     `room = 1.5 + 1.0 = 2.5 s`, and the log confirms the second pass:
     `cue #2 tts=3.79s room=2.50s → resynthesising at 1.52x`.
   - the written track is 11.0 s / 24 kHz / mono and its voiced spans land at each cue's start —
     `0.70–2.22`, `3.64–5.42`, `6.20–8.34` — i.e. sentence 1 at 0.5 s, the respeeds sentence fully inside
     its 6.0 s boundary, sentence 3 at 6.0 s.
   - `GET /api/session/{id}` reports `"dubbed": true`; `/dubbed-audio` returns `200 audio/wav` (528 044
     bytes) and `206` with `Content-Range` for a `Range` request; `/audio?start=3.5&end=4.5` returns a
     1.0 s WAV slice of the **dubbed** source.

## Not included

Writing a dubbed video file (the CLI `dub` subcommand already does that on demand), on-the-fly muxing into
the media stream (it would break HTTP range seeking), room-tone fill, dithering, `remove_watermark`,
ChatTTS, folder/batch dubbing, an async job + progress API, and a voice-picker list (voice stays a server
default; `dub --voice` covers the CLI).