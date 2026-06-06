use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CodingSession {
    pub id: Uuid,
    pub project_id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub active_branch: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub branch_created_at: Option<OffsetDateTime>,
    pub task_summary: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub task_summary_updated_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: String,
    pub content: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub prompt: String,
    pub mode: Option<String>,  // "openclaw" | "hermes"
}
