# Token-Based Authentication Implementation Guide

## Overview
This document describes the complete token-based authentication system implemented across frontend and backend, with session/localStorage token management and automatic navigation protection.

---

## Backend Implementation (FastAPI)

### 1. Core Auth Module (`/backend/app/core/auth.py`)

**Key Functions:**
- `create_access_token(data: dict, expires_delta: timedelta)` - Creates JWT tokens
- `decode_token(token: str)` - Validates and decodes JWT tokens
- `get_current_user(token, db)` - Dependency for protected routes
- `require_auth(token, db)` - Alternative auth helper that accepts raw tokens

**Token Configuration:**
```python
SECRET_KEY = os.getenv("SECRET_KEY", "your-secret-key")
ALGORITHM = "HS256"
ACCESS_TOKEN_EXPIRE_MINUTES = 30
```

### 2. Protected Routes (`/backend/app/main.py`)

**Authentication Flow:**
1. User logs in via `/token` endpoint
2. Backend validates credentials and returns JWT access token
3. Client includes token in `Authorization: Bearer <token>` header
4. Protected routes use `Depends(require_auth)` to validate tokens

**Protected Endpoints:**
- `GET /me` - Get current user info
- `POST /save-word` - Save word to user's vocabulary
- `GET /my-words` - Get user's saved words

**Login Endpoint:**
```python
@app.post("/token", response_model=Token)
async def login(form_data: OAuth2PasswordRequestForm, db: Session)
```

**Token Validation Helper:**
```python
async def require_auth(token: Optional[str] = None, db: Session = Depends(get_db)):
    """Validate token and return current user or raise 401"""
    # Removes 'Bearer ' prefix if present
    # Decodes and validates JWT
    # Fetches user from database
    # Returns user object or raises HTTPException(401)
```

---

## Frontend Implementation (React + TypeScript)

### 1. Auth Context (`/frontend/web/src/providers/auth-context.tsx`)

**State Management:**
```typescript
interface AuthState {
  user: User | null;
  token: string | null;
  isLoading: boolean;
  isAuthenticated: boolean;
  login: (user: User, token: string, rememberMe?: boolean) => void;
  logout: () => void;
  validateToken: () => Promise<boolean>;
}
```

**Storage Strategy:**
- **localStorage**: Persistent sessions (remember me)
- **sessionStorage**: Session-only tokens (tab closes = logout)
- **Keys**: `auth_token`, `auth_user`

**Initialization Flow:**
```typescript
useEffect(() => {
  const initAuth = async () => {
    // 1. Retrieve stored token and user
    const storedToken = localStorage.getItem(TOKEN_KEY) || sessionStorage.getItem(TOKEN_KEY);
    const storedUser = localStorage.getItem(USER_KEY) || sessionStorage.getItem(USER_KEY);
    
    // 2. Validate token with backend
    const isValid = await validateTokenWithBackend(storedToken);
    
    // 3. If valid, restore auth state; otherwise clear storage
    if (isValid) {
      setToken(storedToken);
      setUser(parsedUser);
    } else {
      clearAuthStorage();
    }
    
    setIsLoading(false);
  };
  initAuth();
}, []);
```

**Login Function:**
```typescript
const login = (userData: User, authToken: string, rememberMe: boolean = true) => {
  const storage = rememberMe ? localStorage : sessionStorage;
  storage.setItem(TOKEN_KEY, authToken);
  storage.setItem(USER_KEY, JSON.stringify(userData));
};
```

### 2. App Shell Protection (`/frontend/web/src/layout/app-shell.tsx`)

**Route Guard Implementation:**
```typescript
useEffect(() => {
  if (!isLoading && !isAuthenticated) {
    // Store intended destination
    sessionStorage.setItem('redirect_after_login', location.pathname);
    navigate('/login');
  }
}, [isAuthenticated, isLoading, location.pathname, navigate]);
```

**Loading State:**
- Shows spinner while validating stored tokens
- Prevents flash of unauthenticated content

**Redirect Logic:**
1. User tries to access protected route
2. AppShell detects no auth → stores current path in sessionStorage
3. Redirects to `/login`
4. After successful login, redirects back to stored path

### 3. Login Page (`/frontend/web/src/features/auth/login/index.tsx`)

**Login Flow:**
```typescript
const handleSubmit = async (e: React.FormEvent) => {
  // 1. POST credentials to /api/token
  const response = await fetch('/api/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ username: email, password })
  });
  
  // 2. Extract access token
  const data = await response.json();
  
  // 3. Fetch user info with token
  const userResponse = await fetch('/api/me', {
    headers: { 'Authorization': `Bearer ${data.access_token}` }
  });
  const userData = await userResponse.json();
  
  // 4. Call auth context login (stores token + user)
  login(userData, data.access_token, true);
  
  // 5. Navigate to intended page or home
  const redirectPath = sessionStorage.getItem('redirect_after_login');
  navigate(redirectPath || '/');
};
```

