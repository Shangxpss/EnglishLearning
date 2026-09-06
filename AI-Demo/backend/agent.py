from langchain.agents import create_agent
from langchain_deepseek import ChatDeepSeek
from langchain_core.tools import tool
from langgraph.checkpoint.memory import MemorySaver
from copilotkit import CopilotKitMiddleware
from copilotkit.a2ui import render, create_surface, update_components, update_data_model
from dotenv import load_dotenv
import logging

# Enable A2UI debug logging — force=True to override uvicorn's logging config
logging.basicConfig(level=logging.DEBUG, force=True)
for _name in ["ag_ui_langgraph.a2ui_tool", "ag_ui_a2ui_toolkit.recovery", "copilotkit.middleware.a2ui"]:
    logging.getLogger(_name).setLevel(logging.DEBUG)

load_dotenv()

CATALOG_ID = "generative-agent-catalog"
REGISTER_SURFACE_ID = "register-form"


# --- Basic tools ---

@tool
def get_weather(location: str) -> str:
    """Get weather for a location. Returns weather data as text.
    If you want to display a rich weather card, call display_weather instead."""
    return f"The weather in {location} is sunny with 22°C."


@tool
def calculate(expression: str) -> str:
    """Calculate a mathematical expression."""
    try:
        result = eval(expression)
        return f"Result: {result}"
    except Exception as e:
        return f"Error: {e}"


@tool
def register_user(name: str, password: str) -> str:
    """Register a new user with the given name and password.
    Called when the user submits the registration form.
    Logs the registration and returns a confirmation message.

    Args:
        name: The user's name from the registration form.
        password: The user's password from the registration form.
    """
    logging.getLogger("agent").info(
        "User registered: name=%s, password=%s", name, password)
    return f"User '{name}' registered successfully!"


REGISTER_FORM_DATA = {"name": "", "password": ""}

REGISTER_FORM_COMPONENTS = [
    {
        "id": "root",
        "component": "Card",
        "child": "form-col",
    },
    {
        "id": "form-col",
        "component": "Column",
        "children": ["title", "name-field", "password-field", "btn-row"],
        "gap": 16,
    },
    {
        "id": "title",
        "component": "Text",
        "text": "Create Account",
        "variant": "h2",
    },
    {
        "id": "name-field",
        "component": "TextField",
        "label": "Name",
        "value": {"path": "/name"},
    },
    {
        "id": "password-field",
        "component": "TextField",
        "label": "Password",
        "value": {"path": "/password"},
        "variant": "obscured",
    },
    {
        "id": "btn-row",
        "component": "Row",
        "children": ["submit-btn", "reset-btn"],
        "gap": 12,
    },
    {
        "id": "submit-btn",
        "component": "Button",
        "child": "btn-label",
        "variant": "primary",
        "action": {
            "event": {
                "name": "register",
                "context": {
                    "name": {"path": "/name"},
                    "password": {"path": "/password"},
                },
            },
        },
    },
    {
        "id": "btn-label",
        "component": "Text",
        "text": "Register",
    },
    {
        "id": "reset-btn",
        "component": "ResetButton",
        "label": "Reset",
        "surfaceId": REGISTER_SURFACE_ID,
        "data": REGISTER_FORM_DATA,
    },
]


@tool
def display_register_form() -> str:
    """Show a registration form with name and password fields and a submit button.
    Use this when the user wants to register a new user.
    After this tool returns, the form is already rendered — do NOT call it again.
    When the user clicks the Register button, you will receive an action event
    with the form data. Then call register_user to complete the registration.
    """
    return render(
        operations=[
            create_surface(REGISTER_SURFACE_ID, catalog_id=CATALOG_ID),
            update_components(REGISTER_SURFACE_ID,
                              REGISTER_FORM_COMPONENTS),
            update_data_model(REGISTER_SURFACE_ID, REGISTER_FORM_DATA),
        ],
    )


llm = ChatDeepSeek(
    model="deepseek-chat",
    temperature=0,
)

SYSTEM_PROMPT = """\
You are a helpful AI assistant powered by DeepSeek. You can check the weather, \
perform calculations, and display rich UI cards.

Tool guidance:
- Weather: call get_weather to get weather data as text.
- Registration: call display_register_form to show a registration form with \
name and password fields. When the user clicks the Register button, you will \
receive an action event named "register" with context containing name and \
password. Then call register_user(name, password) to complete the registration. \
The Reset button clears the form locally — no action event is sent for it.
- Dashboards & rich UI: call generate_a2ui to create dashboards with metrics, \
charts, status reports, and cards. It handles rendering automatically. Use this \
whenever the user asks for a visual that isn't covered by the specific tools above.
- Calculations: call calculate for math expressions.
- Keep chat replies to 1-2 sentences and let the UI do the talking.

A2UI Component Usage:
When using generate_a2ui, you MUST use the v0.9 A2UI component schema. \
The available components and their exact props are provided in the schema \
automatically — only use components defined in the schema, never invent new ones.

Available A2UI components (ONLY use these — do NOT invent names like "Title", "Header", "Paragraph"):
- Layout: Row, Column, List, Card
- Display: Text (use variant: h1/h2/h3/h4/h5/caption/body), Image, Icon, Divider
- Interactive: Button, TextField, CheckBox, ChoicePicker, Slider, DateTimeInput
- Container: Tabs, Modal
- Media: Video, AudioPlayer
- Custom: Heading, Grid, Table, KeyValueList, StatusBadge, Metric, InfoRow

For titles/headings, use Text with variant="h1"/"h2"/"h3" or the Heading component.
NEVER use "Title" or "Header" — they do not exist in the catalog.

A2UI Actions:
- When you receive an action event (e.g. "register" with context), call the \
corresponding tool (e.g. register_user) to process it.

Example: If the action is "selectCoffee" with context {"coffee": "曼特宁"}, \
you should reply something like "Great choice! 曼特宁 is a bold, full-bodied \
coffee with herbal and dark chocolate notes." and optionally show a detail card \
with generate_a2ui.
"""

agent = create_agent(
    model=llm,
    tools=[get_weather, calculate, register_user,
           display_register_form],
    middleware=[CopilotKitMiddleware()],
    system_prompt=SYSTEM_PROMPT,
    checkpointer=MemorySaver(),
)
