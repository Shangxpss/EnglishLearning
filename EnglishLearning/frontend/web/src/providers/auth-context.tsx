import { createContext, useContext, useState, useTransition, type ReactNode, useEffect } from 'react';

export interface User {
  id: string;
  name: string;
  email: string;
  avatarUrl?: string;
}

interface AuthState {
  user: User | null;
  token: string | null;
  isLoading: boolean;
  isAuthenticated: boolean;
  login: (user: User, token: string, rememberMe?: boolean) => void;
  logout: () => void;
  validateToken: () => Promise<boolean>;
}

const AuthContext = createContext<AuthState | null>(null);

// Storage keys
const TOKEN_KEY = 'auth_token';
const USER_KEY = 'auth_user';
const REFRESH_KEY = 'auth_refresh_intent';

// ---------------------------------------------------------------------------
// AUTH BYPASS (temporary)
// ---------------------------------------------------------------------------
// Authentication is intentionally disabled in the frontend so the system can
// be used without logging in. Every visitor is treated as a signed-in "Guest".
// Flip AUTH_DISABLED to false to restore the original login-required flow —
// the original logic is kept below and is guarded by this flag.
export const AUTH_DISABLED = true;

// Stand-in session used while authentication is disabled.
const GUEST_USER: User = {
  id: 'guest',
  name: 'Guest',
  email: 'guest@localhost',
};
const GUEST_TOKEN = 'guest-token';

export function AuthProvider({ children }: { children: ReactNode }) {
  // When auth is disabled we start already "signed in" as the guest user.
  const [user, setUser] = useState<User | null>(AUTH_DISABLED ? GUEST_USER : null);
  const [token, setToken] = useState<string | null>(AUTH_DISABLED ? GUEST_TOKEN : null);
  const [isLoading, setIsLoading] = useState(!AUTH_DISABLED);
  const [isPending, startTransition] = useTransition();

  // Initialize auth state from storage on mount
  useEffect(() => {
    // No login required: skip token restore/validation entirely.
    if (AUTH_DISABLED) return;

    const initAuth = async () => {
      const storedToken = localStorage.getItem(TOKEN_KEY) || sessionStorage.getItem(TOKEN_KEY);
      const storedUser = localStorage.getItem(USER_KEY) || sessionStorage.getItem(USER_KEY);

      if (storedToken && storedUser) {
        try {
          const parsedUser = JSON.parse(storedUser);
          // Validate token with backend
          const isValid = await validateTokenWithBackend(storedToken);
          if (isValid) {
            startTransition(() => {
              setToken(storedToken);
              setUser(parsedUser);
            });
          } else {
            // Clear invalid tokens
            clearAuthStorage();
          }
        } catch (error) {
          console.error('Failed to initialize auth:', error);
          clearAuthStorage();
        }
      }
      startTransition(() => {
        setIsLoading(false);
      });
    };

    initAuth();
  }, []);

  const clearAuthStorage = () => {
    localStorage.removeItem(TOKEN_KEY);
    localStorage.removeItem(USER_KEY);
    sessionStorage.removeItem(TOKEN_KEY);
    sessionStorage.removeItem(USER_KEY);
  };

  const validateTokenWithBackend = async (authToken: string): Promise<boolean> => {
    // Auth disabled: never call /api/me, treat every session as valid.
    if (AUTH_DISABLED) return true;
    try {
      const response = await fetch('/api/me', {
        headers: {
          'Authorization': `Bearer ${authToken}`
        }
      });
      return response.ok;
    } catch {
      return false;
    }
  };

  const login = (userData: User, authToken: string, rememberMe: boolean = true) => {
    startTransition(() => {
      setToken(authToken);
      setUser(userData);
      
      // Store in localStorage (persistent) or sessionStorage (session-only)
      const storage = rememberMe ? localStorage : sessionStorage;
      storage.setItem(TOKEN_KEY, authToken);
      storage.setItem(USER_KEY, JSON.stringify(userData));
    });
  };

  const logout = () => {
    startTransition(() => {
      // Auth disabled: "logging out" just restores the guest session so the
      // app stays usable without ever showing a login screen.
      if (AUTH_DISABLED) {
        setToken(GUEST_TOKEN);
        setUser(GUEST_USER);
        return;
      }
      setToken(null);
      setUser(null);
      clearAuthStorage();
    });
  };

  const validateToken = async (): Promise<boolean> => {
    if (!token) return false;
    const isValid = await validateTokenWithBackend(token);
    if (!isValid) {
      logout();
    }
    return isValid;
  };

  const value: AuthState = { 
    user, 
    token, 
    isLoading: isPending || isLoading, 
    // Auth disabled: always report an authenticated session.
    isAuthenticated: AUTH_DISABLED || (!!user && !!token),
    login, 
    logout,
    validateToken
  };

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

// ✅ Custom hook with strict type guard
export function useAuth(): AuthState {
  const context = useContext(AuthContext);
  if (!context) {
    throw new Error('useAuth must be used within <AuthProvider>');
  }
  return context;
}