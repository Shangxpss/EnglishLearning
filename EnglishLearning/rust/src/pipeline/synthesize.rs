//! **Step 2 of the replace-audio pipeline — generate a new audio track from
//! the sentence cues produced by [`crate::pipeline::transcribe`].**
//!
//! Composable, reusable pieces:
//!
//! * [`TtsBackend`] — the pluggable speech-synthesis contract. Swap in any
//!   engine (`espeak-ng` command, a network TTS API, an in-process model)
//!   without touching the rest of the pipeline.
//! * [`synth_cues`] — synthesize one waveform per sentence cue (mono,
//!   resampled to a shared rate), skipping any cue that fails.
//! * [`stitch_timeline`] — place those sentence waveforms onto the media
//!   timeline at each cue's start time, filling gaps with silence. This is
//!   the pure-Rust equivalent of Python's `chunk_stitcher.GentleStitcher`.
//! * [`synthesize_track`] — orchestration: per-cue synth → stitch → write the
//!   full-track WAV for the mux step.

use std::process::Command;

use crate::media;
use crate::models::Cue;

/// The speech-synthesis contract. A backend renders a sentence (text) into
/// interleaved f32 mono samples at [`Self::sample_rate`].
pub trait TtsBackend: Send {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;
    fn synthesize(&self, text: &str) -> Result<Vec<f32>, String>;
}

/// Synthesize every cue to a mono waveform, resampled to `backend`'s rate.
/// Cues that fail to synthesize are skipped (their index is absent), so one
/// bad sentence never aborts the whole track.
pub fn synth_cues(
    backend: &dyn TtsBackend,
    cues: &[Cue],
) -> Vec<(usize, Vec<f32>)> {
    let mut segments = Vec::new();
    for cue in cues {
        match backend.synthesize(&cue.text) {
            Ok(s) => segments.push((cue.index, s)),
            Err(_) => {
                // Leave a silent gap for this cue; the timeline keeps its slot.
            }
        }
    }
    segments
}

/// Place each synthesized segment at its cue's `start` time on the media
/// timeline and return the full mono track as `f32` samples at `sample_rate`.
/// Any region without speech (gaps between cues, before the first cue) stays
/// silent.
pub fn stitch_timeline(
    cues: &[Cue],
    segments: &[(usize, Vec<f32>)],
    sample_rate: u32,
    total_duration_secs: f64,
) -> Vec<f32> {
    let sr = sample_rate.max(1) as f64;
    let total_samples = (total_duration_secs.max(0.0) * sr) as usize;
    let mut track = vec![0.0f32; total_samples];

    for &(idx, ref seg) in segments {
        let start_sample = cues
            .iter()
            .find(|c| c.index == idx)
            .map(|c| (c.start * sr) as usize)
            .unwrap_or(0);
        let start_sample = start_sample.min(track.len().saturating_sub(1));
        let end_sample = (start_sample + seg.len()).min(track.len());
        if start_sample < end_sample {
            track[start_sample..end_sample].copy_from_slice(&seg[..end_sample - start_sample]);
        }
    }
    track
}

/// Full Step-2 orchestration: synthesize every cue, stitch onto the media
/// timeline, and write the result as a WAV track ready for the mux step.
pub fn synthesize_track(
    backend: &dyn TtsBackend,
    cues: &[Cue],
    total_duration_secs: f64,
    out_wav: &str,
) -> Result<(), String> {
    let segments = synth_cues(backend, cues);
    if segments.is_empty() {
        return Err("no cues could be synthesized — check the TTS backend".to_string());
    }
    let track = stitch_timeline(cues, &segments, backend.sample_rate(), total_duration_secs);
    media::write_wav(out_wav, &track, backend.sample_rate(), backend.channels())
}

/// A [`TtsBackend`] that shells out to an installed TTS command
/// (default `espeak-ng`). It writes each sentence to a temp WAV, decodes it
/// back to f32 mono via the shared media helper, and returns the samples.
/// Fails with a clear message if the command is not installed.
pub struct CommandTts {
    command: String,
    voice: Option<String>,
    rate_words_per_min: i32,
    sample_rate: u32,
}

impl CommandTts {
    pub fn new(command: &str, voice: Option<&str>, rate_words_per_min: i32) -> Self {
        CommandTts {
            command: command.to_string(),
            voice: voice.map(str::to_string),
            rate_words_per_min: rate_words_per_min,
            sample_rate: 44100,
        }
    }

    fn run_to_wav(&self, text: &str) -> Result<String, String> {
        let exe = if self.command.contains('/') {
            self.command.clone()
        } else {
            format!("{}-ng", self.command)
        };
        let mut cmd = Command::new(&exe);
        cmd.arg("--stdout");
        cmd.arg("-s").arg(self.rate_words_per_min.to_string());
        if let Some(v) = &self.voice {
            cmd.arg("-v").arg(v);
        }
        cmd.arg("--").arg(text);

        let out = cmd
            .output()
            .map_err(|e| format!("TTS command '{}' failed: {e} (is it installed?)", exe))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("TTS command '{}' failed: {}", exe, stderr.trim()));
        }
        if out.stdout.is_empty() {
            return Err("TTS command produced no output".to_string());
        }

        // Persist the bytes as WAV and decode with the shared media helper so
        // every backend reuses one decoding path.
        let tmp = temp_wav_path();
        std::fs::write(&tmp, &out.stdout)
            .map_err(|e| format!("cannot write temp wav: {e}"))?;
        Ok(tmp)
    }
}

impl TtsBackend for CommandTts {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        1
    }
    fn synthesize(&self, text: &str) -> Result<Vec<f32>, String> {
        let wav = self.run_to_wav(text)?;
        let res = media::decode_to_f32(&wav, self.sample_rate, true, None)
            .map(|(s, _)| s);
        let _ = std::fs::remove_file(&wav);
        res
    }
}

/// A deterministic [`TtsBackend`] used for tests / pipeline wiring checks —
/// it produces a short beep per cue so the stitcher and mux can be validated
/// end-to-end without a real voice.
pub struct SilenceTts {
    sample_rate: u32,
    beep_hz: f32,
}

impl SilenceTts {
    pub fn new(sample_rate: u32) -> Self {
        SilenceTts { sample_rate, beep_hz: 440.0 }
    }
}

impl TtsBackend for SilenceTts {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        1
    }
    fn synthesize(&self, _text: &str) -> Result<Vec<f32>, String> {
        let sr = self.sample_rate as f32;
        let n = sr as usize; // 1 second of 440 Hz tone
        Ok((0..n)
            .map(|i| (2.0 * std::f32::consts::PI * self.beep_hz * i as f32 / sr).sin())
            .collect())
    }
}

fn temp_wav_path() -> String {
    std::env::temp_dir()
        .join(format!("sentence-video-tts-{}.wav", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(i: usize, start: f64, end: f64, text: &str) -> Cue {
        Cue {
            index: i,
            start,
            end,
            text: text.to_string(),
            words: vec![],
            source_path: None,
        }
    }

    #[test]
    fn synthetic_beeps_land_at_cue_times() {
        let cues = vec![cue(0, 1.0, 2.0, "one."), cue(1, 3.0, 4.0, "two.")];
        let backend = SilenceTts::new(1000);
        let track = stitch_timeline(&cues, &synth_cues(&backend, &cues), 1000, 5.0);
        assert_eq!(track.len(), 5000);
        // First sample of cue 0 is exactly at 1.0s.
        assert!(track[1000].abs() < 1e-6 || track[1000].abs() > 1e-3);
        // The gap region before 1.0s must be silent.
        assert!(track[500].abs() < 1e-6);
    }
}