---

## Security Features

### Token Storage
| Storage Type | Use Case | Persistence |
|-------------|----------|-------------|
| localStorage | Remember Me | Until manually cleared |
| sessionStorage | Session Only | Until tab/browser closes |

### Token Validation
1. **On App Load**: Validates stored token against `/api/me`
2. **On Route Access**: AppShell checks `isAuthenticated` state
3. **On API Calls**: Backend validates JWT signature and expiration

### Automatic Logout
- Invalid/expired tokens automatically cleared on failed validation
- Logout function clears both token and user data from storage

---

## API Request Examples

### Login Request
```bash
curl -X POST http://localhost:8000/api/token \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "username=user@example.com&password=secret"
```

**Response:**
```json
{
  "access_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
  "token_type": "bearer"
}
```

### Protected Route Request
```bash
curl -X GET http://localhost:8000/api/me \
  -H "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."
```

**Response:**
```json
{
  "id": 1,
  "username": "johndoe",
  "email": "user@example.com"
}
```

---

## File Locations

### Backend
```
/backend/app/core/auth.py          # JWT utilities, token validation
/backend/app/main.py               # Auth endpoints, protected routes
/backend/app/models/auth_models.py # Pydantic models (Token, UserCreate, etc.)
/backend/app/models/user_models.py # SQLAlchemy User model
```

### Frontend
```
/frontend/web/src/providers/auth-context.tsx  # Auth state management
/frontend/web/src/layout/app-shell.tsx        # Route protection guard
/frontend/web/src/features/auth/login/        # Login page
/frontend/web/src/features/auth/signup/       # Signup page
```

---

## Environment Variables

### Required Backend Variables
```bash
# .env file
SECRET_KEY=your-super-secret-key-change-in-production
ACCESS_TOKEN_EXPIRE_MINUTES=30
DATABASE_URL=postgresql://user:pass@localhost/dbname
```

---

## Testing the Implementation

### Manual Test Flow
1. **Start Backend**: `cd backend && uvicorn app.main:app --reload`
2. **Start Frontend**: `cd frontend/web && npm run dev`
3. **Visit Protected Route**: Navigate to `/subtitle` without logging in
   - Expected: Redirected to `/login`
4. **Login**: Enter valid credentials
   - Expected: Redirected back to `/subtitle`
5. **Refresh Page**: 
   - Expected: Stay logged in (token validated)
6. **Logout**: Click logout button
   - Expected: Redirected to `/login`, storage cleared

### Browser DevTools Check
- **Application → Local Storage**: Should see `auth_token` and `auth_user`
- **Application → Session Storage**: Should see `redirect_after_login` during flow
- **Network Tab**: Verify `Authorization: Bearer` headers on API calls

---

## Common Issues & Solutions

### Issue: Token not persisting after refresh
**Solution**: Ensure `rememberMe` parameter is `true` in login call

### Issue: Infinite redirect loop
**Solution**: Check `isLoading` state in AppShell before redirecting

### Issue: 401 on valid token
**Solution**: Verify SECRET_KEY matches between token creation and validation

### Issue: Token not sent with API requests
**Solution**: Ensure fetch calls include `Authorization: Bearer ${token}` header

---

## Future Enhancements

1. **Refresh Tokens**: Implement long-lived refresh tokens for seamless sessions
2. **Token Expiry Warning**: Notify users before token expires
3. **Multi-device Support**: Track active sessions per user
4. **Rate Limiting**: Prevent brute-force login attempts
5. **2FA Support**: Add two-factor authentication option

---

## Quick Reference

### Login Component Pattern
```tsx
const { login } = useAuth();
const navigate = useNavigate();

const handleLogin = async (credentials) => {
  const response = await fetch('/api/token', { /* ... */ });
  const { access_token } = await response.json();
  
  const userResponse = await fetch('/api/me', {
    headers: { 'Authorization': `Bearer ${access_token}` }
  });
  const userData = await userResponse.json();
  
  login(userData, access_token, true);
  navigate('/');
};
```

### Protected API Call Pattern
```tsx
const { token } = useAuth();

const fetchData = async () => {
  const response = await fetch('/api/my-words', {
    headers: {
      'Authorization': `Bearer ${token}`
    }
  });
  return response.json();
};
```

### Backend Protected Route Pattern
```python
@app.get("/protected")
async def protected_route(current_user: User = Depends(require_auth), db: Session = Depends(get_db)):
    # current_user is guaranteed to be authenticated
    return {"user_id": current_user.id}
```
