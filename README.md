# English Learning App

A comprehensive English learning platform with a monorepo architecture featuring a FastAPI backend, React Native mobile app, and shared components.

## Project Structure

```
english-learning-app/
├── backend/                    # Python FastAPI backend
│   ├── app/
│   │   ├── main.py            # FastAPI application entry point
│   │   ├── models/            # Pydantic response models
│   │   └── services/          # Audio processing & subtitle generation
│   ├── requirements.txt       # Python dependencies
│   ├── pyproject.toml         # UV package manager config
│   └── README.md              # Backend-specific documentation
├── frontend/
│   ├── mobile/                # React Native mobile app (Expo)
│   │   └── package.json
│   ├── shared/                # Shared TypeScript components
│   │   └── package.json
│   └── web/                   # Web application (placeholder)
├── product/                   # Product documentation
│   ├── features/
│   └── roadmap.md
├── material/                  # Learning materials
│   └── subtitle/
├── package.json               # Root package.json with workspace scripts
├── pnpm-workspace.yaml        # PNPM workspace configuration
└── README.md                  # This file
```

## Tech Stack

### Frontend
- **React** 19.2.4 - UI library
- **React Native** 0.81.5 - Mobile development
- **Expo** ~54.0.33 - Mobile development framework
- **TypeScript** ~5.9.3 - Type safety
- **PNPM** - Package manager with workspace support

### Backend
- **Python** >=3.11 - Runtime
- **FastAPI** >=0.135.3 - Web framework
- **Faster-Whisper** >=1.2.1 - Speech-to-text engine
- **Librosa** >=0.10.0 - Audio analysis
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
- **RESTful API**: Well-documented API endpoints

### API Endpoints
- `GET /` - Welcome message
- `GET /health` - Health check
- `POST /transcribe-audio/` - Transcribe audio and generate subtitles
- `POST /analyze-audio/` - Analyze audio for learning metrics
- `POST /upload-subtitle/` - Upload subtitle and extract keywords

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

See `backend/.env.example` for required environment variables:
- API keys for external services
- Database connection strings
- Model configuration options

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
- **Frontend Mobile**: React Native app built with Expo
- **Shared**: Common TypeScript utilities and components
- **Product**: Feature specifications and roadmap documentation

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
