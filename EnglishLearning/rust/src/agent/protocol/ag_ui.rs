//! **AG-UI events** — the event-based protocol between the agent and the UI.
//!
//! Ported from the Python `ag_ui_langgraph` endpoint: the agent emits these as
//! Server-Sent Events, and CopilotKit turns them into chat messages, tool-call
//! cards and A2UI surfaces.
//!
//! The wire format follows `AI-Demo/technical_architecture.md` §4.1: every event
//! is a JSON object whose `type` field is the SCREAMING_SNAKE_CASE event name and
//! whose other fields are camelCase.

use serde::Serialize;

/// Event names, useful for logging and for the SSE `event:` line.
pub mod kind {
    pub const RUN_STARTED: &str = "RUN_STARTED";
    pub const STEP_STARTED: &str = "STEP_STARTED";
    pub const STEP_FINISHED: &str = "STEP_FINISHED";
    pub const TEXT_MESSAGE_START: &str = "TEXT_MESSAGE_START";
    pub const TEXT_MESSAGE_CHUNK: &str = "TEXT_MESSAGE_CHUNK";
    pub const TEXT_MESSAGE_END: &str = "TEXT_MESSAGE_END";
    pub const TOOL_CALL_STARTED: &str = "TOOL_CALL_STARTED";
    pub const TOOL_CALL_ARGS: &str = "TOOL_CALL_ARGS";
    pub const TOOL_CALL_RESULT: &str = "TOOL_CALL_RESULT";
    pub const ACTIVITY_SNAPSHOT: &str = "ACTIVITY_SNAPSHOT";
    pub const RUN_FINISHED: &str = "RUN_FINISHED";
    pub const RUN_ERROR: &str = "RUN_ERROR";
}

/// Activity type used for A2UI surfaces.
pub const A2UI_ACTIVITY_TYPE: &str = "a2ui-surface";

/// A single AG-UI event.
///
/// Internally tagged by `type`, which is what the protocol expects and what
/// `serde` supports for struct variants.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AgUiEvent {
    /// The run started; carries the thread/run correlation ids.
    #[serde(rename = "RUN_STARTED")]
    RunStarted {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "runId")]
        run_id: String,
    },

    /// A processing step began (e.g. `model`, `tools`).
    #[serde(rename = "STEP_STARTED")]
    StepStarted {
        #[serde(rename = "stepName")]
        step_name: String,
    },

    /// A processing step completed.
    #[serde(rename = "STEP_FINISHED")]
    StepFinished {
        #[serde(rename = "stepName")]
        step_name: String,
    },

    /// The assistant started a text message.
    #[serde(rename = "TEXT_MESSAGE_START")]
    TextMessageStart {
        #[serde(rename = "messageId")]
        message_id: String,
        role: String,
    },

    /// An incremental slice of assistant text.
    #[serde(rename = "TEXT_MESSAGE_CHUNK")]
    TextMessageChunk {
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
    },

    /// The assistant finished its text message.
    #[serde(rename = "TEXT_MESSAGE_END")]
    TextMessageEnd {
        #[serde(rename = "messageId")]
        message_id: String,
    },

    /// The model asked for a tool call.
    #[serde(rename = "TOOL_CALL_STARTED")]
    ToolCallStarted {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolCallName")]
        tool_call_name: String,
    },

    /// Incremental JSON arguments for a tool call (emitted once, in full, when
    /// the model does not stream arguments).
    #[serde(rename = "TOOL_CALL_ARGS")]
    ToolCallArgs {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        delta: String,
    },

    /// The tool finished; `content` is what the model will see.
    #[serde(rename = "TOOL_CALL_RESULT")]
    ToolCallResult {
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        content: String,
    },

    /// A UI surface update. A2UI operations ride inside `content`.
    #[serde(rename = "ACTIVITY_SNAPSHOT")]
    ActivitySnapshot {
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "activityType")]
        activity_type: String,
        content: serde_json::Value,
    },

    /// The run completed successfully.
    #[serde(rename = "RUN_FINISHED")]
    RunFinished {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "runId")]
        run_id: String,
    },

    /// The run failed; the frontend surfaces `message` to the user.
    #[serde(rename = "RUN_ERROR")]
    RunError {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

impl AgUiEvent {
    /// Build an `ACTIVITY_SNAPSHOT` carrying A2UI operations.
    pub fn a2ui_snapshot(message_id: impl Into<String>, operations: serde_json::Value) -> Self {
        AgUiEvent::ActivitySnapshot {
            message_id: message_id.into(),
            activity_type: A2UI_ACTIVITY_TYPE.to_string(),
            content: operations,
        }
    }

    /// Build a `RUN_ERROR` with no error code.
    pub fn error(message: impl Into<String>) -> Self {
        AgUiEvent::RunError { message: message.into(), code: None }
    }

    /// The event name, for the SSE `event:` line.
    pub fn name(&self) -> &'static str {
        match self {
            AgUiEvent::RunStarted { .. } => kind::RUN_STARTED,
            AgUiEvent::StepStarted { .. } => kind::STEP_STARTED,
            AgUiEvent::StepFinished { .. } => kind::STEP_FINISHED,
            AgUiEvent::TextMessageStart { .. } => kind::TEXT_MESSAGE_START,
            AgUiEvent::TextMessageChunk { .. } => kind::TEXT_MESSAGE_CHUNK,
            AgUiEvent::TextMessageEnd { .. } => kind::TEXT_MESSAGE_END,
            AgUiEvent::ToolCallStarted { .. } => kind::TOOL_CALL_STARTED,
            AgUiEvent::ToolCallArgs { .. } => kind::TOOL_CALL_ARGS,
            AgUiEvent::ToolCallResult { .. } => kind::TOOL_CALL_RESULT,
            AgUiEvent::ActivitySnapshot { .. } => kind::ACTIVITY_SNAPSHOT,
            AgUiEvent::RunFinished { .. } => kind::RUN_FINISHED,
            AgUiEvent::RunError { .. } => kind::RUN_ERROR,
        }
    }

    /// Serialize the event for the SSE `data:` line.
    pub fn to_sse_data(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            // Serialization of these types cannot fail in practice; degrade to a
            // well-formed error event rather than dropping the stream.
            format!("{{\"type\":\"RUN_ERROR\",\"message\":\"event serialization failed: {e}\"}}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_events_with_their_protocol_name() {
        let ev = AgUiEvent::TextMessageChunk {
            message_id: "m1".into(),
            delta: "hi".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], kind::TEXT_MESSAGE_CHUNK);
        assert_eq!(json["messageId"], "m1");
        assert_eq!(json["delta"], "hi");
        assert_eq!(ev.name(), kind::TEXT_MESSAGE_CHUNK);
    }

    #[test]
    fn activity_snapshot_carries_a2ui_operations() {
        let ops = serde_json::json!([{ "createSurface": { "surfaceId": "s" } }]);
        let ev = AgUiEvent::a2ui_snapshot("m1", ops.clone());
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["activityType"], A2UI_ACTIVITY_TYPE);
        assert_eq!(json["content"], ops);
    }
}
