//! Agent service configuration.
//!
//! Mirrors the settings that were spread across the Python `backend`
//! (`ChatDeepSeek(model="deepseek-chat")`, `A2UIFixedAgent(name="sample_agent")`)
//! and the Bun `agent-runtime` (`CopilotRuntime` agent key + `defaultCatalogId`).
//!
//! **The three ids must stay in sync with the frontend** (see
//! `AI-Demo/technical_architecture.md` §5): the agent id, the catalog id and the
//! model are all configurable here.

use std::env;

/// Default agent id — must match `CopilotKit agent="…"` in the frontend.
pub const DEFAULT_AGENT_ID: &str = "sample_agent";
/// Default A2UI catalog id — must match `createCatalog({ catalogId })`.
pub const DEFAULT_CATALOG_ID: &str = "generative-agent-catalog";
/// Default model served by DeepSeek's OpenAI-compatible endpoint.
pub const DEFAULT_MODEL: &str = "deepseek-chat";
/// Default OpenAI-compatible base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

/// Everything the agent service needs to run.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Bind address (default `127.0.0.1` — local single-user, like the media app).
    pub host: String,
    /// Bind port. `0` lets the OS assign a free one.
    pub port: u16,
    /// Agent id exposed through `/copilotkit/agent/{agent_id}/…`.
    pub agent_id: String,
    /// Human-readable agent name reported by `/copilotkit/info`.
    pub agent_name: String,
    /// Description reported by `/copilotkit/info`.
    pub agent_description: String,
    /// A2UI catalog id advertised to the frontend.
    pub catalog_id: String,
    /// Model name sent to the provider.
    pub model: String,
    /// OpenAI-compatible base URL (DeepSeek by default).
    pub base_url: String,
    /// API key. Empty means "no credentials" → `/run` answers with an error event.
    pub api_key: String,
    /// Sampling temperature. The Python agent used `0`.
    pub temperature: f32,
    /// Maximum model↔tool round-trips per run (guards against loops).
    pub max_steps: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            host: "127.0.0.1".to_string(),
            port: 4000,
            agent_id: DEFAULT_AGENT_ID.to_string(),
            agent_name: "sample_agent".to_string(),
            agent_description:
                "A helpful AI assistant powered by DeepSeek with tool capabilities.".to_string(),
            catalog_id: DEFAULT_CATALOG_ID.to_string(),
            model: DEFAULT_MODEL.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: String::new(),
            temperature: 0.0,
            max_steps: 8,
        }
    }
}

impl AgentConfig {
    /// Build a configuration from the environment, falling back to defaults.
    ///
    /// | variable | field |
    /// | --- | --- |
    /// | `AGENT_HOST` | [`Self::host`] |
    /// | `AGENT_PORT` | [`Self::port`] |
    /// | `AGENT_ID` | [`Self::agent_id`] |
    /// | `AGENT_MODEL` | [`Self::model`] |
    /// | `AGENT_MAX_STEPS` | [`Self::max_steps`] |
    /// | `AGENT_CATALOG_ID` | [`Self::catalog_id`] |
    /// | `DEEPSEEK_BASE_URL` | [`Self::base_url`] |
    /// | `DEEPSEEK_API_KEY` | [`Self::api_key`] |
    pub fn from_env() -> Self {
        let mut cfg = AgentConfig::default();
        if let Some(v) = env_string("AGENT_HOST") {
            cfg.host = v;
        }
        if let Some(v) = env_string("AGENT_PORT").and_then(|v| v.parse().ok()) {
            cfg.port = v;
        }
        if let Some(v) = env_string("AGENT_ID") {
            cfg.agent_id = v;
        }
        if let Some(v) = env_string("AGENT_MODEL") {
            cfg.model = v;
        }
        if let Some(v) = env_string("AGENT_MAX_STEPS").and_then(|v| v.parse().ok()) {
            cfg.max_steps = v;
        }
        if let Some(v) = env_string("AGENT_CATALOG_ID") {
            cfg.catalog_id = v;
        }
        if let Some(v) = env_string("DEEPSEEK_BASE_URL") {
            cfg.base_url = v;
        }
        if let Some(v) = env_string("DEEPSEEK_API_KEY") {
            cfg.api_key = v;
        }
        cfg
    }

    /// `host:port` string suitable for binding and for the printed URL.
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// `{base_url}/chat/completions`, with a single slash between the parts.
    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Whether a usable API key is present.
    pub fn has_credentials(&self) -> bool {
        !self.api_key.trim().is_empty()
    }
}

/// Read a non-empty environment variable, trimming surrounding whitespace.
fn env_string(key: &str) -> Option<String> {
    env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_chat_completions_url_without_double_slash() {
        let mut cfg = AgentConfig::default();
        cfg.base_url = "https://api.deepseek.com/".to_string();
        assert_eq!(cfg.chat_completions_url(), "https://api.deepseek.com/chat/completions");
    }

    #[test]
    fn defaults_are_local_and_credential_free() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.bind_addr(), "127.0.0.1:4000");
        assert!(!cfg.has_credentials());
    }
}
