# Plan: Reading Page + LangChain Story Generation

## Summary
Build a "Reading" page that extracts unfamiliar words from a user's reading session and sends them to LangChain to generate a creative story using those words. Focus on a modular design: backend APIs to extract/store words, a LangChain integration service, and a frontend reading UI that displays unfamiliar words and the generated story.

## Goals
- Detect and store user unfamiliar words during reading sessions
- Provide an endpoint to request a LangChain-generated story built from those words
- Present a clear, accessible reading UI with controls to mark words unfamiliar
- Keep components testable and generator-friendly for CLI scaffolding

## Non-goals
- Do not implement code in this plan phase
- No production-scale ML tuning; use LangChain orchestration and prompt templates

## High-level approach
1. Capture reading text and user's selections in frontend.
2. Send text or selected words to backend API.
3. Backend extracts words, cross-checks user vocab list, persists unfamiliar words.
4. LangChain service receives unfamiliar words + user/context metadata and returns a creative story.
5. Frontend displays story with interactive features (play audio, save words).

## Data model (proposal)
- unfamiliar_words
  - id (uuid)
  - user_id (uuid)
  - word (text)
  - context_snippet (text)
  - source (reading_session_id or url)
  - created_at (timestamp)
  - reviewed (bool)

- reading_session
  - id, user_id, content, started_at, finished_at

## Backend responsibilities
- API: POST /reading_sessions -> create session with content
- API: POST /reading_sessions/{id}/mark_word -> mark word unfamiliar (body: {word, snippet})
- API: GET /users/{id}/unfamiliar_words -> list words (filter reviewed)
- API: POST /stories -> request story generation (body: {words: [str], tone, length})
- Service: langchain_agent.py implementing get_agent(), start/stop hooks, generate_story(words, options)
- Persistence: lightweight DB migrations or ORM models in backend/app/models/

## LangChain integration
- Wrap LangChain prompt templates in a service with deterministic interface:
  - generate_story(words: List[str], tone: str, length: str) -> {text, tokens_used}
- Keep prompt templates in a single file for easy updates and testing
- Include safe-guards for profanity and length; return summarized metadata

## Frontend responsibilities
- Reading page component: displays content, lets users tap/select unfamiliar words
- Side panel: shows collected unfamiliar words, with options (remove, mark reviewed, send to story)
- Story viewer: shows generated story, allows saving to user library and TTS playback
- UI wireframe notes for generator: component names and file paths
  - frontend/web/src/pages/ReadingPage.tsx
  - frontend/web/src/components/WordPanel.tsx
  - frontend/web/src/components/StoryViewer.tsx

## Security & privacy
- Only store words linked to user_id; comply with privacy settings
- Rate-limit story generation API per user
- Sanitize prompts to avoid leaking user-sensitive content

## Testing plan
- Unit tests for word extraction and LangChain prompt construction
- Integration tests: TestClient hitting /stories with mocked LangChain client
- Frontend: component tests for ReadingPage interactions

## Acceptance criteria
- Users can mark unfamiliar words during a reading session
- Backend persists unfamiliar words and returns them via API
- LangChain service returns a coherent story containing the given words
- Frontend displays story and allows saving words

## Milestones / Todos
- reading-page: Create ReadingPage UI, selection UX, word panel
- backend-api: Add reading session endpoints and models
- langchain-service: Implement LangChain wrapper and agent module
- persistence: Add unfamiliar_words model and migrations
- tests-ci: Add unit and integration tests + CI job

## Developer notes for CLI scaffolding
- Agent filename: backend/app/services/langchain_agent.py
- Agent class: LangChainAgent with get_agent() factory
- Router prefix: /agents/langchain
- Models under backend/app/models/langchain_models.py
- Test file: tests/agents/test_langchain_agent.py
- Insert import marker comment in backend/app/api/register_agents.py for generators to append new routers

## Risks & mitigations
- Prompt drift: iterate on templates and add unit tests asserting prompt content
- Cost of generation: add token limit and rate limits
- Bad words in outputs: include filtering post-processing step

## Next steps (after plan approval)
- Scaffold backend agent and endpoints
- Scaffold frontend pages and components
- Implement persistence schema and run migrations
- Integrate LangChain and create prompt templates


---

Created for implementation planning. Update this plan before starting development to reflect priorities or constraints.

## Progress update
- Status: implementation started and initial scaffold completed (mocked LangChain). Key artifacts created:
  - backend/app/services/langchain_agent.py (mocked generator)
  - backend/app/services/langchain_prompts.py (prompt templates)
  - backend/app/models/langchain_models.py
  - backend/app/models/reading_models.py
  - backend/app/main.py (endpoints: /stories, /reading_sessions, /reading_sessions/{id}/mark_word, /users/{id}/unfamiliar_words)
  - frontend/web/src/pages/ReadingPage.tsx
  - frontend/web/src/components/WordPanel.tsx
  - frontend/web/src/components/StoryViewer.tsx
  - tests/agents/test_langchain_agent.py
  - scripts/create_tables.py

## Completed subtasks
- reading-page: scaffolded UI and components (double-click selection, word panel, story viewer)
- backend-api: added endpoints for reading sessions, marking words, listing user words, and story generation
- langchain-service: added agent wrapper and prompt templates (mocked implementation)
- persistence: added ReadingSession model and DB table creation script
- tests: added basic unit test skeleton for LangChain agent

## Next steps
1. Run database table creation: `python scripts/create_tables.py` (requires DATABASE_URL env)
2. Implement real LangChain / OpenAI integration in `langchain_agent.generate_story` and add configuration for API keys
3. Add migrations and more robust models (unfamiliar_words table, associations)
4. Add authentication wiring on frontend and persist reading_session IDs
5. Add unit and integration tests; configure CI job

## Notes for implementers
- Current LangChain implementation is mocked to avoid requiring API keys in CI. Replace `generate_story` with a LangChain client call reading OPENAI_API_KEY from env.
- Generator-friendly conventions were followed for filenames and factories (get_agent()).

---

