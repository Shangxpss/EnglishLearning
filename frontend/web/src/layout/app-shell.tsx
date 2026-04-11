import { Outlet, NavLink } from 'react-router';
import { navItems } from '@/router/navigation';
import { cn } from '@/lib/utils';
import { useAuth } from '@/providers/auth-context';

export function AppShell() {
  const { user, logout } = useAuth();

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