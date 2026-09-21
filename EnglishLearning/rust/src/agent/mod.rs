//! **Rust port of the `AI-Demo` agent stack.**
//!
//! The TypeScript/Bun `agent-runtime` (`CopilotRuntime` + `A2UIMiddleware`) and
//! the Python `backend` (FastAPI + LangGraph + `CopilotKitMiddleware`) are
//! collapsed into one Rust service that speaks the **AG-UI** protocol directly
//! to a CopilotKit frontend and emits **A2UI** payloads itself. The Bun
//! middleman and the Python process are both gone: one binary serves
//! `/copilotkit/*` and streams AG-UI events over SSE.
//!
//! ```text
//! before:  React (CopilotKit) → Bun (CopilotRuntime) → Python (FastAPI + LangGraph)
//! after:   React (CopilotKit) → Rust (axum + AG-UI + A2UI)
//! ```
//!
//! # Layout
//!
//! The **protocol** half is delegated to the [`ag_ui`] crate (event vocabulary,
//! SSE framing, `Agent`/`RunContext` hosting runtime, request type, axum mount).
//! The modules below are the application half — what the SDK deliberately leaves
//! out.
//!
//! | module | responsibility | replaces |
//! | --- | --- | --- |
//! | [`config`] | agent id, catalog id, model, credentials | env/`.env` in both stacks |
//! | [`protocol::a2ui`] | A2UI payloads carried by `ACTIVITY_SNAPSHOT` | `copilotkit.a2ui` |
//! | [`llm`] | OpenAI-compatible streaming chat client (DeepSeek) | `langchain_deepseek.ChatDeepSeek` |
//! | [`tools`] | tool trait + registry + built-in tools | `@tool` decorated functions |
//! | [`engine`] | ReAct loop, thread state, system prompt | `LangGraph.create_agent` + `MemorySaver` |
//! | [`runtime`] | `impl ag_ui::server::Agent`, side endpoints, A2UI middleware | `index.ts` + `main.py` + `CopilotKitMiddleware` |
//!
//! # Running
//!
//! ```text
//! sentence-video agent [--host 127.0.0.1] [--port 4000] [--model deepseek-chat]
//! ```
//!
//! Credentials come from `DEEPSEEK_API_KEY` (see [`config::AgentConfig`] for the
//! full list of environment variables).

pub mod config;
pub mod engine;
pub mod llm;
pub mod protocol;
pub mod runtime;
pub mod tools;

pub use config::AgentConfig;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Monotonic counter backing [`new_id`].
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a short, process-unique identifier.
///
/// The Python/TS stack used `uuid`; the same job is done here with a timestamp
/// plus a counter so the binary gains no extra dependency.
pub fn new_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{nanos:x}-{seq:x}")
}

/// Start the agent service and block until the process is stopped.
///
/// This is the entry point used by the `agent` subcommand.
pub fn run(cfg: AgentConfig) -> Result<(), String> {
    runtime::serve(cfg)
}
