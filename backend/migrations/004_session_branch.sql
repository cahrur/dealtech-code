-- Add active_branch to coding_sessions so each session persists its own branch
ALTER TABLE coding_sessions
    ADD COLUMN IF NOT EXISTS active_branch TEXT,
    ADD COLUMN IF NOT EXISTS branch_created_at TIMESTAMPTZ;
