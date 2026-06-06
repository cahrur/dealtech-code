use axum::{extract::{Path, State}, http::StatusCode, Extension, Json};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::domain::session::CreateSessionRequest;
use crate::error::Result;
use crate::services::{openclaw_service, project_service, session_service};

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
    let started_at = std::time::Instant::now();
    let session = session_service::get(&state.db, session_id).await?;
    project_service::check_member(&state.db, session.project_id, user_id).await?;
    session_service::add_message(&state.db, session_id, "user", &req.prompt).await?;
    let mode = req.mode.as_deref().unwrap_or("openclaw");
    let model = if mode == "hermes" { "hermes-3" } else { "openclaw" };
    let history: Vec<(String, String)> = session_service::messages(&state.db, session_id)
        .await?
        .into_iter()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .filter(|m| !(m.role == "user" && m.content == req.prompt))
        .map(|m| (m.role, m.content))
        .collect();

    let route = openclaw_service::classify_prompt(&req.prompt);
    let history_len = history.len();
    let prompt_len = req.prompt.len();
    let reply = match route {
        openclaw_service::PromptRoute::Smalltalk => {
            openclaw_service::fallback_smalltalk_response(&req.prompt)
        }
        _ => {
            let input = openclaw_service::OpenClawRunInput {
                agent_id: "default".to_string(),
                session_key: format!("chat_{}", session_id),
                user_id: user_id.to_string(),
                instructions: openclaw_service::build_chat_instructions(),
                prompt: req.prompt.clone(),
                model: model.to_string(),
                history,
            };
            let response = openclaw_service::run_chat(&state.config, input)
                .await
                .map_err(crate::error::AppError::Internal)?;
            let safe = openclaw_service::sanitize_user_facing_response(&response);
            if safe.is_empty() { "Tidak ada respons.".to_string() } else { safe }
        }
    };
    session_service::add_message(&state.db, session_id, "assistant", &reply).await?;
    tracing::info!(
        session_id = %session_id,
        user_id = %user_id,
        route = ?route,
        history_len,
        prompt_len,
        reply_len = reply.len(),
        latency_ms = started_at.elapsed().as_millis(),
        "Session chat completed"
    );
    Ok(Json(serde_json::json!({ "data": { "response": reply }, "success": true })))
}
