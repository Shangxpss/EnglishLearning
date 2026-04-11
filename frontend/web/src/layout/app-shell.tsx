import { Outlet, NavLink, useNavigate, useLocation } from 'react-router';
import { navItems } from '@/router/navigation';
import { cn } from '@/lib/utils';
import { useAuth } from '@/providers/auth-context';
import { useEffect } from 'react';

export function AppShell() {
  const { user, isAuthenticated, isLoading, logout } = useAuth();
  const navigate = useNavigate();
  const location = useLocation();

  // Redirect to login if not authenticated
  useEffect(() => {
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

  // Don't render app shell if not authenticated (will redirect)
  if (!isAuthenticated) {
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
            {user ? (
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
    </div>
  );
}