# Agents Documentation

This document provides an overview of the agents (services) used in the English Learning application, their functionality, and how to use them.

## Project Summary

The English Learning application is a comprehensive platform for language learning through audio-visual content. It features a backend with specialized agents for audio processing and subtitle generation, paired with frontend interfaces for web and mobile devices. The project uses modern technologies including FastAPI for the backend, React for the web interface, and React Native for mobile access. Key features include audio analysis, pronunciation evaluation, subtitle generation, and progress tracking. The application follows a modular architecture with clear separation between backend services and frontend components, enabling easy extension and maintenance.

## Table of Contents

- [Project Structure](#project-structure)
- [Backend Agents](#backend-agents)
  - [Audio Processor](#audio-processor)
  - [Subtitle Generator](#subtitle-generator)
- [Frontend Components](#frontend-components)
  - [Web Interface](#web-interface)
  - [Mobile Interface](#mobile-interface)
- [Usage Instructions](#usage-instructions)
  - [Setting Up the Backend](#setting-up-the-backend)
  - [Setting Up the Frontend](#setting-up-the-frontend)
  - [Running the Application](#running-the-application)

## Project Structure

```
AI/EnglishLearning/
├── backend/            # Backend services and API
│   ├── app/            # Main application code
│   │   ├── core/       # Core configuration
│   │   ├── models/     # Data models
│   │   ├── services/   # Agent services
│   │   └── main.py     # FastAPI application entry point
│   ├── audio/          # Audio files
│   └── utils/          # Utility functions
├── frontend/           # Frontend applications
│   ├── web/            # React web interface
│   ├── mobile/         # React Native mobile app
│   └── shared/         # Shared components and types
└── AGENTS.md           # This documentation file
```

## Backend Agents

### Audio Processor

**Location:** `backend/app/services/audio_processor.py`

**Functionality:**
- Processes audio files for English learning content
- Handles audio analysis and processing
- Provides audio-related utilities for the application

**Key Methods:**
- `process_audio()`: Processes audio files to extract features
- `analyze_pronunciation()`: Analyzes pronunciation quality
- `generate_audio_features()`: Generates features from audio data

### Subtitle Generator

**Location:** `backend/app/services/subtitle_generator.py`

**Functionality:**
- Generates subtitles for audio/video content
- Processes subtitle files (SRT format)
- Provides subtitle-related utilities

**Key Methods:**
- `generate_subtitles()`: Generates subtitles from audio
- `parse_subtitle_file()`: Parses existing subtitle files
- `sync_subtitles()`: Synchronizes subtitles with audio

## Frontend Components

### Web Interface

**Location:** `frontend/web/`

**Functionality:**
- React-based web application for English learning
- Provides user interface for interacting with the backend services
- Includes components for progress tracking and subtitle display

**Key Components:**
- `Subtitle` component: Displays subtitles for audio content
- `Progress` component: Tracks learning progress
- `AppShell` layout: Main application layout

### Mobile Interface

**Location:** `frontend/mobile/`

**Functionality:**
- React Native mobile application for English learning
- Provides on-the-go access to learning content
- Synchronizes with web interface data

## Usage Instructions

### Setting Up the Project

The project uses pnpm workspaces and provides scripts in the root package.json to simplify setup. Follow these steps:

1. **Navigate to the project root:**
   ```bash
   cd /home/shang/Desktop/project/finally-mico-service/AI/EnglishLearning
   ```

2. **Run the setup script:**
   ```bash
   pnpm setup
   ```
   This script will:
   - Install all frontend dependencies
   - Sync backend dependencies using uv
   - Start Docker containers for database services

3. **Set up environment variables:**
   - Copy `.env.example` to `.env` in the backend directory
   - Update the environment variables as needed

### Running the Application

#### Run All Services Concurrently

To start both the web frontend and backend together:

```bash
pnpm dev
```

#### Run Individual Services

1. **Backend API:**
   ```bash
   pnpm dev:backend
   ```
   - Accessible at `http://localhost:8000`
   - API documentation available at `http://localhost:8000/docs`

2. **Web Interface:**
   ```bash
   pnpm dev:web
   ```
   - Accessible at `http://localhost:5173`

3. **Mobile App:**
   ```bash
   pnpm dev:mobile
   ```
   - Use the Expo Go app to scan the QR code
   - Or run on an emulator/simulator

## Agent Interaction Flow

1. **User uploads or selects audio content** through the web or mobile interface
2. **Audio Processor** analyzes the audio and extracts features
3. **Subtitle Generator** creates subtitles for the audio content
4. **Frontend** displays the content with subtitles and provides interactive learning features
5. **Progress** is tracked and stored for the user

## Extending the Agents

To add new agents or extend existing ones:

1. **Check latest documentation:** Review the most recent AGENTS.md and project documentation before setting up new packages or components to ensure compatibility and follow current best practices.
2. **Backend agents:** Add new services in `backend/app/services/`
3. **Frontend components:** Add new components in the appropriate frontend directory
4. **Update this documentation** to reflect any changes

## Troubleshooting

- **Backend issues:** Check the server logs for error messages
- **Frontend issues:** Check the browser console or Expo logs
- **Audio processing issues:** Ensure audio files are in supported formats
- **Subtitle issues:** Verify subtitle files are in valid SRT format

## Conclusion

The agents in this English Learning application work together to provide a comprehensive learning experience. The backend agents handle audio processing and subtitle generation, while the frontend components provide an intuitive user interface for interacting with the content. By following the usage instructions, you can set up and run the application to support English learning through audio-visual content.
