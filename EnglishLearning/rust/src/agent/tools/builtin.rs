//! The built-in tools, ported one-for-one from `AI-Demo/backend/agent.py`.
//!
//! | Python tool | Rust type | notes |
//! | --- | --- | --- |
//! | `get_weather` | [`GetWeather`] | identical text response |
//! | `calculate` | [`Calculate`] | `eval()` replaced by [`super::math`] |
//! | `register_user` | [`RegisterUser`] | logs the registration |
//! | `display_register_form` | [`DisplayRegisterForm`] | emits the same A2UI form |
//! | `generate_a2ui` (sub-agent) | [`RenderA2ui`] | renders caller-supplied components |

use super::math;
use super::{Tool, ToolOutcome};
use crate::agent::protocol::a2ui;
use serde_json::{json, Value};

/// Surface id of the registration form (mirrors `REGISTER_SURFACE_ID`).
pub const REGISTER_SURFACE_ID: &str = "register-form";

/// Initial data model of the registration form (`REGISTER_FORM_DATA`).
pub fn register_form_data() -> Value {
    json!({ "name": "", "password": "" })
}

/// `get_weather(location)`.
pub struct GetWeather;

impl Tool for GetWeather {
    fn name(&self) -> &'static str {
        "get_weather"
    }

    fn description(&self) -> &'static str {
        "Get weather for a location. Returns weather data as text. \
         If you want to display a rich weather card, call display_weather instead."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "City or place name." }
            },
            "required": ["location"],
            "additionalProperties": false
        })
    }

    fn call(&self, args: &Value) -> Result<ToolOutcome, String> {
        let location = required_str(args, "location")?;
        Ok(ToolOutcome::text(format!(
            "The weather in {location} is sunny with 22°C."
        )))
    }
}

/// `calculate(expression)` — arithmetic only (no `eval`).
pub struct Calculate;

impl Tool for Calculate {
    fn name(&self) -> &'static str {
        "calculate"
    }

    fn description(&self) -> &'static str {
        "Calculate an arithmetic expression using + - * / % ^ and parentheses. \
         It evaluates numbers only; it cannot run code."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "Arithmetic expression, e.g. \"(2 + 3) * 4\"."
                }
            },
            "required": ["expression"],
            "additionalProperties": false
        })
    }

    fn call(&self, args: &Value) -> Result<ToolOutcome, String> {
        let expression = required_str(args, "expression")?;
        match math::evaluate(&expression) {
            Ok(value) => Ok(ToolOutcome::text(format!("Result: {value}"))),
            // Python returned "Error: {e}" as a normal result; keep that shape so
            // the model can correct itself instead of the run failing.
            Err(e) => Ok(ToolOutcome::text(format!("Error: {e}"))),
        }
    }
}

/// `register_user(name, password)`.
pub struct RegisterUser;

impl Tool for RegisterUser {
    fn name(&self) -> &'static str {
        "register_user"
    }

    fn description(&self) -> &'static str {
        "Register a new user with the given name and password. \
         Called when the user submits the registration form. \
         Logs the registration and returns a confirmation message."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The user's name from the registration form." },
                "password": { "type": "string", "description": "The user's password from the registration form." }
            },
            "required": ["name", "password"],
            "additionalProperties": false
        })
    }

    fn call(&self, args: &Value) -> Result<ToolOutcome, String> {
        let name = required_str(args, "name")?;
        let _password = required_str(args, "password")?;
        // The Python tool logged name + password verbatim; never do that with a
        // credential — log the name only.
        eprintln!("agent: register_user called for name={name}");
        Ok(ToolOutcome::text(format!("User '{name}' registered successfully!")))
    }
}

/// `display_register_form()` — renders the A2UI registration form.
pub struct DisplayRegisterForm;

impl Tool for DisplayRegisterForm {
    fn name(&self) -> &'static str {
        "display_register_form"
    }

    fn description(&self) -> &'static str {
        "Show a registration form with name and password fields and a submit button. \
         Use this when the user wants to register a new user. \
         After this tool returns, the form is already rendered — do NOT call it again. \
         When the user clicks the Register button, you will receive an action event \
         with the form data. Then call register_user to complete the registration."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn call(&self, _args: &Value) -> Result<ToolOutcome, String> {
        Ok(ToolOutcome::with_a2ui(
            "Registration form displayed.",
            register_form_operations(),
        ))
    }
}

/// The A2UI operations for the registration form.
///
/// Component-for-component port of `REGISTER_FORM_COMPONENTS` in `agent.py`.
pub fn register_form_operations() -> Value {
    let mut components = vec![
        a2ui::card("root", "form-col"),
        a2ui::column("form-col", &["title", "name-field", "password-field", "btn-row"], 16),
        a2ui::text("title", "Create Account", "h2"),
        a2ui::text_field("name-field", "Name", "/name", false),
        a2ui::text_field("password-field", "Password", "/password", true),
        a2ui::row("btn-row", &["submit-btn", "reset-btn"], 12),
    ];
    components.extend(a2ui::button(
        "submit-btn",
        "btn-label",
        "Register",
        "register",
        json!({ "name": { "path": "/name" }, "password": { "path": "/password" } }),
    ));
    components.push(a2ui::reset_button(
        "reset-btn",
        "Reset",
        REGISTER_SURFACE_ID,
        register_form_data(),
    ));

    a2ui::render(vec![
        a2ui::create_surface(REGISTER_SURFACE_ID, crate::agent::config::DEFAULT_CATALOG_ID),
        a2ui::update_components(REGISTER_SURFACE_ID, components),
        a2ui::update_data_model(REGISTER_SURFACE_ID, register_form_data()),
    ])
}

