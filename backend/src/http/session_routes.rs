use axum::{extract::{Path, State}, http::StatusCode, Extension, Json};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::domain::session::{ChatRequest, CreateSessionRequest};
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

pub async fn chat(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<crate::domain::session::ChatRequest>,
) -> Result<Json<serde_json::Value>> {
    let session = session_service::get(&state.db, session_id).await?;
    project_service::check_member(&state.db, session.project_id, user_id).await?;
    session_service::add_message(&state.db, session_id, "user", &req.prompt).await?;
    let mode = req.mode.as_deref().unwrap_or("openclaw");
    let model = if mode == "hermes" { "hermes-3" } else { "openclaw" };
    let input = crate::services::openclaw_service::OpenClawRunInput {
        agent_id: "default".to_string(),
        session_key: format!("chat_{}", session_id),
        user_id: user_id.to_string(),
        instructions: "You are a helpful AI assistant. Answer clearly and concisely.".to_string(),
        prompt: req.prompt.clone(),
        model: model.to_string(),
    };
    let response = crate::services::openclaw_service::run_chat(&state.config, input)
        .await
        .map_err(crate::error::AppError::Internal)?;
    let reply = if response.is_empty() { "Tidak ada respons.".to_string() } else { response };
    session_service::add_message(&state.db, session_id, "assistant", &reply).await?;
    Ok(Json(serde_json::json!({ "data": { "response": reply }, "success": true })))
}
