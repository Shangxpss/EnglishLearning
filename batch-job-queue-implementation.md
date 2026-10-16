# Batch job queue & completion notifications — implementation notes

Companion to [`batch-job-queue-and-notification-stack.md`](./batch-job-queue-and-notification-stack.md),
which argues _why_ the stack was chosen. This document describes _what was
built_: the modules, the contracts, the state machine, and the evidence that it
works.

Scope: a user picks N media files → each is pushed onto a durable background
queue → a notification is pushed when each finishes or fails. The user can close
the page, or restart the server, and nothing is lost.

---

## 1. At a glance

```
   browser                     server process (one binary)
   ───────                     ────────────────────────────
   /queue page
     │  POST /api/jobs  (N paths)
     │ ─────────────────────────────►  AppState::enqueue_jobs
     │                                     │  INSERT INTO jobs (queued)
     │ ◄── 202 {jobs:[…], rejected:[…]}     ▼
     │                                 SqliteQueue  ──┐ same sessions.db
     │                                    ▲           │
     │                 claim / heartbeat / │           │
     │                 progress / complete  │           │
     │                                    │           │
     │  GET /api/jobs (poll every 2 s) ────┘           │
     │                                                 ▼
     │                                            WorkerPool
     │                                     (std::thread, default 1)
     │                                                 │
     │                                     AppState::run(job)  ← JobRunner
     │                                       build_session + transcode
     │                                                 │  stage() / request_cancel()
     │                                                 │  read from build_jobs / playable_jobs
     │  GET /api/jobs/events  (SSE)    ◄───────────────┘
     │    event: job  {v,job_id,state,…}     SseHub (broadcast 512)
     └─► toast "Ready to play"
```

Two ports carry the whole design, and both are injected exactly like the
existing database port (`db: Arc<dyn Store>`):

| Port   | Trait                                                 | Local adapter                                            | Future adapter             |
| ------ | ----------------------------------------------------- | -------------------------------------------------------- | -------------------------- |
| queue  | [`JobQueue`](EnglishLearning/rust/src/jobs/mod.rs)    | [`SqliteQueue`](EnglishLearning/rust/src/jobs/sqlite.rs) | RabbitMQ / Redis / MQTT    |
| notify | [`Notifier`](EnglishLearning/rust/src/jobs/notify.rs) | [`SseHub`](EnglishLearning/rust/src/jobs/notify.rs)      | `rumqttc` retained publish |

The application code above these traits never learns which adapter is installed.

---

## 2. Component map

| Path                                                                                                                   | Responsibility                                                                          |
| ---------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| [`rust/src/jobs/mod.rs`](EnglishLearning/rust/src/jobs/mod.rs)                                                         | `Job`, `JobSpec`, `JobPayload`, state constants, `JobQueue`/`JobRunner` traits, `Retry` |
| [`rust/src/jobs/sqlite.rs`](EnglishLearning/rust/src/jobs/sqlite.rs)                                                   | The `jobs` table, claims, leases, backoff, cancel flags, 7 unit tests                   |
| [`rust/src/jobs/notify.rs`](EnglishLearning/rust/src/jobs/notify.rs)                                                   | `JobEvent`, `Notifier`, `SseHub`, 2 unit tests                                          |
| [`rust/src/jobs/worker.rs`](EnglishLearning/rust/src/jobs/worker.rs)                                                   | `WorkerPool`, `execute`, the per-job monitor thread                                     |
| [`rust/src/web.rs`](EnglishLearning/rust/src/web.rs)                                                                   | axum router, the legacy-route bridge, the job API + SSE route                           |
| [`rust/src/server.rs`](EnglishLearning/rust/src/server.rs)                                                             | Request/Response contract structs (no listener any more)                                |
| [`rust/src/app.rs`](EnglishLearning/rust/src/app.rs)                                                                   | `AppState` wiring, the job API, `impl JobRunner for AppState`                           |
| [`rust/src/main.rs`](EnglishLearning/rust/src/main.rs)                                                                 | CLI flags, queue construction, worker pool start, axum runtime                          |
| [`rust/src/picker.rs`](EnglishLearning/rust/src/picker.rs)                                                             | Native multi-select file dialog per platform                                            |
| [`frontend/web/src/features/queue/index.tsx`](EnglishLearning/frontend/web/src/features/queue/index.tsx)               | The `/queue` page                                                                       |
| [`frontend/web/src/providers/job-notifications.tsx`](EnglishLearning/frontend/web/src/providers/job-notifications.tsx) | The app-wide SSE subscription + toast stack                                             |
| [`frontend/web/src/lib/server-api.ts`](EnglishLearning/frontend/web/src/lib/server-api.ts)                             | Typed client for the job API and the SSE feed                                           |

