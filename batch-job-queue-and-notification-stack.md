# Batch Job Queue + Completion Notification — Stack Selection

**Status:** proposal, not yet implemented
**Scope:** a feature that lets a user select many video/audio files, push them onto a
processing queue _without waiting_, and get notified the moment each one finishes or fails.

---

## 0. TL;DR (read this if you read nothing else)

| Question               | Answer                                                                                                                                                                                                                                                                                    |
| ---------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Kafka?                 | **No.** Kafka is a distributed _log_, not a work queue. It has no per-message ack/redelivery, no visibility timeout, no built-in dead-letter path, and consumers must poll. For "a person drops 20 files and wants a toast", it is the single most expensive wrong answer.                |
| RabbitMQ?              | **Not now, but yes later.** It has the correct _work-queue_ semantics (per-message ack/nack, redelivery, DLX, priority, TTL). Adopt it only when you actually run workers on more than one machine.                                                                                       |
| MQTT?                  | **Right tool for the _notification_, wrong tool for the _queue_.** MQTT has no visibility timeout and no DLQ, so it cannot protect a job. It is, however, the best browser-reachable push transport you have — **browsers cannot speak raw MQTT over TCP; they use MQTT over WebSocket.** |
| Web layer?             | **axum — a single web crate in production** (settled, §4.3). Today the repo hand-rolls `server.rs` _and_ runs axum for `agent`; production consolidates on axum.                                                                                                                          |
| So what do I build?    | **A durable `jobs` table in the SQLite you already ship + a worker pool on `spawn_blocking` + push over `axum` SSE/WebSocket, with MQTT as the k8s adapter.** Tomorrow (k8s): swap the adapter behind two traits — no application code changes.                                           |
| The real defect to fix | `POST /api/session` is _synchronous_, so a long build has **no job identity to poll** and the UI looks broken for minutes. That is the bug; the broker choice is secondary.                                                                                                               |

Design principle, taken from your own codebase: **two ports, adapters chosen at startup** —
exactly like `db::Db::open(..)` → `Arc<dyn Store>` today.

---

## 1. The requirement, and the constraints it must respect

**Functional**

1. Multi-select N media files → enqueue N jobs → return immediately (HTTP `202 Accepted`).
2. Jobs run in the background with bounded concurrency; the process survives a restart.
3. Per-job observable state: `queued → running (stage, %, elapsed) → done | failed | cancelled`.
4. On terminal state, actively **push** a notification to the user (your ask: MQTT or WebSocket).
5. Cancel a queued or running job.
6. The user must never lose work: a failed job is retryable and never silently vanishes.

**Constraints inherited from this repo (these decide the answer)**

