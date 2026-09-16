//! **The replace-audio pipeline.** Composes three reusable steps into one
//! operation:
//!
//! 1. [`transcribe`] — get sentence cues from the source media (subtitle
//!    file, or a future whisper.cpp backend).
//! 2. [`synthesize`] — generate a new full audio track from those cues.
//! 3. [`crate::media::mux_video_audio`] — swap the media's original audio for
//!    the generated track.
//!
//! Each step is an independent, reusable module/function; the [`dub`]
//! orchestration below is just one way to wire them together.

pub mod synthesize;
pub mod transcribe;

use crate::media;
use crate::models::Cue;
use synthesize::TtsBackend;
use transcribe::cues_from_media;

/// Options shared across the pipeline steps.
#[derive(Debug, Clone)]
pub struct DubOptions {
    /// AAC bitrate (bits/sec) for the mux step.
    pub audio_bitrate: usize,
    /// If true, keep the generated audio at its full length instead of
    /// trimming it to the video duration.
    pub keep_full_audio: bool,
    /// Return the intermediate combined WAV path to the caller (used by the
    /// CLI's `--keep-wav`); otherwise the temp file is removed.
    pub emit_intermediate_wav: bool,
}

impl Default for DubOptions {
    fn default() -> Self {
        DubOptions { audio_bitrate: 128_000, keep_full_audio: false, emit_intermediate_wav: false }
    }
}

/// Full replace-audio pipeline:
/// `video + subtitle → cues → generated audio → video with new audio`.
///
/// Steps are invoked as reusable functions so a caller can re-use any one of
/// them. Returns the path of the generated intermediate wav (if
/// `emit_intermediate_wav`) alongside the mux output.
pub fn dub(
    video: &str,
    subtitle: Option<&str>,
    output: &str,
    backend: &dyn TtsBackend,
    opts: &DubOptions,
) -> Result<Option<String>, String> {
    // Step 1 — get the cues (the "subtitle") for this media.
    let cues: Vec<Cue> = cues_from_media(video, subtitle)?;
    let duration = media::probe_duration(video)?;

    // Step 2 — generate the new audio track from the cues.
    let tmp_wav = intermediate_wav_path();
    synthesize::synthesize_track(backend, &cues, duration, &tmp_wav)?;

    // Step 3 — replace the original audio with the generated track.
    media::mux_video_audio(video, &tmp_wav, output, opts.audio_bitrate, !opts.keep_full_audio)?;

    if opts.emit_intermediate_wav {
        Ok(Some(tmp_wav))
    } else {
        let _ = std::fs::remove_file(&tmp_wav);
        Ok(None)
    }
}

fn intermediate_wav_path() -> String {
    std::env::temp_dir()
        .join(format!("sentence-video-dub-{}.wav", std::process::id()))
        .to_string_lossy()
        .into_owned()
}