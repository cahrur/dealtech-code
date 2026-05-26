use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    PreparingWorkspace,
    RunningAgent,
    CollectingDiff,
    RunningTests,
    AutoCommit,
    AutoPushOrPr,
    Completed,
    FailedAgent,
    FailedTests,
    BlockedByPolicy,
    Cancelled,
    TimedOut,
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self).unwrap_or_default();
        write!(f, "{}", s.as_str().unwrap_or("unknown"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoMode {
    AutoSafe,
    AutoTrusted,
    AutoFull,
}

impl Default for AutoMode {
    fn default() -> Self { Self::AutoTrusted }
}

impl std::fmt::Display for AutoMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self).unwrap_or_default();
        write!(f, "{}", s.as_str().unwrap_or("auto_trusted"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AgentRun {
    pub id: Uuid,
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub user_id: Uuid,
    pub prompt: String,
    pub status: String,
    pub auto_mode: String,
    pub model: String,
    pub openclaw_agent_id: String,
    pub openclaw_session_key: String,
    pub branch_name: Option<String>,
    pub worktree_path: Option<String>,
    pub commit_sha: Option<String>,
    pub pr_url: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub error_message: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub timeout_at: Option<OffsetDateTime>,
    pub tokens_input: i32,
    pub tokens_output: i32,
    pub cost_usd: f64,
    pub diff_stat: Option<String>,
    pub telegram_chat_id: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RunEvent {
    pub id: Uuid,
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub seq: i64,
    pub event_type: String,
    pub payload: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
pub struct CreateRunRequest {
    pub prompt: String,
    pub auto_mode: Option<String>,
    pub model: Option<String>,
}
