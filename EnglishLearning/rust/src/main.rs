//! `sentence-video` — a single Rust executable that splits a media file into
//! sentence-level segments and serves them to the OS default browser.
//!
//! Usage:
//!   sentence-video [serve] <media-file> [--subtitle <srt|vtt>] [--no-browser]
//!                  [--host <addr>] [--port <n>]
//!
//! With no arguments a server starts with an empty workspace (media can be
//! added later via `POST /api/session`).

// The agent service sits behind a feature so a media-only build stays small.
// `run_agent_command` below needs this declaration: without it the whole
// `src/agent/` tree is silently never compiled, and `agent::…` fails to resolve.
#[cfg(feature = "agent")]
mod agent;
// wav2vec2 forced alignment: experimental, off by default, and the only module
// that reads a multi-hundred-MB model from disk instead of embedding it.
#[cfg(feature = "asr-align")]
mod align;
mod app;
mod asr;
mod db;
mod logs;
mod media;
mod models;
mod picker;
mod pipeline;
mod platform;
mod segmenter;
mod server;
mod subtitle;

use std::sync::Arc;

use ffmpeg_next::ffi::fprintf;

const MEDIA_EXTS: &[&str] = &[
    "mp4", "mkv", "mov", "webm", "m4a", "mp3", "wav", "ogg", "aac",
];

fn print_usage() {
    println!(
        "sentence-video — sentence-segmented media player (pure Rust, single binary)\n\n\
         USAGE:\n\
         \x20 sentence-video [serve] <media-file> [OPTIONS]\n\
         \x20 sentence-video replace-audio <video> <audio> <output> [--bitrate <bps>] [--full]\n\
         \x20 sentence-video dub <video> <output> [--subtitle <srt>|<vtt>] [OPTIONS]\n\
         \x20 sentence-video agent [--host <addr>] [--port <n>] [OPTIONS]\n\
         \x20 sentence-video align <media> [--transcript <text|file>] [OPTIONS]\n\n\
         AGENT MODE (CopilotKit / AG-UI service; replaces the AI-Demo Bun runtime + Python backend):\n\
         \x20   --host <addr>          bind address (default 127.0.0.1)\n\
         \x20   --port <n>             bind port (default 4000)\n\
         \x20   --model <name>         model id (default deepseek-chat)\n\
         \x20   --agent-id <id>        agent id (default sample_agent)\n\
         \x20   --catalog-id <id>      A2UI catalog id (default generative-agent-catalog)\n\
         \x20   --max-steps <n>        max model turns per run (default 8)\n\
         \x20   --api-key <key>        API key; prefer the DEEPSEEK_API_KEY env var\n\n\
         PLAY MODE (opens default browser):\n\
         \x20   --subtitle <srt|vtt>   subtitle file next to / for the media\n\
         \x20   --no-browser           don't auto-open the browser\n\
         \x20   --host <addr>          bind address (default 127.0.0.1)\n\
         \x20   --port <n>             bind port; 0 = OS-assigned (default)\n\
         \x20   --data-dir <path>      folder for the SQLite session database\n\
         \x20   --asr-model <path>     speech-to-text model file\n\
         \x20                         (default: the model embedded in this binary)\n\
         \x20   --asr-language <code>  spoken language for transcription (default: en)\n\
         \x20   --align                refine word timings with wav2vec2 forced alignment\n\
         \x20                         (needs the weights; also SENTENCE_VIDEO_ALIGN=1)\n\
         \x20   --dub-engine <name>    TTS backend for the 'Dub audio' option:\n\
         \x20                         edge-tts (default) | espeak-ng | silence\n\
         \x20                         (also SENTENCE_VIDEO_DUB_ENGINE)\n\
         \x20   --dub-voice <name>     voice for that backend; default en-GB-MaisieNeural\n\
         \x20                         for edge-tts (also SENTENCE_VIDEO_DUB_VOICE)\n\
         \x20   --dub-proxy <url>      proxy for edge-tts, which ignores https_proxy\n\
         \x20                         (also SENTENCE_VIDEO_DUB_PROXY)\n\
         \x20   --log-level <level>    debug|info|warn|error for the log database\n\
         \x20                         (default: debug; also SENTENCE_VIDEO_LOG_LEVEL)\n\
         \x20   --no-log-file          keep logs in logs.db only, no text mirror\n\n\
         REPLACE-AUDIO (batch, replaces a video's audio with a supplied track):\n\
         \x20   <video>    media whose audio will be replaced\n\
         \x20   <audio>    new audio track (mp3/wav/m4a/…)\n\
         \x20   <output>   output path (e.g. out.mp4); video copied, audio → AAC\n\
         \x20   --bitrate  AAC bitrate in bits/sec (default 128000)\n\
         \x20   --full     keep audio at full length (default trims to video)\n\n\
         DUB (modular pipeline: cues → generated audio → replace):\n\
         \x20   <video>        source media to dub\n\
         \x20   --subtitle     cues source (default: auto-detect <video>.srt/.vtt)\n\
         \x20   --voice <name> TTS voice passed to the backend (default: backend default)\n\
         \x20   --tts-command  TTS executable; default 'espeak' → 'espeak-ng'\n\
         \x20   --voice-rate   TTS speaking rate in wpm (default 170)\n\
         \x20   --silence      test backend: one beep per sentence (no voice needed)\n\
         \x20   --bitrate      AAC bitrate in bits/sec (default 128000)\n\
         \x20   --full         keep full audio length (default trims to video)\n\
         \x20   --keep-wav     keep the generated intermediate WAV\n\n\
         The binary serves an embedded player at http://<host>:<port>/ and opens\n\
         it in the default browser."
    );
}

