//! **A2UI v0.9 payloads** — the declarative UI the agent renders.
//!
//! Ported from the Python `copilotkit.a2ui` helpers (`create_surface`,
//! `update_components`, `update_data_model`, `render`). A2UI is just JSON, so
//! there is no framework to port: these functions build the same structures with
//! `serde_json`, and the components are plain values so a caller can add any
//! component the frontend catalog defines.
//!
//! Operations are single-key objects (`{"createSurface": {…}}`) and are shipped
//! to the browser inside an AG-UI `ACTIVITY_SNAPSHOT` event — see
//! [`activity_content`], which builds the object that event carries.

use ag_ui::JsonObject;
use serde_json::{json, Value};

/// Operation name constants (A2UI v0.9).
pub mod op {
    pub const CREATE_SURFACE: &str = "createSurface";
    pub const UPDATE_COMPONENTS: &str = "updateComponents";
    pub const UPDATE_DATA_MODEL: &str = "updateDataModel";
    pub const DELETE_SURFACE: &str = "deleteSurface";
}

/// Component names from the `generative-agent-catalog` used by this project.
///
/// Only names that exist in the frontend catalog may be emitted — the Python
/// system prompt says the same thing ("never invent new ones").
pub mod component {
    pub const CARD: &str = "Card";
    pub const COLUMN: &str = "Column";
    pub const ROW: &str = "Row";
    pub const LIST: &str = "List";
    pub const TEXT: &str = "Text";
    pub const TEXT_FIELD: &str = "TextField";
    pub const BUTTON: &str = "Button";
    pub const RESET_BUTTON: &str = "ResetButton";
    pub const DIVIDER: &str = "Divider";
    pub const IMAGE: &str = "Image";
    pub const ICON: &str = "Icon";
    pub const CHECK_BOX: &str = "CheckBox";
    pub const CHOICE_PICKER: &str = "ChoicePicker";
    pub const SLIDER: &str = "Slider";
    pub const TABS: &str = "Tabs";
    pub const MODAL: &str = "Modal";
    pub const VIDEO: &str = "Video";
    pub const AUDIO_PLAYER: &str = "AudioPlayer";
    // Custom components registered by the frontend catalog.
    pub const HEADING: &str = "Heading";
    pub const GRID: &str = "Grid";
    pub const TABLE: &str = "Table";
    pub const KEY_VALUE_LIST: &str = "KeyValueList";
    pub const STATUS_BADGE: &str = "StatusBadge";
    pub const METRIC: &str = "Metric";
    pub const INFO_ROW: &str = "InfoRow";
    pub const BAR_CHART: &str = "BarChart";
    pub const PIE_CHART: &str = "PieChart";
    pub const USER_CARD: &str = "UserCard";
    pub const SPACER: &str = "Spacer";
}

// ─────────────────────────────────────────────────────────────────────────
// Operations
// ─────────────────────────────────────────────────────────────────────────

/// `createSurface` — create a new surface bound to a catalog.
pub fn create_surface(surface_id: &str, catalog_id: &str) -> Value {
    json!({
        op::CREATE_SURFACE: {
            "surfaceId": surface_id,
            "catalogId": catalog_id,
        }
    })
}

/// `updateComponents` — add or replace components on a surface.
pub fn update_components(surface_id: &str, components: Vec<Value>) -> Value {
    json!({
        op::UPDATE_COMPONENTS: {
            "surfaceId": surface_id,
            "components": components,
        }
    })
}

/// `updateDataModel` — set the surface's application state.
pub fn update_data_model(surface_id: &str, data: Value) -> Value {
    json!({
        op::UPDATE_DATA_MODEL: {
            "surfaceId": surface_id,
            "data": data,
        }
    })
}

/// `deleteSurface` — remove a surface from the UI.
pub fn delete_surface(surface_id: &str) -> Value {
    json!({ op::DELETE_SURFACE: { "surfaceId": surface_id } })
}

/// Wrap a list of operations the way Python's `render(operations=[…])` did:
/// a JSON array ready to be embedded in an `ACTIVITY_SNAPSHOT`.
pub fn render(operations: Vec<Value>) -> Value {
    Value::Array(operations)
}

