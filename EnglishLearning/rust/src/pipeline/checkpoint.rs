//! Checkpointing for the long-running pipeline steps.
//!
//! The expensive work here — whisper transcription, wav2vec2 alignment, and one
//! TTS call per sentence — runs all-or-nothing in memory, so a crash halfway
//! leaves nothing behind and the next run repeats everything. These helpers let
//! each step leave its result on disk as it goes, so an interrupted run resumes
//! instead of starting over. The Python reference states the principle: *"the
//! filesystem IS the checkpoint"*.
//!
//! Two rules make that safe:
//!
//! * **Writes are atomic** (temp file + rename). A half-written file is worse
//!   than no file — a reader would decode garbage and treat it as a finished
//!   result.
//! * **A checkpoint is only valid for the recipe that produced it.** A
//!   directory carries a `recipe.txt`; when the recipe changes the directory is
//!   cleared, so switching `--dub-voice` can never reuse the previous voice's
//!   audio. This is the same idea as `PLAYABLE_RECIPE` in `crate::app`.

use std::path::{Path, PathBuf};

/// FNV-1a — a stable filename hash.
///
/// Deliberately not `std::collections::hash_map::DefaultHasher`: its output is
/// not guaranteed stable across Rust releases, and a changed hash would
/// silently orphan every checkpoint on disk.
pub fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// [`fnv1a`] as 8 hex digits — long enough to notice an edited sentence, short
/// enough to keep a filename readable.
pub fn short_hash(s: &str) -> String {
    format!("{:08x}", fnv1a(s) as u32)
}

/// Write `bytes` to `dir/name` atomically.
///
/// The temp file is created **in the same directory** as the target so the
/// rename cannot cross a filesystem boundary (which would make it a copy, and
/// therefore not atomic).
pub fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!("{name}.part"));
    std::fs::write(&tmp, bytes).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(name))
        .map_err(|e| format!("cannot move {} into place: {e}", tmp.display()))
}

/// Serialize `value` into `dir/name` atomically.
pub fn write_json<T: serde::Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| format!("cannot serialize {name}: {e}"))?;
    write_atomic(dir, name, &bytes)
}

/// Load a JSON checkpoint, or `None` when it is absent or unreadable.
///
/// A corrupt checkpoint is reported as **missing**, never as an error: redoing
/// the work is always correct, whereas failing the run because a cache file
/// went bad is not. The `.part` temp files never match `name`, so an interrupted
/// write cannot be mistaken for a finished checkpoint.
pub fn read_json<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Option<T> {
    let bytes = std::fs::read(dir.join(name)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A checkpoint directory whose contents are only valid for one recipe.
///
/// Opening it with a different recipe wipes it, so the artifacts inside can be
/// trusted without inspecting them individually.
pub struct RecipeDir {
    dir: PathBuf,
}

impl RecipeDir {
    /// Open (creating if needed) `dir`, clearing it when `recipe` differs from
    /// the recorded one.
    pub fn open(dir: PathBuf, recipe: &str) -> Result<RecipeDir, String> {
        let marker = dir.join("recipe.txt");
        let recorded = std::fs::read_to_string(&marker).ok();
        if recorded.as_deref() != Some(recipe) {
            if dir.exists() {
                std::fs::remove_dir_all(&dir)
                    .map_err(|e| format!("cannot clear {}: {e}", dir.display()))?;
            }
            write_atomic(&dir, "recipe.txt", recipe.as_bytes())?;
        }
        Ok(RecipeDir { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The local path of a checkpoint file inside this directory.
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private directory per test so parallel tests cannot collide.
    fn tmp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sv-checkpoint-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn json_round_trips() {
        let dir = tmp_dir("roundtrip");
        let value = vec!["alpha".to_string(), "beta".to_string()];
        write_json(&dir, "words.json", &value).unwrap();
        let back: Vec<String> = read_json(&dir, "words.json").expect("checkpoint present");
        assert_eq!(back, value);
    }

    #[test]
    fn a_missing_checkpoint_reads_as_none() {
        let dir = tmp_dir("missing");
        let back: Option<Vec<String>> = read_json(&dir, "words.json");
        assert!(back.is_none());
    }

    #[test]
    fn a_half_written_temp_file_is_never_mistaken_for_a_checkpoint() {
        // Exactly the crash state: the `.part` file exists, the target does not.
        let dir = tmp_dir("partial");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("words.json.part"), b"{ not json").unwrap();
        let back: Option<Vec<String>> = read_json(&dir, "words.json");
        assert!(back.is_none(), "a partial write must not be read as a result");
    }

    #[test]
    fn atomic_write_leaves_no_temp_file_behind() {
        let dir = tmp_dir("atomic");
        write_atomic(&dir, "a.txt", b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "hello");
        assert!(!dir.join("a.txt.part").exists());
    }

    #[test]
    fn a_changed_recipe_clears_the_previous_artifacts() {
        let dir = tmp_dir("recipe");
        let first = RecipeDir::open(dir.clone(), "edge-tts|en-GB-MaisieNeural").unwrap();
        write_json(first.dir(), "sent_0000.wav.json", &1u8).unwrap();
        assert!(first.dir().join("sent_0000.wav.json").exists());

        // A different voice must not reuse the previous voice's sentence audio.
        let second = RecipeDir::open(dir, "edge-tts|en-US-AriaNeural").unwrap();
        assert!(
            !second.dir().join("sent_0000.wav.json").exists(),
            "a recipe change must invalidate the checkpoints"
        );
    }

    #[test]
    fn the_same_recipe_keeps_the_artifacts() {
        let dir = tmp_dir("recipe-same");
        let first = RecipeDir::open(dir.clone(), "edge-tts|en-GB-MaisieNeural").unwrap();
        write_json(first.dir(), "keep.json", &7u8).unwrap();

        let again = RecipeDir::open(dir, "edge-tts|en-GB-MaisieNeural").unwrap();
        let kept: Option<u8> = read_json(again.dir(), "keep.json");
        assert_eq!(kept, Some(7), "an unchanged recipe must resume, not restart");
    }

    #[test]
    fn short_hash_is_stable_and_text_sensitive() {
        assert_eq!(short_hash("Hello there."), short_hash("Hello there."));
        assert_ne!(short_hash("Hello there."), short_hash("Hello there!"));
        assert_eq!(short_hash("x").len(), 8);
    }
}