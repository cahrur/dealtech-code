-- Token & cost tracking per API key
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS container_id   TEXT;
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS container_name TEXT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS model TEXT NOT NULL DEFAULT 'claude-sonnet-4-6';

CREATE TABLE IF NOT EXISTS usage_logs (
    id             UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    api_key_id     UUID        REFERENCES api_keys(id) ON DELETE SET NULL,
    run_id         UUID        REFERENCES agent_runs(id) ON DELETE SET NULL,
    model          TEXT        NOT NULL DEFAULT 'unknown',
    input_tokens   BIGINT      NOT NULL DEFAULT 0,
    output_tokens  BIGINT      NOT NULL DEFAULT 0,
    total_tokens   BIGINT      NOT NULL DEFAULT 0,
    cost_usd       NUMERIC(12,8) NOT NULL DEFAULT 0,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_usage_logs_api_key    ON usage_logs(api_key_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_run        ON usage_logs(run_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_created_at ON usage_logs(created_at);
