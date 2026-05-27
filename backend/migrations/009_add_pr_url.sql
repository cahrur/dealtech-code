-- Migration 009: add pr_url to agent_runs for /pr command
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS pr_url TEXT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS pr_number INTEGER;
