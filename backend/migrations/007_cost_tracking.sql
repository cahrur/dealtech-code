-- Cost tracking columns
ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS tokens_input  INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS tokens_output INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cost_usd      DOUBLE PRECISION NOT NULL DEFAULT 0;

-- Diff summary and telegram chat tracking
ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS diff_stat TEXT,
    ADD COLUMN IF NOT EXISTS telegram_chat_id BIGINT;
