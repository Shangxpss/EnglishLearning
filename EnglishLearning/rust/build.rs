//! Build script: embed the speech-to-text model into the executable.
//!
//! Keeps the shipped binary self-contained — the end user runs one file with no
//! installs and no runtime downloads. If the model is absent the build still
//! succeeds; speech-to-text then requires `--asr-model <path>` at runtime.
//!
//! Override the model with `ASR_MODEL=<path>` at build time.

use std::path::{Path, PathBuf};

const DEFAULT_MODEL: &str = "assets/models/ggml-base.en-q5_1.bin";

fn main() {
    println!("cargo:rustc-check-cfg=cfg(has_embedded_model)");
    println!("cargo:rerun-if-env-changed=ASR_MODEL");

    let model = std::env::var("ASR_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let path = PathBuf::from(&model);
    println!("cargo:rerun-if-changed={model}");

    if path.is_file() {
        let abs = path.canonicalize().unwrap_or(path);
        let mb = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0) as f64 / 1_000_000.0;
        println!("cargo:rustc-env=ASR_MODEL_PATH={}", abs.display());
        println!("cargo:rustc-cfg=has_embedded_model");
        println!(
            "cargo:warning=embedding speech-to-text model {} ({mb:.1} MB)",
            abs.display()
        );
    } else {
        println!(
            "cargo:warning=no ASR model at {} — the binary will need `--asr-model <path>`. \
             Run `rust/scripts/fetch-asr-model.sh` to embed one.",
            Path::new(&model).display()
        );
    }
}
