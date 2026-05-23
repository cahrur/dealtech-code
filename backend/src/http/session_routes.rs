use axum::{extract::{Path, State}, http::StatusCode, Extension, Json};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::domain::session::CreateSessionRequest;
use crate::error::Result;
use crate::services::{project_service, session_service};

pub async fn create(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    project_service::check_member(&state.db, project_id, user_id).await?;
    let session = session_service::create(&state.db, project_id, user_id, req).await?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "data": session, "success": true }))))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    project_service::check_member(&state.db, project_id, user_id).await?;
    let sessions = session_service::list(&state.db, project_id).await?;
    Ok(Json(serde_json::json!({ "data": sessions, "success": true })))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let session = session_service::get(&state.db, session_id).await?;
    project_service::check_member(&state.db, session.project_id, user_id).await?;
    Ok(Json(serde_json::json!({ "data": session, "success": true })))
}

pub async fn messages(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let session = session_service::get(&state.db, session_id).await?;
    project_service::check_member(&state.db, session.project_id, user_id).await?;
    let msgs = session_service::messages(&state.db, session_id).await?;
    Ok(Json(serde_json::json!({ "data": msgs, "success": true })))
}
