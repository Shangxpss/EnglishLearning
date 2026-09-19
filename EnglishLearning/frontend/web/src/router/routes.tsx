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
import { LoginPage } from '@/features/auth/login';
import { SignupPage } from '@/features/auth/signup';

export const routes: RouteObject[] = [
  {
    path: '/',
    element: <AppShell />,
    children: [
      { index: true, element: <HomePage /> },

      // ── Rust `sentence-video` backend compatible routes ──────────────────
      // /player uses /api/sessions, /api/session, /cues, /media, /segments —
      // all implemented in rust/src/app.rs.
      { path: 'player', element: <PlayerPage /> },

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
