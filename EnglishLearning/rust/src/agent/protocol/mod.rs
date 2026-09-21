//! Wire protocols shared with the CopilotKit frontend.
//!
//! * **AG-UI** — the event stream the browser consumes. This is now provided by
//!   the [`ag_ui`] crate (`ag_ui::Event`, `ag_ui::server`, `ag_ui::axum`), which
//!   implements the full event vocabulary and SSE framing. Nothing in this
//!   module re-implements it; the crate is used directly from
//!   [`crate::agent::engine`] and [`crate::agent::runtime`].
//! * [`a2ui`] — the declarative UI payloads carried inside AG-UI
//!   `ACTIVITY_SNAPSHOT` events. AG-UI has no opinion about these, so they are
//!   built here with `serde_json` (replacing `copilotkit.a2ui`).

pub mod a2ui;

/// Activity type used for A2UI surfaces inside `ACTIVITY_SNAPSHOT` events.
///
/// AG-UI leaves `activityType` to the application; the frontend A2UI renderer
/// keys off this value.
pub const A2UI_ACTIVITY_TYPE: &str = "a2ui-surface";
