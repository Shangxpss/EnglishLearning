//! The axum web layer: **one** listener on the media port serves the whole app.
//!
//! Two things share it:
//!
//! 1. The entire legacy route table, reached through the synchronous
//!    [`server::Handler`] trait. A catch-all [`fallback`] converts an axum request
//!    into a [`server::Request`], runs the handler on `spawn_blocking` (it is
//!    blocking: `rusqlite` + filesystem), and converts the [`server::Response`]
//!    back. That is deliberate: **~30 routes compile unchanged**, and the
//!    hand-rolled `Range` headers built in `app::stream_file_range`
//!    (`206` / `Content-Range` / `Accept-Ranges` / `416`) survive verbatim, so
//!    seeking in the player keeps working.
//! 2. The new **native async** job API and its SSE feed, written directly in
//!    axum — the reason this crate is the web layer at all.
//!
//! `Handler` and `JobQueue` stay synchronous because both are handed out as
//! `Arc<dyn …>`, and `async fn` in a trait is not `dyn`-compatible. The bridge is
//! `spawn_blocking`, never a blocking call on a tokio worker thread.

use crate::app::AppState;
use crate::jobs::{JobEvent, JobFilter};
use crate::server::{self, Handler, Request, Response};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Response as AxumResponse, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Requests carry small JSON payloads (a path list, options). Generous but
/// bounded, so a mistaken large upload cannot be buffered into memory.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// How often the SSE stream emits a keep-alive comment. Long transcriptions mean
/// minutes of silence otherwise, and an idle proxy may drop the connection.
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);

/// Build the router: the job API first (it must win over the catch-all), then
/// the bridge that serves everything else.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/jobs", get(list_jobs).post(create_jobs))
        .route("/api/jobs/events", get(job_events))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .fallback(dispatch)
        .with_state(state)
}

/// Bind and serve until the process ends. The listener is bound *here* so the
/// caller learns the real address (with the OS-assigned port when `--port 0`)
/// before traffic starts, which is when it opens the browser.
pub async fn serve(
    bind: &str,
    app: Router,
    on_bound: impl FnOnce(std::net::SocketAddr),
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("cannot bind {bind}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    on_bound(addr);
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("media server stopped: {e}"))
}

// ─────────────────────────────────────────────────────────────────────────
// The sync → async bridge
// ─────────────────────────────────────────────────────────────────────────

/// Catch-all: hand every unmatched request to the existing synchronous handler.
async fn dispatch(State(app): State<Arc<AppState>>, req: axum::extract::Request) -> AxumResponse {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => return into_axum(Response::error(413, "request body too large")),
    };
    // `server::Request` carries a *decoded* path and parsed query, matching what
    // the old wire parser produced — the route table depends on both.
    let request = Request {
        method: parts.method.as_str().to_string(),
        path: server::url_decode(parts.uri.path()),
        query: parts
            .uri
            .query()
            .map(server::parse_query)
            .unwrap_or_default(),
        headers: parts
            .headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_string(), v.to_string()))
            })
            .collect(),
        body,
    };
    // Blocking work (rusqlite, filesystem, and the media read behind `Range`):
    // it must not run on a tokio worker thread.
    let app = Arc::clone(&app);
    match tokio::task::spawn_blocking(move || app.handle(&request)).await {
        Ok(res) => into_axum(res),
        Err(_) => into_axum(Response::error(500, "the request handler panicked")),
    }
}

/// Convert the internal response into an axum one.
///
/// The `Range` headers are copied through unchanged and `Content-Length` is left
/// to axum/hyper (the internal response never sets it), so media seeking behaves
/// exactly as it did on the hand-rolled server.
fn into_axum(res: Response) -> AxumResponse {
    let status = StatusCode::from_u16(res.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut out = AxumResponse::new(Body::from(res.body));
    *out.status_mut() = status;
    let headers = out.headers_mut();
    for (name, value) in res.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            headers.append(name, value);
        }
    }
    out
}

/// A JSON response with an explicit status (the bridge always answers 200, so
/// the job API needs its own helper to say `202`, `404`, …).
fn json_response(status: StatusCode, value: &serde_json::Value) -> AxumResponse {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut res = AxumResponse::new(Body::from(body));
    *res.status_mut() = status;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    res
}

// ─────────────────────────────────────────────────────────────────────────
// Job API
// ─────────────────────────────────────────────────────────────────────────

