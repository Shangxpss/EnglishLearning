import { Home, FileText, BarChart, Book, Bot, Video, Film, Wand2, type LucideIcon } from 'lucide-react';

export interface NavItem {
  label: string;
  path: string;
  icon: LucideIcon;
}

export const navItems: NavItem[] = [
  { label: 'Home', path: '/', icon: Home },
  { label: 'Subtitle Tool', path: '/subtitle', icon: FileText },
  { label: 'Subtitle → Video', path: '/subtitle-video', icon: Video },
  { label: 'Video Dubbing', path: '/video-dub', icon: Film },
  { label: 'Dubbing Studio', path: '/dubbing-studio', icon: Wand2 },
  { label: 'Reading', path: '/reading', icon: Book },
  { label: 'Progress', path: '/progress', icon: BarChart },
  { label: 'AI Assistant', path: '/assistant', icon: Bot },
];