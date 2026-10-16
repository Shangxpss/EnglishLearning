import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ThemeProvider } from 'next-themes';
import { AuthProvider } from './auth-context';
import { CopilotKitProvider } from './copilotkit-provider';
import { type ReactNode } from 'react';

// Initialize outside component to prevent recreation on renders
const queryClient = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 1000 * 60 * 5, retry: 1 },
  },
});

// NOTE: this sits *above* the router, so it must not contain anything that
// needs router context (a `<Link>`, `useNavigate`). The job-notification stack
// links into the player, so it is mounted inside `AppShell` instead — see
// providers/job-notifications.tsx.
export function RootProvider({ children }: { children: ReactNode }) {
  return (
    <ThemeProvider attribute="class" defaultTheme="system" enableSystem>
      <QueryClientProvider client={queryClient}>
        <AuthProvider>
          <CopilotKitProvider>{children}</CopilotKitProvider>
        </AuthProvider>
      </QueryClientProvider>
    </ThemeProvider>
  );
}