| Constraint                                                                                                                                                                                                                          | Evidence                                                                                                                                                                                                                                                                           |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "pure Rust, single executable" is an explicit design goal                                                                                                                                                                           | [Cargo.toml](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/Cargo.toml#L1-L7) — "Sentence-Segmented Video Player (pure Rust, single executable)"                                                                                               |
| SQLite is _bundled into the binary_ — no system libsqlite3, no external service                                                                                                                                                     | [Cargo.toml](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/Cargo.toml#L58-L60)                                                                                                                                                                |
| The media HTTP server is **hand-rolled on `std::net`** with a **blocking, synchronous** `Handler` trait and a fully-buffered `Response`                                                                                             | [server.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/server.rs#L1-L20) — "serves static assets, JSON, and media files with `Range` support. No tokio/hyper weight → tiny, single static binary"                                      |
| That server sets `Content-Length` + `Connection: close` — one request per socket, **no streaming**                                                                                                                                  | [server.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/server.rs#L159-L171), [start()](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/server.rs#L176-L197)                                     |
| **axum already runs in this process** for the `agent` feature (bound `127.0.0.1:4000`), with `ag_ui::axum::RouterExt` owning its SSE framing                                                                                        | [agent/runtime/mod.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/agent/runtime/mod.rs#L325-L378), [agent/config.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/agent/config.rs#L117-L117) |
| There are already **two listeners**, and the frontend proxies to both (`/api` → 8018, `/copilotkit` → 4000)                                                                                                                         | [vite.config.ts](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/frontend/web/vite.config.ts#L13-L27)                                                                                                                                                |
| `tokio` / `axum` sit behind the `agent` feature — **and by decision (§4.3) they are promoted to default deps**, because axum becomes the production web crate; the media-only `--no-default-features` build is consciously given up | [Cargo.toml](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/Cargo.toml#L24-L40)                                                                                                                                                                |
| Background work today is `std::thread::spawn` + in-memory job maps                                                                                                                                                                  | [app.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L178-L204)                                                                                                                                                                  |
| You want the queue/notifier **injectable**, like the DB already is                                                                                                                                                                  | user requirement; pattern at [main.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/main.rs#L1148-L1185)                                                                                                                                 |

**Consequence:** the _only_ dependency that is genuinely fatal in **every** profile is a C
dependency — `rdkafka` → librdkafka breaks the single-static-binary goal outright. A pure-Rust
async client (`lapin`, `rumqttc`, `axum`) is now an accepted cost, because axum is the chosen
production web crate and tokio travels with it. Broker adapters stay feature-gated anyway, so a
minimal build remains possible; but do not pretend `--no-default-features` is dependency-free
once the media server itself is axum.

---

## 2. Baseline: what the code does today, and why it hurts

Your job tracking is five in-memory maps:

```rust
playable_jobs: Arc<Mutex<HashMap<String, String>>>,   // H.264 transcode,  "running" | "failed: …"
mux_jobs:      Arc<Mutex<HashMap<String, String>>>,
dub_jobs:      Arc<Mutex<HashMap<String, DubJob>>>,   // TTS per sentence
platform_jobs: Arc<Mutex<HashMap<String, String>>>,
build_jobs:    Arc<Mutex<HashMap<String, BuildProgress>>>, // transcribed/alignment in flight
```

— [app.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L184-L201).

The `BuildProgress` doc comment states the exact defect you are trying to solve, better than I
could:

> A build is **synchronous**: `POST /api/session` only returns when the session exists, and the
> session row only reaches the database at the very end. For a video with no subtitle that means
> whisper plus wav2vec2 alignment run before anything is visible anywhere — **minutes of CPU with
> no session id to poll**, so the video looks like it was never submitted.

— [app.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L133-L141)

Four problems follow, in order of severity:

| #   | Problem                                                                                                                                                                                                                                         | Why it is a _queue_ problem                                                                |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| 1   | **No job identity while work is in flight.** `build_jobs` is keyed by media path _because_ there is no id yet.                                                                                                                                  | You cannot address a job, so you cannot cancel it, retry it, or link a notification to it. |
| 2   | **Nothing is durable.** A `Ctrl-C` or crash drops every job map; a `running` build simply disappears. The code admits this: `dub_status` falls back to "what the run left on disk" when `dub_jobs` has no entry because "the server restarted". | A queue whose contents die with the process is not a queue.                                |
| 3   | **No real queue.** `ensure_playable_async` starts work the instant it is asked for; there is no ordering, no admission control, no "wait your turn". N simultaneous requests fight for CPU.                                                     | This is why multi-select today would start N transcodes at once.                           |
| 4   | **Polling only.** The Manage page re-fetches every 3 s.                                                                                                                                                                                         | 3 s latency, wasted requests, and it cannot tell "done" from "server died".                |

A `POST /api/builds/cancel` endpoint and an `AtomicBool` cancel flag already exist
(`BuildProgress::cancel`, read through the pipeline sink at every stage boundary —
[app.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L146-L158)). That mechanism is good and survives the redesign; the design below simply gives
it a durable identity to hang off.

**Note the good news:** your build is _already_ idempotent and content-addressed. The cache key
is media path + size + mtime (`media_cache_key`, [app.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L226-L248)), and `insert_session` is documented
"idempotent for an existing id" ([db.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/db.rs#L60-L61)). So re-enqueueing the same file is naturally safe — you
get idempotency for free, which is the hardest part of most queue designs.

---

## 3. Queue backend: the candidates

The single most common industry mistake here is treating "queue" and "notification" as one
decision. They are not. A queue needs **durability, ordering, at-least-once delivery, retry,
visibility timeout, and a dead-letter path**. A notification needs **a transport the user's
eyeball can receive**. Only one product on your list (RabbitMQ, partially) serves the first;
only one (MQTT) serves the second brilliantly. Kafka is a _third_ thing — an event log.

### 3.1 Comparison

|                              | Kafka                                                    | RabbitMQ                               | MQTT                                                  | Redis Streams                      | NATS JetStream         | SQS/SNS                        | **SQLite jobs table**                       |
| ---------------------------- | -------------------------------------------------------- | -------------------------------------- | ----------------------------------------------------- | ---------------------------------- | ---------------------- | ------------------------------ | ------------------------------------------- |
| What it actually is          | Distributed append-only log                              | AMQP broker                            | Pub/sub for constrained devices                       | Log + consumer groups _in_ Redis   | Lightweight stream     | Managed AWS queue              | A table + your own workers                  |
| Model                        | **Pull** (consumer polls)                                | **Push** (`basic.consume`) or pull     | Push (broker → subscriber)                            | Pull (`XREADGROUP`)                | Push/pull              | Pull                           | Pull                                        |
| Per-message ack / redelivery | ✗ (offset commit only)                                   | ✓ `basic.ack`/`nack`                   | ✓-ish (QoS 1/2, at broker level)                      | ✓ `XACK`                           | ✓                      | ✓ (visibility timeout)         | ✓ (you write it)                            |
| Visibility timeout           | ✗                                                        | ✓ (`prefetch` + redelivery)            | ✗                                                     | ✓ via PEL + `XAUTOCLAIM`           | ✓ (`ack_wait`)         | ✓                              | ✓ (a `lease_expires_at` column)             |
| Dead-letter queue            | ✗ (app-level retry topics)                               | ✓ built-in DLX                         | ✗                                                     | ✗ (you build it)                   | ✓ (max deliveries)     | ✓ built-in                     | ✓ (you write it)                            |
| Ordering                     | Per-partition only                                       | Per-queue (with caveats)               | Per-topic, QoS 0/1 unordered                          | Per-stream                         | Per-subject            | FIFO queue only                | `ORDER BY enqueued_at`                      |
| Browser can consume directly | ✗                                                        | ✗                                      | **✓ only via MQTT-over-WebSocket**                    | ✗                                  | ✓ (WS)                 | ✗                              | ✗                                           |
| Ops burden                   | **Very high** (brokers, partitions, KRaft/ZK, rebalance) | Medium (Erlang runtime; quorum queues) | Low single-node (Mosquitto) / medium clustered (EMQX) | Low (you already tolerate a store) | Low (single Go binary) | None (managed, but cloud-only) | **None** — a file you already ship          |
| Rust client                  | `rdkafka` (**C** librdkafka), `rskafka` (pure)           | `lapin` (pure) / `amiquip` (sync)      | `rumqttc` (pure)                                      | `redis`                            | `async-nats`           | `aws-sdk-sqs`                  | **none needed** — `rusqlite` already linked |
| Fits single-binary goal      | ✗                                                        | ~ (pure-Rust clients)                  | ✓                                                     | ✓                                  | ✓                      | ✗                              | ✓✓                                          |
| Fits k8s/multi-node          | ✓                                                        | ✓✓                                     | ✓                                                     | ✓                                  | ✓                      | ✓                              | ✗ (single writer)                           |
| Verdict for _you, now_       | **No**                                                   | **Later**                              | **For notify**                                        | Plausible later                    | Plausible later        | No (cloud-only)                | **Yes, now**                                |

### 3.2 Why each verdict

**Kafka — reject outright.** It is designed for _event streaming_: retention, replay,
fan-out to many independent consumer groups, millions of messages/sec. Three of its properties
are actively hostile to a work queue:

- **No acknowledgement.** Consumers commit an _offset_. There is no "I failed this one message,
  redeliver it after 30 s". Retry means you build retry topics and schedulers yourself.
- **No DLQ.** Poison messages need Kafka Connect's DLQ or your own parking topic.
- **Consumers must poll.** It cannot push to a browser, and it cannot push to your Rust process
  without you long-polling.

And the cost side: a Kafka cluster is a multi-process distributed system to own and monitor.
You would be adding brokers, partitions, and consumer-group rebalancing to serve a person who
is annoyed their transcription is slow. Your throughput is **jobs per hour, not messages per
second**. Every property you are paying for is one you will never use. `rdkafka` also
requires librdkafka (a C library), which directly contradicts the single-static-binary goal.

> The one legitimate future use of Kafka in this repo is _not_ this feature: it would be an
> audit/analytics stream of completed sessions ("who watched what") feeding dashboards. Even
> then, a table in SQLite is fine until that data genuinely outgrows one machine.

**RabbitMQ — the correct queue, adopt when you actually go multi-node.** You get exactly the
semantics a work queue needs, for free:

- `basic.consume` pushes work to idle consumers (no polling, no latency).
- `basic.ack` / `basic.nack` + `requeue` + `prefetch` = real visibility-timeout behaviour.
- **Dead-letter exchanges (DLX)** with a message TTL = built-in retry with backoff, and a real
  DLQ for poison messages.
- Priority queues and per-queue TTL, if you want to jump a user's job to the front.

This is the point where RabbitMQ stops being ceremony and starts earning its keep: **the moment
workers live on different machines than the HTTP server**, an in-process queue cannot exist and
something must own the messages. That is your k8s transition. Until then, it is an Erlang
runtime to install, monitor and upgrade for a workload of ~20 files.

Crate note: now that tokio is a default dependency, the choice is `lapin` (async, pure Rust, the
best-maintained option) — `amiquip`'s "avoids tokio" advantage is gone, so prefer `lapin`.
Either way the adapter is feature-gated.

**MQTT — the right notification transport, and it is not a queue.** Be precise about what MQTT
does and does not give you:

- ✓ Pub/sub with topics and wildcards; QoS 0 (at most once), 1 (at least once), 2 (exactly
  once); **retained messages** (a late subscriber immediately gets the last status);
  **last-will-and-testament** (the broker can notify on unexpected disconnect).
- ✓ MQTT 5 adds **shared subscriptions** (`$share/{group}/{topic}`) so several workers can
  compete for the same messages, plus **response topics + correlation data** for request/reply.
- ✗ **No visibility timeout.** If a worker takes a job and dies, nothing redelivers it. This is
  the single biggest gap and the reason MQTT alone cannot be your job queue.
- ✗ **No DLQ.** Poison-message handling is entirely yours to build.
- ✓ **The browser fact that matters to you:** browsers have no raw TCP socket API, so
  **MQTT.js and every browser MQTT client connect over WebSocket** (`ws://host:8083/mqtt`, WS
  subprotocol `mqtt`). Raw MQTT on 1883 is for native clients only.

The elegant consequence: **if the broker owns the fan-out, your web server never has to be the
WebSocket endpoint for remote clients.** The Rust app only needs an MQTT _client_ to `PUBLISH` a
small JSON event; the broker pushes it to every subscribed browser tab. Broker picks:
**Mosquitto** (one tiny binary, one node — ideal for the single-machine phase),
**EMQX / HiveMQ / VerneMQ** when you need clustering on k8s.

**Redis Streams — the pragmatic also-ran.** `XADD` / `XREADGROUP` / `XACK` give you consumer
groups, and the **Pending Entries List** plus `XAUTOCLAIM` gives you the recovery semantics MQTT
lacks. It sits between "SQLite table" and "RabbitMQ" in capability and ops cost. Only worth it
if a Redis instance already exists in your stack — which, today, it does not.

**NATS JetStream — the lightest real broker.** A single Go binary, `async-nats`, work-queue
streams, `ack_wait` visibility timeouts, max-deliveries → DLQ. Genuinely attractive for k8s, and
much lighter than RabbitMQ. The only reason it is not my primary later-adapter recommendation is
that RabbitMQ's DLX/TTL model is more battle-tested for _job retry with backoff_ specifically.

**SQS/SNS — excluded by the constraint.** Correct semantics (visibility timeout, DLQ, FIFO), zero
ops, and SNS does push (email/HTTP/SMS). But it is cloud-only and needs network + credentials,
which cannot work for a local single-machine app, and it would sit badly with the
"no installs, no downloads" promise in [AGENTS.md](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/AGENTS.md).

**SQLite `jobs` table — the right answer for now.** It wins on a criterion none of the others
can match: **the queue is transactional with your session rows, in the same file, in the same
process, with a library you already link.**

```sql
CREATE TABLE IF NOT EXISTS jobs (
    id                TEXT PRIMARY KEY,      -- uuid; the handle the API returns
    media_path        TEXT NOT NULL,
    cache_key         TEXT NOT NULL,         -- media path + size + mtime (idempotency)
    kind              TEXT NOT NULL,         -- 'build' | 'transcode' | 'dub' | 'platform'
    state             TEXT NOT NULL,         -- 'queued'|'running'|'done'|'failed'|'cancelled'
    stage             TEXT,                  -- 'decode' | 'transcribe' | 'align' | 'stitch' …
    done              INTEGER NOT NULL DEFAULT 0,
    total             INTEGER NOT NULL DEFAULT 0,
    attempts          INTEGER NOT NULL DEFAULT 0,
    max_attempts      INTEGER NOT NULL DEFAULT 3,
    lease_expires_at  INTEGER,               -- visibility timeout: NULL when not running
    cancel_requested  INTEGER NOT NULL DEFAULT 0,
    last_error        TEXT,
    enqueued_at       INTEGER NOT NULL,
    started_at        INTEGER,
    finished_at       INTEGER,
    UNIQUE(media_path, cache_key, kind, state) ON CONFLICT IGNORE
);
CREATE INDEX IF NOT EXISTS idx_jobs_claimable ON jobs(state, enqueued_at);
```

The claim query is the whole queue:

```sql
-- Inside a BEGIN IMMEDIATE transaction: SQLite has no `FOR UPDATE SKIP LOCKED`
-- (that is Postgres-only), so the global write lock plus IMMEDIATE is the
-- correctness mechanism for a single-writer queue.
UPDATE jobs
   SET state = 'running',
       stage = 'decode',
       attempts = attempts + 1,
       started_at = :now,
       lease_expires_at = :now + :lease_secs
 WHERE id = (SELECT id FROM jobs
              WHERE state = 'queued'
              ORDER BY enqueued_at
              LIMIT 1)
RETURNING *;
```

Two facts to state plainly, because they are the usual objections:

- **SQLite has no `SKIP LOCKED`.** That is fine here and only here: the process is a _single
  writer_, so the write lock serialises claims correctly. This is precisely why this design
  **does not survive multi-node** — and why the port exists, so you swap it out before then.
- **The lease column is your visibility timeout.** A worker crash leaves a `running` row whose
  `lease_expires_at` has passed; a sweeper (on startup and on a timer) reclaims it, which is the
  recovery guarantee an in-memory map can never give you. Enable `PRAGMA journal_mode=WAL` and
  keep transactions short so builders aren't blocked by claim writes.

### 3.3 The decision, stated once

- **Single machine (today):** `jobs` table in the SQLite you already ship + a worker pool. Zero
  new runtime dependencies, zero new processes, durable across restarts.
- **Multi-node (k8s, later):** implement the _same_ `JobQueue` trait over **RabbitMQ** (or NATS
  JetStream / Redis Streams, in that order of preference for job-retry semantics).
- **Never:** Kafka for this feature.

---

## 4. Notification transport, and the web layer

Your requirement is push. As of the decision below (§4.3), the web layer is **axum**, so the
question is no longer "can `server.rs` stream?" but "which transport do we push on, given axum
is available?"

|                                  | Polling (today)  | SSE                                   | WebSocket                         | **MQTT over WebSocket**                               | OS toast / email / webhook |
| -------------------------------- | ---------------- | ------------------------------------- | --------------------------------- | ----------------------------------------------------- | -------------------------- |
| Direction                        | client pulls     | server → client                       | bidirectional                     | server → client (via broker)                          | server → out-of-band       |
| Latency                          | ≤ 3 s            | ~ms                                   | ~ms                               | ~ms                                                   | instant / seconds          |
| Protocol                         | plain HTTP       | plain HTTP (`text/event-stream`)      | HTTP Upgrade + RFC 6455           | WS, subprotocol `mqtt`                                | platform-specific          |
| On axum, costs                   | nothing          | `axum::response::sse::Sse` + a stream | `axum::extract::WebSocketUpgrade` | **nothing in the web server** — only an MQTT _client_ | nothing                    |
| Needs a broker / extra process   | ✗                | ✗                                     | ✗                                 | ✓ (Mosquitto locally, EMQX on k8s)                    | ✗                          |
| Works when browser tab is closed | ✗                | ✗                                     | ✗                                 | ✗                                                     | **✓**                      |
| Rust crate                       | —                | axum (already in-tree)                | axum (already in-tree)            | `rumqttc`                                             | Windows toast, or SMTP     |
| Verdict                          | keep as fallback | **recommended local adapter**         | optional; free once axum is in    | **recommended k8s adapter; this is your ask**         | add later for "tab closed" |

### 4.1 What hand-rolling would have cost (why axum, not `server.rs`)

Kept here because it is the justification for §4.3, not because it is still on the table. The
hand-rolled server's own header claims `Range` support and "no tokio/hyper weight". Adding a
second, streaming transport to it means:

- **SSE** = a new streaming response path: omit `Content-Length`, write
  `Content-Type: text/event-stream` + `Cache-Control: no-cache`, then keep the `TcpStream` and
  loop `data: {...}\n\n` until the client drops. `write_response` hard-codes `Content-Length` and
  `Connection: close` ([server.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/server.rs#L159-L171)), and `start()` handles one request per socket
  ([server.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/server.rs#L176-L197)), so this is a structural change, not a new route. ~40 lines,
  thread-per-connection.
- **WebSocket** = the same, plus a version-13 handshake (Sec-WebSocket-Accept = base64(SHA-1(key +
  GUID))) and an RFC 6455 frame parser/writer for masked frames and ping/pong. Real work, and
  SHA-1 + base64 are new dependencies.
- Both are, done properly, **axum** — which is why we stop pretending otherwise and adopt it.
- The one place the hand-rolled server is genuinely good is binary size. Its
  `--no-default-features` build is the ~25 MB media-only binary — and the decision in §4.3
  consciously gives that up.

### 4.2 The push contract (transport-agnostic)

Whatever the transport, publish the same event. Keep it small and versioned, and include the
terminal state plus the ids needed to dedupe:

```json
{
  "v": 1,
  "job_id": "3f9a…",
  "kind": "build",
  "state": "done",
  "media_path": "…/lecture.mp4",
  "session_id": "s42",
  "stage": null,
  "done": 128,
  "total": 128,
  "error": null,
  "finished_at": 1760000000
}
```

Topics/keys, mirroring how your `logs` module already channellises by subsystem
(`logs::info("session", …)` — [main.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/main.rs#L1132-L1143)):

- MQTT: `sentence-video/jobs/{job_id}/status`, with a wildcard subscription
  `sentence-video/jobs/+/status` for the task panel; publish **retained** so a tab opened later
  still sees the last status.
- SSE: one stream endpoint, e.g. `GET /api/jobs/events`, emitting the same JSON as `data:` lines.

**Delivery is at-least-once** (MQTT QoS 1; SSE with `Last-Event-ID` replay). Therefore the
_subscriber_ must be idempotent — dedupe on `(job_id, state)`. This is the standard
at-least-once → idempotent-consumer pairing and the reason for the `job_id` in the payload.

**rumqttc gotcha, worth knowing before you write the adapter:** `AsyncClient` only publishes when
something is continuously driving the `EventLoop` (`loop { eventloop.poll().await }`). If nobody
owns that task, `publish()` silently does nothing. So the `MqttPublisher` adapter must own a
long-lived tokio task that polls the event loop — do not spawn the client and drop the loop.

### 4.3 Settled: axum is the production web crate

**Decision (yours, §0): use a single web crate in production — axum.** The 25 MB
`--no-default-features` media-only build is consciously given up. This closes the question the
earlier draft left open, and it is a defensible call: `axum` is already in this repo, its SSE
framing is already owned by `ag_ui::axum::RouterExt`
([agent/runtime/mod.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/agent/runtime/mod.rs#L43-L43)), and it replaces hand-written RFC 6455 framing with
`WebSocketUpgrade`.

**But async buys nothing for the _work_ — only for the _edge_.** This is the load-bearing
technical point, and the reason the rest of this document does not change. Every expensive step
is _blocking, CPU-bound work with no await points at all_:

- FFmpeg decode (`media::decode_to_f32`) is a blocking C call;
- whisper.cpp inference is blocking C that **spawns and manages its own threads** inside
  `whisper-rs-sys`;
- the wav2vec2 aligner is blocking, CPU-bound and single-threaded by construction —
  [main.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/main.rs#L790-L798)
  states it: "self-attention is quadratic and this implementation is single-threaded".

A CPU-bound task on a tokio worker thread **blocks the executor**. So under axum every one of
these moves into `spawn_blocking`, and `spawn_blocking` _is_ a thread pool: you keep the same OS
threads and add a scheduler. Since concurrency should be **N≈1** anyway (the
[Cargo.toml](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/Cargo.toml#L103-L113)
note records ~3 min per 60 s clip in release and >18 min in debug), you are not overflowing any
pool.

**Two consequences that keep churn near zero:**

1. **Keep `JobQueue` and `Handler` _synchronous_.** They are the things handed out as
   `Arc<dyn JobQueue>` / `Arc<dyn Handler>`, and **`async fn` in a trait is not `dyn`-compatible**.
   Making them async would break the very injection pattern this design is built on. Instead,
   wrap the blocking calls in `spawn_blocking` at the axum call site (§5.6). The SQLite adapter,
   the `Store` calls and the log writes stay exactly as they are.
2. **Everything already built against `Handler` is reused verbatim**, not rewritten (§5.6).

### 4.4 Server consolidation: one listener, or two?

There are already two listeners in the process — media on 8018, and the `agent` feature's axum on
`127.0.0.1:4000` ([agent/config.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/agent/config.rs#L117-L117)) — and the frontend proxies to both
([vite.config.ts](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/frontend/web/vite.config.ts#L13-L27)):

| Option                                                        | What it means                                                                                                             | Verdict                                                                                                                                                                                                                                                                         |
| ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Put the job API + SSE on the **agent's** axum (4000)          | Reuses the existing axum listener; no new `server.rs` code                                                                | **No.** The job queue and the `jobs` table live with the session store in the media app. Splitting them across two servers means the queue's owner and its API are separate processes, and the frontend would need a new proxy rule (job routes don't live under `/copilotkit`) |
| Put the job API + SSE on the **media** axum app (8018)        | The media app becomes the axum app; **port unchanged**, so the existing `/api` proxy and the entire frontend keep working | **Yes.** One web crate, one listener for app traffic, no new proxy rules                                                                                                                                                                                                        |
| Leave media on `server.rs` and only add axum routes elsewhere | Two web stacks in one process                                                                                             | **No** — that is exactly the "single web crate" decision being made                                                                                                                                                                                                             |

So: **the media app becomes the axum app on 8018**; `/copilotkit` → 4000 remains as-is. The
frontend does not change its routing at all.

**The media-serving caveat — and the acceptance test.** `Range` handling is hand-rolled in
`serve_file` ([app.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L2198-L2253)): it emits `206`, `Content-Range`, `Accept-Ranges: bytes`, and a
`416` with `Content-Range: bytes */{len}`. This is **not** a `tower_http::ServeDir` swap — the
bytes are addressed per registered session file, not by a directory — so the port must preserve
those exact headers. Name the acceptance test explicitly: **seeking in the player works**, in
both directions, on a long video. Do not let this be implicit.

### 4.5 Transport recommendation

- **Local adapter: axum SSE.** No broker to run, no new process, ~ms latency, and it replaces the
  3 s poll. `axum::response::sse::Sse` plus a `tokio::sync::broadcast` hub.
- **k8s adapter: MQTT.** A broker (EMQX on k8s, Mosquitto locally if you want to try it) fans out
  to browsers over WebSocket; the Rust side only publishes. Same event contract as SSE.
- **Optional third: WebSocket.** Now free — `WebSocketUpgrade` — and useful only if you need the
  client to talk back (e.g. a "retry this job" button over the same socket).
- **Later: OS toast / email** for jobs long enough that the tab will be closed.

---

## 5. Recommended design

### 5.1 Two ports, mirroring `Store`

`Store` is the model to copy: a small trait ([db.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/db.rs#L56-L70)),
a concrete `Db` behind it, the engine chosen once at startup and handed out as `Arc<dyn Store>`
([main.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/main.rs#L1148-L1185)), and — critically — **request code that never learns which engine is
in use**.

```rust
/// Durable, ordered, at-least-once work queue. Implemented by the embedded
/// SQLite adapter locally, and by the broker adapter on k8s.
///
/// Deliberately SYNCHRONOUS: it is handed out as `Arc<dyn JobQueue>`, and
/// `async fn` in a trait is not dyn-compatible. Callers wrap it in
/// `spawn_blocking` at the axum boundary (see §5.6).
pub trait JobQueue: Send + Sync {
    fn enqueue(&self, spec: JobSpec) -> Result<JobId, String>;
    /// Lease the next job (visibility timeout = `lease`). None = queue empty.
    fn claim(&self, lease: Duration) -> Result<Option<Job>, String>;
    fn heartbeat(&self, id: &JobId) -> Result<(), String>;   // extend the lease
    fn progress(&self, id: &JobId, p: Progress) -> Result<(), String>;
    fn complete(&self, id: &JobId) -> Result<(), String>;
    /// Release for retry (backoff) or park in the DLQ once attempts are spent.
    fn fail(&self, id: &JobId, err: &str) -> Result<Retry, String>;
    fn request_cancel(&self, id: &JobId) -> Result<(), String>;
    fn cancel_requested(&self, id: &JobId) -> Result<bool, String>;
    fn list(&self, filter: JobFilter) -> Vec<Job>;
    /// Requeue rows whose lease expired (crash recovery).
    fn reclaim_expired(&self) -> Result<usize, String>;
}

/// Push a small status event to whoever is watching. Implemented by an
/// in-process SSE hub locally, and by an MQTT publisher on k8s.
pub trait Notifier: Send + Sync {
    fn publish(&self, ev: &JobEvent) -> Result<(), String>;
}
```

Startup wiring becomes a near-copy of the DB block:

```rust
let queue: Arc<dyn JobQueue> = match queue_cfg.backend {
    // Local: same bundled rusqlite, same file, WAL on. No new crate.
    Backend::Embedded => Arc::new(jobs::SqliteQueue::open(&db_path)?),
    // k8s: feature-gated so a minimal build is still possible.
    #[cfg(feature = "broker")]
    Backend::Rabbit => Arc::new(jobs::RabbitQueue::connect(&queue_cfg.url)?),
    #[cfg(feature = "broker")]
    Backend::Redis => Arc::new(jobs::RedisStreamQueue::connect(&queue_cfg.url)?),
};
let notifier: Arc<dyn Notifier> = match notify_cfg.backend {
    NotifyBackend::Sse => Arc::new(notify::SseHub::new()),                  // local
    #[cfg(feature = "mqtt")]
    NotifyBackend::Mqtt => Arc::new(notify::MqttPublisher::connect(&notify_cfg.broker)?),
};
```

`AppState` then holds `queue: Arc<dyn JobQueue>` next to `db: Arc<dyn Store>`
([app.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L178-L204)). The five existing `*_jobs` maps are deleted: `build_jobs`,
`playable_jobs`, `mux_jobs`, `dub_jobs`, `platform_jobs` all become rows with `kind` set
accordingly. `BuildProgress` ([app.rs:141](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L141-L176)) maps 1:1 onto the `stage`/`done`/`total`/`elapsed`
columns — its `to_json` shape can stay byte-identical, so
[manage/index.tsx](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/frontend/web/src/features/manage/index.tsx) keeps working during migration.

### 5.2 Worker pool

- `N` workers on the **`spawn_blocking` pool**, pulling via `claim()`. **Default N = 1**, and make
  it configurable. Justification from your own numbers: `Cargo.toml` records that a 60 s clip took
  ~3 min in release for the alignment pass, and **18 min and still running in debug**
  ([Cargo.toml](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/Cargo.toml#L103-L113)). Parallelism multiplies memory (whisper model + weights) and makes the
  machine unusable. A queue's job is to _bound_ concurrency, not inflate it.
- Use a `tokio::sync::Semaphore` for the concurrency cap rather than spawning unbounded tasks —
  a `Semaphore` and a fixed thread pool are equivalent tools here, and the `sync` feature is
  already enabled ([Cargo.toml](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/Cargo.toml#L75-L81)).
- Stage boundaries are already the cancellation points (the sink returns `bool` —
  [app.rs:271-280](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L271-L280)). Read the flag from the row instead of an `AtomicBool`, and treat a
  cancelled build as a terminal state that **keeps** its partial work dir (checkpoints already
  exist under `work/` and `dubbed/*.tts`) so resume is cheap.
- Backoff: `attempts` + exponential delay in `enqueued_at` (embedded) or a DLX+TTL pair
  (RabbitMQ). After `max_attempts`, park in the DLQ and publish a `failed` event.

### 5.3 API surface

| Endpoint                                      | Change                                                                                                              |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `POST /api/jobs`                              | **New.** Body = list of media paths. Returns `202` + `[{job_id, media_path}]` **immediately**. This is the feature. |
| `GET /api/jobs`                               | **New.** Replaces the polling of `GET /api/builds` with a full job list (any state).                                |
| `GET /api/jobs/events`                        | **New (SSE adapter).** An axum `Sse` route streaming `JobEvent`s.                                                   |
| `POST /api/jobs/{id}/cancel`                  | **New.** Generalises the existing `POST /api/builds/cancel`.                                                        |
| `POST /api/session`                           | **Keep, unchanged**, for the single-file sync path. Add `?async=1` later if you want one code path.                 |
| `GET /api/builds` / `POST /api/builds/cancel` | **Keep** as thin aliases during migration, then retire.                                                             |

The frontend keeps its 3 s poll as a _fallback_ and adds an SSE/MQTT subscription that
invalidates the job list on each event — the standard "push for latency, poll for truth" pairing.

### 5.4 Crate choices

| Adapter                    | Crate                                                 | Runtime               | Note                                                                                      |
| -------------------------- | ----------------------------------------------------- | --------------------- | ----------------------------------------------------------------------------------------- |
| Web layer (**production**) | `axum` + `tokio` + `tower-http`                       | tokio                 | Promoted out of `agent` to default deps. Replaces `server.rs`'s accept loop               |
| Embedded queue             | _(none — `rusqlite` already linked)_                  | via `spawn_blocking`  | `PRAGMA journal_mode=WAL`; `BEGIN IMMEDIATE` claims; keep transactions short              |
| SSE notifier               | `axum::response::sse::Sse` + `tokio::sync::broadcast` | tokio                 | The hub is a `broadcast::Sender<JobEvent>`; no hand-rolled framing                        |
| WebSocket notifier         | `axum::extract::WebSocketUpgrade`                     | tokio                 | Free once axum is in; no RFC 6455 code to write                                           |
| MQTT notifier              | `rumqttc`                                             | tokio (`AsyncClient`) | Pure Rust. **Must own a task that polls `EventLoop` continuously** — see §4.2             |
| RabbitMQ queue             | `lapin`                                               | tokio                 | `amiquip`'s "avoids tokio" advantage is gone now; `lapin` is the better-maintained choice |
| Redis Streams              | `redis`                                               | sync or async         |                                                                                           |
| NATS JetStream             | `async-nats`                                          | tokio                 |                                                                                           |
| Kafka — **if you ever**    | `rskafka` (pure) over `rdkafka`                       |                       | Avoid `rdkafka`: librdkafka is C and breaks the single-static-binary goal **everywhere**  |
| OS toast                   | `winrt-notification` / `tauri` plugin                 |                       | Only needed for "tab closed"                                                              |

Gate `broker` and `mqtt` as new optional features next to `agent`
([Cargo.toml:24-40](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/Cargo.toml#L24-L40)). Do **not** gate `axum`/`tokio` any more — they are the web layer now.

### 5.5 The sync→async bridge (and the Range caveat)

The `Handler` surface is large: `impl Handler for AppState` ([app.rs](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L1116-L1120)) dispatches ~30 routes
(sessions, logs, builds, `pick-file`, plus per-session `dub` / `align` / `retranscode` / `mux` /
`media` / `audio` / `progress` / `platform-subtitle`). Every one of those methods is
**synchronous and blocking** (rusqlite, filesystem). Do not convert them route by route.

Instead, port **one** adapter:

```rust
// One catch-all axum handler reuses the entire existing sync route table.
// The body is blocking (rusqlite + fs), so it must run on spawn_blocking —
// never on a tokio worker thread.
async fn dispatch(State(app): State<Arc<AppState>>, req: Request) -> Response {
    let (req, app) = (to_server_request(req), app.clone());
    tokio::task::spawn_blocking(move || app.handle(&req))   // Handler stays sync
        .await
        .unwrap_or_else(|_| server::Response::error(500, "handler panicked"))
}
```

This keeps `server::Request`/`server::Response` as the internal contract (they are plain structs,
[server.rs:15-48](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/server.rs#L15-L48)), so the ~30 route methods compile unchanged, and only the **new**
streaming routes (`GET /api/jobs/events`, optionally a WS route) are written natively in axum.
The streamed media response in `serve_file` is returned through the same path, so
`206` / `Content-Range` / `Accept-Ranges` / `416` semantics are preserved — and the acceptance
test in §4.4 covers the regression risk.

---

## 6. Migration path (each step ships independently)

1. **Make the media app an axum app — no feature change.** Promote `axum`/`tokio`/`tower-http` to
   default deps, mount the catch-all `Handler` adapter (§5.5) on an axum `Router` bound to the
   **same port 8018**, delete `server.rs`'s accept loop. Verify: player loads, **seeking works**
   (§4.4), all existing `/api` routes behave identically. No behaviour change is the goal.
2. **Durable jobs + worker pool (embedded).** Create the `jobs` table, add `JobQueue` +
   `SqliteQueue`, move all five `*_jobs` maps onto it, enqueue via `Arc<dyn JobQueue>` from
   `spawn_blocking`. Fixes problems 1, 2 and 3 from §2. `POST /api/jobs` returns 202.
3. **SSE notifier.** `Notifier` + `SseHub` over `broadcast`, `GET /api/jobs/events` as a native
   axum `Sse` route, and the frontend subscription. Fixes problem 4; latency drops from ≤3 s to
   ~ms.
4. **Optional: OS toast / email** for long jobs, so the browser tab need not stay open.
5. **k8s cutover.** Add `broker` feature → `RabbitQueue`/`lapin`; add `mqtt` feature →
   `MqttPublisher` + an EMQX container. **No application logic changes** — only the two `match`
   blocks at startup. Point `--queue-backend rabbit --notify-backend mqtt`.
6. **Only if a genuine analytics need appears:** a Kafka audit stream of _completed sessions_
   (not jobs). Note "if", not "when".

**What must be true before step 5 is worth doing:** workers on separate machines, or a shared
queue with multiple processes, or a need for the queue to outlive a single node. Until then steps
1–3 give you the entire user-visible feature with no new external service.

---

## 7. Anti-patterns to avoid

1. **Don't reach for Kafka "so we're ready to scale."** You would pay the full distributed-systems
   tax today for a property (throughput, replay, fan-out) this feature never exercises. It is also
   the wrong _shape_: a log cannot do per-message ack with a backoff retry.
2. **Don't use MQTT as the queue.** It has no visibility timeout: a worker that dies mid-job
   leaves that job invisible forever, and nothing will redeliver it. Use it only for the
   notification, paired with a real queue.
3. **Don't keep job state in `HashMap`s.** Already acknowledged in-code ("either never asked, or
   the server restarted" — [app.rs:328-331](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L328-L331)). Restart-durability is the point of a queue.
4. **Don't make `JobQueue` or `Handler` `async fn` in the trait.** `async fn` in a trait is not
   `dyn`-compatible, and both are handed out as `Arc<dyn …>`. Keep them synchronous and bridge
   with `spawn_blocking` at the axum boundary (§5.5). If you ever truly need an async trait, opt
   into `#[async_trait]` deliberately — not by accident.
5. **Don't run blocking work on a tokio worker thread.** rusqlite, the `Store` calls, the log
   writes, FFmpeg and whisper are all blocking. Every one of them goes through `spawn_blocking`;
   blocking the executor is the one mistake that makes an async web layer worse than threads.
6. **Don't forget to drive the MQTT event loop.** `rumqttc::AsyncClient::publish` is a no-op
   unless a task continuously polls `EventLoop` (§4.2).
7. **Don't touch the `Range` headers when porting media serving.** The seek/buffer behaviour of
   the whole player depends on `206` / `Content-Range` / `Accept-Ranges` staying exact
   ([app.rs:2198-2253](file:///c:/Users/sys49780/Desktop/TestProject/EnglishLearning/EnglishLearning/rust/src/app.rs#L2198-L2253)). "Seeking works" is the acceptance test for step 1 of §6.
8. **Don't let a _C_ dependency in.** `rdkafka` → librdkafka breaks the single-static-binary goal
   in **every** profile — that is the real prohibition now, not "tokio". Pure-Rust clients only.
9. **Don't treat notifications as exactly-once.** Both MQTT QoS 1 and SSE replay are
   at-least-once → dedupe on `(job_id, state)` in the UI handler, or users see double toasts.
10. **Don't default concurrency to "number of cores."** Alignment is the hot path and is already
    minutes-per-clip; the queue exists to serialise, and N=1 is defensible.
11. **Don't re-architect `POST /api/session`.** Leave the synchronous path intact and add the
    async job path beside it; the session cache key already makes the two idempotent with each
    other.
12. **Don't publish large payloads over MQTT.** Publish ids + a state; the UI fetches details from
    `GET /api/jobs`. (The dubber already caps clips at 60 s, so you know payload discipline
    matters here.)

---

## 8. Open questions to settle before implementation

1. **~~Do you want the media-only build to stay real?~~ Resolved — no.** axum is the production
   web crate and `--no-default-features` is consciously given up (§4.3). Consequence: `axum`/
   `tokio`/`tower-http` are no longer feature-gated, and `AGENTS.md`'s "no tokio weight" note in
   `server.rs` becomes historical.
2. **Notify when the browser tab is closed?** If yes, budget for the OS-toast/email step (§6.4) —
   SSE, WebSocket and MQTT all deliver only to a live subscriber.
3. **Should a cancelled build keep or discard its checkpoints?** The checkpoints under
   `work/` and `dubbed/*.tts` make resume nearly free; discarding them is simpler but wastes the
   work. I would keep them and add a `resume` action.
4. **Is `kind` the right partition?** `build`/`transcode`/`dub`/`platform` differ wildly in cost.
   Consider a per-`kind` concurrency cap rather than one global N.
5. **Do you want per-user queues eventually?** The frontend has auth features
   (`providers/auth-context.tsx`, `features/auth/login|signup`) but `AppState` has no user concept
   today. If multi-user is coming, add a `user_id` column now — nearly free in the embedded
   design, painful to retrofit in a broker.
6. **Should `jobs` live in `sessions.db` or its own file?** Same file gives you one
   `BEGIN IMMEDIATE` covering both job transition and session insert (atomic, no dual-write bug);
   a separate file isolates queue contention. Note the queue is now reached through
   `spawn_blocking`, so a separate file also keeps one blocking pool from waiting on the other.
   I lean same-file until you measure contention, WAL on.
7. **Should the media app keep a `--no-default-features` build at all?** If the goal is now "one
   binary, axum everywhere", consider deleting the flag rather than leaving a build that no
   longer matches production. Decide explicitly, so nobody ships the 25 MB build by habit.
