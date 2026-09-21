//! OpenAI-compatible streaming chat client.
//!
//! DeepSeek exposes an OpenAI-compatible `/chat/completions`, so this one client
//! covers it (and any other compatible provider). It replaces
//! `ChatDeepSeek`/`async-openai` with ~150 lines of `reqwest` + `serde`, which
//! keeps the dependency surface small and the streaming behaviour explicit.
//!
//! Responses are consumed as Server-Sent Events: each `data:` line is a partial
//! completion that is turned into one or more [`Delta`]s.

use super::{ChatMessage, Delta, LlmError, ToolSpec};
use crate::agent::config::AgentConfig;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;

/// Streaming client for an OpenAI-compatible chat API.
pub struct OpenAiClient {
    http: reqwest::Client,
    url: String,
    api_key: String,
    model: String,
    temperature: f32,
}

impl OpenAiClient {
    /// Build a client from the agent configuration.
    ///
    /// Credentials are **not** validated here — a missing key is reported by
    /// [`Self::chat_stream`] so the service can still start and answer
    /// `/health` and `/copilotkit/info`.
    pub fn new(cfg: &AgentConfig) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| LlmError::Http(e.to_string()))?;
        Ok(OpenAiClient {
            http,
            url: cfg.chat_completions_url(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            temperature: cfg.temperature,
        })
    }

    /// The model name in use.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The endpoint being called.
    pub fn endpoint(&self) -> &str {
        &self.url
    }

    /// Stream a completion for `messages`, offering `tools` to the model.
    ///
    /// The returned stream yields [`Delta`]s and always ends with
    /// [`Delta::Done`] (also when the provider closed the connection without
    /// sending `[DONE]`), so callers can rely on a single terminator.
    pub async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<impl Stream<Item = Result<Delta, LlmError>>, LlmError> {
        if self.api_key.trim().is_empty() {
            return Err(LlmError::MissingCredentials);
        }

        // Build the body explicitly so empty `tools` are omitted entirely
        // (some providers reject `"tools": []`).
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), serde_json::json!(self.model));
        body.insert("messages".to_string(), serde_json::json!(messages));
        body.insert("stream".to_string(), serde_json::json!(true));
        body.insert("temperature".to_string(), serde_json::json!(self.temperature));
        if !tools.is_empty() {
            body.insert("tools".to_string(), serde_json::json!(tools));
            body.insert("tool_choice".to_string(), serde_json::json!("auto"));
        }

        let response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::Value::Object(body))
            .send()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Api { status: status.as_u16(), body });
        }

        let mut bytes = response.bytes_stream();

        Ok(async_stream::stream! {
            let mut buffer = String::new();
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(LlmError::Http(e.to_string()));
                        return;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // SSE frames are newline delimited; keep the trailing partial line.
                while let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].trim_end_matches('\r').to_string();
                    buffer.drain(..=pos);

                    let Some(payload) = line.strip_prefix("data:") else { continue };
                    let payload = payload.trim();
                    if payload.is_empty() {
                        continue;
                    }
                    if payload == "[DONE]" {
                        yield Ok(Delta::Done);
                        return;
                    }
                    match parse_chunk(payload) {
                        Ok(deltas) => {
                            for delta in deltas {
                                yield Ok(delta);
                            }
                        }
                        // A single unreadable frame must not kill the run.
                        Err(e) => yield Err(e),
                    }
                }
            }
            yield Ok(Delta::Done);
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Streaming chunk decoding
// ─────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    delta: ChunkDelta,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Deserialize)]
struct ChunkToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChunkFunction>,
}

#[derive(Deserialize)]
struct ChunkFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Decode one SSE payload into the deltas it carries.
fn parse_chunk(payload: &str) -> Result<Vec<Delta>, LlmError> {
    let chunk: Chunk = serde_json::from_str(payload)
        .map_err(|e| LlmError::Parse(format!("{e} (payload: {payload})")))?;

    let mut out = Vec::new();
    for choice in chunk.choices {
        if let Some(text) = choice.delta.content {
            if !text.is_empty() {
                out.push(Delta::Content(text));
            }
        }
        if let Some(calls) = choice.delta.tool_calls {
            for call in calls {
                let (name, arguments) = match call.function {
                    Some(f) => (f.name, f.arguments),
                    None => (None, None),
                };
                out.push(Delta::ToolCall { index: call.index, id: call.id, name, arguments });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_text_delta() {
        let deltas = parse_chunk(r#"{"choices":[{"delta":{"content":"Hello"}}]}"#).unwrap();
        assert_eq!(deltas, vec![Delta::Content("Hello".to_string())]);
    }

    #[test]
    fn decodes_a_tool_call_fragment() {
        let deltas = parse_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"loc"}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(
            deltas,
            vec![Delta::ToolCall {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("get_weather".to_string()),
                arguments: Some("{\"loc".to_string()),
            }]
        );
    }

    #[test]
    fn ignores_empty_deltas_and_unknown_fields() {
        let deltas = parse_chunk(r#"{"id":"x","choices":[{"finish_reason":null,"delta":{}}]}"#).unwrap();
        assert!(deltas.is_empty());
    }

    #[test]
    fn malformed_payloads_are_reported_not_panicked() {
        assert!(parse_chunk("{not json").is_err());
    }

    #[test]
    fn client_requires_credentials_before_calling_out() {
        let cfg = AgentConfig::default();
        let client = OpenAiClient::new(&cfg).unwrap();
        assert_eq!(client.endpoint(), "https://api.deepseek.com/chat/completions");
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let err = rt
            .block_on(client.chat_stream(&[], &[]))
            .err()
            .expect("missing credentials must fail");
        assert!(matches!(err, LlmError::MissingCredentials));
    }
}
