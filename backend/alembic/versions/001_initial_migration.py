"""Initial migration

Revision ID: 001
Revises: 
Create Date: 2026-04-10 00:00:00.000000

"""
from alembic import op
import sqlalchemy as sa


# revision identifiers, used by Alembic.
revision = '001'
down_revision = None
branch_labels = None
depends_on = None


def upgrade() -> None:
    # Create users table
    op.create_table('users',
        sa.Column('id', sa.Integer(), nullable=False),
        sa.Column('username', sa.String(length=50), nullable=True),
        sa.Column('email', sa.String(length=100), nullable=True),
        sa.Column('password_hash', sa.String(length=255), nullable=True),
        sa.Column('created_at', sa.DateTime(), nullable=True),
        sa.Column('updated_at', sa.DateTime(), nullable=True),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_users_email'), 'users', ['email'], unique=True)
    op.create_index(op.f('ix_users_id'), 'users', ['id'], unique=False)
    op.create_index(op.f('ix_users_username'), 'users', ['username'], unique=True)
    
    # Create user_words table
    op.create_table('user_words',
        sa.Column('id', sa.Integer(), nullable=False),
        sa.Column('user_id', sa.Integer(), nullable=True),
        sa.Column('word', sa.String(length=100), nullable=True),
        sa.Column('score', sa.Integer(), nullable=True),
        sa.Column('familiarity', sa.String(length=50), nullable=True),
        sa.Column('created_at', sa.DateTime(), nullable=True),
        sa.Column('updated_at', sa.DateTime(), nullable=True),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_user_words_id'), 'user_words', ['id'], unique=False)
    op.create_index(op.f('ix_user_words_user_id'), 'user_words', ['user_id'], unique=False)
    op.create_index(op.f('ix_user_words_word'), 'user_words', ['word'], unique=False)


def downgrade() -> None:
    # Drop user_words table
    op.drop_index(op.f('ix_user_words_word'), table_name='user_words')
    op.drop_index(op.f('ix_user_words_user_id'), table_name='user_words')
    op.drop_index(op.f('ix_user_words_id'), table_name='user_words')
    op.drop_table('user_words')
    
    # Drop users table
    op.drop_index(op.f('ix_users_username'), table_name='users')
    op.drop_index(op.f('ix_users_id'), table_name='users')
    op.drop_index(op.f('ix_users_email'), table_name='users')
    op.drop_table('users')
