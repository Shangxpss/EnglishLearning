import uvicorn
import os
from app.main import app  # noqa: F401

if __name__ == "__main__":
    port = int(os.getenv("PORT", 8000))
    host = os.getenv("HOST", "0.0.0.0")

    print(f"Starting A English Learning API server on {host}:{port}")
    print("API Documentation available at http://localhost:8000/docs")

    uvicorn.run(
        "app.main:app",
        host=host,
        port=port,
        reload=True,  # Set to False in production
        log_level="info"
    )
