"""CopilotKit LangGraph agent for the English Learning application.

This module integrates the AI-Demo's CopilotKit + LangGraph + DeepSeek stack
into the English Learning backend. It exposes English-learning-specific tools
(story generation, vocabulary management, word cards, dashboards) that the
agent can call, and renders rich UI surfaces via A2UI.

Architecture (mirrors AI-Demo/backend/agent.py + main.py):
    Frontend (CopilotChat) → agent-runtime (Bun, /copilotkit)
        → this LangGraph agent (FastAPI, mounted at /copilotkit-agent)

The agent is mounted as a LangGraphAGUIAgent endpoint so the
CopilotKit runtime can stream events (tool calls, A2UI surfaces,
messages) back to the frontend.
"""

from __future__ import annotations

import logging
import os
from typing import Annotated, NotRequired

from copilotkit import CopilotKitMiddleware, a2ui
from copilotkit.copilotkit_lg_middleware import StateSchema
from dotenv import load_dotenv
from langchain.agents import create_agent
from langchain_core.tools import tool
from langchain_deepseek import ChatDeepSeek
from langgraph.channels.last_value import LastValue
from langgraph.checkpoint.memory import MemorySaver

from app.core.config import settings
from app.services.langchain_agent import LangChainAgent

load_dotenv()

logger = logging.getLogger(__name__)

# Catalog id must match the frontend's createCatalog({ catalogId: ... }) call
# so the A2UIMiddleware can pair server-side surfaces with client renderers.
CATALOG_ID = "english-learning-catalog"

# Surface ids used by the A2UI tools below. Keeping them as constants avoids
# typos between the tool that creates a surface and the one that updates it.
STORY_SURFACE_ID = "story-surface"
VOCAB_SURFACE_ID = "vocabulary-surface"
WORD_SURFACE_ID = "word-surface"


class AgentStateSchema(StateSchema):
    """LangGraph state schema.

    `ag_ui` is required for CopilotKitMiddleware to receive the A2UI
    configuration (inject_a2ui_tool, a2ui_schema) from the runtime.
    The hyphen/underscore mismatch is handled by A2UIFixedAgent in main.py.
    """

    ag_ui: NotRequired[Annotated[dict, LastValue]]  # type: ignore[misc]
    tools: NotRequired[Annotated[list, LastValue]]  # type: ignore[misc]


class EnglishLearningMiddleware(CopilotKitMiddleware):
    """CopilotKit middleware wired to the English Learning state schema."""

    state_schema = AgentStateSchema


# ---------------------------------------------------------------------------
# Tool: generate_story
# ---------------------------------------------------------------------------
# Wraps the existing LangChainAgent story generator so the conversational
# agent can produce a story from a list of words. The result is also rendered
# as an A2UI story card so the user sees formatted output, not raw text.

_story_agent = LangChainAgent()


@tool
async def generate_story(words: list[str], tone: str = "fantastic", length: str = "short") -> str:
    """Generate a short story that contains the given English words.

    Use this when the learner wants to practice vocabulary in context.
    The story is rendered as a rich card automatically.

    Args:
        words: List of English words to include in the story.
        tone: Story tone (fantastic, educational, humorous, mysterious...).
        length: Story length (short, medium, long).
    """
    if not _story_agent.initialized:
        await _story_agent.start()

    result = await _story_agent.generate_story(words, tone=tone, length=length)
    story_text = result.get("text", "")

    return a2ui.render(
        operations=[
            a2ui.create_surface(STORY_SURFACE_ID, catalog_id=CATALOG_ID),
            a2ui.update_components(
                STORY_SURFACE_ID,
                [
                    {"id": "root", "component": "Card", "child": "story-col"},
                    {
                        "id": "story-col",
                        "component": "Column",
                        "children": ["story-title", "story-meta", "story-body"],
                        "gap": 12,
                    },
                    {
                        "id": "story-title",
                        "component": "Text",
                        "text": "Generated Story",
                        "variant": "h2",
                    },
                    {
                        "id": "story-meta",
                        "component": "InfoRow",
                        "label": "Words",
                        "value": ", ".join(words),
                    },
                    {
                        "id": "story-body",
                        "component": "Text",
                        "text": story_text,
                    },
                ],
            ),
            a2ui.update_data_model(STORY_SURFACE_ID, {}),
        ],
    )


# ---------------------------------------------------------------------------
# Tool: display_word_card
# ---------------------------------------------------------------------------
# Shows a single word with its definition, example, and familiarity badge.
# Demonstrates the custom WordCard A2UI component defined on the frontend.

