CREATE TABLE telegram_users (
    id              UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    telegram_id     BIGINT      NOT NULL UNIQUE,
    user_id         UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name            TEXT        NOT NULL,
    active_project_id UUID      REFERENCES projects(id) ON DELETE SET NULL,
    added_by        UUID        REFERENCES users(id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_telegram_users_telegram_id ON telegram_users(telegram_id);
