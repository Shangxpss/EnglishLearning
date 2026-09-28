//! Speech-to-text with word-level timestamps, built on whisper.cpp.
//!
//! **Self-containment.** `whisper-rs-sys` vendors whisper.cpp and links it
//! statically, and the model is embedded into the executable at build time
//! (see `build.rs`). The shipped binary therefore needs no installs, no Python,
//! and no runtime downloads. `--asr-model <path>` overrides the embedded model.
//!
//! Whisper reports times in centiseconds (10 ms units); everything here is
//! converted to seconds to match [`crate::models`].

use crate::models::Word;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// whisper.cpp requires 16 kHz mono `f32` samples.
pub const SAMPLE_RATE: u32 = 16_000;

/// Message shown when speech-to-text is requested but no model is available.
pub const NO_MODEL_HELP: &str = "no speech-to-text model available: this build has no embedded \
     model (run `rust/scripts/fetch-asr-model.sh` and rebuild) and no `--asr-model <path>` was given";

/// Runtime ASR options, initialised once from the command line.
#[derive(Debug, Default, Clone)]
pub struct AsrConfig {
    /// External model file; `None` means "use the embedded model".
    pub model: Option<PathBuf>,
    /// ISO language code passed to whisper (defaults to `en`).
    pub language: Option<String>,
    /// Refine whisper's word timings with wav2vec2 forced alignment
    /// (`--align`, `SENTENCE_VIDEO_ALIGN=1`, or `"align": true` in the API
    /// body). Off by default: it needs the 378 MB weights and costs minutes of
    /// CPU on a long video.
    pub align: bool,
}

static CONFIG: OnceLock<AsrConfig> = OnceLock::new();

/// Set the process-wide ASR options (first call wins).
pub fn init(model: Option<PathBuf>, language: Option<String>, align: bool) {
    let _ = CONFIG.set(AsrConfig {
        model,
        language,
        align,
    });
}

/// The active ASR options.
pub fn config() -> &'static AsrConfig {
    CONFIG.get_or_init(AsrConfig::default)
}

/// The model compiled into this binary, if one was present at build time.
pub fn embedded_model() -> Option<&'static [u8]> {
    #[cfg(has_embedded_model)]
    {
        Some(include_bytes!(env!("ASR_MODEL_PATH")))
    }
    #[cfg(not(has_embedded_model))]
    {
        None
    }
}

/// True when a model exists, either embedded or supplied at runtime.
pub fn model_available(external: Option<&Path>) -> bool {
    external.map(|p| p.is_file()).unwrap_or(false) || embedded_model().is_some()
}

/// Where transcription gets its model, for logging.
pub fn model_description(external: Option<&Path>) -> String {
    if let Some(p) = external {
        format!("{} (external)", p.display())
    } else if embedded_model().is_some() {
        "embedded in the executable".to_string()
    } else {
        "none".to_string()
    }
}

/// File name of the model embedded at build time (used to pick a DTW preset).
fn embedded_model_file() -> Option<&'static str> {
    #[cfg(has_embedded_model)]
    {
        Some(env!("ASR_MODEL_FILE"))
    }
    #[cfg(not(has_embedded_model))]
    {
        None
    }
}

/// The whisper.cpp DTW preset matching a model file, or `None` if unrecognised.
///
/// DTW ("dynamic time warping") token timestamps are markedly more accurate
/// than whisper's default token timestamps — the default tends to drift by
/// fractions of a second, which makes cue boundaries land mid-word. The preset
/// must match the loaded model, so it is derived from the file name
/// (`ggml-base.en-*.bin` → `BaseEn`).
#[cfg(feature = "asr-whisper")]
fn dtw_preset(model: Option<&Path>) -> Option<whisper_rs::DtwModelPreset> {
    use whisper_rs::DtwModelPreset as P;

    let name = match model {
        Some(p) => p.file_name()?.to_string_lossy().into_owned(),
        None => embedded_model_file()?.to_string(),
    }
    .to_ascii_lowercase();
    // English-only models carry a `.en` marker and have their own presets.
    let en = name.contains(".en");

    Some(if name.contains("tiny") {
        if en {
            P::TinyEn
        } else {
            P::Tiny
        }
    } else if name.contains("base") {
        if en {
            P::BaseEn
        } else {
            P::Base
        }
    } else if name.contains("small") {
        if en {
            P::SmallEn
        } else {
            P::Small
        }
    } else if name.contains("medium") {
        if en {
            P::MediumEn
        } else {
            P::Medium
        }
    } else if name.contains("turbo") || name.contains("large-v3-turbo") {
        P::LargeV3Turbo
    } else if name.contains("large-v3") {
        P::LargeV3
    } else if name.contains("large-v2") {
        P::LargeV2
    } else if name.contains("large") {
        P::LargeV1
    } else {
        return None;
    })
}

