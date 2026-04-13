# English Learning App (EnglishPro)

A comprehensive English learning platform with a monorepo architecture featuring a FastAPI backend, React Native mobile app, React web application, and shared components.

## Project Structure

```
english-learning-app/
├── backend/                    # Python FastAPI backend
│   ├── app/
│   │   ├── main.py            # FastAPI application entry point
│   │   ├── api/               # API router registration
│   │   ├── services/          # Business logic services
│   │   │   ├── audio_processor.py    # Audio analysis service
│   │   │   ├── subtitle_processor.py # Subtitle generation service
│   │   │   ├── langchain_agent.py    # LLM-powered story generation
│   │   │   └── langchain_prompts.py  # Prompt templates
│   │   ├── models/            # SQLAlchemy & Pydantic models
│   │   │   ├── user_models.py        # User & UserWord tables
│   │   │   ├── reading_models.py     # ReadingSession table
│   │   │   ├── auth_models.py        # Auth DTOs
│   │   │   ├── response_models.py    # API response schemas
│   │   │   └── langchain_models.py   # Story generation schemas
│   │   └── core/              # Core utilities
│   │       ├── config.py             # Environment settings
│   │       ├── database.py           # SQLAlchemy DB connection
│   │       └── auth.py               # JWT authentication
│   ├── alembic/               # Database migrations
│   ├── requirements.txt       # Python dependencies
│   ├── pyproject.toml         # UV package manager config
│   └── README.md              # Backend-specific documentation
├── frontend/
│   ├── mobile/                # React Native mobile app (Expo)
│   │   ├── App.tsx
│   │   └── package.json
│   ├── shared/                # Shared TypeScript components & types
│   │   ├── src/
│   │   │   ├── components/    # Reusable UI components
│   │   │   ├── hooks/         # Custom React hooks
│   │   │   ├── types/         # Shared TypeScript types
│   │   │   └── index.ts       # Package entry point
│   │   └── package.json
│   └── web/                   # React web application (Vite + TypeScript)
│       ├── src/
│       │   ├── assets/        # Static assets (images, fonts)
│       │   ├── components/    # Web-specific UI components
│       │   │   ├── ui/        # shadcn/ui components
│       │   │   ├── StoryViewer.tsx
│       │   │   ├── WordCard.tsx
│       │   │   └── WordPanel.tsx
│       │   ├── features/      # Feature-based modules
│       │   │   ├── home/      # Home page feature
│       │   │   ├── subtitle/  # Subtitle processing feature
│       │   │   ├── progress/  # Progress tracking feature
│       │   │   └── auth/      # Login & signup features
│       │   ├── layout/        # Layout components
│       │   │   └── app-shell.tsx # Main app shell layout
│       │   ├── lib/           # Utility functions
│       │   │   └── utils.ts   # Helper utilities (cn, etc.)
│       │   ├── providers/     # Context providers
│       │   │   ├── index.tsx  # Provider exports
│       │   │   └── auth-context.tsx # Authentication context
│       │   ├── router/        # Routing configuration
│       │   │   ├── routes.tsx # Route definitions
│       │   │   └── navigation.tsx # Navigation helpers
│       │   ├── pages/         # Page components
│       │   │   └── ReadingPage.tsx
│       │   ├── App.tsx        # Root application component
│       │   └── main.tsx       # Application entry point
│       ├── public/            # Public static files
│       ├── index.html         # HTML template
│       ├── vite.config.ts     # Vite configuration
│       ├── tailwind.config.ts # Tailwind CSS configuration
│       └── package.json
├── product/                   # Product documentation
│   ├── features/              # Feature specifications
│   └── roadmap.md             # Product roadmap
├── material/                  # Learning materials
│   ├── audio/                 # Audio files
│   └── subtitle/              # Subtitle files
├── scripts/                   # Utility scripts
│   └── create_tables.py       # Database table creation script
├── tests/                     # Test suites
│   ├── agents/                # Agent-specific tests
│   └── test_example.py
├── package.json               # Root package.json with workspace scripts
├── pnpm-workspace.yaml        # PNPM workspace configuration
└── README.md                  # This file
```

## Tech Stack

### Frontend Web
- **React** 19.2.4 - UI library
- **React Router** 7.14.0 - Client-side routing
- **TypeScript** ~5.9.3 - Type safety
- **Vite** 8.0.1 - Build tool and dev server
- **Tailwind CSS** 4.2.2 - Utility-first styling
- **shadcn/ui** - UI component library built on Radix UI
- **Radix UI** 1.4.3 - Accessible UI primitives
- **TanStack Query** 5.96.2 - Data fetching and caching
- **Lucide React** 1.7.0 - Icon library
- **next-themes** 0.4.6 - Theme management (dark/light mode)
- **PNPM** - Package manager with workspace support

