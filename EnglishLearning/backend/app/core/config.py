from pydantic_settings import BaseSettings


class Settings(BaseSettings):
    DATABASE_URL: str = "postgresql://user:pass@localhost:5432/english_learning"
    OPENAI_API_KEY: str = ""
    DEEPSEEK_API_KEY: str = ""
    DEEPSEEK_BASE_URL: str = "https://api.deepseek.com"
    ENVIRONMENT: str = "development"


settings = Settings()