WORD_CARD_COMPONENTS = [
    {"id": "root", "component": "Card", "child": "word-col"},
    {
        "id": "word-col",
        "component": "Column",
        "children": ["word-header", "word-definition", "word-example", "word-badge"],
        "gap": 10,
    },
    {
        "id": "word-header",
        "component": "Text",
        "text": "",  # filled in dynamically
        "variant": "h2",
    },
    {
        "id": "word-definition",
        "component": "Text",
        "text": "",
    },
    {
        "id": "word-example",
        "component": "Text",
        "text": "",
    },
    {
        "id": "word-badge",
        "component": "StatusBadge",
        "text": "New",
        "variant": "info",
    },
]


@tool
def display_word_card(word: str, definition: str, example: str, familiarity: str = "new") -> str:
    """Display a rich word card with definition, example, and familiarity badge.

    Use this when the learner asks about a specific word's meaning or usage.

    Args:
        word: The English word to display.
        definition: A short definition of the word.
        example: An example sentence using the word.
        familiarity: One of "new", "learning", "familiar", "mastered".
    """
    variant_map = {
        "new": "info",
        "learning": "warning",
        "familiar": "info",
        "mastered": "success",
    }
    components = [dict(c) for c in WORD_CARD_COMPONENTS]
    components[2]["text"] = word
    components[3]["text"] = f"Definition: {definition}"
    components[4]["text"] = f"Example: {example}"
    components[5]["text"] = familiarity.capitalize()
    components[5]["variant"] = variant_map.get(familiarity, "info")

    return a2ui.render(
        operations=[
            a2ui.create_surface(WORD_SURFACE_ID, catalog_id=CATALOG_ID),
            a2ui.update_components(WORD_SURFACE_ID, components),
            a2ui.update_data_model(WORD_SURFACE_ID, {}),
        ],
    )


# ---------------------------------------------------------------------------
# Tool: display_vocabulary_dashboard
# ---------------------------------------------------------------------------
# Renders a dashboard summarising the learner's saved vocabulary. Demonstrates
# the Metric, BarChart, and Table A2UI components.

VOCAB_DASHBOARD_COMPONENTS = [
    {"id": "root", "component": "Card", "child": "vocab-col"},
    {
        "id": "vocab-col",
        "component": "Column",
        "children": [
            "vocab-title",
            "vocab-metrics-row",
            "vocab-chart",
            "vocab-table",
        ],
        "gap": 16,
    },
    {"id": "vocab-title", "component": "Text", "text": "Vocabulary Dashboard", "variant": "h2"},
    {
        "id": "vocab-metrics-row",
        "component": "Row",
        "children": ["metric-total", "metric-learning", "metric-mastered"],
        "gap": 24,
    },
    {"id": "metric-total", "component": "Metric", "label": "Total Words", "value": "0", "trend": "neutral"},
    {"id": "metric-learning", "component": "Metric", "label": "Learning", "value": "0", "trend": "up"},
    {"id": "metric-mastered", "component": "Metric", "label": "Mastered", "value": "0", "trend": "up"},
    {
        "id": "vocab-chart",
        "component": "BarChart",
        "title": "Words by Familiarity",
        "bars": [],
    },
    {
        "id": "vocab-table",
        "component": "Table",
        "caption": "Recent Words",
        "headers": ["Word", "Familiarity", "Score"],
        "rows": [],
    },
]


@tool
def display_vocabulary_dashboard(words: list[dict]) -> str:
    """Display a dashboard summarising the learner's saved vocabulary.

    Use this when the learner asks for an overview of their progress.

    Args:
        words: List of word objects, each with keys "word", "familiarity",
            and "score". Example:
            [{"word": "ephemeral", "familiarity": "learning", "score": 2}, ...]
    """
    total = len(words)
    learning = sum(1 for w in words if w.get("familiarity") in ("learning", "new", "unfamiliar"))
    mastered = sum(1 for w in words if w.get("familiarity") in ("mastered", "familiar"))

    components = [dict(c) for c in VOCAB_DASHBOARD_COMPONENTS]
    components[5]["value"] = str(total)
    components[6]["value"] = str(learning)
    components[7]["value"] = str(mastered)
    components[8]["bars"] = [
        {"label": "New", "value": sum(1 for w in words if w.get("familiarity") == "new"), "color": "blue"},
        {"label": "Learning", "value": sum(1 for w in words if w.get("familiarity") == "learning"), "color": "amber"},
        {"label": "Familiar", "value": sum(1 for w in words if w.get("familiarity") == "familiar"), "color": "emerald"},
        {"label": "Mastered", "value": sum(1 for w in words if w.get("familiarity") == "mastered"), "color": "emerald"},
    ]
    components[9]["rows"] = [
        {
            "Word": w.get("word", ""),
            "Familiarity": w.get("familiarity", "-"),
            "Score": str(w.get("score", 0)),
        }
        for w in words[:10]
    ]

    return a2ui.render(
        operations=[
            a2ui.create_surface(VOCAB_SURFACE_ID, catalog_id=CATALOG_ID),
            a2ui.update_components(VOCAB_SURFACE_ID, components),
            a2ui.update_data_model(VOCAB_SURFACE_ID, {}),
        ],
    )