### Frontend Mobile
- **React Native** 0.81.5 - Mobile development
- **Expo** ~54.0.33 - Mobile development framework
- **TypeScript** ~5.9.3 - Type safety

### Shared Components
- **React** 19.2.4 (peer dependency)
- **TypeScript** ~5.9.3

### Backend
- **Python** >=3.11 - Runtime
- **FastAPI** >=0.135.3 - Web framework
- **Faster-Whisper** >=1.2.1 - Speech-to-text engine
- **Librosa** >=0.10.0 - Audio analysis
- **LangChain** >=0.1.0 - LLM integration
- **UV** - Python package manager
- **Uvicorn** >=0.29.0 - ASGI server

## Package Versions

### Root Dependencies (`package.json`)
| Package | Version |
|---------|---------|
| react | ^19.2.4 |
| react-dom | ^19.2.4 |
| @types/node | ^24.12.0 |
| @types/react | ^19.2.14 |
| @types/react-dom | ^19.2.3 |
| typescript | ~5.9.3 |
| concurrently (dev) | ^8.2.0 |

### Web Application (`frontend/web/package.json`)
| Package | Version |
|---------|---------|
| react | ^19.2.4 |
| react-dom | ^19.2.4 |
| react-router-dom | ^7.14.0 |
| @tanstack/react-query | ^5.96.2 |
| radix-ui | ^1.4.3 |
| lucide-react | ^1.7.0 |
| tailwindcss | ^4.2.2 |
| vite | ^8.0.1 |
| typescript | ~5.9.3 |
| shadcn | ^4.1.2 |
| next-themes | ^0.4.6 |

### Mobile App (`frontend/mobile/package.json`)
| Package | Version |
|---------|---------|
| expo | ~54.0.33 |
| expo-status-bar | ~3.0.9 |
| react | ^19.2.4 |
| react-native | 0.81.5 |
| @types/react | ^19.2.14 |
| typescript | ~5.9.3 |

### Shared Components (`frontend/shared/package.json`)
| Package | Version |
|---------|---------|
| react (peer) | ^19.2.4 |
| @types/react | ^19.2.14 |
| typescript | ~5.9.3 |

### Backend Dependencies (`backend/requirements.txt`)
| Package | Version |
|---------|---------|
| fastapi[standard] | >=0.135.3 |
| faster-whisper | >=1.2.1 |
| ffmpeg-python | >=0.2.0 |
| librosa | >=0.10.0 |
| numpy | >=1.24.0 |
| uvicorn | >=0.29.0 |

### Backend Dependencies (`backend/pyproject.toml`)
| Package | Version |
|---------|---------|
| fastapi | >=0.109.0 |
| uvicorn[standard] | >=0.27.0 |
| pydantic | >=2.5.0 |
| psycopg2-binary | >=2.9.9 |
| python-dotenv | >=1.0.0 |
| langchain | >=0.1.0 |
| langchain-openai | >=0.0.5 |
| python-multipart | >=0.0.24 |
| faster-whisper | >=1.2.1 |
| librosa | >=0.11.0 |

**Dev Dependencies:**
- pytest >=7.4.0
- black >=23.0.0
- ruff >=0.1.0

## Features

### Backend Services
- **Audio Transcription**: Convert speech to text using Whisper ASR model
- **Subtitle Generation**: Create SRT format subtitles with timestamps
- **Audio Analysis**: Analyze audio for English learning metrics (tempo, MFCC, spectral centroid, etc.)
- **Keyword Extraction**: Extract important vocabulary from subtitles
- **Story Generation**: Generate creative stories using LLM (DeepSeek API) for vocabulary practice
- **User Authentication**: JWT-based authentication with signup/login endpoints
- **Word Management**: Save and track unfamiliar words with familiarity scores
- **Reading Sessions**: Track reading practice sessions and mark unfamiliar words
- **RESTful API**: Well-documented API endpoints

### API Endpoints
- `GET /` - Welcome message
- `GET /health` - Health check
- `POST /signup` - User registration
- `POST /token` - Login and get access token
- `POST /save-word` - Save unfamiliar word (protected)
- `GET /my-words` - Get current user's saved words (protected)
- `GET /me` - Get current user info (protected)
- `POST /transcribe-audio/` - Transcribe audio and generate subtitles
- `POST /analyze-audio/` - Analyze audio for learning metrics
- `POST /upload-subtitle/` - Upload subtitle and extract keywords
- `POST /process-audio-with-subtitles/` - Process audio, generate subtitles, and extract keywords
- `POST /stories` - Generate story from word list using LLM
- `POST /reading_sessions` - Create a reading session (protected)
- `POST /reading_sessions/{session_id}/mark_word` - Mark word as unfamiliar during reading (protected)
- `GET /users/{user_id}/unfamiliar_words` - Get user's unfamiliar words (protected)

