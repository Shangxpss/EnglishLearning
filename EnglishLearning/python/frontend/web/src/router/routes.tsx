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
import { LoginPage } from '@/features/auth/login';
import { SignupPage } from '@/features/auth/signup';

export const routes: RouteObject[] = [
  {
    path: '/',
    element: <AppShell />,
    children: [
      { index: true, element: <HomePage /> },
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
