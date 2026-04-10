# Feature Plan: Sentence-by-Sentence Listening Practice

## 1. Overview
This document outlines the implementation plan for a new feature allowing users to practice listening by clicking on individual subtitle sentences to play their corresponding audio segments instantly. This builds upon the existing monorepo structure (FastAPI backend + React/Vite frontend).

## 2. User Story
> "As a language learner, I want to see a list of all subtitle sentences. When I click a sentence, I want to hear *only* that specific part of the audio immediately, so I can focus on difficult phrases without scrubbing through the whole video."

## 3. Technical Architecture

### 3.1 Backend (FastAPI + Python)
**Location:** `backend/app/`

#### A. New API Endpoint
- **Route:** `GET /api/v1/jobs/{job_id}/audio/stream`
- **Query Parameters:**
  - `start`: float (seconds)
  - `end`: float (seconds)
- **Response:** `AudioFileResponse` (streaming binary)
- **Logic:**
  1. Validate `job_id` and retrieve original audio path.
  2. Use `ffmpeg` (via `subprocess` or `pydub`) to seek and slice the audio in-memory.
  3. Stream the sliced chunk directly to the client with `Content-Type: audio/mpeg` (or original format).
  4. **Optimization:** Add `Cache-Control` headers if the same segment is requested repeatedly.

#### B. Data Models (`backend/app/models/`)
- Update `SubtitleSegment` response model to ensure it strictly includes:
  ```python
  {
    "id": int,
    "text": str,
    "start_time": float, # Critical for slicing
    "end_time": float,   # Critical for slicing
    "speaker": Optional[str]
  }
  ```

#### C. Service Layer (`backend/app/services/`)
- Create `audio_stream_service.py`:
  - Function: `generate_audio_chunk(file_path, start, end)`
  - Implementation: Use `ffmpeg` with `-ss` (seek) and `-t` (duration) flags for low-latency extraction.
  - Error Handling: Handle cases where timestamps exceed file duration.

---

### 3.2 Frontend Web (React + Vite + TypeScript)
**Location:** `frontend/web/src/`

#### A. New Components
1.  **`SentenceList` (`src/features/practice/components/SentenceList.tsx`)**
    - Renders a virtualized list (for performance with long subtitles) of all sentences.
    - Props: `segments`, `activeId`, `onPlay`.
2.  **`SentenceItem` (`src/features/practice/components/SentenceItem.tsx`)**
    - Displays text, timestamp, and a play icon.
    - States: `idle`, `playing`, `active`.
    - Visuals: Highlight background when active.
3.  **`PracticePlayer` (`src/features/practice/hooks/usePracticePlayer.ts`)**
    - Custom hook managing the HTML5 `Audio` object.
    - Methods: `playSegment(start, end)`, `stop()`, `setRate(speed)`.

#### B. State Management
- **Active Segment ID:** Track which sentence is currently playing to apply visual highlighting.
- **Playback Status:** `playing` | `paused` | `stopped`.
- **Settings:** Playback speed (0.5x - 2.0x), Loop toggle.

#### C. User Interaction Flow
1.  User navigates to `/practice/:job_id`.
2.  Frontend fetches subtitle JSON via existing API.
3.  User clicks a `SentenceItem`.
4.  `usePracticePlayer` constructs URL: `/api/v1/jobs/{id}/audio/stream?start={s}&end={e}`.
5.  Audio plays immediately.
6.  On `ended` event, the player stops and clears the `activeId` (or moves to next if "Auto-advance" is enabled).

#### D. UX Enhancements (Phase 2)
- **Keyboard Shortcuts:**
  - `Space`: Play/Pause current.
  - `Arrow Up/Down`: Navigate list.
  - `L`: Toggle Loop.
- **Loop Mode:** Re-play the same segment 3 times.
- **Gap Smoothing:** Apply small fade-in/fade-out (50ms) to avoid clicking sounds from hard cuts.

---

### 3.3 Shared Types
**Location:** `shared/types/` (or `frontend/web/src/types/`)
- Ensure `SubtitleSegment` interface matches the backend response exactly.

## 4. Implementation Phases

### Phase 1: Core Infrastructure (MVP)
- [ ] **Backend:** Implement `ffmpeg` streaming endpoint.
- [ ] **Backend:** Verify low latency (<200ms start time).
- [ ] **Frontend:** Create basic `SentenceList` UI.
- [ ] **Frontend:** Connect click event to audio stream.
- [ ] **Integration:** Test end-to-end playback.

### Phase 2: Player Controls & Polish
- [ ] **Frontend:** Add "Active State" styling (highlighting).
- [ ] **Frontend:** Implement Playback Speed control.
- [ ] **Frontend:** Add "Loop Current Sentence" toggle.
- [ ] **Frontend:** Add Keyboard navigation support.

### Phase 3: Advanced Features
- [ ] **Backend:** Implement caching layer for frequent segments.
- [ ] **Frontend:** "Auto-advance" to next sentence after playback.
- [ ] **Frontend:** Dictation mode (hide text, listen, then reveal).

## 5. Technical Considerations & Risks

| Area | Consideration | Mitigation |
| :--- | :--- | :--- |
| **Latency** | Generating audio on-the-fly might be slow for large files. | Use `ffmpeg` `-ss` before input `-i` for fast seeking. Stream response instead of saving temp files. |
| **Concurrency** | Many users clicking rapidly could spike CPU. | Limit concurrent ffmpeg processes; use queue if necessary. |
| **Mobile** | iOS Safari requires user interaction to play audio. | Ensure `Audio.play()` is called directly inside the `onClick` handler. |
| **Precision** | Audio cut might not align perfectly with speech. | Allow a small buffer (e.g., +100ms) in the backend slicing logic if needed. |
| **Formats** | Browser compatibility with audio codecs. | Force output to MP3 or AAC in ffmpeg for universal browser support. |

## 6. File Structure Changes

```text
backend/
├── app/
│   ├── api/
│   │   └── v1/
│   │       └── endpoints/
│   │           └── audio.py          # NEW: stream endpoint
│   ├── services/
│   │   └── audio_stream_service.py   # NEW: ffmpeg logic
│   └── models/
│       └── subtitle.py               # UPDATE: ensure time fields

frontend/web/
├── src/
│   ├── features/
│   │   └── practice/                 # NEW: feature module
│   │       ├── components/
│   │       │   ├── SentenceList.tsx
│   │       │   └── SentenceItem.tsx
│   │       ├── hooks/
│   │       │   └── usePracticePlayer.ts
│   │       └── pages/
│   │           └── PracticePage.tsx
│   └── router/
│       └── index.tsx                 # UPDATE: add /practice route
```

## 7. Next Steps
1.  Confirm backend dependency (`ffmpeg`) is available in the deployment environment.
2.  Draft the `audio_stream_service.py` logic.
3.  Scaffold the `PracticePage` in the frontend.
