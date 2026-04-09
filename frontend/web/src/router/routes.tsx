import type { RouteObject } from 'react-router-dom';
import { AppShell } from '@/layout/app-shell';
import { HomePage } from '@/features/home';
import { SubtitlePage } from '@/features/subtitle';
import { ProgressPage } from '@/features/progress';

export const routes: RouteObject[] = [
  {
    path: '/',
    element: <AppShell />,
    children: [
      { index: true, element: <HomePage /> },
      { path: 'subtitle', element: <SubtitlePage /> },
      { path: 'progress', element: <ProgressPage /> },
    ],
  },
];