/// Transcribe 16 kHz mono `f32` samples into word-level timings.
///
/// Returns `Err` when no model is available or whisper.cpp fails.
#[cfg(feature = "asr-whisper")]
pub fn transcribe(
    samples: &[f32],
    external_model: Option<&Path>,
    language: Option<&str>,
) -> Result<Vec<Word>, String> {
    use whisper_rs::{
        DtwMode, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
        WhisperContextParameters,
    };

    // CPU-only by design: the target is a plain desktop machine with no GPU.
    // `flash_attn` must stay false — whisper.cpp disables DTW when it is on.
    let mut ctx_params = WhisperContextParameters {
        use_gpu: false,
        ..Default::default()
    };
    match dtw_preset(external_model) {
        Some(model_preset) => {
            ctx_params.dtw_parameters = DtwParameters {
                mode: DtwMode::ModelPreset { model_preset },
                ..Default::default()
            };
        }
        None => eprintln!(
            "sentence-video: unrecognised ASR model file name — DTW word timestamps disabled"
        ),
    }

    let ctx = match external_model {
        Some(path) => WhisperContext::new_with_params(path, ctx_params)
            .map_err(|e| format!("cannot load ASR model {}: {e}", path.display()))?,
        None => {
            let bytes = embedded_model().ok_or_else(|| NO_MODEL_HELP.to_string())?;
            WhisperContext::new_from_buffer_with_params(bytes, ctx_params)
                .map_err(|e| format!("cannot load the embedded ASR model: {e}"))?
        }
    };

    let mut state = ctx
        .create_state()
        .map_err(|e| format!("cannot create the ASR state: {e}"))?;

    let threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .clamp(1, 8);

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(threads);
    // Token timestamps are what DTW refines.
    params.set_token_timestamps(true);
    params.set_split_on_word(true);
    params.set_language(Some(language.unwrap_or("en")));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, samples)
        .map_err(|e| format!("transcription failed: {e}"))?;

    // Flatten every segment's tokens: (raw text, start, end, probability) in
    // centiseconds. DTW fills `t_dtw`; `t0`/`t1` are the rough estimates.
    let mut tokens: Vec<(String, i64, i64, f32, bool)> = Vec::new();
    for seg_idx in 0..state.full_n_segments() {
        let Some(segment) = state.get_segment(seg_idx) else {
            continue;
        };
        for tok_idx in 0..segment.n_tokens() {
            let Some(token) = segment.get_token(tok_idx) else {
                continue;
            };
            let raw = token
                .to_str_lossy()
                .map_err(|e| format!("cannot read token text: {e}"))?
                .into_owned();
            // Skip whisper's special tokens (`[_BEG_]`, `[_TT_…]`, …).
            if raw.starts_with("[_") {
                continue;
            }
            let data = token.token_data();
            let dtw = data.t_dtw >= 0;
            let start = if dtw { data.t_dtw } else { data.t0 };
            tokens.push((raw, start, data.t1, data.p, dtw));
        }
    }

    if std::env::var_os("SENTENCE_VIDEO_ASR_DEBUG").is_some() {
        let dtw = tokens.iter().filter(|t| t.4).count();
        let pct = if tokens.is_empty() {
            0.0
        } else {
            100.0 * dtw as f64 / tokens.len() as f64
        };
        eprintln!(
            "sentence-video: ASR debug: tokens={} with_dtw_timestamp={} ({pct:.0}%)",
            tokens.len(),
            dtw
        );
    }

    let mut words: Vec<Word> = Vec::new();
    for (i, (raw, start_cs, t1_cs, p, _)) in tokens.iter().enumerate() {
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        // A token runs until the next one begins. Deriving the end this way
        // (instead of trusting `t1`) is what keeps cue boundaries from landing
        // in the middle of the following word.
        let end_cs = tokens.get(i + 1).map(|t| t.1).unwrap_or(*t1_cs);
        let start = *start_cs as f64 / 100.0;
        let end = (end_cs as f64 / 100.0).max(start);

        // Whisper emits sub-word tokens; a word starts where the raw token
        // carries a leading space. Punctuation is its own token but belongs to
        // the preceding word.
        let starts_word = raw.starts_with(' ');
        let is_punct = text.chars().all(|c| c.is_ascii_punctuation());
        match words.last_mut() {
            Some(prev) if !starts_word || is_punct => {
                prev.text.push_str(text);
                prev.end = end;
                prev.score = prev.score.min(*p as f64);
            }
            _ => words.push(Word {
                text: text.to_string(),
                start,
                end,
                score: *p as f64,
            }),
        }
    }

    snap_word_starts_to_onsets(&mut words, samples, SAMPLE_RATE);
    Ok(words)
}

