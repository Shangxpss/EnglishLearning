import { Home, Play, type LucideIcon } from 'lucide-react';

export interface NavItem {
  label: string;
  path: string;
  icon: LucideIcon;
}

/**
 * Top-nav entries.
 *
 * Only routes backed by the Rust `sentence-video` server are listed. The
 * remaining pages (subtitle, subtitle-video, video-dub, dubbing-studio,
 * reading, progress, assistant) depend on the legacy Python/FastAPI backend,
 * so they are intentionally hidden from the nav — see router/routes.tsx.
 */
export const navItems: NavItem[] = [
  { label: 'Home', path: '/', icon: Home },
  { label: 'Player', path: '/player', icon: Play },
];
