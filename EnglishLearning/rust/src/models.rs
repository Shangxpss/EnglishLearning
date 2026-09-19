//! Shared data models (ported from Python `services/sync/models.py`).

use serde::{Deserialize, Serialize};

/// A single word/cue segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    /// Start time in seconds.
    pub start: f64,
    /// End time in seconds.
    pub end: f64,
    /// Alignment confidence in `[0, 1]`.
    pub score: f64,
}

impl Word {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// A complete sentence cue with a span and optional word-level breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cue {
    pub index: usize,
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
    /// Absolute path to the source media file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

impl Cue {
    /// Seconds of silence between this cue's end and the next cue's start.
    pub fn pause_after(&self, next_start: f64) -> f64 {
        (next_start - self.end).max(0.0)
    }
}

/// A parsed + segmented session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub media_path: String,
    pub media_duration: f64,
    pub source_format: String,
    pub cues: Vec<Cue>,
}
