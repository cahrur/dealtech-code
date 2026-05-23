-- Drop sessions and user auth tables, replace with API key auth

-- Drop FK constraints referencing users
ALTER TABLE project_members DROP CONSTRAINT IF EXISTS project_members_user_id_fkey;
ALTER TABLE coding_sessions DROP CONSTRAINT IF EXISTS coding_sessions_user_id_fkey;
ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_user_id_fkey;
ALTER TABLE audit_logs DROP CONSTRAINT IF EXISTS audit_logs_user_id_fkey;

-- Drop user sessions table
DROP TABLE IF EXISTS user_sessions;

-- Drop users table
DROP TABLE IF EXISTS users;

-- Create api_keys table
CREATE TABLE api_keys (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    key_hash    TEXT NOT NULL UNIQUE,
    key_prefix  TEXT NOT NULL,
    role        TEXT NOT NULL DEFAULT 'developer' CHECK (role IN ('admin', 'developer', 'viewer')),
    created_by  TEXT NOT NULL DEFAULT 'system',
    last_used_at TIMESTAMPTZ,
    expires_at  TIMESTAMPTZ,
    revoked_at  TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_api_keys_key_hash ON api_keys (key_hash);
CREATE INDEX idx_api_keys_key_prefix ON api_keys (key_prefix);
