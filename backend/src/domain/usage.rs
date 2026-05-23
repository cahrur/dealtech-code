use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UsageLog {
    pub id: Uuid,
    pub api_key_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct UsageSummary {
    pub total_runs: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub by_model: Vec<ModelUsage>,
}

#[derive(Debug, Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub runs: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}
