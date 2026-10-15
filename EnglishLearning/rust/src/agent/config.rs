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
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Default agent id — must match `CopilotKit agent="…"` in the frontend.
pub const DEFAULT_AGENT_ID: &str = "sample_agent";
/// Default A2UI catalog id — must match `createCatalog({ catalogId })`.
///
/// The frontend registers its catalog under this id
/// (`frontend/web/src/a2ui/catalog.ts`), and every `createSurface` operation
/// names it, so a mismatch makes the client renderer report
/// "Catalog not found" and drop the whole surface.
pub const DEFAULT_CATALOG_ID: &str = "english-learning-catalog";
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
    /// Folder holding `threads.db`. `None` keeps chat history in memory only
    /// (what the tests get); the running service always resolves a real folder.
    pub data_dir: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            host: "127.0.0.1".to_string(),
            port: 4000,
            agent_id: DEFAULT_AGENT_ID.to_string(),
            agent_name: "sample_agent".to_string(),
            agent_description: "A helpful AI assistant powered by DeepSeek with tool capabilities."
                .to_string(),
            catalog_id: DEFAULT_CATALOG_ID.to_string(),
            model: DEFAULT_MODEL.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: String::new(),
            temperature: 0.0,
            max_steps: 8,
            data_dir: None,
        }
    }
}

impl AgentConfig {
    /// Build a configuration from the environment, falling back to defaults.
    ///
    /// `.env` is read first (see [`load_dotenv`]), so `DEEPSEEK_API_KEY` can live
    /// in a gitignored file instead of the shell or the command line.
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
    /// | `AGENT_DATA_DIR` | [`Self::data_dir`] |
    pub fn from_env() -> Self {
        load_dotenv();
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
        if let Some(v) = env_string("AGENT_DATA_DIR") {
            cfg.data_dir = Some(v);
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
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ─────────────────────────────────────────────────────────────────────────
// .env loading
// ─────────────────────────────────────────────────────────────────────────

/// The `.env` a run reads, resolved once.
///
/// `None` means "nothing found", including "searched and missed" — the answer is
/// cached either way, so only the first caller can log the miss.
static DOTENV_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Load `.env` into the process environment, without overriding anything already
/// set there.
///
/// An exported `DEEPSEEK_API_KEY=… ./sentence-video agent` must win over the file:
/// that precedence is what makes a stale `.env` harmless when you are testing a
/// different key. The file lives in the working directory or in any ancestor of
/// it, so `pnpm run dev:agent` (which runs from the workspace root) picks up the
/// one next to `package.json`.
///
/// Variables already present in the environment are skipped, which also means a
/// repeated call is harmless.
///
/// Set `DOTENV_PATH` to read a specific file instead of searching; that is also
/// the hook the tests use.
pub fn load_dotenv() {
    if let Some(path) = dotenv_path() {
        load_dotenv_from(path);
    }
}

/// The `.env` to read, for diagnostics and tests.
pub fn dotenv_path() -> Option<&'static Path> {
    DOTENV_PATH
        .get_or_init(|| {
            if let Some(explicit) = env_string("DOTENV_PATH") {
                let path = PathBuf::from(explicit);
                if !path.is_file() {
                    eprintln!(
                        "sentence-video agent: DOTENV_PATH={} is not a file — ignoring it",
                        path.display()
                    );
                    return None;
                }
                return Some(path);
            }

            let cwd = env::current_dir().ok()?;
            let mut dir: Option<&Path> = Some(cwd.as_path());
            while let Some(current) = dir {
                let candidate = current.join(".env");
                if candidate.is_file() {
                    return Some(candidate);
                }
                dir = current.parent();
            }

            eprintln!(
                "sentence-video agent: no .env found from {} upward — \
                 DEEPSEEK_API_KEY must be exported",
                cwd.display()
            );
            None
        })
        .as_deref()
}

/// Apply one dotenv file.
fn load_dotenv_from(path: &Path) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("sentence-video agent: cannot read {}: {e}", path.display());
            return;
        }
    };

    let mut loaded = 0usize;
    for line in text.lines() {
        let Some((key, value)) = parse_dotenv_line(line) else {
            continue;
        };
        // An already-exported variable wins over the file, so testing a one-off
        // key needs no edit to `.env`.
        if env::var_os(&key).is_some() {
            continue;
        }
        // `set_var` is unsafe as of edition 2024 because it races with other
        // threads reading the environment. This runs once, before the runtime is
        // built, so there is nothing to race with.
        unsafe {
            env::set_var(&key, &value);
        }
        loaded += 1;
    }
    eprintln!(
        "sentence-video agent: loaded {loaded} variable(s) from {}",
        path.display()
    );
}

