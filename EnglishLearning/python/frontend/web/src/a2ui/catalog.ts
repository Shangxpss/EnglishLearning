import { createCatalog } from "@copilotkit/a2ui-renderer";

import { customDefinitions } from "./definitions";
import { myRenderers } from "./renderers";

/**
 * A2UI catalog for the EnglishPro assistant.
 *
 * Mirrors AI-Demo/front/src/a2ui/catalog.ts. `createCatalog` with
 * `includeBasicCatalog: true` merges the 18 built-in components
 * (Text, Row, Column, Card, List, Button, Divider, Tabs, Modal, Image,
 * Icon, Video, AudioPlayer, TextField, CheckBox, ChoicePicker, Slider,
 * DateTimeInput) with our custom English-learning definitions.
 *
 * The catalogId MUST match the `defaultCatalogId` configured on the
 * agent-runtime (see agent-runtime/index.ts) and the backend
 * `CATALOG_ID` (see backend/app/services/copilotkit_agent.py) so that
 * server-side A2UI surfaces pair with the client-side renderers.
 *
 * The A2UIMiddleware on the runtime side injects the component schema
 * into the LLM context (via includeSchema + injectA2UITool), so the LLM
 * knows the exact component definitions — no need for frontend-level
 * fallback components or schema overrides.
 */
export const myCatalog = createCatalog(customDefinitions, myRenderers, {
  catalogId: "english-learning-catalog",
  includeBasicCatalog: true,
});
