//! Application state + request routing.

use crate::media;
use crate::models::Session;
use crate::segmenter;
use crate::server::{Handler, Request, Response};
use crate::subtitle;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const STYLES_CSS: &str = include_str!("../assets/styles.css");

pub struct AppState {
    sessions: Mutex<HashMap<String, Session>>,
    cache_dir: std::path::PathBuf,
}

impl AppState {
    pub fn new(cache_dir: std::path::PathBuf) -> Self {
        std::fs::create_dir_all(&cache_dir).ok();
        AppState {
            sessions: Mutex::new(HashMap::new()),
            cache_dir,
        }
    }
}

fn gen_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}",
        (nanos.rotate_left(32) as u64 ^ ((c as u64) << 40)) & 0xFFFFFFFFFFFFFFu64
    )
}

impl Handler for AppState {
    fn handle(&self, req: &Request) -> Response {
        let path = req.path.as_str();
        match (req.method.as_str(), path) {
            ("GET", "/") | ("GET", "/index.html") => bytes_ok(INDEX_HTML.as_bytes(), "text/html; charset=utf-8"),
            ("GET", "/app.js") => bytes_ok(APP_JS.as_bytes(), "application/javascript; charset=utf-8"),
            ("GET", "/styles.css") => bytes_ok(STYLES_CSS.as_bytes(), "text/css; charset=utf-8"),
            ("GET", "/health") => Response::text(200, "application/json", r#"{"status":"ok"}"#),
            ("GET", "/api/sessions") => self.list_sessions(),

            ("POST", "/api/session") => self.create_session(req),

            (m, p) if m == "GET" && p.starts_with("/api/session/") => {
                self.route_session(p, req)
            }

            _ => Response::not_found(),
        }
    }
}

impl AppState {
    fn create_session(&self, req: &Request) -> Response {
        let body = match serde_json::from_slice::<serde_json::Value>(&req.body) {
            Ok(b) => b,
            Err(_) => return Response::error(400, "invalid JSON body"),
        };
        let media_path = match body.get("media_path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => media::normalize_path(p),
            _ => return Response::error(400, "media_path is required"),
        };
        let subtitle_path = body
            .get("subtitle_path")
            .and_then(|v| v.as_str())
            .map(|p| media::normalize_path(p));

        match self.build_session(&media_path, subtitle_path) {
            Ok(session) => {
                let body = serde_json::json!({
                    "session_id": session.id,
                    "media_duration": session.media_duration,
                    "cue_count": session.cues.len(),
                });
                Response::text(200, "application/json; charset=utf-8", body.to_string())
            }
            Err((status, msg)) => Response::error(status, &msg),
        }
    }

    /// Build and register a session from a media path and optional subtitle.
    /// Returns the session id. Errors carry an HTTP status + message.
    pub fn build_session(
        &self,
        media_path: &str,
        subtitle_path: Option<String>,
    ) -> Result<Session, (u16, String)> {
        let media_path = media::normalize_path(media_path);
        if !Path::new(&media_path).is_file() {
            return Err((404, format!("media file not found: {media_path}")));
        }
        let media_duration = media::probe_duration(&media_path).map_err(|e| (500, e))?;

        // v1 requires a subtitle file (a whisper.cpp transcription path is a
        // future enhancement).
        let subtitle_path = match subtitle_path {
            Some(p) if Path::new(&p).is_file() => media::normalize_path(&p),
            Some(p) => return Err((404, format!("subtitle file not found: {p}"))),
            None => match auto_subtitle(&media_path) {
                Some(p) => p,
                None => {
                    return Err((422, "no subtitle provided; supply subtitle_path or place a .srt/.vtt next to the media".to_string()))
                }
            },
        };

        let raw_cues = subtitle::parse_subtitle(&subtitle_path).map_err(|e| (422, e))?;
        let cues = segmenter::finalize(raw_cues, media_duration, &media_path);

        let id = gen_id();
        let session = Session {
            id: id.clone(),
            media_path,
            media_duration,
            source_format: subtitle::detect_format(&subtitle_path).to_string(),
            cues,
        };
        self.sessions.lock().unwrap().insert(id.clone(), session.clone());
        Ok(session)
    }

    fn list_sessions(&self) -> Response {
        let guard = self.sessions.lock().unwrap();
        let summary: Vec<serde_json::Value> = guard
            .values()
            .map(|s| {
                serde_json::json!({
                    "session_id": s.id,
                    "media_path": s.media_path,
                    "media_duration": s.media_duration,
                    "cue_count": s.cues.len(),
                })
            })
            .collect();
        Response::text(200, "application/json; charset=utf-8", serde_json::to_string(&summary).unwrap())
    }

    fn route_session(&self, path: &str, req: &Request) -> Response {
        let rest = &path["/api/session/".len()..];
        let mut segs = rest.split('/');
        let id = segs.next().unwrap_or_default().to_string();
        let sub = segs.next();

        let session = {
            let guard = self.sessions.lock().unwrap();
            match guard.get(&id) {
                Some(s) => s.clone(),
                None => return Response::error(404, "unknown session id"),
            }
        };

        match sub {
            None | Some("") => {
                let body = serde_json::to_vec(&session).map_err(|e| e.to_string());
                match body {
                    Ok(b) => bytes_ok(&b, "application/json; charset=utf-8"),
                    Err(e) => Response::error(500, &e),
                }
            }
            Some("cues") => {
                let body = serde_json::to_vec(&session.cues).unwrap();
                bytes_ok(&body, "application/json; charset=utf-8")
            }
            Some("media") => self.stream_media(&session, req),
            Some("segments") => {
                let idx = segs.next().unwrap_or_default();
                self.route_segment(&session, idx, req)
            }
            Some(_) => Response::not_found(),
        }
    }

