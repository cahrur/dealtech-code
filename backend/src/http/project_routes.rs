use axum::{extract::{Path, State}, http::StatusCode, Extension, Json};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::domain::policy::{ProjectPolicy, UpdatePolicyRequest};
use crate::domain::project::{CreateProjectRequest, UpdateProjectRequest};
use crate::error::{AppError, Result};
use crate::services::{audit_service, project_service};

pub async fn list(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let projects = project_service::list(&state.db, user_id).await?;
    Ok(Json(serde_json::json!({ "data": projects, "success": true })))
}

pub async fn get_one(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let project = project_service::get(&state.db, project_id, user_id).await?;
    Ok(Json(serde_json::json!({ "data": project, "success": true })))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Json(mut req): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    if req.create_github_repo.unwrap_or(false) {
        match &state.config.github_token {
            Some(token) => {
                let repo = crate::infra::github::create_repo(
                    token,
                    &req.name,
                    req.description.as_deref(),
                    req.github_private.unwrap_or(true),
                    req.github_org.as_deref(),
                ).await.map_err(AppError::Internal)?;
                req.repo_url = Some(repo.clone_url);
            }
            None => return Err(AppError::BadRequest("GITHUB_TOKEN not configured".to_string())),
        }
    }
    if req.repo_url.is_none() {
        return Err(AppError::BadRequest(
            "repo_url required, or set create_github_repo: true".to_string(),
        ));
    }
    let project = project_service::create(&state.db, user_id, user_id, req).await?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "data": project, "success": true }))))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<UpdateProjectRequest>,
) -> Result<Json<serde_json::Value>> {
    project_service::check_member(&state.db, project_id, user_id).await?;
    let project = project_service::update(&state.db, project_id, req).await?;
    Ok(Json(serde_json::json!({ "data": project, "success": true })))
}

pub async fn get_policy(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    project_service::check_member(&state.db, project_id, user_id).await?;
    let policy = sqlx::query_as::<_, ProjectPolicy>(
        "SELECT * FROM project_policies WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(Json(serde_json::json!({ "data": policy, "success": true })))
}

pub async fn update_policy(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<UpdatePolicyRequest>,
) -> Result<Json<serde_json::Value>> {
    project_service::check_member(&state.db, project_id, user_id).await?;
    sqlx::query(
        "INSERT INTO project_policies (id, project_id, auto_mode, policy)
         VALUES ($1, $2, 'auto_trusted', $3)
         ON CONFLICT (project_id) DO UPDATE SET policy = $3, updated_at = NOW()",
    )
    .bind(Uuid::new_v4()).bind(project_id).bind(&req.policy)
    .execute(&state.db).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

pub async fn audit_logs(
    State(state): State<AppState>,
    Extension(user_id): Extension<Uuid>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    project_service::check_member(&state.db, project_id, user_id).await?;
    let logs = audit_service::list(&state.db, project_id, 100)
        .await.map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "data": logs, "success": true })))
}
