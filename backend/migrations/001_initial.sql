CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE users (
    id           UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    email        TEXT        NOT NULL UNIQUE,
    password_hash TEXT       NOT NULL,
    name         TEXT        NOT NULL,
    role         TEXT        NOT NULL DEFAULT 'developer',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE user_sessions (
    id                  UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id             UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    refresh_token_hash  TEXT        NOT NULL UNIQUE,
    expires_at          TIMESTAMPTZ NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE projects (
    id                 UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    team_id            UUID        NOT NULL,
    name               TEXT        NOT NULL,
    slug               TEXT        NOT NULL,
    repo_url           TEXT        NOT NULL,
    openclaw_agent_id  TEXT        NOT NULL DEFAULT 'default',
    description        TEXT,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE project_members (
    id          UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    project_id  UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role        TEXT        NOT NULL DEFAULT 'developer',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(project_id, user_id)
);

CREATE TABLE coding_sessions (
    id          UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    project_id  UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id     UUID        NOT NULL REFERENCES users(id),
    title       TEXT        NOT NULL DEFAULT 'New Session',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE messages (
    id          UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    session_id  UUID        NOT NULL REFERENCES coding_sessions(id) ON DELETE CASCADE,
    role        TEXT        NOT NULL,
    content     TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE agent_runs (
    id                    UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    session_id            UUID        NOT NULL REFERENCES coding_sessions(id),
    project_id            UUID        NOT NULL REFERENCES projects(id),
    user_id               UUID        NOT NULL REFERENCES users(id),
    prompt                TEXT        NOT NULL,
    status                TEXT        NOT NULL DEFAULT 'queued',
    auto_mode             TEXT        NOT NULL DEFAULT 'auto_trusted',
    openclaw_agent_id     TEXT        NOT NULL,
    openclaw_session_key  TEXT        NOT NULL,
    branch_name           TEXT,
    worktree_path         TEXT,
    commit_sha            TEXT,
    pr_url                TEXT,
    started_at            TIMESTAMPTZ,
    finished_at           TIMESTAMPTZ,
    error_message         TEXT,
    usage                 JSONB       NOT NULL DEFAULT '{}',
    metadata              JSONB       NOT NULL DEFAULT '{}',
    created_at            TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE run_events (
    id          UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    run_id      UUID        NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    session_id  UUID        NOT NULL REFERENCES coding_sessions(id),
    seq         BIGINT      NOT NULL,
    event_type  TEXT        NOT NULL,
    payload     JSONB       NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(session_id, seq)
);

CREATE TABLE project_policies (
    id          UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    project_id  UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    auto_mode   TEXT        NOT NULL DEFAULT 'auto_trusted',
    policy      JSONB       NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(project_id)
);

CREATE TABLE audit_logs (
    id          UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id     UUID        REFERENCES users(id),
    project_id  UUID        REFERENCES projects(id),
    run_id      UUID        REFERENCES agent_runs(id),
    action      TEXT        NOT NULL,
    details     JSONB       NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_project_members_user       ON project_members(user_id);
CREATE INDEX idx_project_members_project    ON project_members(project_id);
CREATE INDEX idx_coding_sessions_project    ON coding_sessions(project_id);
CREATE INDEX idx_messages_session           ON messages(session_id);
CREATE INDEX idx_agent_runs_session         ON agent_runs(session_id);
CREATE INDEX idx_agent_runs_project         ON agent_runs(project_id);
CREATE INDEX idx_agent_runs_status          ON agent_runs(status);
CREATE INDEX idx_run_events_session_seq     ON run_events(session_id, seq);
CREATE INDEX idx_audit_logs_project         ON audit_logs(project_id);
CREATE INDEX idx_user_sessions_user         ON user_sessions(user_id);
