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

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  const [token, setToken] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [isPending, startTransition] = useTransition();

  // Initialize auth state from storage on mount
  useEffect(() => {
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
    isAuthenticated: !!user && !!token,
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