/// Open the given URL in the OS default browser (no Python launcher).
fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

fn is_media_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    MEDIA_EXTS.iter().any(|e| lower.ends_with(e))
}

// ─────────────────────────────────────────────────────────────────────────
// replace-audio subcommand
// ─────────────────────────────────────────────────────────────────────────

/// `sentence-video replace-audio <video> <audio> <output> [--bitrate <bps>] [--full]`
///
/// Replaces the video's audio track with a new one, mirroring the Python
/// version's `av_utils.mux_video_audio`:
///   * the video stream is copied verbatim (no re-encode, no quality loss),
///   * the new audio is decoded and re-encoded to AAC,
///   * by default the audio is trimmed to the video length (`-shortest`).
fn run_replace_audio(args: &[String]) {
    let mut video_path: Option<String> = None;
    let mut audio_path: Option<String> = None;
    let mut output_path: Option<String> = None;
    let mut bitrate: usize = 128_000;
    let mut full = false; // false = trim audio to video duration (+ -shortest)

    let mut i = 0;
    let mut positional = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bitrate" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    bitrate = v.parse().unwrap_or(128_000);
                }
            }
            "--full" => full = true,
            other => match positional {
                0 => video_path = Some(other.to_string()),
                1 => audio_path = Some(other.to_string()),
                2 => output_path = Some(other.to_string()),
                _ => {}
            },
        }
        if !args[i].starts_with('-') {
            positional += 1;
        }
        i += 1;
    }

    if output_path.is_none() {
        println!(
            "sentence-video: replace-audio requires <video> <audio> <output>\n\n\
             USAGE:\n\
             \x20 sentence-video replace-audio <video> <audio> <output> [--bitrate <bps>] [--full]\n\n\
             \x20 <video>    media file whose audio will be replaced\n\
             \x20 <audio>    new audio track (mp3/wav/m4a/…)\n\
             \x20 <output>   output path (e.g. out.mp4)\n\
             \x20 --bitrate  AAC bitrate in bits/sec (default 128000)\n\
             \x20 --full     keep the audio at its full length (default trims to video)."
        );
        std::process::exit(1);
    }

    let video = media::normalize_path(video_path.as_deref().unwrap_or(""));
    let audio = media::normalize_path(audio_path.as_deref().unwrap_or(""));
    let output = media::normalize_path(output_path.as_deref().unwrap());

    println!("sentence-video: replacing audio of '{video}' with '{audio}' → '{output}'");
    match media::mux_video_audio(&video, &audio, &output, bitrate, !full) {
        Ok(()) => println!("sentence-video: done → {output}"),
        Err(e) => {
            eprintln!("sentence-video: failed to replace audio: {e}");
            std::process::exit(1);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// dub subcommand — the modular replace-audio pipeline
// ─────────────────────────────────────────────────────────────────────────

/// `sentence-video dub <video> <output> [--subtitle <srt|vtt>] [--voice <name>]
///                     [--tts-command <cmd>] [--voice-rate <wpm>]
///                     [--bitrate <bps>] [--full] [--keep-wav] [--silence]`
///
/// Runs the full pipeline: get cues (Step 1) → generate audio from them
/// (Step 2) → swap the video's original audio for the generated track (Step 3).
fn run_dub(args: &[String]) {
    let mut video: Option<String> = None;
    let mut output: Option<String> = None;
    let mut subtitle: Option<String> = None;
    let mut voice: Option<String> = None;
    let mut tts_command = "espeak".to_string();
    let mut voice_rate = 170i32;
    let mut bitrate = 128_000usize;
    let mut full = false;
    let mut keep_wav = false;
    let mut use_silence = false;

    let mut positional = 0usize;
    let mut i = 0usize;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--subtitle" => {
                i += 1;
                subtitle = args.get(i).cloned();
            }
            "--voice" => {
                i += 1;
                voice = args.get(i).cloned();
            }
            "--tts-command" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    tts_command = v.clone();
                }
            }
            "--voice-rate" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    voice_rate = v.parse().unwrap_or(voice_rate);
                }
            }
            "--bitrate" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    bitrate = v.parse().unwrap_or(bitrate);
                }
            }
            "--full" => full = true,
            "--keep-wav" => keep_wav = true,
            "--silence" => use_silence = true,
            _ => {
                if !a.is_empty() {
                    match positional {
                        0 => video = Some(a.clone()),
                        1 => output = Some(a.clone()),
                        _ => {}
                    }
                    positional += 1;
                }
            }
        }
        i += 1;
    }

    let Some(video) = video else {
        eprintln!(
            "sentence-video: dub requires <video> <output>\n\n\
             sentence-video dub <video> <output> [--subtitle <srt|vtt>] [--voice <name>]\n\
             \x20   [--tts-command <cmd>]  TTS executable (default espeak -> espeak-ng)\n\
             \x20   [--voice-rate <wpm>]   TTS speaking rate (default 170)\n\
             \x20   --silence              test backend: beep per sentence (no voice needed)\n\
             \x20   --bitrate <bps>        AAC bitrate (default 128000)\n\
             \x20   --full                 keep full audio length (default trims to video)\n\
             \x20   --keep-wav             keep the generated intermediate WAV"
        );
        std::process::exit(1);
    };
    let output = output.unwrap_or_else(|| {
        let mut out = video.clone();
        if let Some(stem) = out.rfind('.') {
            out.truncate(stem);
        }
        format!("{out}.dub.mp4")
    });

    let video = media::normalize_path(&video);
    let output = media::normalize_path(&output);
    let subtitle = subtitle.map(|s| media::normalize_path(&s));

    let opts = pipeline::DubOptions {
        audio_bitrate: bitrate,
        keep_full_audio: full,
        emit_intermediate_wav: keep_wav,
    };

    let backend: Box<dyn pipeline::synthesize::TtsBackend> = if use_silence {
        Box::new(pipeline::synthesize::SilenceTts::new(44100))
    } else {
        Box::new(pipeline::synthesize::CommandTts::new(
            &tts_command,
            voice.as_deref(),
            voice_rate,
        ))
    };

    println!("sentence-video: dubbing '{video}' → '{output}'");
    match pipeline::dub(&video, subtitle.as_deref(), &output, &*backend, &opts) {
        Ok(Some(wav)) => {
            println!("sentence-video: done → {output}");
            println!("sentence-video: kept generated audio → {wav}");
        }
        Ok(None) => println!("sentence-video: done → {output}"),
        Err(e) => {
            eprintln!("sentence-video: dub failed: {e}");
            std::process::exit(1);
        }
    }
}

