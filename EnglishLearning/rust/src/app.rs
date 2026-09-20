//! Application state + request routing.

use crate::db::{Db, Progress, SourceKey};
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
    /// Hot cache of sessions; the SQLite database is the source of truth.
    sessions: Mutex<HashMap<String, Session>>,
    cache_dir: std::path::PathBuf,
    db: Db,
}

impl AppState {
    pub fn new(cache_dir: std::path::PathBuf, db: Db) -> Self {
        std::fs::create_dir_all(&cache_dir).ok();
        AppState {
            sessions: Mutex::new(HashMap::new()),
            cache_dir,
            db,
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

            // Opens the OS file dialog on the machine running this server and
            // returns the chosen absolute path (the browser cannot provide it).
            ("POST", "/api/pick-file") => self.pick_file(req),

            (m, p) if (m == "GET" || m == "POST") && p.starts_with("/api/session/") => {
                self.route_session(p, req)
            }

            // SPA fallback: any other GET that is neither an API call nor a
            // static asset is a client-side route (e.g. `/player`, `/subtitle`).
            // Serve the embedded `index.html` so the frontend router handles it
            // instead of the user getting a 404 on refresh/deep-link.
            (m, p) if m == "GET" && !p.starts_with("/api/") && !has_extension(p) => {
                bytes_ok(INDEX_HTML.as_bytes(), "text/html; charset=utf-8")
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
        // `?force=1` rebuilds even when a cached session exists.
        let force = matches!(
            req.query.get("force").map(String::as_str),
            Some("1") | Some("true")
        );

        match self.build_session(&media_path, subtitle_path, force) {
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

    /// `POST /api/pick-file?kind=media|subtitle`
    ///
    /// Opens the native OS file dialog on the server machine and returns the
    /// chosen absolute path as `{"path": "..."}` (or `{"path": null}` when the
    /// user cancels). Guarded against cross-origin callers so a random website
    /// cannot drive the local dialog.
    fn pick_file(&self, req: &Request) -> Response {
        let origin = req.headers.get("origin").map(|s| s.as_str());
        if !local_origin_ok(origin) {
            return Response::error(
                403,
                "the file picker is only available from the same machine (loopback)",
            );
        }
        let kind = req.query.get("kind").map(|s| s.as_str()).unwrap_or("media");
        match crate::picker::pick_file(kind) {
            Ok(Some(path)) => Response::text(
                200,
                "application/json; charset=utf-8",
                serde_json::json!({ "path": path }).to_string(),
            ),
            Ok(None) => Response::text(200, "application/json; charset=utf-8", r#"{"path":null}"#),
            Err(e) => Response::error(500, &e),
        }
    }

    /// Build and register a session from a media path and optional subtitle.
    /// Returns the session id. Errors carry an HTTP status + message.
    ///
    /// The result is persisted to SQLite and keyed by the media path plus the
    /// media/subtitle modification times, so re-opening the same source later
    /// returns the stored sentence list instead of re-parsing (`force` skips
    /// the cache and rebuilds).
    pub fn build_session(
        &self,
        media_path: &str,
        subtitle_path: Option<String>,
        force: bool,
    ) -> Result<Session, (u16, String)> {
        let media_path = media::normalize_path(media_path);
        if !Path::new(&media_path).is_file() {
            return Err((404, format!("media file not found: {media_path}")));
        }
        let media_duration = media::probe_duration(&media_path).map_err(|e| (500, e))?;

        // Cue source: an explicit subtitle, else one sitting next to the media,
        // else speech-to-text (the model is embedded in the executable).
        let subtitle_path: Option<String> = match subtitle_path {
            Some(p) if Path::new(&p).is_file() => Some(media::normalize_path(&p)),
            Some(p) => return Err((404, format!("subtitle file not found: {p}"))),
            None => auto_subtitle(&media_path),
        };

        let key = SourceKey {
            media_path: media_path.clone(),
            media_mtime: file_mtime(&media_path),
            subtitle_path: subtitle_path.clone(),
            subtitle_mtime: subtitle_path.as_deref().and_then(file_mtime),
        };

        // Cache hit: the same media + subtitle (unchanged) was processed before.
        if !force {
            if let Some(existing) = self.db.find_by_source(&key) {
                self.sessions
                    .lock()
                    .unwrap()
                    .insert(existing.id.clone(), existing.clone());
                return Ok(existing);
            }
        }

        let (cues, source_format) = match &subtitle_path {
            Some(path) => {
                let raw_cues = subtitle::parse_subtitle(path).map_err(|e| (422, e))?;
                (
                    segmenter::finalize(raw_cues, media_duration, &media_path),
                    subtitle::detect_format(path).to_string(),
                )
            }
            None => {
                let transcript = crate::pipeline::transcribe::transcribe_words(&media_path)
                    .map_err(|e| (422, format!("no subtitle found and transcription failed: {e}")))?;
                if transcript.words.is_empty() {
                    return Err((422, format!("no speech found in '{media_path}'")));
                }
                (
                    crate::pipeline::transcribe::build_cues(
                        &transcript.words,
                        media_duration,
                        &media_path,
                    ),
                    "asr".to_string(),
                )
            }
        };

        let id = gen_id();
        let session = Session {
            id: id.clone(),
            media_path,
            media_duration,
            source_format,
            cues,
        };
        self.db
            .insert_session(&session, &key)
            .map_err(|e| (500, format!("cannot persist session: {e}")))?;
        self.sessions.lock().unwrap().insert(id, session.clone());
        Ok(session)
    }

    /// Fetch a session from the hot cache, falling back to SQLite.
    fn load_session(&self, id: &str) -> Option<Session> {
        if let Some(s) = self.sessions.lock().unwrap().get(id) {
            return Some(s.clone());
        }
        let session = self.db.get_session(id)?;
        self.sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), session.clone());
        Some(session)
    }

    fn list_sessions(&self) -> Response {
        let summary = self.db.list_sessions();
        match serde_json::to_string(&summary) {
            Ok(body) => Response::text(200, "application/json; charset=utf-8", body),
            Err(e) => Response::error(500, &e.to_string()),
        }
    }

    fn route_session(&self, path: &str, req: &Request) -> Response {
        let rest = &path["/api/session/".len()..];
        let mut segs = rest.split('/');
        let id = segs.next().unwrap_or_default().to_string();
        let sub = segs.next();

        let session = match self.load_session(&id) {
            Some(s) => s,
            None => return Response::error(404, "unknown session id"),
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
            // Exact audio slice of an arbitrary time range, decoded server-side.
            Some("audio") => self.route_audio(&session, req),
            Some("progress") => self.route_progress(&session, req),
            Some(_) => Response::not_found(),
        }
    }

    /// `GET /api/session/{id}/progress` → `{ last_index, position }`
    /// `POST /api/session/{id}/progress` with the same body → persists it.
    fn route_progress(&self, session: &Session, req: &Request) -> Response {
        match req.method.as_str() {
            "GET" => {
                let progress = self.db.get_progress(&session.id).unwrap_or(Progress {
                    last_index: -1,
                    position: 0.0,
                });
                match serde_json::to_string(&progress) {
                    Ok(body) => Response::text(200, "application/json; charset=utf-8", body),
                    Err(e) => Response::error(500, &e.to_string()),
                }
            }
            "POST" => {
                let progress: Progress = match serde_json::from_slice(&req.body) {
                    Ok(p) => p,
                    Err(_) => return Response::error(400, "invalid JSON body"),
                };
                match self.db.set_progress(&session.id, progress) {
                    Ok(()) => Response::text(200, "application/json; charset=utf-8", r#"{"ok":true}"#),
                    Err(e) => Response::error(500, &e),
                }
            }
            _ => Response::not_found(),
        }
    }

    fn stream_media(&self, session: &Session, req: &Request) -> Response {
        let path = &session.media_path;
        let media_type = mime_for(path);
        stream_file_range(path, media_type, req)
    }

    /// `GET /api/session/{id}/audio?start=<sec>&end=<sec>`
    ///
    /// Decodes exactly that slice of the source into a 24 kHz mono WAV clip.
    /// This is the server-side counterpart to `/media`: `/media` lets the
    /// browser play and seek the original file itself, whereas this guarantees
    /// sample-exact boundaries and returns audio only. `start`/`end` are
    /// clamped to the media duration, and the temp WAV is deleted right after
    /// it is read into the response.
    fn route_audio(&self, session: &Session, req: &Request) -> Response {
        let start = req
            .query
            .get("start")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            .max(0.0);
        let end = req
            .query
            .get("end")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(session.media_duration)
            .min(session.media_duration);
        if end <= start {
            return Response::error(400, "end must be greater than start");
        }
        let sample_rate = 24000u32;
        let out_path = self
            .cache_dir
            .join(format!("clip_{}_{}.wav", session.id, gen_id()));
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

/// Modification time in whole seconds since the Unix epoch, when available.
/// Used as part of the cache key so an edited subtitle rebuilds the session.
fn file_mtime(path: &str) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
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

/// True when the last path segment looks like a static asset (contains a `.`),
/// e.g. `/assets/index-abc.js`. Those must 404 rather than fall back to the
/// SPA `index.html`; extension-less paths are client-side routes.
fn has_extension(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .map(|seg| seg.contains('.'))
        .unwrap_or(false)
}

/// True when the request may drive the native file dialog: no `Origin` at all
/// (curl, same-origin navigation) or a loopback origin. A public website cannot
/// read the response without CORS headers, but this also stops it from popping
/// the dialog in the first place.
fn local_origin_ok(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let origin = origin.trim().to_ascii_lowercase();
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(end) = authority.strip_prefix('[') {
        end.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spa_fallback_distinguishes_routes_from_assets() {
        assert!(has_extension("/assets/index-abc.js"));
        assert!(has_extension("/favicon.svg"));
        assert!(!has_extension("/player"));
        assert!(!has_extension("/subtitle-video"));
        assert!(!has_extension("/"));
    }

    #[test]
    fn file_picker_allows_only_loopback_origins() {
        // Non-browser clients and same-origin navigations send no Origin.
        assert!(local_origin_ok(None));
        // Dev server (vite) and the binary itself.
        assert!(local_origin_ok(Some("http://127.0.0.1:5173")));
        assert!(local_origin_ok(Some("http://localhost:8018")));
        assert!(local_origin_ok(Some("http://[::1]:8018")));
        // A public site must not be able to open the dialog.
        assert!(!local_origin_ok(Some("https://evil.example")));
        assert!(!local_origin_ok(Some("http://127.0.0.1.evil.example")));
        assert!(!local_origin_ok(Some("http://192.168.1.5:5173")));
    }
}