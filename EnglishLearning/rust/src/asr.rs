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
}

static CONFIG: OnceLock<AsrConfig> = OnceLock::new();

/// Set the process-wide ASR options (first call wins).
pub fn init(model: Option<PathBuf>, language: Option<String>) {
    let _ = CONFIG.set(AsrConfig { model, language });
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

/// Transcribe 16 kHz mono `f32` samples into word-level timings.
///
/// Returns `Err` when no model is available or whisper.cpp fails.
#[cfg(feature = "asr-whisper")]
pub fn transcribe(
    samples: &[f32],
    external_model: Option<&Path>,
    language: Option<&str>,
) -> Result<Vec<Word>, String> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    // CPU-only by design: the target is a plain desktop machine with no GPU.
    let ctx_params = WhisperContextParameters {
        use_gpu: false,
        ..Default::default()
    };

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
    // One token per word, with its own timestamps.
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

    let mut words: Vec<Word> = Vec::new();
    for seg_idx in 0..state.full_n_segments() {
        let Some(segment) = state.get_segment(seg_idx) else {
            continue;
        };
        for tok_idx in 0..segment.n_tokens() {
            let Some(token) = segment.get_token(tok_idx) else {
                continue;
            };
            let raw = token.to_str_lossy().map_err(|e| format!("cannot read token text: {e}"))?;
            // Skip whisper's special tokens (`[_BEG_]`, `[_TT_…]`, …).
            if raw.starts_with("[_") {
                continue;
            }
            let text = raw.trim();
            if text.is_empty() {
                continue;
            }
            let data = token.token_data();
            let start = data.t0 as f64 / 100.0;
            let end = (data.t1 as f64 / 100.0).max(start);

            // Whisper emits sub-word tokens; a word starts where the raw token
            // carries a leading space. Punctuation is its own token but belongs
            // to the preceding word.
            let starts_word = raw.starts_with(' ');
            let is_punct = text.chars().all(|c| c.is_ascii_punctuation());
            match words.last_mut() {
                Some(prev) if !starts_word || is_punct => {
                    prev.text.push_str(text);
                    prev.end = end;
                    prev.score = prev.score.min(data.p as f64);
                }
                _ => words.push(Word {
                    text: text.to_string(),
                    start,
                    end,
                    score: data.p as f64,
                }),
            }
        }
    }
    Ok(words)
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
