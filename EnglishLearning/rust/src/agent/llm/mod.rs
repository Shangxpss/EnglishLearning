//! **Model layer** — the types exchanged with an OpenAI-compatible chat API.
//!
//! Replaces `langchain_deepseek.ChatDeepSeek` + LangChain's message classes: the
//! wire format of an OpenAI-compatible `/chat/completions` call is small enough
//! to model directly, which removes the whole framework from the binary.
//!
//! The transport lives in [`openai`]; this module holds only data types so tools,
//! the engine and tests can use them without touching HTTP.

pub mod openai;

pub use openai::OpenAiClient;

use serde::{Deserialize, Serialize};
use std::fmt;

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// One entry of the conversation sent to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    /// Text content. Absent for an assistant turn that only calls tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls requested by the assistant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Which tool call this message answers (only for [`Role::Tool`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// A `system` message.
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage { role: Role::System, content: Some(content.into()), tool_calls: None, tool_call_id: None }
    }

    /// A `user` message.
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage { role: Role::User, content: Some(content.into()), tool_calls: None, tool_call_id: None }
    }

    /// An `assistant` message containing text.
    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage { role: Role::Assistant, content: Some(content.into()), tool_calls: None, tool_call_id: None }
    }

    /// An `assistant` message that only requests tool calls.
    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        ChatMessage { role: Role::Assistant, content: None, tool_calls: Some(tool_calls), tool_call_id: None }
    }

    /// A `tool` message carrying a tool's result back to the model.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A tool the model is allowed to call, in OpenAI's `tools` format.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    /// Always `"function"` for now.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: FunctionSpec,
}

impl ToolSpec {
    /// Build a `function` tool specification.
    pub fn function(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        ToolSpec {
            kind: "function",
            function: FunctionSpec {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

/// The `function` object inside a [`ToolSpec`].
#[derive(Debug, Clone, Serialize)]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// A tool call requested by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

impl ToolCall {
    /// Convenience constructor used by tests and non-streaming paths.
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: impl Into<String>) -> Self {
        ToolCall {
            id: id.into(),
            kind: function_kind(),
            function: FunctionCall { name: name.into(), arguments: arguments.into() },
        }
    }
}

fn function_kind() -> String {
    "function".to_string()
}

/// Name + raw JSON arguments of a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON encoded as a string, exactly as the model produced it.
    #[serde(default)]
    pub arguments: String,
}

impl FunctionCall {
    /// Parse `arguments` as JSON, tolerating an empty string (means `{}`).
    pub fn arguments_json(&self) -> Result<serde_json::Value, String> {
        let raw = self.arguments.trim();
        if raw.is_empty() {
            return Ok(serde_json::json!({}));
        }
        serde_json::from_str(raw).map_err(|e| format!("invalid tool arguments for '{}': {e}", self.name))
    }
}

/// An incremental piece of a streamed completion.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    /// A slice of assistant text.
    Content(String),
    /// A slice of a tool call. `index` identifies the call within the message.
    ToolCall {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    /// The provider signalled end of stream (`data: [DONE]`).
    Done,
}

/// Errors surfaced by the model layer.
#[derive(Debug, Clone)]
pub enum LlmError {
    /// Transport-level failure (DNS, TLS, connection, …).
    Http(String),
    /// The provider answered with a non-2xx status.
    Api { status: u16, body: String },
    /// A response chunk could not be understood.
    Parse(String),
    /// No credentials were configured.
    MissingCredentials,
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::Http(e) => write!(f, "model request failed: {e}"),
            LlmError::Api { status, body } => write!(f, "model API returned {status}: {body}"),
            LlmError::Parse(e) => write!(f, "could not parse model response: {e}"),
            LlmError::MissingCredentials => {
                write!(f, "no model credentials configured (set DEEPSEEK_API_KEY)")
            }
        }
    }
}

impl std::error::Error for LlmError {}

/// Accumulates streamed [`Delta`]s into whole messages.
///
/// OpenAI streams tool calls as fragments (`id` and `name` usually arrive once,
/// `arguments` arrives piecemeal), so the engine needs this to rebuild them.
#[derive(Debug, Default)]
pub struct MessageAccumulator {
    text: String,
    calls: Vec<PartialToolCall>,
}