/// `render_a2ui(surfaceId, components, data)` — the generic A2UI renderer.
///
/// The Python stack reached A2UI through a `generate_a2ui` **sub-agent** that was
/// handed the catalog schema and asked to produce components. There is no second
/// model call here: the main agent composes the components (it receives the
/// catalog guidelines from
/// [`crate::agent::runtime::middleware::A2uiMiddleware`]) and this tool turns them
/// into operations. That removes a model round-trip and the schema-drift it
/// caused, at the cost of the main model writing the components itself.
pub struct RenderA2ui;

impl Tool for RenderA2ui {
    fn name(&self) -> &'static str {
        "render_a2ui"
    }

    fn description(&self) -> &'static str {
        "Render A2UI components in the chat. Use this for dashboards, metrics, \
         charts, tables, status reports and cards that the specific tools do not cover. \
         Pass the component definitions using the A2UI v0.9 schema from the catalog \
         guidelines: every component is a flat object with a unique `id` and a \
         `component` name, and children are referenced by id. \
         Never invent component names — only use names from the catalog."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "surfaceId": {
                    "type": "string",
                    "description": "Identifier for the surface. Reuse an id to update an existing surface."
                },
                "components": {
                    "type": "array",
                    "description": "A2UI components, each a flat object with `id` and `component`.",
                    "items": { "type": "object" }
                },
                "data": {
                    "type": "object",
                    "description": "Optional data model for the surface, referenced from components via {\"path\": \"/…\"}."
                }
            },
            "required": ["surfaceId", "components"],
            "additionalProperties": false
        })
    }

    fn call(&self, args: &Value) -> Result<ToolOutcome, String> {
        let surface_id = required_str(args, "surfaceId")?;

        let components = args
            .get("components")
            .and_then(Value::as_array)
            .ok_or_else(|| "'components' must be an array of A2UI components".to_string())?
            .clone();
        if components.is_empty() {
            return Err("'components' must not be empty".to_string());
        }
        for (i, c) in components.iter().enumerate() {
            if !c.is_object() {
                return Err(format!("component {i} must be an object"));
            }
            if c.get("id").and_then(Value::as_str).is_none() {
                return Err(format!("component {i} is missing a string 'id'"));
            }
            if c.get("component").and_then(Value::as_str).is_none() {
                return Err(format!("component {i} is missing a string 'component' name"));
            }
        }

        let data = args.get("data").cloned().unwrap_or_else(|| json!({}));

        let mut operations = vec![
            a2ui::create_surface(&surface_id, crate::agent::config::DEFAULT_CATALOG_ID),
            a2ui::update_components(&surface_id, components),
        ];
        if data.is_object() {
            operations.push(a2ui::update_data_model(&surface_id, data));
        }

        Ok(ToolOutcome::with_a2ui(
            format!("Rendered {} component(s) on surface '{surface_id}'.", operations.len()),
            a2ui::render(operations),
        ))
    }
}

/// Read a required string argument, with an error the model can act on.
fn required_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing required string argument '{key}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_matches_the_python_response() {
        let out = GetWeather.call(&json!({ "location": "Beijing" })).unwrap();
        assert_eq!(out.content, "The weather in Beijing is sunny with 22°C.");
        assert!(out.a2ui.is_none());
    }

    #[test]
    fn calculate_reports_errors_as_results_not_failures() {
        let ok = Calculate.call(&json!({ "expression": "(2 + 3) * 4" })).unwrap();
        assert_eq!(ok.content, "Result: 20");

        let bad = Calculate.call(&json!({ "expression": "import os" })).unwrap();
        assert!(bad.content.starts_with("Error: "), "{}", bad.content);
    }

    #[test]
    fn missing_arguments_produce_actionable_errors() {
        let err = GetWeather.call(&json!({})).unwrap_err();
        assert!(err.contains("location"), "{err}");
    }

    #[test]
    fn register_user_confirms_without_echoing_the_password() {
        let out = RegisterUser
            .call(&json!({ "name": "Ada", "password": "secret" }))
            .unwrap();
        assert_eq!(out.content, "User 'Ada' registered successfully!");
        assert!(!out.content.contains("secret"));
    }

    #[test]
    fn register_form_renders_the_python_component_set() {
        let out = DisplayRegisterForm.call(&json!({})).unwrap();
        let ops = out.a2ui.expect("the form must produce A2UI operations");
        assert_eq!(ops[0]["createSurface"]["surfaceId"], REGISTER_SURFACE_ID);

        let components = ops[1]["updateComponents"]["components"].as_array().unwrap();
        let ids: Vec<&str> = components.iter().filter_map(|c| c["id"].as_str()).collect();
        for expected in ["root", "form-col", "title", "name-field", "password-field", "btn-row", "submit-btn", "btn-label", "reset-btn"] {
            assert!(ids.contains(&expected), "missing component '{expected}' in {ids:?}");
        }
        let password = components.iter().find(|c| c["id"] == "password-field").unwrap();
        assert_eq!(password["variant"], "obscured");
        assert_eq!(ops[2]["updateDataModel"]["data"]["name"], "");
    }

    #[test]
    fn render_a2ui_validates_components() {
        let ok = RenderA2ui
            .call(&json!({
                "surfaceId": "dash",
                "components": [{ "id": "root", "component": "Card", "child": "m" }],
                "data": { "total": 1 }
            }))
            .unwrap();
        let ops = ok.a2ui.unwrap();
        assert_eq!(ops[1]["updateComponents"]["surfaceId"], "dash");
        assert_eq!(ops[2]["updateDataModel"]["data"]["total"], 1);

        assert!(RenderA2ui.call(&json!({ "surfaceId": "d", "components": [] })).is_err());
        assert!(RenderA2ui
            .call(&json!({ "surfaceId": "d", "components": [{ "component": "Card" }] }))
            .unwrap_err()
            .contains("id"));
    }
}
