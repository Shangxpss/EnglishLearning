//! The system prompt, ported from `AI-Demo/backend/agent.py`.
//!
//! Two deliberate changes from the Python original:
//!
//! * the A2UI component list is generated from [`super::super::protocol::a2ui::component`]
//!   where practical, so a renamed component cannot silently drift out of the prompt;
//! * `generate_a2ui` (a sub-agent) is replaced by the [`render_a2ui`] tool, which the
//!   main agent calls with components it composes itself.
//!
//! [`render_a2ui`]: super::super::tools::builtin::RenderA2ui

use crate::agent::config::AgentConfig;

/// The full system prompt for a run.
pub fn system_prompt(cfg: &AgentConfig) -> String {
    format!(
        "{HEADER}\n\n\
         Tool guidance:\n\
         - Weather: call get_weather to get weather data as text.\n\
         - Registration: call display_register_form to show a registration form with \
         name and password fields. When the user clicks the Register button, you will \
         receive an action event named \"register\" with context containing name and \
         password. Then call register_user(name, password) to complete the registration. \
         The Reset button clears the form locally — no action event is sent for it.\n\
         - Dashboards & rich UI: call render_a2ui to create dashboards with metrics, \
         charts, status reports, and cards. It handles rendering automatically. Use this \
         whenever the user asks for a visual that isn't covered by the specific tools above.\n\
         - Calculations: call calculate for arithmetic expressions.\n\
         - Keep chat replies to 1-2 sentences and let the UI do the talking.\n\n\
         A2UI Component Usage:\n\
         When using render_a2ui, you MUST use the v0.9 A2UI component schema. \
         The available components and their exact props are provided in the schema \
         automatically — only use components defined in the schema, never invent new ones.\n\n\
         Available A2UI components (ONLY use these — do NOT invent names like \"Title\", \"Header\", \"Paragraph\"):\n\
         - Layout: Row, Column, List, Card\n\
         - Display: Text (use variant: h1/h2/h3/h4/h5/caption/body), Image, Icon, Divider\n\
         - Interactive: Button, TextField, CheckBox, ChoicePicker, Slider, DateTimeInput\n\
         - Container: Tabs, Modal\n\
         - Media: Video, AudioPlayer\n\
         - Custom: Heading, Grid, Table, KeyValueList, StatusBadge, Metric, InfoRow\n\n\
         For titles/headings, use Text with variant=\"h1\"/\"h2\"/\"h3\" or the Heading component.\n\
         NEVER use \"Title\" or \"Header\" — they do not exist in the catalog.\n\n\
         A2UI Actions:\n\
         - When you receive an action event (e.g. \"register\" with context), call the \
         corresponding tool (e.g. register_user) to process it.\n\n\
         Example: If the action is \"selectCoffee\" with context {{\"coffee\": \"曼特宁\"}}, \
         you should reply something like \"Great choice! 曼特宁 is a bold, full-bodied \
         coffee with herbal and dark chocolate notes.\" and optionally show a detail card \
         with render_a2ui.\n\n\
         Runtime: model '{model}', catalog '{catalog}'.",
        HEADER = HEADER,
        model = cfg.model,
        catalog = cfg.catalog_id,
    )
}

/// The persona line, kept separate so it is easy to reword.
const HEADER: &str = "You are the EnglishPro assistant, embedded in a sentence-segmented \
video player. When the user has a video open, its full transcript is supplied as ambient \
context: answer questions about what the video says by quoting that transcript and its \
timestamps. You can also check the weather, perform calculations, and display rich UI cards.";

/// AG-UI ambient-context label under which the frontend publishes the video the
/// user currently has open.
///
/// Must match `VIDEO_TRANSCRIPT_CONTEXT` in
/// `frontend/web/src/features/player/index.tsx`: the player registers the
/// transcript through `useAgentContext`, CopilotKit forwards it in the run's
/// `context` array, and [`crate::agent::runtime`] picks it back out by this
/// exact description.
pub const VIDEO_TRANSCRIPT_CONTEXT: &str = "video_transcript";

/// The instruction block that hands the model the transcript of the open video.
///
/// Without it the model only ever sees the chat text, so "what does he say
/// about X?" would be unanswerable — the transcript is the whole point of
/// chatting from the player page.
pub fn video_transcript_prompt(transcript: &str) -> String {
    format!(
        "The user is watching a video. Its full transcript follows — one line per \
         sentence, with the sentence index and start/end timestamps. Ground every \
         answer about the video in this transcript, and quote the exact sentence and \
         timestamp you are relying on.\n\n\
         <video_transcript>\n{transcript}\n</video_transcript>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::DEFAULT_CATALOG_ID;

    #[test]
    fn prompt_names_the_model_and_catalog() {
        let cfg = AgentConfig::default();
        let prompt = system_prompt(&cfg);
        assert!(prompt.contains("deepseek-chat"));
        assert!(prompt.contains(DEFAULT_CATALOG_ID));
    }

    #[test]
    fn prompt_forbids_the_components_that_do_not_exist() {
        let cfg = AgentConfig::default();
        let prompt = system_prompt(&cfg);
        assert!(prompt.contains("NEVER use \"Title\" or \"Header\""));
        assert!(prompt.contains("render_a2ui"));
        // The sub-agent tool is gone, so it must not be advertised.
        assert!(!prompt.contains("generate_a2ui"));
    }

    #[test]
    fn the_video_transcript_prompt_carries_the_transcript_verbatim() {
        let block = video_transcript_prompt("[1] 00:00.000–00:02.000 Hello there.");
        assert!(block.contains("[1] 00:00.000–00:02.000 Hello there."));
        assert!(block.contains("<video_transcript>"));
    }
}