## Getting Started

### Prerequisites
- Node.js 18+ and PNPM
- Python 3.11+
- Docker (for database services)
- FFmpeg (for audio processing)
- At least 4GB RAM (for Whisper models)

### Installation

1. **Clone the repository**
   ```bash
   git clone <repository-url>
   cd english-learning-app
   ```

2. **Install all dependencies**
   ```bash
   pnpm run setup
   ```
   
   This will:
   - Install Node.js dependencies with PNPM
   - Install Python dependencies with UV
   - Start Docker containers

3. **Manual installation (alternative)**
   ```bash
   # Install Node.js dependencies
   pnpm install
   
   # Install Python dependencies
   cd backend && uv sync && cd ..
   
   # Start Docker services
   docker-compose up -d
   ```

### Development

Start all services in development mode:

```bash
# Start web frontend and backend
pnpm run dev

# Start mobile app only
pnpm run dev:mobile

# Start backend only
pnpm run dev:backend
```

### Database Commands

```bash
# Start database
pnpm run db:up

# Stop database
pnpm run db:down

# View database logs
pnpm run db:logs
```

### Cleanup

Remove all installed dependencies:

```bash
pnpm run clean
```

## Configuration

### Environment Variables

Create a `.env` file in the `backend/` directory with the following variables:

```bash
# Database
DATABASE_URL=postgresql://user:pass@localhost:5432/english_learning

# JWT Authentication
SECRET_KEY=your-secret-key-here

# LLM Configuration (DeepSeek API)
DEEPSEEK_API_KEY=your-deepseek-api-key
DEEPSEEK_BASE_URL=https://api.deepseek.com

# Optional: HuggingFace mirror for model downloads
HF_ENDPOINT=https://hf-mirror.com
HF_HUB_ENABLE_HF_TRANSFER=1
```

### Network Configuration

If you encounter network issues downloading models:

```bash
export HF_ENDPOINT=https://hf-mirror.com
export HF_HUB_ENABLE_HF_TRANSFER=1
```

## Usage Examples

### Transcribe Audio

```bash
curl -X POST "http://localhost:8001/transcribe-audio/" \
  -F "file=@audio.mp3" \
  -F "language=en" \
  -F "model_size=small"
```

### Analyze Audio

```bash
curl -X POST "http://localhost:8001/analyze-audio/" \
  -F "file=@audio.mp3"
```

## API Documentation

Once the backend is running, visit:
- Interactive API docs: http://localhost:8001/docs
- Alternative docs: http://localhost:8001/redoc

## Architecture

The application follows a monorepo structure with:
- **Backend**: FastAPI service handling audio processing and AI-powered transcription
- **Frontend Web**: React web application built with Vite, TypeScript, Tailwind CSS, and shadcn/ui components
  - Feature-based organization (`src/features/`)
  - AppShell layout pattern for consistent UI structure
  - React Router for client-side navigation
  - TanStack Query for server state management
  - Authentication context provider
- **Frontend Mobile**: React Native app built with Expo
- **Shared**: Common TypeScript utilities, components, hooks, and types shared across web and mobile
- **Product**: Feature specifications and roadmap documentation

### Web Application Architecture

The web frontend uses a modern component-based architecture:

```
src/
├── main.tsx              # Entry point - mounts React app
├── App.tsx               # Root component with providers
├── index.css             # Global styles (Tailwind)
├── features/             # Feature modules (business logic)
│   ├── home.tsx          # Home page
│   ├── subtitle.tsx      # Subtitle processing feature
│   └── progress.tsx      # Progress tracking feature
├── components/           # Reusable UI components
│   └── ui/               # shadcn/ui primitive components
├── layout/               # Layout components
│   └── app-shell.tsx     # Main application shell
├── router/               # Routing configuration
│   ├── routes.tsx        # Route definitions
│   └── navigation.tsx    # Navigation components
├── providers/            # React context providers
│   ├── index.tsx         # Provider exports
│   └── auth-context.tsx  # Authentication state
├── lib/                  # Utility functions
│   └── utils.ts          # Helper utilities (cn, etc.)
└── assets/               # Static assets
```

**Key Design Patterns:**
- **Feature-based organization**: Each feature contains its own components, hooks, and logic
- **AppShell layout**: Consistent header, sidebar, and content area structure
- **Context providers**: Centralized state management for auth and themes
- **shadcn/ui components**: Accessible, customizable UI primitives built on Radix UI
- **Tailwind CSS**: Utility-first styling with dark mode support

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

MIT License - see LICENSE file for details.

## Support

For issues and questions, please open an issue on the GitHub repository.
