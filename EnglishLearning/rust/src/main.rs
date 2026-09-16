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
         \x20 sentence-video [serve] <media-file> [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20   --subtitle <srt|vtt>   subtitle file next to / for the media\n\
         \x20   --no-browser           don't auto-open the browser\n\
         \x20   --host <addr>          bind address (default 127.0.0.1)\n\
         \x20   --port <n>             bind port; 0 = OS-assigned (default)\n\
         \x20   --help                 show this help\n\n\
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

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

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