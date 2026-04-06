from pydantic_settings import BaseSettings


class Settings(BaseSettings):
    DATABASE_URL: str = "postgresql://user:pass@localhost:5432/english_learning"
    OPENAI_API_KEY: str = ""
    ENVIRONMENT: str = "development"


settings = Settings()