/// Parse one dotenv line into a key/value pair.
///
/// Understands the shape a `.env` is actually written in: blank lines, `#`
/// comments, an optional `export ` prefix, `KEY=value`, single or double quotes,
/// and a trailing `# comment` on an unquoted value. Returns `None` for anything
/// that is not a pair, so a malformed file degrades instead of failing startup.
fn parse_dotenv_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(line);

    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }

    let value = value.trim();
    let value = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        // Quoted: the quotes are literal boundaries, so a `#` inside stays.
        value[1..value.len() - 1].to_string()
    } else {
        // Unquoted: an unescaped `#` starts a trailing comment.
        let cut = value
            .find(" #")
            .or_else(|| value.strip_prefix('#').map(|_| 0));
        match cut {
            Some(0) => String::new(),
            Some(at) => value[..at].trim_end().to_string(),
            None => value.to_string(),
        }
    };

    Some((key.to_string(), value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_chat_completions_url_without_double_slash() {
        let mut cfg = AgentConfig::default();
        cfg.base_url = "https://api.deepseek.com/".to_string();
        assert_eq!(
            cfg.chat_completions_url(),
            "https://api.deepseek.com/chat/completions"
        );
    }

    #[test]
    fn parses_the_dotenv_shapes_a_real_file_uses() {
        assert_eq!(
            parse_dotenv_line("DEEPSEEK_API_KEY=sk-abc"),
            Some(("DEEPSEEK_API_KEY".into(), "sk-abc".into()))
        );
        assert_eq!(
            parse_dotenv_line("export AGENT_MODEL=\"deepseek-chat\""),
            Some(("AGENT_MODEL".into(), "deepseek-chat".into()))
        );
        assert_eq!(
            parse_dotenv_line("AGENT_HOST='127.0.0.1'"),
            Some(("AGENT_HOST".into(), "127.0.0.1".into()))
        );
        assert_eq!(
            parse_dotenv_line("AGENT_PORT=4000 # the agent port"),
            Some(("AGENT_PORT".into(), "4000".into()))
        );
        assert_eq!(
            parse_dotenv_line("EMPTY="),
            Some(("EMPTY".into(), String::new()))
        );
    }

    #[test]
    fn ignores_comments_blanks_and_junk() {
        assert_eq!(parse_dotenv_line("# a comment"), None);
        assert_eq!(parse_dotenv_line("   "), None);
        assert_eq!(parse_dotenv_line("not a pair"), None);
        assert_eq!(parse_dotenv_line("=novalue"), None);
    }

    #[test]
    fn a_quoted_value_keeps_a_hash() {
        assert_eq!(
            parse_dotenv_line("SECRET=\"abc#def\""),
            Some(("SECRET".into(), "abc#def".into()))
        );
        // Unquoted, the `#` opens a comment only when preceded by whitespace.
        assert_eq!(
            parse_dotenv_line("SECRET=abc#def"),
            Some(("SECRET".into(), "abc#def".into()))
        );
    }

    #[test]
    fn defaults_are_local_and_credential_free() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.bind_addr(), "127.0.0.1:4000");
        assert!(!cfg.has_credentials());
    }
}
