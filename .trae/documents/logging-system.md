# Logging system — logs.db + text mirror + Logs page

## Context

`sentence-video` (Rust backend on :8018, Vite frontend on :5173) is a local, single-user
app. When something goes wrong today the only evidence is whatever `println!`/`eprintln!`
happened to be on the terminal that is still open: HTTP request handling in
[app.rs](file:///home/shang/Desktop/project/EnglishLearning/EnglishLearning/rust/src/app.rs),
dubbing in `pipeline/synthesize.rs`, ASR/alignment in `pipeline/transcribe.rs` and
`align/`. Nothing is persisted, so a failure noticed later (a stuck dubbing job, a mux
that never produced a file, a session that vanished) cannot be traced after the fact.

The goal is a real logging system that records what the server did into a database and a
text file, and a page to read it — so any problem can be traced after the fact.

Decisions (confirmed with the user):
- **Scope:** everything, including debug-level internal steps.
- **Storage:** a **separate `logs.db`** SQLite file in the data dir (keeps `sessions.db`
  lean; logs can be pruned/wiped independently).
- **Reading:** a **Logs page** in the web UI + REST API, **and** a rotating plain-text
  mirror on disk.
- **Retention:** last **50 000 rows / 30 days**.

## Approach

### 1. New module `rust/src/logs.rs`

A self-contained, dependency-free logger (only `rusqlite`, already present; no `log`/
`tracing` crate, matching the single-binary ethos).

- `Level` (`Debug|Info|Warn|Error`, with `as_str`/`from_str`).
- `LogRecord` (`Serialize`): `id, ts_ms, level, category, message, session_id?,
  media_path?, detail?, duration_ms?`. `category` is a short string
  (`server`/`http`/`session`/`build`/`asr`/`align`/`dub`/`mux`/`transcode`/`platform`/`db`).
- `Entry` builder: `logs::info("dub", "dubbing started").session(&id).media(&p)
  .detail(..).duration_ms(..).send();` — with `logs::debug/warn/error` peers. `send()`
  is **non-blocking**: it pushes onto an `mpsc::Sender` (fire-and-forget; a full/absent
  sink falls back to `eprintln!`, never panics or blocks the caller).
- `init(db_path, mirror_path: Option<PathBuf>, min_level) -> Result<(), String>` opens
  `logs.db`, applies the schema, spawns **one writer thread** that drains the channel,
  batches inserts in a transaction, appends the text mirror, and periodically prunes.
  Stored in a `static OnceLock<Sink>` so *any* module/thread can log without plumbing a
  handle through (logging is genuinely cross-cutting; this is the one global).
- `query(filters) -> Vec<LogRecord>`, `stats() -> Value`, `clear() -> usize` open a short
  lived **read-only** connection (WAL allows readers alongside the writer).

Schema (created in `init`):

```sql
CREATE TABLE IF NOT EXISTS logs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms       INTEGER NOT NULL,
    level       TEXT    NOT NULL,
    category    TEXT    NOT NULL,
    message     TEXT    NOT NULL,
    session_id  TEXT,
    media_path  TEXT,
    detail      TEXT,
    duration_ms INTEGER
);
CREATE INDEX IF NOT EXISTS idx_logs_ts       ON logs(ts_ms);
CREATE INDEX IF NOT EXISTS idx_logs_level    ON logs(level);
CREATE INDEX IF NOT EXISTS idx_logs_category ON logs(category);
CREATE INDEX IF NOT EXISTS idx_logs_session  ON logs(session_id);
```

Retention (writer thread, ~every 2 min + once at startup): delete rows past 30 days, then
keep only the newest 50 000 (`DELETE FROM logs WHERE id NOT IN (SELECT id FROM logs ORDER
BY id DESC LIMIT 50000)`).

Text mirror: `data_dir/logs/sentence-video.log`, one line per record
(`ts level category message · session · detail`); manual rotation at ~10 MB to `.1`…`.3`.

### 2. Request chokepoint — `rust/src/app.rs`

Extract the current body of `impl Handler for AppState::handle`
([app.rs#L971-L1020](file:///home/shang/Desktop/project/EnglishLearning/EnglishLearning/rust/src/app.rs#L971-L1020))
into `fn dispatch(&self, req) -> Response`, and wrap it in `handle`:

```rust
fn handle(&self, req: &Request) -> Response {
    let started = std::time::Instant::now();
    let res = self.dispatch(req);
    if should_log_request(&req.path) {
        logs::log(level_for(res.status), "http",
                  format!("{} {} → {}", req.method, req.path, res.status))
            .detail(error_body(&res))            // 4xx/5xx JSON body
            .duration_ms(started.elapsed().as_millis() as i64)
            .send();
    }
    res
}
```

`level_for`: 5xx→`error`, 4xx→`warn`, else `info`.

**Skip high-frequency paths** or the DB explodes / the Logs page feeds itself:
`/media`, `/muxed`, `/dubbed-audio`, `/audio` (video streaming issues many `Range`
requests), `/app.js`, `/styles.css`, `/`, `/index.html`, `/health`, and `/api/logs*`.
This is deliberate and documented in the module header.

### 3. Background-job instrumentation

Emit structured entries at every job boundary (these are today's `println!/eprintln!`
sites plus a few new ones), routed through `logs::` so they also land in the DB:

- **startup** (`main.rs` ~L735-775): data_dir, `sessions.db` path, ASR model, dub engine,
  port, whether align is on. Also log the `sessions.db` open failure / temp fallback.
- **build** (`app.rs::begin_build` L255, `build_session`): start → decode → transcribe →
  align → finish/fail/cancel.
- **asr** (`pipeline/transcribe.rs`, `asr.rs`): model load, transcribe start/finish,
  word-snap counts.
- **align** (`align/mod.rs` / `pipeline/transcribe.rs`): per-window start/finish/fail at
  **debug** (user chose debug scope).
- **dub** (`app.rs::start_dub` L620-730 + `pipeline/synthesize.rs`): start, **per-sentence
  at debug**, stitch, finish (with the failed-sentence list), fail.
- **mux** (`app.rs::auto_mux` L833, `route_mux`, `ensure_muxed_async` L503): start →
  finish/fail.
- **transcode** (`app.rs::ensure_playable_async` L391-437): start → finish/fail.
- **platform export** (`app.rs::start_platform_subtitle` ~L551-588): start → finish/fail.

Pattern: keep the existing console line (logger also echoes to stderr) and add a
`logs::…` entry beside it, rather than deleting the prints.

### 4. AppState + API

`AppState::new` gains nothing (the logger is global); but the API handlers live on it:

- `GET /api/logs?level=&category=&session=&media=&q=&since_ms=&before_id=&limit=` →
  `{ records: [...], total: n, stats: {...} }`, newest first, `limit` default 500 / max 5000.
- `GET /api/logs/stats` → counts per level + per category + oldest/newest ts.
- `DELETE /api/logs` → clear the table (and truncate the mirror).

Registered in `handle`/`dispatch` next to `("GET", "/api/sessions")` etc.
`rust/src/db.rs`/the `Store` trait is **not** touched — `logs.db` is independent.

### 5. Frontend

- `src/lib/server-api.ts`: add `LogRecord`, `LogQuery`, `LogStats` + `getLogs()`,
  `getLogStats()`, `clearLogs()`, matching the existing `readError`/`fetch` style.
- New `src/features/logs/index.tsx`: level chips, category select, session/text search,
  a table (time · level · category · message · session/media · expandable detail),
  auto-refresh toggle (poll `GET /api/logs` every ~2 s), a Clear button, and a
  download/`复制` of the current view. Reuse `cn` ([utils.ts](file:///home/shang/Desktop/project/EnglishLearning/EnglishLearning/frontend/web/src/lib/utils.ts)) and
  `ui/button.tsx`; model the fetch/poll structure on the Manage page
  ([manage/index.tsx](file:///home/shang/Desktop/project/EnglishLearning/EnglishLearning/frontend/web/src/features/manage/index.tsx#L123-L213)).
- Register the route in [routes.tsx](file:///home/shang/Desktop/project/EnglishLearning/EnglishLearning/frontend/web/src/router/routes.tsx#L16-L51)
  (`{ path: 'logs', element: <LogsPage /> }`) and a nav item in
  [navigation.tsx](file:///home/shang/Desktop/project/EnglishLearning/EnglishLearning/frontend/web/src/router/navigation.tsx#L18-L20)
  (label "Logs", a `lucide-react` icon such as `ScrollText`).

### 6. CLI / config (`main.rs`)

- `--log-level <debug|info|warn|error>` (default `debug`, per the user's choice);
  env `SENTENCE_VIDEO_LOG_LEVEL` as the fallback.
- Logs always mirrored to `data_dir/logs/sentence-video.log`; `--no-log-file` opts out.
- `--data-dir` already exists — `logs.db` sits beside `sessions.db` there.

## Critical files

- **New:** `rust/src/logs.rs`, `frontend/web/src/features/logs/index.tsx`.
- **Edit:** `rust/src/main.rs` (declare `mod logs;`, init logger, new flags, log startup),
  `rust/src/app.rs` (dispatch split, request logging, `/api/logs*` handlers, job entries),
  `rust/src/server.rs` (only if the writer needs a lifespan hook — likely untouched),
  `rust/src/pipeline/transcribe.rs`, `rust/src/pipeline/synthesize.rs`,
  `rust/src/pipeline/align` path (`align/mod.rs`) for debug entries,
  `frontend/web/src/lib/server-api.ts`,
  `frontend/web/src/router/routes.tsx`, `frontend/web/src/router/navigation.tsx`.

## Notes / pitfalls

- Logging must never block a request or die: `send()` is fire-and-forget with an
  `eprintln!` fallback before `init` runs.
- The request-path skip list is essential — media `Range` requests and the Logs page's own
  poll would otherwise flood the table.
- The writer thread and the API reader use **separate connections** (WAL), so a long query
  never stalls logging.
- `agent` (axum) traffic is out of scope for v1 (it does not go through `Handler`); noted
  as a follow-up.

## Verification

1. `bash rust/scripts/cargo-shim.sh build --features asr-align` (cwd `.../EnglishLearning`)
   — expect 0 errors (pre-existing warnings only); never raw `cargo` (FFmpeg hang).
2. `cargo test --features asr-align` for the new `logs.rs` unit tests (insert → query →
   filter → prune) — run with the dev watcher stopped to avoid incremental-cache races.
3. Restart the dev server; confirm `data_dir/logs.db` and
   `data_dir/logs/sentence-video.log` are created and `logs.db` has the `logs` table.
4. `curl` a few endpoints and check rows land: `curl /api/sessions`, then
   `curl '/api/logs?limit=20'`; `curl '/api/logs?level=error'`; `curl -X DELETE /api/logs`
   then re-list. Verify startup + job entries (e.g. open a session → `build`/`http` rows).
5. Confirm the mirror file grows and rotates.
6. `tsc -b` in `frontend/web` → only the 5 known pre-existing errors.
7. In the browser: `/logs` shows records, level/category/search filters work, auto-refresh
   tails new lines while a dub/mux runs, and Clear empties the table.
