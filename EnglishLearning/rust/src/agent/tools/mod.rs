//! **Tool layer** — the Rust replacement for Python's `@tool` decorated
//! functions.
//!
//! A [`Tool`] owns its name, description, JSON Schema and behaviour; the
//! [`ToolRegistry`] turns them into the `tools` array the model sees and
//! dispatches the calls it asks for.
//!
//! A tool may also return A2UI operations ([`ToolOutcome::a2ui`]). That is how
//! `display_register_form` renders a form without a sub-agent round-trip: the
//! operations are forwarded by the runtime as an `ACTIVITY_SNAPSHOT`.

pub mod builtin;
pub mod math;

use super::llm::ToolSpec;
use serde_json::Value;

/// What a tool returns: the text the model reads, plus any UI it produced.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    /// Tool result handed back to the model.
    pub content: String,
    /// A2UI operations (a JSON array produced by [`super::protocol::a2ui::render`]).
    pub a2ui: Option<Value>,
}

impl ToolOutcome {
    /// A tool that only returns text.
    pub fn text(content: impl Into<String>) -> Self {
        ToolOutcome { content: content.into(), a2ui: None }
    }

    /// A tool that returns text and A2UI operations.
    pub fn with_a2ui(content: impl Into<String>, operations: Value) -> Self {
        ToolOutcome { content: content.into(), a2ui: Some(operations) }
    }
}

/// A capability the model may invoke.
pub trait Tool: Send + Sync {
    /// Function name exposed to the model (snake_case).
    fn name(&self) -> &'static str;

    /// Description shown to the model — this is how it decides when to call.
    fn description(&self) -> &'static str;

    /// JSON Schema for the arguments object.
    fn parameters(&self) -> Value;

    /// Execute the tool. `args` is the parsed arguments object; an error string
    /// is returned to the model as the tool result (so it can retry).
    fn call(&self, args: &Value) -> Result<ToolOutcome, String>;
}

/// An ordered collection of tools.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        ToolRegistry { tools: Vec::new() }
    }

    /// The registry used by the service: weather, maths, registration, the A2UI
    /// form and the generic A2UI renderer.
    pub fn with_builtins() -> Self {
        ToolRegistry::new()
            .register(builtin::GetWeather)
            .register(builtin::Calculate)
            .register(builtin::RegisterUser)
            .register(builtin::DisplayRegisterForm)
            .register(builtin::RenderA2ui)
    }

    /// Add a tool, builder style.
    pub fn register(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.push(Box::new(tool));
        self
    }

    /// The `tools` array sent with every model request.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|t| ToolSpec::function(t.name(), t.description(), t.parameters()))
            .collect()
    }

    /// Registered tool names, in registration order.
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// Look a tool up by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
    }

    /// Dispatch a call, reporting an unknown tool as a readable error.
    pub fn call(&self, name: &str, args: &Value) -> Result<ToolOutcome, String> {
        match self.get(name) {
            Some(tool) => tool.call(args),
            None => Err(format!(
                "unknown tool '{name}'; available tools: {}",
                self.names().join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtins_expose_a_spec_each() {
        let registry = ToolRegistry::with_builtins();
        let specs = registry.specs();
        assert_eq!(specs.len(), registry.names().len());
        assert!(registry.names().contains(&"get_weather"));
        assert!(registry.names().contains(&"render_a2ui"));
        for spec in specs {
            assert_eq!(spec.kind, "function");
            assert!(spec.function.parameters.is_object());
        }
    }

    #[test]
    fn unknown_tools_list_the_available_ones() {
        let registry = ToolRegistry::with_builtins();
        let err = registry.call("nope", &json!({})).unwrap_err();
        assert!(err.contains("unknown tool 'nope'"));
        assert!(err.contains("get_weather"));
    }

    #[test]
    fn registration_order_is_preserved() {
        let registry = ToolRegistry::with_builtins();
        assert_eq!(registry.names()[0], "get_weather");
        assert_eq!(registry.names()[1], "calculate");
    }
}