#[derive(Debug, Default, Clone)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl MessageAccumulator {
    /// Create an empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one delta into the message under construction.
    pub fn push(&mut self, delta: &Delta) {
        match delta {
            Delta::Content(text) => self.text.push_str(text),
            Delta::ToolCall { index, id, name, arguments } => {
                let slot = self.slot(*index);
                if let Some(id) = id {
                    slot.id = id.clone();
                }
                if let Some(name) = name {
                    slot.name = name.clone();
                }
                if let Some(args) = arguments {
                    slot.arguments.push_str(args);
                }
            }
            Delta::Done => {}
        }
    }

    fn slot(&mut self, index: usize) -> &mut PartialToolCall {
        while self.calls.len() <= index {
            self.calls.push(PartialToolCall::default());
        }
        &mut self.calls[index]
    }

    /// The assistant text accumulated so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether any text was produced.
    pub fn has_text(&self) -> bool {
        !self.text.is_empty()
    }

    /// The tool calls, with unnamed/empty entries dropped.
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.calls
            .iter()
            .filter(|c| !c.name.is_empty())
            .enumerate()
            .map(|(i, c)| ToolCall {
                id: if c.id.is_empty() { format!("call_{i}") } else { c.id.clone() },
                kind: function_kind(),
                function: FunctionCall { name: c.name.clone(), arguments: c.arguments.clone() },
            })
            .collect()
    }

    /// Whether the model asked for any tool.
    pub fn has_tool_calls(&self) -> bool {
        self.calls.iter().any(|c| !c.name.is_empty())
    }

    /// Convert the accumulated turn into a conversation message.
    pub fn into_message(self) -> ChatMessage {
        let calls = self.tool_calls();
        if calls.is_empty() {
            ChatMessage::assistant(self.text)
        } else {
            ChatMessage::assistant_tool_calls(calls)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tool_arguments_mean_an_empty_object() {
        let call = FunctionCall { name: "display_register_form".into(), arguments: String::new() };
        assert_eq!(call.arguments_json().unwrap(), serde_json::json!({}));
    }

    #[test]
    fn invalid_tool_arguments_are_reported_with_the_tool_name() {
        let call = FunctionCall { name: "calculate".into(), arguments: "{oops".into() };
        let err = call.arguments_json().unwrap_err();
        assert!(err.contains("calculate"), "{err}");
    }

    #[test]
    fn accumulator_rebuilds_fragmented_tool_calls() {
        let mut acc = MessageAccumulator::new();
        acc.push(&Delta::Content("Let me check. ".into()));
        acc.push(&Delta::ToolCall { index: 0, id: Some("call_a".into()), name: Some("get_weather".into()), arguments: Some("{\"loca".into()) });
        acc.push(&Delta::ToolCall { index: 0, id: None, name: None, arguments: Some("tion\":\"Beijing\"}".into()) });
        acc.push(&Delta::Done);

        assert_eq!(acc.text(), "Let me check. ");
        assert!(acc.has_tool_calls());
        let calls = acc.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(calls[0].function.arguments_json().unwrap()["location"], "Beijing");
    }

    #[test]
    fn accumulator_fills_missing_ids_and_skips_unnamed_calls() {
        let mut acc = MessageAccumulator::new();
        acc.push(&Delta::ToolCall { index: 0, id: None, name: Some("calculate".into()), arguments: Some("{}".into()) });
        acc.push(&Delta::ToolCall { index: 1, id: Some("x".into()), name: None, arguments: None });
        let calls = acc.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_0");
    }

    #[test]
    fn accumulator_without_tools_becomes_a_text_message() {
        let mut acc = MessageAccumulator::new();
        acc.push(&Delta::Content("hello".into()));
        let msg = acc.into_message();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content.as_deref(), Some("hello"));
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn tool_messages_serialize_with_the_call_id() {
        let msg = ChatMessage::tool_result("call_a", "sunny");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_a");
        assert!(json.get("tool_calls").is_none());
    }
}
