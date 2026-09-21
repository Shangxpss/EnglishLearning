//! Wire protocols shared with the CopilotKit frontend.
//!
//! * [`ag_ui`] — the event stream the browser consumes (replaces `ag_ui_langgraph`).
//! * [`a2ui`] — the declarative UI payloads embedded in those events (replaces
//!   `copilotkit.a2ui`).
//!
//! Both are plain JSON, so the Rust side builds them with `serde` instead of
//! pulling in a framework.

pub mod a2ui;
pub mod ag_ui;

pub use ag_ui::AgUiEvent;
