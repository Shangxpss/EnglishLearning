//! **A2UI middleware** — the Rust replacement for the Bun `A2UIMiddleware` and
//! the Python `CopilotKitMiddleware`/`A2UIAwareMiddleware`.
//!
//! The two middlewares used to do five things; in a single Rust service most of
//! them collapse:
//!
//! | old step | here |
//! | --- | --- |
//! | `processUserAction` (button clicks) | the frontend sends the action as the next user message; nothing to intercept |
//! | `injectSchemaContext` | [`A2uiMiddleware::prepare`] adds the catalog guidelines to the request |
//! | `injectToolAndFlag` | the `render_a2ui` tool is always registered in [`crate::agent::tools::ToolRegistry::with_builtins`] |
//! | `injectToolGuidelines` | same system message as `injectSchemaContext` |
//! | `processStream` → `ACTIVITY_SNAPSHOT` | [`A2uiMiddleware::snapshot`] |
//!
//! What is left is small, and it is the only place that knows about the A2UI
//! catalog — which is why the engine only sees the [`RequestEnricher`] trait.
//! The `ACTIVITY_SNAPSHOT` it builds is a real [`ag_ui::Event`], so it passes
//! through the crate's ordering verifier like any other event.

use crate::agent::engine::RequestEnricher;
use crate::agent::llm::{ChatMessage, Role};
use crate::agent::new_id;
use crate::agent::protocol::{a2ui, A2UI_ACTIVITY_TYPE};
use ag_ui::Event;
use serde_json::Value;

/// Adds A2UI catalog guidance to requests and turns tool output into UI events.
#[derive(Debug, Clone)]
pub struct A2uiMiddleware {
    catalog_id: String,
}

impl A2uiMiddleware {
    /// Build the middleware for a catalog.
    pub fn new(catalog_id: impl Into<String>) -> Self {
        A2uiMiddleware { catalog_id: catalog_id.into() }
    }

    /// The catalog this middleware advertises.
    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }

    /// The system message injected before the model call.
    ///
    /// This is what makes `render_a2ui` usable: it tells the model which
    /// components exist and how they are shaped, so it does not invent
    /// `Title`/`Header` (the failure mode the Python sub-agent had to recover
    /// from with `validate_a2ui_components`).
    pub fn guidelines(&self) -> String {
        format!(
            "A2UI catalog '{catalog}':\n\
             Every component is a flat JSON object: {{ \"id\": \"unique-id\", \"component\": \"Name\", ...props }}.\n\
             Containers reference their children by id (`child` for one, `children` for many).\n\
             Data bindings are objects of the form {{ \"path\": \"/some/field\" }} and resolve against the surface data.\n\
             Available components — use ONLY these:\n\
             - Layout: Card, Row, Column, List\n\
             - Display: Text (variant: h1|h2|h3|h4|h5|caption|body), Heading, Image, Icon, Divider, StatusBadge, Metric, InfoRow, KeyValueList\n\
             - Interactive: Button, TextField (variant: obscured for passwords), CheckBox, ChoicePicker, Slider, DateTimeInput, ResetButton\n\
             - Container: Tabs, Modal\n\
             - Media: Video, AudioPlayer\n\
             - Data: Grid, Table, BarChart, PieChart, UserCard, Spacer\n\
             There is no 'Title', 'Header' or 'Paragraph' component — use Text with a variant, or Heading.\n\
             Pass components to the `render_a2ui` tool with a `surfaceId`; reuse the same `surfaceId` to update a surface in place.",
            catalog = self.catalog_id
        )
    }
}

impl RequestEnricher for A2uiMiddleware {
    fn prepare(&self, messages: &mut Vec<ChatMessage>) {
        // Keep the persona system prompt first, then the catalog guidance, so the
        // most specific instructions are closest to the conversation.
        let index = if messages.first().map(|m| m.role == Role::System).unwrap_or(false) {
            1
        } else {
            0
        };
        messages.insert(index, ChatMessage::system(self.guidelines()));
    }

    fn snapshot(&self, tool_name: &str, operations: Value) -> Event {
        // AG-UI wants an object payload, A2UI produces an array — the wrapper in
        // `a2ui::activity_content` reconciles the two and records the tool.
        Event::activity_snapshot(
            new_id("a2ui"),
            A2UI_ACTIVITY_TYPE,
            a2ui::activity_content(tool_name, operations),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event_type(event: &Event) -> String {
        serde_json::to_value(event).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn guidelines_name_the_catalog_and_the_real_components() {
        let mw = A2uiMiddleware::new("generative-agent-catalog");
        let text = mw.guidelines();
        assert_eq!(mw.catalog_id(), "generative-agent-catalog");
        assert!(text.contains("generative-agent-catalog"));
        assert!(text.contains("render_a2ui"));
        assert!(text.contains("ResetButton"));
        assert!(text.contains("no 'Title'"));
    }

    #[test]
    fn prepare_keeps_the_persona_prompt_first() {
        let mw = A2uiMiddleware::new("cat");
        let mut messages = vec![
            ChatMessage::system("persona"),
            ChatMessage::user("hello"),
        ];
        mw.prepare(&mut messages);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content.as_deref(), Some("persona"));
        assert!(messages[1].content.as_deref().unwrap().contains("A2UI catalog 'cat'"));
        assert_eq!(messages[2].content.as_deref(), Some("hello"));
    }

    #[test]
    fn prepare_handles_a_conversation_without_a_system_prompt() {
        let mw = A2uiMiddleware::new("cat");
        let mut messages = vec![ChatMessage::user("hello")];
        mw.prepare(&mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[1].role, Role::User);
    }

    #[test]
    fn snapshot_wraps_the_operations_in_an_activity_object() {
        let mw = A2uiMiddleware::new("cat");
        let ops = json!([{ "createSurface": { "surfaceId": "s" } }]);
        let event = mw.snapshot("display_register_form", ops.clone());

        assert_eq!(event_type(&event), "ACTIVITY_SNAPSHOT");
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["activityType"], A2UI_ACTIVITY_TYPE);
        assert_eq!(value["content"]["tool"], "display_register_form");
        assert_eq!(value["content"]["operations"], ops);
    }
}
