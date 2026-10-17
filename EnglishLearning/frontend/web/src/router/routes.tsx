import type { RouteObject } from 'react-router-dom';
import { AppShell } from '@/layout/app-shell';
import { HomePage } from '@/features/home';
import { SubtitlePage } from '@/features/subtitle';
import { SubtitleVideoPage } from '@/features/subtitle-video';
import { VideoDubPage } from '@/features/video-dub';
import { DubbingStudioPage } from '@/features/dubbing-studio';
import { ProgressPage } from '@/features/progress';
import { ReadingPage } from '@/features/reading';
import { AssistantPage } from '@/features/assistant';
import { PlayerPage } from '@/features/player';
import { ManagePage } from '@/features/manage';
import { QueuePage } from '@/features/queue';
import { LogsPage } from '@/features/logs';
import { ChatHistoryPage } from '@/features/chat-history';
import { LoginPage } from '@/features/auth/login';
import { SignupPage } from '@/features/auth/signup';

export const routes: RouteObject[] = [
  {
    path: '/',
    element: <AppShell />,
    children: [
      { index: true, element: <HomePage /> },

      // ── Rust `sentence-video` backend compatible routes ──────────────────
      // /player uses /api/sessions, /api/session, /cues, /media, /audio —
      // all implemented in rust/src/app.rs.
      { path: 'player', element: <PlayerPage /> },
      // /queue submits batches to the server's durable job queue (POST
      // /api/jobs) and reports the outcome, pushed over GET /api/jobs/events.
      { path: 'queue', element: <QueuePage /> },
      // /manage is CRUD over the processed videos plus their processing state;
      // /player is for watching. `?session=<id>` opens one specific session.
      { path: 'manage', element: <ManagePage /> },
      // /logs reads the server's logs.db — requests + background jobs — so a
      // problem noticed after the fact can still be traced.
      { path: 'logs', element: <LogsPage /> },
      // /chat-history reads threads.db — past conversations with the floating
      // assistant, recorded by the agent runtime.
      { path: 'chat-history', element: <ChatHistoryPage /> },

      // ── Legacy Python/FastAPI routes (hidden from the nav) ───────────────
      // These call endpoints the Rust binary does not implement. They stay
      // reachable by URL for reference but are not linked in navigation.
      { path: 'subtitle', element: <SubtitlePage /> },
      { path: 'subtitle-video', element: <SubtitleVideoPage /> },
      { path: 'video-dub', element: <VideoDubPage /> },
      { path: 'dubbing-studio', element: <DubbingStudioPage /> },
      { path: 'progress', element: <ProgressPage /> },
      { path: 'reading', element: <ReadingPage /> },
      { path: 'assistant', element: <AssistantPage /> },
    ],
  },
  {
    path: '/login',
    element: <LoginPage />,
  },
  {
    path: '/signup',
    element: <SignupPage />,
  },
];
