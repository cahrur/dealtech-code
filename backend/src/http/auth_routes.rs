use axum::{extract::State, http::StatusCode, Extension, Json};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::domain::auth::{LoginRequest, RefreshRequest, RegisterRequest, UserPublic};
use crate::error::{AppError, Result};
use crate::services::auth_service;

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    let resp = auth_service::register(&state.db, &state.config, req).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "data": resp, "success": true })),
    ))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>> {
    let resp = auth_service::login(&state.db, &state.config, req).await?;
    Ok(Json(serde_json::json!({ "data": resp, "success": true })))
}

pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> Result<Json<serde_json::Value>> {
    let resp = auth_service::refresh(&state.db, &state.config, req).await?;
    Ok(Json(serde_json::json!({ "data": resp, "success": true })))
}

pub async fn logout(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<serde_json::Value>> {
    auth_service::logout(&state.db, user_id).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn me(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let user = sqlx::query_as::<_, UserPublic>(
        "SELECT id, email, name, role FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;
    Ok(Json(serde_json::json!({ "data": user, "success": true })))
}