/// Build the `content` object of an AG-UI `ACTIVITY_SNAPSHOT` carrying A2UI.
///
/// AG-UI requires the activity payload to be a JSON **object**
/// (`ag_ui::Event::activity_snapshot` takes a [`JsonObject`]), whereas A2UI
/// operations are an **array**. They therefore ride under `operations`, with the
/// tool that produced them recorded next to them — which is what makes a
/// mis-rendered surface traceable in the browser.
pub fn activity_content(tool_name: &str, operations: Value) -> JsonObject {
    let mut content = JsonObject::new();
    content.insert("tool".to_string(), Value::String(tool_name.to_string()));
    content.insert("operations".to_string(), operations);
    content
}

// ─────────────────────────────────────────────────────────────────────────
// Component builders
// ─────────────────────────────────────────────────────────────────────────

/// Build a component from an id, a catalog component name and extra props.
///
/// `props` must be a JSON object; its keys are merged at the top level, which is
/// how A2UI components are shaped (flat, not nested under a `props` key).
pub fn component(id: &str, kind: &str, props: Value) -> Value {
    let mut obj = match props {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert("id".to_string(), Value::String(id.to_string()));
    obj.insert("component".to_string(), Value::String(kind.to_string()));
    Value::Object(obj)
}

/// A `Card` with a single child component id.
pub fn card(id: &str, child: &str) -> Value {
    component(id, component::CARD, json!({ "child": child }))
}

/// A `Column`/`Row` container with ordered children and a pixel gap.
pub fn container(id: &str, kind: &str, children: &[&str], gap: i64) -> Value {
    component(
        id,
        kind,
        json!({ "children": children, "gap": gap }),
    )
}

/// A `Column` with ordered children and a pixel gap.
pub fn column(id: &str, children: &[&str], gap: i64) -> Value {
    container(id, component::COLUMN, children, gap)
}

/// A `Row` with ordered children and a pixel gap.
pub fn row(id: &str, children: &[&str], gap: i64) -> Value {
    container(id, component::ROW, children, gap)
}

/// A `Text` node. Use `variant` `h1`…`h5` / `caption` / `body` for headings —
/// the catalog has no `Title` or `Header` component.
pub fn text(id: &str, value: &str, variant: &str) -> Value {
    component(id, component::TEXT, json!({ "text": value, "variant": variant }))
}

/// A `TextField` bound to a data-model path (e.g. `/name`).
///
/// `obscured` maps to the catalog's `variant: "obscured"` (password fields).
pub fn text_field(id: &str, label: &str, path: &str, obscured: bool) -> Value {
    let mut props = json!({ "label": label, "value": { "path": path } });
    if obscured {
        props["variant"] = Value::String("obscured".to_string());
    }
    component(id, component::TEXT_FIELD, props)
}

/// A `Button` with a label child that emits an A2UI action event.
///
/// `context` values may be data bindings such as `{"path": "/name"}` so the
/// emitted event carries the current form contents.
pub fn button(id: &str, label_id: &str, label: &str, event: &str, context: Value) -> Vec<Value> {
    vec![
        component(
            id,
            component::BUTTON,
            json!({
                "child": label_id,
                "variant": "primary",
                "action": { "event": { "name": event, "context": context } },
            }),
        ),
        text(label_id, label, "body"),
    ]
}

/// A `ResetButton` that clears a surface locally (no action event is emitted).
pub fn reset_button(id: &str, label: &str, surface_id: &str, data: Value) -> Value {
    component(
        id,
        component::RESET_BUTTON,
        json!({ "label": label, "surfaceId": surface_id, "data": data }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_merges_id_and_kind_flat() {
        let c = text("title", "Create Account", "h2");
        assert_eq!(c["id"], "title");
        assert_eq!(c["component"], "Text");
        assert_eq!(c["text"], "Create Account");
        assert_eq!(c["variant"], "h2");
    }

    #[test]
    fn obscured_field_sets_variant() {
        let plain = text_field("f", "Name", "/name", false);
        assert!(plain.get("variant").is_none());
        let secret = text_field("f", "Password", "/password", true);
        assert_eq!(secret["variant"], "obscured");
    }

    #[test]
    fn render_produces_an_operation_array() {
        let ops = render(vec![
            create_surface("s", "cat"),
            update_data_model("s", json!({ "name": "" })),
        ]);
        assert!(ops.is_array());
        assert_eq!(ops[0]["createSurface"]["surfaceId"], "s");
        assert_eq!(ops[1]["updateDataModel"]["data"]["name"], "");
    }

    #[test]
    fn button_emits_an_action_event_with_bindings() {
        let parts = button("submit", "label", "Register", "register", json!({ "name": { "path": "/name" } }));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["action"]["event"]["name"], "register");
        assert_eq!(parts[0]["action"]["event"]["context"]["name"]["path"], "/name");
    }
}
