import { createContext, useContext, useState, useTransition, type ReactNode } from 'react';

export interface User {
  id: string;
  name: string;
  email: string;
  avatarUrl?: string;
}

interface AuthState {
  user: User | null;
  isLoading: boolean;
  login: (user: User) => void;
  logout: () => void;
}

const AuthContext = createContext<AuthState | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [isPending, startTransition] = useTransition(); // React 19: Non-blocking state updates

  const login = (userData: User) => {
    startTransition(() => {
      setUser(userData);
      // Optional: persist to localStorage/session
      localStorage.setItem('auth_user', JSON.stringify(userData));
    });
  };

  const logout = () => {
    startTransition(() => {
      setUser(null);
      localStorage.removeItem('auth_user');
    });
  };

  // React 19: Context updates are automatically batched & optimized
  const value: AuthState = { user, isLoading: isPending, login, logout };

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