**Why the traits are synchronous.** `Handler` and `JobQueue` are handed out as
`Arc<dyn …>`, and `async fn` in a trait is not `dyn`-compatible, so the async
boundary is bridged instead of propagated: the axum layer calls the blocking
`Handler::handle` inside `tokio::task::spawn_blocking`. The work being queued is
CPU-bound and has no await points anyway (FFmpeg, whisper.cpp, the wav2vec2
aligner), so a blocking pool is the honest shape — and a fixed pool of OS
threads _is_ the scheduler, which is also how concurrency is bounded.

---

## 3. HTTP surface

Everything below is native axum (route parameters use axum 0.8's `{id}` syntax).

### Submit a batch

```
POST /api/jobs
{ "media_paths": ["/abs/a.mp4", "/abs/b.mkv"], "kind": "build",
  "align": false, "subtitle_path": null }

202 Accepted
{ "jobs":     [{ "job_id": "00dee6c00ac4d850", "media_path": "/abs/a.mp4" }, …],
  "rejected": [{ "media_path": "/abs/gone.mp4", "error": "no such file" }] }
```

`202`, not `200`: the row is durable but the work has not started. Per-file
rejection is deliberate — one missing path must not cost the other nine files
their place in the queue. `kind` currently accepts only `"build"`; `align`
overrides the process-wide alignment default for this job, and `subtitle_path`
pins the cue source (otherwise [`locate_subtitle`](EnglishLearning/rust/src/pipeline/transcribe.rs)
resolves `<stem>_full_sentence.srt` or transcribes).

Re-submitting a file that is **already queued or running** returns that job's
existing id rather than creating a duplicate — the partial unique index
`ON jobs(media_path, cache_key, kind) WHERE state IN ('queued','running')`
rejects the second insert and the error is mapped back to the live id.

### Read the queue

```
GET /api/jobs?state=&kind=&limit=200        → { "jobs": [ Job, … ] }   newest first
GET /api/jobs/{id}                          → Job | 404
```

A `Job` row as serialised (note `payload` is `#[serde(skip_serializing)]` — it
can hold a subtitle path and has no business in the UI):

```json
{
  "id": "…",
  "media_path": "…",
  "cache_key": "…",
  "kind": "build",
  "state": "running",
  "stage": "transcode",
  "done": 0,
  "total": 0,
  "attempts": 1,
  "max_attempts": 3,
  "last_error": null,
  "session_id": null,
  "enqueued_at": 1792137467,
  "started_at": 1792137468,
  "finished_at": 1792137491,
  "elapsed_secs": 23
}
```

`stage`/`done`/`total` are overlaid live at serialisation time from the in-memory
maps the pipeline already keeps (`build_jobs` for decode/transcribe/align,
`playable_jobs` for the transcode) — see `build_stage` in [app.rs](EnglishLearning/rust/src/app.rs).
No pipeline changes were needed to get progress out of it.

### Cancel

```
POST /api/jobs/{id}/cancel                  → { "job_id": "…", "state": "cancelled" }
```

A **queued** job stops outright. A **running** job is asked to stop and does so
at its next stage boundary: a whisper pass or a wav2vec2 window cannot be cut in
half, and pretending otherwise would report a state the pipeline is not in. The
response carries the state the job is in _now_, which is why the value may be
`running` even though cancellation has been recorded.

### Notifications

```
GET /api/jobs/events        → text/event-stream

event: job
data: {"v":1,"job_id":"…","kind":"build","state":"done","media_path":"…",
       "session_id":"…","stage":null,"done":0,"total":0,"attempts":1,
       "error":null,"finished_at":1792137491}

: keep-alive            (every 15 s)
```

Every connection is first sent the **current snapshot** (`AppState::list_job_events`),
then live updates. Delivery is **at-least-once**: the snapshot is replayed after
each reconnect, so a consumer must dedupe on `(job_id, state)`. That pairing is
why `job_id` is in the payload at all. A subscriber that lags the 512-event
broadcast buffer drops old events rather than stalling the publisher; the client
re-syncs from `GET /api/jobs`.

### File picker (multi-select)

```
POST /api/pick-file?kind=media|subtitle[&multi=1]
  single → { "path": "/abs/a.mp4" } | { "path": null }      (null = cancelled)
  multi  → { "paths": ["/abs/a.mp4", "/abs/b.mkv"] }        ([] = cancelled)
```

Browsers hide the real path of a `<input type="file">`, so the page asks the
server — which runs on the same machine in the default loopback setup — to open
the OS dialog. Each platform is driven natively (`zenity --multiple`,
`kdialog --multiple`, `osascript … with multiple selections allowed`,
`OpenFileDialog.Multiselect`); all of them are asked for newline-separated output
so one parser handles single and multi alike. The route is guarded against
cross-origin callers so a random website cannot drive a local dialog.

---

## 4. Job lifecycle

```
                   enqueue
                     │
                     ▼
                 ┌────────┐  claim (BEGIN IMMEDIATE, oldest first,
                 │ queued │  gated on enqueued_at <= now for backoff)
                 └────────┘
                     │
        cancel ◄─────┤
        (instant)    ▼
                 ┌─────────┐  heartbeat (lease)  ← monitor thread, every 500 ms
                 │ running │  progress (stage)   ← mirrored from the pipeline
                 └─────────┘  cancel_requested  → flips the pipeline flag
                     │
      ┌──────────────┼───────────────────┬──────────────┐
      ▼              ▼                   ▼              ▼
  ┌────────┐   ┌─────────┐         ┌──────────┐   ┌───────────┐
  │  done  │   │ queued  │         │  failed  │   │ cancelled │
  └────────┘   │ (retry) │         │ (DLQ)    │   └───────────┘
   terminal    └─────────┘         └──────────┘    terminal
                    │                   ▲
                    └── attempts > 3 ───┘
```

- **Retry.** A failure is retried while `attempts < max_attempts` (3), with
  `5·2^attempts` seconds of backoff capped at 300 s. The backoff gate is the
  `enqueued_at <= now` condition in the claim query, so a requeued job simply
  waits. Past the cap it is **dead-lettered**: state `failed`, `last_error` kept,
  no further retries. A job that fails deterministically cannot loop forever.
- **Cancelled is not failed.** `JobFailure::Cancelled` is a clean terminal state
  with no retry and no alarm; only a real error is reported as an error.
- **Lease.** A claim sets `lease_expires_at`; the monitor thread extends it every
  500 ms. If the process dies mid-job, the lease stops being extended and the
  worker that starts next calls `reclaim_expired` (also at boot), which returns
  the row to `queued`. Because the attempt was already counted at claim time, a
  job that reliably kills the process still burns its attempts and dies in the
  DLQ rather than crash-looping.
- **`done` means playable.** The worker runs the browser-playable H.264 transcode
  inline before completing the job, so a `done` notification means "you can press
  play", not "a session row exists".

---

## 5. The axum bridge, and why seeking still works

~30 routes predate this work and are plain synchronous `(Request) -> Response`
methods on `impl Handler for AppState`. Rather than rewrite them, the axum router
installs a single `fallback` that:

1. reads the body (`to_bytes`, capped at 16 MB),
2. converts `axum::extract::Request` → `server::Request` (method, decoded path,
   parsed query, headers lowercased),
3. calls `app.handle(&request)` on `spawn_blocking`,
4. converts `server::Response` → axum, copying status, headers and body and
   setting **no** `Content-Length` (hyper chooses it).

Nothing in the existing handlers changed, which is why the media route's `Range`
handling survived verbatim — that was the acceptance test for the port, because
seeking is the one behaviour a player cannot lose:

| Request                  | Response                                        |
| ------------------------ | ----------------------------------------------- |
| `Range: bytes=0-99`      | `206 Partial Content`, correct `Content-Range`  |
| `Range: bytes=1000-1099` | `206`, offset window                            |
| `Range: bytes=99999999-` | `416 Range Not Satisfiable`                     |
| no `Range`               | `200`, `Content-Length`, `Accept-Ranges: bytes` |

The cost of this approach is that the media-only `--no-default-features` build
(~25 MB) is consciously given up: axum, tokio, tower-http, async-stream and
futures-util are now always compiled, because the queue API and the SSE feed are
part of the product rather than an optional extra.

---

## 6. Frontend

**Two transports, two jobs.** The `/queue` page polls `GET /api/jobs` every 2 s
for the table, because the only source of truth for a job's state is the server's
table and a poll is trivially correct. The _notification_, though, is pushed:
[`job-notifications.tsx`](EnglishLearning/frontend/web/src/providers/job-notifications.tsx)
holds one `EventSource` for the whole app and renders the toast stack.

That split matters because of where the subscription lives. A job takes minutes,
so by the time it ends the user has usually navigated away — a subscription owned
by the queue page would report nothing. The provider is therefore mounted in
[`app-shell.tsx`](EnglishLearning/frontend/web/src/layout/app-shell.tsx), which
keeps it alive across route changes _and_ gives it router context (the "done"
toast deep-links to `/player?session=…`, which needs `<Link>`).

Two guards keep the at-least-once feed from becoming a wall of toasts:

- **Dedupe** on `(job_id, state)`, tracked for the life of the tab.
- **Freshness** — a terminal event whose `finished_at` is more than 15 s old is
  treated as history and added to the _seen_ set without being shown. Without it,
  every page load would re-announce every job ever run, because the SSE snapshot
  carries no way to distinguish "just now" from "yesterday" except the timestamp.

Toasts auto-dismiss after 10 s, show the failure reason when there is one, and
show "Open in player →" when the job produced a session.

The page itself shows the queue depth (the default of one worker means five files
take five times one file, which has to look deliberate rather than stuck), a
live stage word per running row, per-row cancel, and the per-file rejection list
from the last submit.

---

## 7. Configuration & operations

```bash
pnpm run dev:server                 # rust dev server on :8018
pnpm run dev:web                    # vite on :5173, proxies /api → :8018

sentence-video serve --port 8018 --data-dir ~/.local/share/sentence-video \
                     --workers 2 --no-browser
```

| Setting                                    | Default                         | Note                                                                              |
| ------------------------------------------ | ------------------------------- | --------------------------------------------------------------------------------- |
| `--workers <n>` / `SENTENCE_VIDEO_WORKERS` | 1                               | Concurrency. `>1` trades RAM (whisper model + weights per worker) for throughput. |
| `--data-dir <path>`                        | `~/.local/share/sentence-video` | The `jobs` table lives in the same `sessions.db` as the sessions.                 |
| lease                                      | 30 s                            | Passed from `main.rs`; the monitor heartbeats every 500 ms.                       |
| `max_attempts`                             | 3                               | Per job; backoff `5·2^n` s, capped at 300 s.                                      |
| idle poll                                  | 250 ms                          | How quickly a queued job starts.                                                  |
| SSE keep-alive                             | 15 s                            | Keeps proxies from closing an idle stream.                                        |

Log lines (`logs.db`, filter by component `queue`): `worker started`,
`job started` (with `kind=… attempt=n/3`), `job finished` (with the session id
and duration), `job failed; will retry`, `job failed permanently`, `job cancelled`.

Restart behaviour: the process can be killed at any point. On the next boot
`reclaim_expired` returns orphaned `running` rows to `queued`, and the pool picks
them up — the attempt was already counted, so this is a retry, not a fresh start.

---

## 8. Testing

Unit tests, all hermetic (temp SQLite files, no network, no media):

- `jobs/sqlite.rs` (7): oldest-first claim, re-submit idempotency, terminal states
  free the unique key, cancelling a queued vs a running job, backoff → dead-letter,
  lease reclaim, progress/heartbeat only accepted on a running job.
- `jobs/notify.rs` (2): publish reaches a subscriber; publishing with no
  subscriber is not an error.
- `picker.rs` (2): dialog titles; one path per line, so single and multi
  selection share the parser.

End-to-end evidence gathered against a running server:

| Check          | Result                                                                                                                                                                                     |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `cargo test`   | 133 passed, 0 failed                                                                                                                                                                       |
| `cargo check`  | no warnings introduced by this work                                                                                                                                                        |
| `tsc -b`       | only the 5 documented pre-existing errors                                                                                                                                                  |
| `vite build`   | succeeds                                                                                                                                                                                   |
| Batch submit   | 8 files → `202`, `rejected: []`                                                                                                                                                            |
| SSE stream     | `queued → running → done` delivered, with `session_id`                                                                                                                                     |
| Media `Range`  | `206` + `Content-Range`; `416` unsatisfiable; `200` full                                                                                                                                   |
| Crash recovery | server hard-killed mid-job → on restart the job was reclaimed and re-run (`attempts: 2`), nothing orphaned                                                                                 |
| Browser        | RUNNING row showed stage "encoding H.264", QUEUED row "waiting for a worker", then two sequential **"Ready to play"** toasts with "Open in player →" links, matching rows flipping to DONE |

One pre-existing Windows-only test failure was repaired along the way:
`an_in_flight_build_is_reported_until_it_ends` registered a build with the
POSIX-shaped `/media/a.mp4` while `cancel_build` normalises the path that arrives
over HTTP — on Windows those two spellings differ (`C:\media\a.mp4`). The test now
derives both from one `media::normalize_path` call.

---

## 9. Deliberate deferrals and known limits

Deferred, by design (design doc §4–§6): the feature-gated `mqtt` (`rumqttc`) and
`broker` (`lapin`) notifier/queue adapters, OS-level toasts, and the k8s cutover.
[`AppState::with_notifier`](EnglishLearning/rust/src/app.rs) is marked as the
one seam a second transport gets installed at.

Known limits worth stating plainly:

- **One process.** The `SseHub` is in-memory, so it federates nothing: a second
  instance of the server would not see the first one's events, and its workers
  would compete for the same table without sharing a notify path. That is exactly
  the case the broker adapter exists for.
- **SQLite is single-writer.** All queue access goes through one `Mutex<Connection>`,
  so raising `--workers` does not raise DB throughput. Contention is small — the
  lock is held only for short claim/update statements — but it is a ceiling.
- **The transcode phase has no percentage.** `ensure_playable_*` records presence
  and state only, so the queue can say _what_ is happening (and does: stage
  `transcode`), not how far along. The decode/transcribe/align stages do have
  counters and do show a moving bar.
- **Field naming is asymmetric.** The SSE payload calls it `job_id` (it is an
  event about a job) while `GET /api/jobs` rows call it `id` (it is the job). The
  client handles both; a future payload version could unify them, which is why
  every event carries `"v": 1`.
- **Five in-memory maps remain.** `dub`, `mux`, `platform`, `playable` and
  `build` job state still lives in `AppState` rather than in the queue table.
  Routes that use them work across a restart only because the session cache is in
  SQLite; their progress is lost. Consolidating them onto the queue is the
  obvious next increment.
