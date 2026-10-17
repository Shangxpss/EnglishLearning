import { Outlet, NavLink, useNavigate, useLocation } from 'react-router';
import { CopilotPopup, useAgent } from '@copilotkit/react-core/v2';
import { navItems } from '@/router/navigation';
import { cn } from '@/lib/utils';
import { useAuth, AUTH_DISABLED } from '@/providers/auth-context';
import { getChatThreadId, CHAT_THREAD_CHANGED_EVENT } from '@/lib/chat-thread';
import { getChatMessages, type ChatMessage } from '@/lib/chat-api';
import { useEffect, useRef, useState } from 'react';

/** Runtime agent id the floating assistant is registered under (see the provider). */
const ASSISTANT_AGENT_ID = 'sample_agent';

/**
 * Map the runtime's stored transcript onto the AG-UI messages the popup renders.
 *
 * Only user/assistant text is kept: the server records nothing else (tool
 * traffic is dropped at ingestion) and the chat view has no bubble for it. Ids
 * are synthesised because AG-UI messages carry one and the store does not.
 */
function seedMessages(history: ChatMessage[]) {
  return history
    .filter((m) => (m.role === 'user' || m.role === 'assistant') && m.content?.trim())
    .map((m, i) => ({
      id: `history-${i}`,
      role: m.role as 'user' | 'assistant',
      content: m.content as string,
    }));
}

/**
 * Seed the floating assistant with the pinned thread's stored transcript.
 *
 * The v2 chat view never fetches thread history — that lives behind the
 * licence-gated threads drawer — and the AG-UI client mints a fresh thread per
 * page load, so without this the popup would come back empty after a reload
 * even though the conversation is still on the server. Runs once per thread;
 * a switch to another thread replaces what is on screen.
 */
function ChatThreadHydrator({ threadId }: { threadId: string }) {
  const { agent, isReady } = useAgent({ agentId: ASSISTANT_AGENT_ID });
  const seeded = useRef<string | null>(null);
  const previous = useRef<string | null>(null);

  useEffect(() => {
    // The provisional pre-sync agent is a stand-in that gets swapped out, so
    // only touch the real one.
    if (!isReady || seeded.current === threadId) return;
    // A deliberate switch — re-pinning an older conversation from the History
    // page, or starting a new one — must replace whatever is on screen. Only the
    // first load has to be careful not to clobber a message that raced the fetch.
    const switched = previous.current !== null && previous.current !== threadId;
    previous.current = threadId;
    let cancelled = false;
    getChatMessages(threadId)
      .then((history) => {
        if (cancelled) return;
        seeded.current = threadId;
        const seed = seedMessages(history);
        if (!switched && seed.length > 0 && agent.messages.length > 0) return;
        agent.setMessages(seed);
      })
      .catch(() => {
        // History is a convenience: a failed read just means an empty popup.
      });
    return () => {
      cancelled = true;
    };
  }, [isReady, agent, threadId]);

  return null;
}

export function AppShell() {
  const { user, isAuthenticated, isLoading, logout } = useAuth();
  const navigate = useNavigate();
  const location = useLocation();

  // The thread the floating assistant is pinned to. Read once, then follow
  // change events so the popup (mounted for the whole session) switches
  // without a reload — both to a fresh conversation ("New chat") and to an
  // older one picked on the History page.
  const [chatThreadId, setChatThreadId] = useState(() => getChatThreadId());
  useEffect(() => {
    const onThreadChange = () => setChatThreadId(getChatThreadId());
    window.addEventListener(CHAT_THREAD_CHANGED_EVENT, onThreadChange);
    return () => window.removeEventListener(CHAT_THREAD_CHANGED_EVENT, onThreadChange);
  }, []);

  // Redirect to login if not authenticated.
  // AUTH BYPASS: authentication is disabled, so this guard is skipped and the
  // app is reachable without logging in. Set AUTH_DISABLED to false in
  // providers/auth-context.tsx to restore the redirect.
  useEffect(() => {
    if (AUTH_DISABLED) return;
    if (!isLoading && !isAuthenticated) {
      // Store the intended destination
      sessionStorage.setItem('redirect_after_login', location.pathname);
      navigate('/login');
    }
  }, [isAuthenticated, isLoading, location.pathname, navigate]);

  // Show loading state while checking auth
  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto"></div>
          <p className="mt-4 text-muted-foreground">Loading...</p>
        </div>
      </div>
    );
  }

  // Don't render app shell if not authenticated (will redirect).
  // AUTH BYPASS: skipped while authentication is disabled.
  if (!AUTH_DISABLED && !isAuthenticated) {
    return null;
  }

  return (
    <div className="min-h-screen flex flex-col bg-background">
      <header className="sticky top-0 z-50 w-full border-b bg-background/95 backdrop-blur supports-backdrop-filter:bg-background/60">
        <div className="container flex h-14 items-center justify-between">
          <nav className="flex items-center gap-6 text-sm font-medium">
            <span className="mr-2 text-lg font-bold tracking-tight">📚 EnglishPro</span>
            {navItems.map((item) => (
              <NavLink
                key={item.path}
                to={item.path}
                end={item.path === '/'}
                className={({ isActive }) =>
                  cn(
                    'transition-colors hover:text-foreground/80',
                    isActive ? 'text-foreground font-semibold' : 'text-muted-foreground'
                  )
                }
              >
                {item.label}
              </NavLink>
            ))}
          </nav>

          <div className="flex items-center gap-4">
            {AUTH_DISABLED ? (
              // AUTH BYPASS: no login screen — every visitor browses as a guest.
              <span className="text-sm text-muted-foreground">Guest mode · no login required</span>
            ) : user ? (
              <>
                <span className="text-sm text-muted-foreground">Hi, {user.name}</span>
                <button
                  onClick={logout}
                  className="text-sm text-destructive hover:underline focus:outline-none focus:ring-2 focus:ring-ring rounded-md px-2 py-1"
                >
                  Logout
                </button>
              </>
            ) : (
              <>
                <NavLink
                  to="/login"
                  className={({ isActive }) =>
                    cn(
                      'text-sm transition-colors hover:text-foreground/80',
                      isActive ? 'text-foreground font-semibold' : 'text-muted-foreground'
                    )
                  }
                >
                  Login
                </NavLink>
                <NavLink
                  to="/signup"
                  className={({ isActive }) =>
                    cn(
                      'text-sm transition-colors hover:text-foreground/80',
                      isActive ? 'text-foreground font-semibold' : 'text-muted-foreground'
                    )
                  }
                >
                  Signup
                </NavLink>
              </>
            )}
          </div>
        </div>
      </header>

      <main className="flex-1 container py-6">
        <Outlet />
      </main>

      {/* CopilotKit floating assistant: a chat button pinned to the corner,
          present on every page. While the player has a video open it publishes
          that video's transcript as ambient context (see features/player), so
          the agent can answer questions about what the user is watching. */}
      <ChatThreadHydrator threadId={chatThreadId} />
      <CopilotPopup agentId={ASSISTANT_AGENT_ID} threadId={chatThreadId} />
    </div>
  );
}