from sqlalchemy import Column, Integer, String, ForeignKey, DateTime, Float
from sqlalchemy.orm import relationship
from . import Base

class User(Base):
    __tablename__ = "users"

    id = Column(Integer, primary_key=True, index=True)
    username = Column(String(50), unique=True, index=True)
    email = Column(String(100), unique=True, index=True)
    password_hash = Column(String(255))
    created_at = Column(DateTime)
    updated_at = Column(DateTime)

    # Relationship to UserWord
    words = relationship("UserWord", back_populates="user")

class UserWord(Base):
    __tablename__ = "user_words"

    id = Column(Integer, primary_key=True, index=True)
    user_id = Column(Integer, ForeignKey("users.id"), index=True)
    word = Column(String(100), index=True)
    score = Column(Integer)
    familiarity = Column(String(50))
    created_at = Column(DateTime)
    updated_at = Column(DateTime)

    # Relationship to User
    user = relationship("User", back_populates="words")
