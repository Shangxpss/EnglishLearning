from sqlalchemy.ext.declarative import declarative_base

Base = declarative_base()

from .user_models import User, UserWord
from .reading_models import ReadingSession