/// `POST /api/jobs` body.
#[derive(serde::Deserialize)]
struct CreateJobs {
    /// The media files to process. Absolute paths, as returned by the picker.
    #[serde(default)]
    media_paths: Vec<String>,
    /// `build` (default) | `transcode` | `dub` | `platform`.
    #[serde(default)]
    kind: Option<String>,
    /// Per-job override of the process-wide wav2vec2 alignment setting.
    #[serde(default)]
    align: Option<bool>,
    #[serde(default)]
    subtitle_path: Option<String>,
}

/// `POST /api/jobs` — accept a batch and return immediately.
///
/// This is the whole point of the design: the request returns `202` with a job
/// id per file instead of blocking for minutes with no handle to poll.
async fn create_jobs(
    State(app): State<Arc<AppState>>,
    Json(req): Json<CreateJobs>,
) -> AxumResponse {
    if req.media_paths.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({ "error": "media_paths must not be empty" }),
        );
    }
    let outcome = tokio::task::spawn_blocking(move || {
        app.enqueue_jobs(req.media_paths, req.kind, req.align, req.subtitle_path)
    })
    .await;
    match outcome {
        Ok(value) => json_response(StatusCode::ACCEPTED, &value),
        Err(_) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({ "error": "the queue handler panicked" }),
        ),
    }
}

/// `GET /api/jobs?state=&kind=&limit=` — every job, newest first.
async fn list_jobs(State(app): State<Arc<AppState>>, uri: axum::extract::RawQuery) -> AxumResponse {
    let query = uri
        .0
        .as_deref()
        .map(server::parse_query)
        .unwrap_or_default();
    let filter = JobFilter {
        state: query.get("state").filter(|s| !s.is_empty()).cloned(),
        kind: query.get("kind").filter(|s| !s.is_empty()).cloned(),
        limit: query.get("limit").and_then(|v| v.parse().ok()),
    };
    let outcome = tokio::task::spawn_blocking(move || app.list_jobs(&filter)).await;
    match outcome {
        Ok(value) => json_response(StatusCode::OK, &value),
        Err(_) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({ "error": "the queue handler panicked" }),
        ),
    }
}

/// `GET /api/jobs/{id}` — one job, or `404`.
async fn get_job(State(app): State<Arc<AppState>>, Path(id): Path<String>) -> AxumResponse {
    let outcome = tokio::task::spawn_blocking(move || app.get_job(&id)).await;
    match outcome {
        Ok(Some(value)) => json_response(StatusCode::OK, &value),
        Ok(None) => json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({ "error": "unknown job id" }),
        ),
        Err(_) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({ "error": "the queue handler panicked" }),
        ),
    }
}

/// `POST /api/jobs/{id}/cancel` — a queued job is cancelled outright, a running
/// one stops at its next stage boundary; either way the current state is
/// returned so the caller can render it.
async fn cancel_job(State(app): State<Arc<AppState>>, Path(id): Path<String>) -> AxumResponse {
    let outcome = tokio::task::spawn_blocking(move || app.cancel_job(&id)).await;
    match outcome {
        Ok(Some(value)) => json_response(StatusCode::OK, &value),
        Ok(None) => json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({ "error": "unknown job id" }),
        ),
        Err(_) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({ "error": "the queue handler panicked" }),
        ),
    }
}

/// `GET /api/jobs/events` — the push transport, as Server-Sent Events.
///
/// The current snapshot is replayed first, so a tab opened after the work
/// started is immediately in sync (the SSE equivalent of an MQTT retained
/// message). Delivery is at-least-once: the client dedupes on `(job_id, state)`.
async fn job_events(
    State(app): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AxumResponse> {
    let mut rx = app.subscribe_jobs();
    let snapshot = {
        let app = Arc::clone(&app);
        tokio::task::spawn_blocking(move || app.list_job_events())
            .await
            .unwrap_or_default()
    };

    let stream = async_stream::stream! {
        for event in snapshot {
            yield Ok(Event::default().event("job").data(event_json(&event)));
        }
        loop {
            match rx.recv().await {
                Ok(event) => yield Ok(Event::default().event("job").data(event_json(&event))),
                // A slow client dropped some events; it re-syncs from
                // `GET /api/jobs` on the next one, so skipping is correct.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE)))
}

/// One-line JSON, as `data:` requires (SSE forbids raw newlines in a field).
fn event_json(event: &JobEvent) -> String {
    serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string())
}