# ---------------------------------------------------------------------------
# Tool: explain_grammar
# ---------------------------------------------------------------------------
# A lightweight grammar explainer that returns a formatted card. Keeps the
# conversational agent useful even when no external services are wired up.

@tool
def explain_grammar(topic: str, explanation: str, examples: list[str]) -> str:
    """Explain a grammar topic with examples, rendered as a card.

    Use this when the learner asks about tenses, articles, conditionals, etc.

    Args:
        topic: The grammar topic (e.g. "Present Perfect").
        explanation: A short explanation of the topic.
        examples: A list of example sentences illustrating the topic.
    """
    example_items = [
        {"label": f"Example {i + 1}", "value": ex} for i, ex in enumerate(examples)
    ]
    return a2ui.render(
        operations=[
            a2ui.create_surface("grammar-surface", catalog_id=CATALOG_ID),
            a2ui.update_components(
                "grammar-surface",
                [
                    {"id": "root", "component": "Card", "child": "grammar-col"},
                    {
                        "id": "grammar-col",
                        "component": "Column",
                        "children": ["grammar-title", "grammar-explanation", "grammar-examples"],
                        "gap": 12,
                    },
                    {"id": "grammar-title", "component": "Text", "text": topic, "variant": "h2"},
                    {"id": "grammar-explanation", "component": "Text", "text": explanation},
                    {"id": "grammar-examples", "component": "KeyValueList", "title": "Examples", "items": example_items},
                ],
            ),
            a2ui.update_data_model("grammar-surface", {}),
        ],
    )


# ---------------------------------------------------------------------------
# Build the LangGraph agent
# ---------------------------------------------------------------------------

def _build_llm() -> ChatDeepSeek:
    api_key = settings.DEEPSEEK_API_KEY or os.getenv("DEEPSEEK_API_KEY")
    if not api_key:
        logger.warning(
            "DEEPSEEK_API_KEY is not set; the CopilotKit agent will fail to "
            "call the LLM. Set it in .env to enable the assistant."
        )
    return ChatDeepSeek(model="deepseek-chat", temperature=0)


SYSTEM_PROMPT = """\
You are EnglishPro Assistant, an AI tutor integrated into the EnglishPro \
English-learning app. You help learners practise vocabulary, generate stories \
from their words, explain grammar, and review their progress.

Tool guidance:
- Story generation: call generate_story when the learner wants a story that \
uses a list of words. Pass the words, an optional tone, and an optional length.
- Word explanations: call display_word_card to show a rich card with a word's \
definition, example, and familiarity badge.
- Vocabulary overview: call display_vocabulary_dashboard to render a dashboard \
with metrics, a bar chart, and a table of the learner's saved words.
- Grammar: call explain_grammar to render a card explaining a grammar topic \
with examples.
- Dashboards & rich UI: call generate_a2ui to create ad-hoc visuals that aren't \
covered by the specific tools above. It handles rendering automatically.

A2UI Component Usage:
When using generate_a2ui, you MUST use the v0.9 A2UI component schema. \
The available components and their exact props are provided in the schema \
automatically — only use components defined in the schema, never invent new ones.

Keep chat replies to 1-2 sentences and let the UI do the talking.
"""


def build_agent():
    """Build and return the LangGraph agent used by the CopilotKit endpoint."""
    llm = _build_llm()
    return create_agent(
        model=llm,
        tools=[
            generate_story,
            display_word_card,
            display_vocabulary_dashboard,
            explain_grammar,
        ],
        middleware=[EnglishLearningMiddleware()],
        system_prompt=SYSTEM_PROMPT,
        checkpointer=MemorySaver(),
    )


# Module-level singleton, mirroring AI-Demo/backend/agent.py.
agent = build_agent()
