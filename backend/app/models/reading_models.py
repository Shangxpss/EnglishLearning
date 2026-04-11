from sqlalchemy import Column, Integer, String, DateTime, Text, ForeignKey
from sqlalchemy.orm import relationship
from . import Base

class ReadingSession(Base):
    __tablename__ = "reading_sessions"

    id = Column(Integer, primary_key=True, index=True)
    user_id = Column(Integer, ForeignKey("users.id"), index=True)
    content = Column(Text)
    started_at = Column(DateTime)
    finished_at = Column(DateTime, nullable=True)

    # relationship if needed
    # user = relationship("User")