/// Refine word starts against the **audio itself**.
///
/// whisper's DTW timestamps are consistently a little late at phrase/sentence
/// boundaries (~0.2–0.3 s for `base.en`), which makes a cue's audio run into
/// the first word of the next cue. Where a clear pause precedes a word, this
/// finds the real acoustic onset — the point where energy jumps out of the
/// noise floor — and snaps the word start to it. Words that follow speech
/// directly are left alone (there is no onset to find).
fn snap_word_starts_to_onsets(words: &mut [Word], samples: &[f32], sample_rate: u32) {
    if words.len() < 2 || samples.is_empty() || sample_rate == 0 {
        return;
    }

    // 20 ms non-overlapping RMS frames, in dBFS.
    let win = (sample_rate as f64 * 0.02) as usize;
    let frames = samples.len() / win;
    if win == 0 || frames < 12 {
        return;
    }
    let mut db = vec![0f32; frames];
    for (i, frame) in samples.chunks_exact(win).take(frames).enumerate() {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / win as f32).sqrt();
        db[i] = 20.0 * rms.max(1e-9).log10();
    }

    // Adaptive levels: the 90th percentile approximates the speech level, so
    // the thresholds follow the recording's loudness instead of a fixed dBFS.
    let mut sorted = db.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let loud = sorted[(sorted.len() * 9 / 10).min(sorted.len() - 1)];
    let active = loud - 25.0;

    // An onset is a **sharp rise** out of a quieter stretch: speech-level now,
    // at least 10 dB above the previous 120 ms, and with something clearly
    // quiet in that run (so a breath, which ramps gently, is not an onset).
    let onsets: Vec<usize> = (6..frames)
        .filter(|&i| {
            if db[i] < active {
                return false;
            }
            let prev_max = db[i - 6..i]
                .iter()
                .cloned()
                .fold(f32::NEG_INFINITY, f32::max);
            db[i] - prev_max >= 10.0 && db[i - 6..i].iter().any(|&v| v <= active - 8.0)
        })
        .collect();
    if onsets.is_empty() {
        return;
    }

    let frame_of = |t: f64| ((t * sample_rate as f64) / win as f64).round() as i64;
    let mut snapped = 0usize;
    let mut prev_start = f64::NEG_INFINITY;

    for w in words.iter_mut() {
        let f = frame_of(w.start);
        if f < 0 || f >= frames as i64 {
            prev_start = prev_start.max(w.start);
            continue;
        }
        // Nearest onset in the window: up to 0.6 s before the start (whisper is
        // usually late) to 0.1 s after.
        if let Some(&o) = onsets
            .iter()
            .filter(|&&o| (o as i64) >= f - 30 && (o as i64) <= f + 5)
            .min_by_key(|&&o| (o as i64 - f).abs())
        {
            let candidate = o as f64 * win as f64 / sample_rate as f64;
            if candidate > prev_start && candidate < w.end {
                w.start = candidate;
                snapped += 1;
            }
        }
        if w.start < prev_start {
            w.start = prev_start;
        }
        prev_start = w.start;
    }

    // Ends: a word that is followed by a pause stops where the audio actually
    // stops, **not** at the next word's onset. Ending on the next onset puts
    // the cue boundary on the first phoneme of the following sentence, so any
    // playback overshoot (the player polls, and audio output buffers) voices
    // half of that word. Ending at the offset lets the pause absorb it.
    let mut trimmed = 0usize;
    for i in 0..words.len() {
        let f_start = (frame_of(words[i].start).max(0) as usize).min(frames);
        let f_limit = match words.get(i + 1) {
            Some(next) => (frame_of(next.start).max(0) as usize).min(frames),
            // Last word: search a bounded window rather than all remaining audio.
            None => (f_start + 500).min(frames),
        };
        if f_start >= f_limit {
            words[i].end = words[i].end.max(words[i].start);
            continue;
        }
        match (f_start..f_limit).rev().find(|&f| db[f] >= active) {
            Some(last_loud) => {
                let quiet_after = f_limit - (last_loud + 1);
                words[i].end = if quiet_after >= 4 {
                    // Real pause before the next word: end at the speech offset.
                    trimmed += 1;
                    (last_loud + 1) as f64 * win as f64 / sample_rate as f64
                } else {
                    // Continuous speech: stay flush with the next word.
                    let next_start = words.get(i + 1).map(|n| n.start).unwrap_or(words[i].end);
                    next_start
                };
            }
            // No speech-level frame found in the window; leave the end alone.
            None => {}
        }
        if words[i].end < words[i].start {
            words[i].end = words[i].start;
        }
    }

    if std::env::var_os("SENTENCE_VIDEO_ASR_DEBUG").is_some() {
        eprintln!(
            "sentence-video: ASR debug: snapped {snapped} word start(s), \
             trimmed {trimmed} word end(s) to speech offsets"
        );
    }
}

/// Stub used when the crate is built without the `asr-whisper` feature.
#[cfg(not(feature = "asr-whisper"))]
pub fn transcribe(
    _samples: &[f32],
    _external_model: Option<&Path>,
    _language: Option<&str>,
) -> Result<Vec<Word>, String> {
    Err("this build was compiled without the `asr-whisper` feature".to_string())
}