/// An environment variable as a non-empty string, if it is set.
fn env_setting(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Where the SQLite session database lives. Priority: `--data-dir`, then the
/// platform user-data directory, then `.sentence-video` in the working dir.
fn resolve_data_dir(explicit: Option<String>) -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Some(d) = explicit {
        return PathBuf::from(media::normalize_path(&d));
    }
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("sentence-video");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Application Support/sentence-video");
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("sentence-video");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share/sentence-video");
    }
    PathBuf::from(".sentence-video")
}

// ─────────────────────────────────────────────────────────────────────────
// agent subcommand — CopilotKit / AG-UI service (Rust port of AI-Demo)
// ─────────────────────────────────────────────────────────────────────────

/// `sentence-video agent [--host <addr>] [--port <n>] [--model <name>] …`
///
/// Serves the CopilotKit/AG-UI endpoints that previously needed the Bun
/// `agent-runtime` *and* the Python `backend` — now one binary.
/// See [`agent`] for the module layout.
#[cfg(feature = "agent")]
fn run_agent_command(args: &[String]) {
    let mut cfg = agent::AgentConfig::from_env();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    cfg.host = v.clone();
                }
            }
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    cfg.port = v.parse().unwrap_or(cfg.port);
                }
            }
            "--model" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    cfg.model = v.clone();
                }
            }
            "--agent-id" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    cfg.agent_id = v.clone();
                }
            }
            "--catalog-id" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    cfg.catalog_id = v.clone();
                }
            }
            "--max-steps" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    cfg.max_steps = v.parse().unwrap_or(cfg.max_steps);
                }
            }
            "--api-key" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    cfg.api_key = v.clone();
                }
            }
            other => eprintln!("sentence-video agent: ignoring unknown argument '{other}'"),
        }
        i += 1;
    }

    if let Err(e) = agent::run(cfg) {
        eprintln!("sentence-video agent: {e}");
        std::process::exit(1);
    }
}

