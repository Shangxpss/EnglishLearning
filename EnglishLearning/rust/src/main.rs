//! `sentence-video` — a single Rust executable that splits a media file into
//! sentence-level segments and serves them to the OS default browser.
//!
//! Usage:
//!   sentence-video [serve] <media-file> [--subtitle <srt|vtt>] [--no-browser]
//!                  [--host <addr>] [--port <n>]
//!
//! With no arguments a server starts with an empty workspace (media can be
//! added later via `POST /api/session`).

mod app;
mod media;
mod models;
mod pipeline;
mod segmenter;
mod server;
mod subtitle;

use std::sync::Arc;

const MEDIA_EXTS: &[&str] = &[
    "mp4", "mkv", "mov", "webm", "m4a", "mp3", "wav", "ogg", "aac",
];

fn print_usage() {
    println!(
        "sentence-video — sentence-segmented media player (pure Rust, single binary)\n\n\
         USAGE:\n\
         \x20 sentence-video [serve] <media-file> [OPTIONS]\n\
         \x20 sentence-video replace-audio <video> <audio> <output> [--bitrate <bps>] [--full]\n\
         \x20 sentence-video dub <video> <output> [--subtitle <srt>|<vtt>] [OPTIONS]\n\n\
         PLAY MODE (opens default browser):\n\
         \x20   --subtitle <srt|vtt>   subtitle file next to / for the media\n\
         \x20   --no-browser           don't auto-open the browser\n\
         \x20   --host <addr>          bind address (default 127.0.0.1)\n\
         \x20   --port <n>             bind port; 0 = OS-assigned (default)\n\n\
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
            "--subtitle" => { i += 1; subtitle = args.get(i).cloned(); }
            "--voice" => { i += 1; voice = args.get(i).cloned(); }
            "--tts-command" => { i += 1; if let Some(v) = args.get(i) { tts_command = v.clone(); } }
            "--voice-rate" => { i += 1; if let Some(v) = args.get(i) { voice_rate = v.parse().unwrap_or(voice_rate); } }
            "--bitrate" => { i += 1; if let Some(v) = args.get(i) { bitrate = v.parse().unwrap_or(bitrate); } }
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

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--subtitle" => {
                i += 1;
                subtitle_path = args.get(i).cloned();
            }
            "--no-browser" => open_browser_flag = false,
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
    let state = Arc::new(app::AppState::new(cache_dir));

    // Auto-create a session when a media file was passed in.
    if let Some(mp) = &media_path {
        match state.build_session(mp, subtitle_path.clone()) {
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