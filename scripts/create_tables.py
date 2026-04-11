from backend.app.core.database import engine
from backend.app.models import Base

if __name__ == '__main__':
    print('Creating database tables...')
    Base.metadata.create_all(bind=engine)
    print('Done')
