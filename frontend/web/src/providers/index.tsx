import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ThemeProvider } from 'next-themes';
import { AuthProvider } from './auth-context';
import { type ReactNode } from 'react';

// Initialize outside component to prevent recreation on renders
const queryClient = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 1000 * 60 * 5, retry: 1 },
  },
});

export function RootProvider({ children }: { children: ReactNode }) {
  return (
    <ThemeProvider attribute="class" defaultTheme="system" enableSystem>
      <QueryClientProvider client={queryClient}>
        <AuthProvider>
          {children}
        </AuthProvider>
      </QueryClientProvider>
    </ThemeProvider>
  );
}