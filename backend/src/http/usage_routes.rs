use axum::{extract::{Query, State}, Extension, Json};
use serde::Deserialize;

use crate::app_state::AppState;
use crate::domain::api_key::ApiKeyClaims;
use crate::error::Result;
use crate::services::usage_service;

#[derive(Debug, Deserialize)]
pub struct UsageLogsQuery {
    pub limit: Option<i64>,
}

pub async fn summary(
    State(state): State<AppState>,
    Extension(claims): Extension<ApiKeyClaims>,
) -> Result<Json<serde_json::Value>> {
    let data = usage_service::summary_for_key(&state.db, claims.key_id).await?;
    Ok(Json(serde_json::json!({ "data": data, "success": true })))
}

pub async fn logs(
    State(state): State<AppState>,
    Extension(claims): Extension<ApiKeyClaims>,
    Query(q): Query<UsageLogsQuery>,
) -> Result<Json<serde_json::Value>> {
    let data = usage_service::list_for_key(&state.db, claims.key_id, q.limit.unwrap_or(50)).await?;
    Ok(Json(serde_json::json!({ "data": data, "success": true })))
}