/// Without the `agent` feature the subcommand still exists, so the CLI can say
/// exactly how to get it back instead of failing with "unknown argument".
#[cfg(not(feature = "agent"))]
fn run_agent_command(_args: &[String]) {
    eprintln!(
        "sentence-video: the agent service is not compiled in.\n\
         Rebuild with the 'agent' feature (enabled by default):\n\
         \x20 cargo build --release --features agent"
    );
    std::process::exit(1);
}

// ─────────────────────────────────────────────────────────────────────────
// align subcommand — wav2vec2 forced alignment (feature `asr-align`)
// ─────────────────────────────────────────────────────────────────────────

/// `sentence-video align <media> [--transcript <text|file>] [--align-model <path>]
///                     [--start <n>] [--max-seconds <n>]`
///
/// Diagnostics entry point for the wav2vec2 aligner: with no transcript it
/// greedy-decodes the audio (does the encoder hear the right words?), and with
/// one it prints word timings. The window is `--start` (default 0) for
/// `--max-seconds`, because self-attention is quadratic and this implementation
/// is single-threaded. Reported times are absolute on the media timeline, so a
/// window can be compared directly against a subtitle.
#[cfg(feature = "asr-align")]
fn run_align_command(args: &[String]) {
    use std::path::PathBuf;

    let mut media_path: Option<String> = None;
    let mut transcript: Option<String> = None;
    let mut model: Option<String> = None;
    let mut start_seconds = 0.0f64;
    let mut max_seconds = 30.0f64;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--transcript" => {
                i += 1;
                transcript = args.get(i).cloned();
            }
            "--align-model" => {
                i += 1;
                model = args.get(i).cloned();
            }
            "--start" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    start_seconds = v.parse().unwrap_or(start_seconds);
                }
            }
            "--max-seconds" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    max_seconds = v.parse().unwrap_or(max_seconds);
                }
            }
            other => {
                if media_path.is_none() && !other.starts_with('-') {
                    media_path = Some(other.to_string());
                }
            }
        }
        i += 1;
    }

    let Some(media_path) = media_path else {
        eprintln!(
            "sentence-video: align requires a media file\n\n\
             sentence-video align <media> [--transcript <text|file>] [--align-model <path>]\n\
             \x20   [--start <n>] [--max-seconds <n>]\n\n\
             \x20 --transcript   words to align against (inline text, or a file to read);\n\
             \x20                omitted → greedy-decode the audio instead\n\
             \x20 --align-model  wav2vec2-base-960h.safetensors (default: auto-detected)\n\
             \x20 --start        offset of the window in seconds (default 0)\n\
             \x20 --max-seconds  length of the window in seconds (default 30)"
        );
        std::process::exit(1);
    };
    let media_path = media::normalize_path(&media_path);

    let weights: PathBuf = match model {
        Some(p) => PathBuf::from(media::normalize_path(&p)),
        None => match align::default_weights_path() {
            Some(p) => p,
            None => {
                eprintln!("sentence-video: {}", align::NO_WEIGHTS_HELP);
                std::process::exit(1);
            }
        },
    };
    println!("sentence-video: alignment weights → {}", weights.display());

    let aligner = match align::Aligner::load(&weights) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sentence-video: cannot load alignment weights: {e}");
            std::process::exit(1);
        }
    };

    // `decode_to_f32` only takes a leading budget, so decode as far as the end of
    // the window and drop the part before it. Decoding is cheap next to the
    // encoder, so re-decoding the prefix costs nothing measurable.
    let (mut samples, _channels) = match media::decode_to_f32(
        &media_path,
        align::wav2vec2::SAMPLE_RATE,
        true,
        Some(start_seconds + max_seconds),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sentence-video: cannot decode '{media_path}': {e}");
            std::process::exit(1);
        }
    };
    let skip =
        ((start_seconds * align::wav2vec2::SAMPLE_RATE as f64).round() as usize).min(samples.len());
    samples.drain(..skip);
    println!(
        "sentence-video: window {:.3}–{:.3}s ({} samples) from {media_path}",
        start_seconds,
        start_seconds + samples.len() as f64 / align::wav2vec2::SAMPLE_RATE as f64,
        samples.len()
    );

    let started = std::time::Instant::now();
    let outcome = match transcript.as_deref() {
        Some(t) => {
            // A value naming an existing file is read as the transcript.
            let text = if std::path::Path::new(t).is_file() {
                match std::fs::read_to_string(t) {
                    Ok(text) => text,
                    Err(e) => {
                        eprintln!("sentence-video: cannot read transcript {t}: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                t.to_string()
            };
            let words: Vec<&str> = text.split_whitespace().collect();
            println!("sentence-video: aligning {} words", words.len());
            aligner.align(&samples, &words).map(Alignment::Words)
        }
        None => {
            println!("sentence-video: no transcript given — greedy decoding instead");
            aligner.transcribe(&samples).map(Alignment::Text)
        }
    };

    match outcome {
        Ok(Alignment::Words(mut words)) => {
            // The aligner reports times relative to the window; add the offset
            // back so every printed time is absolute on the media timeline.
            for w in words.iter_mut() {
                w.start += start_seconds;
                w.end += start_seconds;
            }
            for w in &words {
                println!(
                    "{:8.3} {:8.3}  {}  [score {:.2}]",
                    w.start, w.end, w.text, w.score
                );
            }
            println!(
                "sentence-video: {} words in {:.1}s",
                words.len(),
                started.elapsed().as_secs_f64()
            );
        }
        Ok(Alignment::Text(text)) => {
            println!("{text}");
            println!(
                "sentence-video: decoded in {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }
        Err(e) => {
            eprintln!("sentence-video: alignment failed: {e}");
            std::process::exit(1);
        }
    }
}

/// The two things `align` can produce, so one `match` can format either.
#[cfg(feature = "asr-align")]
enum Alignment {
    Words(Vec<models::Word>),
    Text(String),
}

/// Without the feature the subcommand still exists, so the CLI can say exactly
/// how to get it back instead of failing with "unknown argument".
#[cfg(not(feature = "asr-align"))]
fn run_align_command(_args: &[String]) {
    eprintln!(
        "sentence-video: wav2vec2 forced alignment is not compiled in.\n\
         Rebuild with the 'asr-align' feature (it needs the weights on disk):\n\
         \x20 cargo build --release --features asr-align"
    );
    std::process::exit(1);
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // `replace-audio` / `dub` are pure batch operations — no server. Handle
    // them before anything else so flag parsing below never sees these args.
    if args.first().map(String::as_str) == Some("replace-audio") {
        args.remove(0);
        run_replace_audio(&args);
        return;
    }
    if args.first().map(String::as_str) == Some("dub") {
        args.remove(0);
        run_dub(&args);
        return;
    }
    if args.first().map(String::as_str) == Some("agent") {
        args.remove(0);
        run_agent_command(&args);
        return;
    }
    if args.first().map(String::as_str) == Some("align") {
        args.remove(0);
        run_align_command(&args);
        return;
    }

    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        if args.is_empty() {
            // Still start the server so the workspace is usable via the API.
            args = vec![];
        } else {
            return;
        }
    }

    if args.first().map(String::as_str) == Some("serve") {
        args.remove(0);
    }

    let mut media_path: Option<String> = None;
    let mut subtitle_path: Option<String> = None;
    let mut open_browser_flag = true;
    let mut host = "127.0.0.1".to_string();
    let mut port = 0;
    let mut data_dir: Option<String> = None;
    let mut asr_model: Option<std::path::PathBuf> = None;
    let mut asr_language: Option<String> = None;
    let mut align = false;
    let mut dub_engine: Option<String> = None;
    let mut dub_voice: Option<String> = None;
    let mut dub_proxy: Option<String> = None;
    let mut log_level: Option<String> = None;
    let mut no_log_file = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--subtitle" => {
                i += 1;
                subtitle_path = args.get(i).cloned();
            }
            "--no-browser" => open_browser_flag = false,
            "--data-dir" => {
                i += 1;
                if let Some(d) = args.get(i) {
                    data_dir = Some(d.clone());
                }
            }
            "--asr-model" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    asr_model = Some(std::path::PathBuf::from(media::normalize_path(p)));
                }
            }
            "--asr-language" => {
                i += 1;
                if let Some(l) = args.get(i) {
                    asr_language = Some(l.clone());
                }
            }
            "--align" => align = true,
            "--log-level" => {
                i += 1;
                if let Some(l) = args.get(i) {
                    log_level = Some(l.clone());
                }
            }
            "--no-log-file" => no_log_file = true,
            "--dub-engine" => {
                i += 1;
                dub_engine = args.get(i).cloned();
            }
            "--dub-voice" => {
                i += 1;
                dub_voice = args.get(i).cloned();
            }
            "--dub-proxy" => {
                i += 1;
                dub_proxy = args.get(i).cloned();
            }
            "--host" => {
                i += 1;
                if let Some(h) = args.get(i) {
                    host = h.clone();
                }
            }
            "--port" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    port = p.parse().unwrap_or(0);
                }
            }
            other => {
                if media_path.is_none() && is_media_file(other) {
                    media_path = Some(other.to_string());
                } else if media_path.is_none() {
                    media_path = Some(other.to_string());
                }
            }
        }
        i += 1;
    }

    let cache_dir = std::env::temp_dir().join("sentence-video-cache");
    let data_dir = resolve_data_dir(data_dir);

    // Logging: a `logs.db` beside the session database, mirrored to a text file
    // so the log can also be `tail`ed outside the app. Initialized before the
    // session database so a failure opening *that* is itself recorded.
    let log_level = log_level
        .or_else(|| env_setting("SENTENCE_VIDEO_LOG_LEVEL"))
        .and_then(|l| logs::Level::from_str(&l))
        .unwrap_or(logs::Level::Debug);
    let log_mirror = (!no_log_file).then(|| data_dir.join("logs").join("sentence-video.log"));
    let logs_db = data_dir.join("logs.db");
    if let Err(e) = logs::init(&logs_db, log_mirror.clone(), log_level) {
        eprintln!("sentence-video: logging disabled: {e}");
    }
    logs::info("server", "starting")
        .detail(format!(
            "data_dir={} · logs.db={} · mirror={} · log_level={}",
            data_dir.display(),
            logs_db.display(),
            log_mirror
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "off".to_string()),
            log_level.as_str(),
        ))
        .send();

    let db_path = data_dir.join("sessions.db");
    // Which storage engine this is stays entirely here: the rest of the app only
    // ever sees `dyn Store`, so swapping the backend never touches request code.
    let db: Arc<dyn db::Store> = match db::Db::open(&db_path) {
        Ok(db) => {
            println!("sentence-video: session database → {}", db_path.display());
            logs::info("db", "session database opened")
                .detail(db_path.display().to_string())
                .send();
            Arc::new(db)
        }
        Err(e) => {
            // Degrade gracefully rather than refusing to start: fall back to a
            // temp database.
            let fallback_dir = std::env::temp_dir().join("sentence-video");
            eprintln!(
                "sentence-video: {e}; falling back to {}",
                fallback_dir.join("sessions.db").display()
            );
            logs::error(
                "db",
                "cannot open the session database — using a temp database",
            )
            .detail(format!(
                "{e} · fallback={}",
                fallback_dir.join("sessions.db").display()
            ))
            .send();
            match db::Db::open(&fallback_dir.join("sessions.db")) {
                Ok(db) => Arc::new(db),
                Err(e) => {
                    eprintln!("sentence-video: cannot open a session database: {e}");
                    logs::error("db", "cannot open any session database; exiting")
                        .detail(e)
                        .send();
                    std::process::exit(1);
                }
            }
        }
    };
    let state = Arc::new(app::AppState::new(cache_dir, data_dir, db));

    // Dubbing: which TTS backend the 'Dub audio' option uses. Flags win, then
    // the environment, then the built-in default. The engine itself is not
    // resolved until a session actually asks to be dubbed, so a missing
    // `edge-tts` never stops the server from starting. Leaving `voice` as
    // `None` keeps the engine's own default (en-GB Maisie for `edge-tts`).
    let dub_defaults = pipeline::DubConfig::default();
    let dub_engine = dub_engine
        .or_else(|| env_setting("SENTENCE_VIDEO_DUB_ENGINE"))
        .unwrap_or(dub_defaults.engine);
    let dub_voice = dub_voice.or_else(|| env_setting("SENTENCE_VIDEO_DUB_VOICE"));
    let dub_proxy = dub_proxy.or_else(|| env_setting("SENTENCE_VIDEO_DUB_PROXY"));
    println!("sentence-video: dubbing engine → {dub_engine}");
    logs::info("dub", "dubbing engine configured")
        .detail(format!("engine={dub_engine}"))
        .send();
    pipeline::init_dub(pipeline::DubConfig {
        engine: dub_engine,
        voice: dub_voice,
        rate_words_per_min: dub_defaults.rate_words_per_min,
        proxy: dub_proxy,
    });

    // Speech-to-text: the model is embedded in this executable, so a media file
    // with no subtitle can still be segmented. `--asr-model` overrides it.
    // `--align` (or `SENTENCE_VIDEO_ALIGN=1`) additionally refines the word
    // timings with wav2vec2 forced alignment.
    let align = align
        || matches!(
            std::env::var("SENTENCE_VIDEO_ALIGN").as_deref(),
            Ok("1") | Ok("true")
        );
    asr::init(asr_model, asr_language, align);
    let asr_cfg = asr::config();
    let asr_model = asr_cfg.model.as_deref();
    if asr::model_available(asr_model) {
        println!(
            "sentence-video: speech-to-text model → {}",
            asr::model_description(asr_model)
        );
        logs::info("asr", "speech-to-text ready")
            .detail(format!(
                "model={} · align={align}",
                asr::model_description(asr_model)
            ))
            .send();
    } else {
        eprintln!(
            "sentence-video: speech-to-text unavailable: {}",
            asr::NO_MODEL_HELP
        );
        logs::warn("asr", "speech-to-text unavailable")
            .detail(asr::NO_MODEL_HELP)
            .send();
    }

    // Auto-create (or resume) a session when a media file was passed in.
    if let Some(mp) = &media_path {
        match state.build_session(mp, subtitle_path.clone(), false, align) {
            Ok(session) => {
                println!(
                    "sentence-video: loaded '{}' ({}s, {} sentences)",
                    session.media_path,
                    session.media_duration,
                    session.cues.len()
                );
                let _ = session;
            }
            Err((status, msg)) => {
                eprintln!("sentence-video: cannot open media ({status}): {msg}");
                logs::error(
                    "session",
                    "cannot open the media passed on the command line",
                )
                .media(mp.clone())
                .detail(format!("status={status} · {msg}"))
                .send();
                if std::env::args().skip(1).len() > 0 {
                    // Keep serving so the user can still POST a valid session.
                    eprintln!("sentence-video: starting an empty workspace instead.");
                }
            }
        }
    }

    let bind = format!("{host}:{port}");
    let state_arc: Arc<app::AppState> = Arc::clone(&state);
    let handler: Arc<dyn server::Handler + Send + Sync> = state_arc;
    let addr = server::start(&bind, handler);
    logs::info("server", "listening")
        .detail(format!("http://{addr}/"))
        .send();

    // Open the browser once the server reports a bound address.
    if open_browser_flag {
        let url = format!("http://{addr}/");
        println!("sentence-video: auto-opening browser at {url}");
        open_browser(&url);
    }

    // Keep the process alive until Ctrl-C.
    loop {
        std::thread::park();
    }
}
