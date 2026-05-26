use axum::{extract::{Path, Query, State}, http::StatusCode, Extension, Json};
use redis::AsyncCommands;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::domain::agent_run::{AgentRun, CreateRunRequest};
use crate::domain::policy::{PolicyConfig};
use crate::error::{AppError, Result};
use crate::services::{project_service, realtime_service, run_orchestrator, session_service};

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub after_seq: Option<i64>,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<CreateRunRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    let session = session_service::get(&state.db, session_id).await?;
    let project = project_service::get(&state.db, session.project_id, user_id).await?;
    project_service::check_member(&state.db, session.project_id, user_id).await?;

    let run = run_orchestrator::create_run(
        &state.db, session_id, session.project_id, user_id, req, &project.openclaw_agent_id,
    ).await.map_err(AppError::Internal)?;

    // Improvement 4: Publish to Redis channel so worker picks up immediately
    {
        let mut redis_conn = state.redis.clone();
        let payload = serde_json::json!({"run_id": run.id}).to_string();
        let _: std::result::Result<i64, _> = redis_conn.publish("agent_run:queued", &payload).await;
    }

    // Load policy dari DB; fallback ke default jika belum dikonfigurasi
    let policy_config = {
        let row = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT policy FROM project_policies WHERE project_id = $1"
        )
        .bind(session.project_id)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);
        match row {
            Some(val) => serde_json::from_value::<PolicyConfig>(val).unwrap_or_default(),
            None => PolicyConfig::default(),
        }
    };

    let db = Arc::new(state.db.clone());
    let redis = state.redis.clone();
    let config = state.config.clone();
    let run_id = run.id;
    let project_slug = project.slug.clone();
    let repo_url = project.repo_url.clone();

    tokio::spawn(async move {
        run_orchestrator::execute_run(
            db, redis, config, run_id,
            "default".to_string(), project_slug, repo_url,
            policy_config,
        ).await;
    });

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "data": run, "success": true }))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let run = sqlx::query_as::<_, AgentRun>("SELECT * FROM agent_runs WHERE id = $1")
        .bind(run_id).fetch_optional(&state.db).await?
        .ok_or_else(|| AppError::NotFound("Run not found".to_string()))?;
    project_service::check_member(&state.db, run.project_id, user_id).await?;
    Ok(Json(serde_json::json!({ "data": run, "success": true })))
}

pub async fn cancel(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let run = sqlx::query_as::<_, AgentRun>("SELECT * FROM agent_runs WHERE id = $1")
        .bind(run_id).fetch_optional(&state.db).await?
        .ok_or_else(|| AppError::NotFound("Run not found".to_string()))?;
    project_service::check_member(&state.db, run.project_id, user_id).await?;
    sqlx::query("UPDATE agent_runs SET status='cancelled', finished_at=NOW() WHERE id=$1")
        .bind(run_id).execute(&state.db).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn diff(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let run = sqlx::query_as::<_, AgentRun>("SELECT * FROM agent_runs WHERE id = $1")
        .bind(run_id).fetch_optional(&state.db).await?
        .ok_or_else(|| AppError::NotFound("Run not found".to_string()))?;
    project_service::check_member(&state.db, run.project_id, user_id).await?;
    let diff = if let Some(path) = &run.worktree_path {
        crate::services::git_service::get_diff(&std::path::PathBuf::from(path))
            .await.unwrap_or_default()
    } else { String::new() };
    Ok(Json(serde_json::json!({ "data": { "diff": diff }, "success": true })))
}

pub async fn events(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(run_id): Path<Uuid>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<serde_json::Value>> {
    let run = sqlx::query_as::<_, AgentRun>("SELECT * FROM agent_runs WHERE id = $1")
        .bind(run_id).fetch_optional(&state.db).await?
        .ok_or_else(|| AppError::NotFound("Run not found".to_string()))?;
    project_service::check_member(&state.db, run.project_id, user_id).await?;
    let events = realtime_service::get_events_after(
        &state.db, run.session_id, q.after_seq.unwrap_or(0),
    ).await.map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "data": events, "success": true })))
}
