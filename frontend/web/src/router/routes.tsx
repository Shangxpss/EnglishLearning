import type { RouteObject } from 'react-router-dom';
import { AppShell } from '@/layout/app-shell';
import { HomePage } from '@/features/home';
import { SubtitlePage } from '@/features/subtitle';
import { ProgressPage } from '@/features/progress';
import { ReadingPage } from '@/features/reading';
import { LoginPage } from '@/features/auth/login';
import { SignupPage } from '@/features/auth/signup';

export const routes: RouteObject[] = [
  {
    path: '/',
    element: <AppShell />,
    children: [
      { index: true, element: <HomePage /> },
      { path: 'subtitle', element: <SubtitlePage /> },
      { path: 'progress', element: <ProgressPage /> },
      { path: 'reading', element: <ReadingPage /> },
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
