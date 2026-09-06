import { createCatalog } from "@copilotkit/a2ui-renderer";

import { customDefinitions } from "./definitions";
import { myRenderers } from "./renderers";

// ─── Build catalog ────────────────────────────────────────────────────
// createCatalog with includeBasicCatalog: true merges the 18 built-in
// components (Text, Row, Column, Card, List, Button, Divider, Tabs,
// Modal, Image, Icon, Video, AudioPlayer, TextField, CheckBox,
// ChoicePicker, Slider, DateTimeInput) with our custom definitions.
//
// The A2UIMiddleware on the runtime side injects the component schema into
// the LLM context (via includeSchema + injectA2UITool), so the LLM knows
// the exact component definitions — no need for frontend-level fallback
// components or schema overrides.

export const myCatalog = createCatalog(
  customDefinitions,
  myRenderers,
  {
    catalogId: "generative-agent-catalog",
    includeBasicCatalog: true,
  },
);
