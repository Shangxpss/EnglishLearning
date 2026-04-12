import { Home, FileText, BarChart, Book, type LucideIcon } from 'lucide-react';

export interface NavItem {
  label: string;
  path: string;
  icon: LucideIcon;
}

export const navItems: NavItem[] = [
  { label: 'Home', path: '/', icon: Home },
  { label: 'Subtitle Tool', path: '/subtitle', icon: FileText },
  { label: 'Reading', path: '/reading', icon: Book },
  { label: 'Progress', path: '/progress', icon: BarChart },
];