    fn stream_media(&self, session: &Session, req: &Request) -> Response {
        let path = &session.media_path;
        let media_type = mime_for(path);
        stream_file_range(path, media_type, req)
    }

    fn route_segment(&self, session: &Session, _idx: &str, req: &Request) -> Response {
        // Echo the requested slice back as a WAV decoded from the source.
        let start = req.query.get("start").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let end = req.query.get("end").and_then(|v| v.parse::<f64>().ok()).unwrap_or(session.media_duration);
        if end < start {
            return Response::error(400, "end must be >= start");
        }
        let sample_rate = 24000u32;
        let out_path = self.cache_dir.join(format!("seg_{}_{}.wav", session.id, gen_id()));
        match media::decode_segment_wav(&session.media_path, start, end, sample_rate, &out_path.to_string_lossy()) {
            Ok(()) => {
                let data = match std::fs::read(&out_path) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = std::fs::remove_file(&out_path);
                        return Response::error(500, &e.to_string());
                    }
                };
                let _ = std::fs::remove_file(&out_path);
                Response::bytes(200, "OK", "audio/wav", data)
            }
            Err(e) => Response::error(500, &e),
        }
    }
}

fn auto_subtitle(media_path: &str) -> Option<String> {
    let stem = Path::new(media_path).with_extension("");
    for ext in ["srt", "vtt"] {
        let mut p = stem.as_os_str().to_owned();
        p.push(format!(".{ext}"));
        let p = p.to_string_lossy().into_owned();
        if Path::new(&p).is_file() {
            return Some(p);
        }
    }
    None
}

fn mime_for(path: &str) -> &'static str {
    let l = path.to_ascii_lowercase();
    if l.ends_with(".mp4") {
        "video/mp4"
    } else if l.ends_with(".webm") {
        "video/webm"
    } else if l.ends_with(".mkv") {
        "video/x-matroska"
    } else if l.ends_with(".mov") {
        "video/quicktime"
    } else if l.ends_with(".ogg") {
        "audio/ogg"
    } else if l.ends_with(".wav") {
        "audio/wav"
    } else if l.ends_with(".mp3") {
        "audio/mpeg"
    } else if l.ends_with(".m4a") {
        "audio/mp4"
    } else if l.ends_with(".aac") {
        "audio/aac"
    } else {
        "application/octet-stream"
    }
}

/// Serve a file with HTTP `Range` support.
fn stream_file_range(path: &str, content_type: &str, req: &Request) -> Response {
    let len = match std::fs::metadata(path).map(|m| m.len()) {
        Ok(l) => l,
        Err(_) => return Response::error(404, "media file unavailable"),
    };
    let accept = req.headers.get("range").cloned().unwrap_or_default();

    if accept.starts_with("bytes=") && !accept.trim().eq_ignore_ascii_case("bytes=0-") {
        let spec = accept.trim_start_matches("bytes=").trim();
        if let Some((s, e)) = parse_range(spec, len) {
            let mut file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(_) => return Response::error(500, "cannot open media file"),
            };
            if s >= len {
                return Response {
                    status: 416,
                    reason: "Range Not Satisfiable",
                    headers: vec![("Content-Range".into(), format!("bytes */{len}"))],
                    body: Vec::new(),
                };
            }
            let eff_end = e.min(len.saturating_sub(1));
            let count = eff_end - s + 1;
            let mut buf = vec![0u8; count as usize];
            file.seek(SeekFrom::Start(s)).ok();
            let _ = file.read_exact(&mut buf);
            return Response {
                status: 206,
                reason: "Partial Content",
                headers: vec![
                    ("Content-Type".into(), content_type.to_string()),
                    ("Accept-Ranges".into(), "bytes".into()),
                    ("Content-Range".into(), format!("bytes {}-{}/{}", s, eff_end, len)),
                ],
                body: buf,
            };
        }
    }

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Response::error(500, "cannot open media file"),
    };
    let mut buf = Vec::with_capacity(len as usize);
    let _ = file.read_to_end(&mut buf);
    Response {
        status: 200,
        reason: "OK",
        headers: vec![
            ("Content-Type".into(), content_type.to_string()),
            ("Accept-Ranges".into(), "bytes".into()),
        ],
        body: buf,
    }
}

/// Parse `start-end`, `start-`, or `-suffix` from a byte-range spec.
fn parse_range(spec: &str, len: u64) -> Option<(u64, u64)> {
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        // suffix range: last N bytes
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        let start = len.saturating_sub(n);
        return Some((start, len.saturating_sub(1)));
    }
    let start: u64 = a.trim().parse().ok()?;
    let end = if b.trim().is_empty() {
        len.saturating_sub(1)
    } else {
        b.trim().parse().ok()?
    };
    Some((start, end))
}

fn bytes_ok(data: &[u8], content_type: &str) -> Response {
    Response::bytes(200, "OK", content_type, data.to_vec())
}