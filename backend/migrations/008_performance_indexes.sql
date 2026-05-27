-- Performance indexes for high-traffic queries

-- agent_runs: status polling (worker picks up queued runs every 5s)
CREATE INDEX IF NOT EXISTS idx_agent_runs_status ON agent_runs(status);

-- agent_runs: user rate limiting query
CREATE INDEX IF NOT EXISTS idx_agent_runs_user_created ON agent_runs(user_id, created_at DESC);

-- agent_runs: project history (used by /runs command)
CREATE INDEX IF NOT EXISTS idx_agent_runs_project_user ON agent_runs(project_id, user_id, created_at DESC);

-- agent_runs: timeout recovery worker
CREATE INDEX IF NOT EXISTS idx_agent_runs_timeout ON agent_runs(timeout_at) WHERE status IN ('running_agent', 'processing', 'queued');

-- agent_runs: cleanup worker (finished old runs)
CREATE INDEX IF NOT EXISTS idx_agent_runs_finished ON agent_runs(finished_at) WHERE worktree_path IS NOT NULL;

-- coding_sessions: get_or_create_session (daily session lookup)
CREATE INDEX IF NOT EXISTS idx_coding_sessions_project_user_date ON coding_sessions(project_id, user_id, created_at DESC);

-- coding_sessions: branch reuse lookup
CREATE INDEX IF NOT EXISTS idx_coding_sessions_branch ON coding_sessions(project_id, user_id, active_branch) WHERE active_branch IS NOT NULL;

-- telegram_users: auth lookup on every message
CREATE INDEX IF NOT EXISTS idx_telegram_users_telegram_id ON telegram_users(telegram_id);

-- project_members: membership check on every API call
CREATE INDEX IF NOT EXISTS idx_project_members_user ON project_members(user_id, project_id);
