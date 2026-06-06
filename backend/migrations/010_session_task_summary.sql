-- Persist compact task summary per coding session for faster follow-up context
ALTER TABLE coding_sessions
    ADD COLUMN IF NOT EXISTS task_summary TEXT,
    ADD COLUMN IF NOT EXISTS task_summary_updated_at TIMESTAMPTZ;
