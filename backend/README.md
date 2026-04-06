# English Learning Application

A comprehensive English learning application built with FastAPI that provides audio processing, transcription, and learning analytics.

## Features

- **Audio Transcription**: Convert speech to text using Whisper ASR model
- **Subtitle Generation**: Create SRT format subtitles with timestamps
- **Audio Analysis**: Analyze audio for English learning metrics
- **API Interface**: RESTful API for integration with other applications
- **Multiple Languages**: Support for various languages
- **Model Selection**: Choose from different Whisper model sizes

## Architecture

```
AI/EnglishLearning/
├── app/
│   ├── __init__.py
│   ├── main.py                 # FastAPI application
│   ├── models/
│   │   ├── __init__.py
│   │   └── response_models.py  # Pydantic models
│   └── services/
│       ├── __init__.py
│       ├── audio_processor.py  # Audio analysis
│       └── subtitle_generator.py # Transcription
├── server.py                   # Server startup
├── requirements.txt           # Dependencies
├── pyproject.toml             # Project configuration
├── README.md                  # This file
└── .env.example              # Environment variables
```

## Prerequisites

- Python 3.12+
- FFmpeg installed on the system
- At least 4GB RAM (for Whisper models)

## Installation

1. **Clone or navigate to the project directory**:
   ```bash
   cd /home/shang/Desktop/project/finally-mico-service/AI/EnglishLearning
   ```

2. **Create a virtual environment**:
   ```bash
   python3 -m venv venv
   source venv/bin/activate  # On Windows: venv\Scripts\activate
   ```

3. **Install dependencies**:
   ```bash
   pip install -r requirements.txt
   # or if using uv:
   uv pip install -r requirements.txt
   ```

4. **Network Configuration (if needed)**:
   If you encounter network issues downloading models, configure a mirror:
   ```bash
   export HF_ENDPOINT=https://hf-mirror.com
   export HF_HUB_ENABLE_HF_TRANSFER=1
   ```

## Running the Application

### Development Mode

```bash
python server.py
```

Or using uvicorn directly:

```bash
uvicorn app.main:app --reload --host 0.0.0.0 --port 8000
```

The API will be available at `http://localhost:8000` with interactive documentation at `http://localhost:8000/docs`

### Production Mode

```bash
uvicorn app.main:app --host 0.0.0.0 --port 8000 --workers 4
```

## API Endpoints

- `GET /` - Health check and welcome message
- `GET /health` - System health check
- `POST /transcribe-audio/` - Transcribe audio and generate subtitles
- `POST /analyze-audio/` - Analyze audio for learning metrics

## Usage Examples

### Audio Transcription

Upload an audio file to get transcribed text with timestamps:

```bash
curl -X POST "http://localhost:8000/transcribe-audio/" \
  -F "file=@your_audio.mp3" \
  -F "language=en" \
  -F "model_size=small"
```

### Audio Analysis

Analyze an audio file for learning metrics:

```bash
curl -X POST "http://localhost:8000/analyze-audio/" \
  -F "file=@your_audio.mp3"
```

## Configuration Options

- **Model Size**: Choose from "tiny", "small", "medium", "large", "turbo" based on your needs and computational resources
- **Language**: Specify the language of the audio ("en", "zh", "es", etc.)
- **Environment Variables**: See `.env.example` for configuration options

## Audio Processing Capabilities

The application analyzes the following metrics:

- **Duration**: Total length of the audio
- **Tempo**: Speaking pace estimation
- **MFCC**: Mel-frequency cepstral coefficients for speech analysis
- **Spectral Centroid**: Brightness of the audio
- **Zero Crossing Rate**: Frequency of sign changes in the signal
- **Silence Ratio**: Proportion of silent parts
- **Learning Level**: Suggested English proficiency level

## Model Performance

| Model | Size | Speed | Memory | Quality |
|-------|------|-------|--------|---------|
| tiny  | 75 MB | Fast | ~1 GB | Lower |
| small | 480 MB | Fast | ~2 GB | Better |
| medium | 1.5 GB | Medium | ~5 GB | Good |
| large | 3.0 GB | Slow | ~10 GB | Best |

## Security Considerations

- Validate file uploads (size, type)
- Implement rate limiting
- Sanitize inputs
- Use HTTPS in production

## Deployment

For production deployment:

1. Use a process manager like PM2 or systemd
2. Set up a reverse proxy (nginx)
3. Configure SSL certificates
4. Implement proper logging
5. Monitor resource usage

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests if applicable
5. Submit a pull request

## License

This project is licensed under the MIT License.