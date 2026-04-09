import { Home, FileText, BarChart, type LucideIcon } from 'lucide-react';

export interface NavItem {
  label: string;
  path: string;
  icon: LucideIcon;
}

export const navItems: NavItem[] = [
  { label: 'Home', path: '/', icon: Home },
  { label: 'Subtitle Tool', path: '/subtitle', icon: FileText },
  { label: 'Progress', path: '/progress', icon: BarChart },
];