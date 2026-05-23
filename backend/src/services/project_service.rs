use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::project::{CreateProjectRequest, Project, UpdateProjectRequest};
use crate::error::{AppError, Result};

pub async fn list(db: &PgPool, user_id: Uuid) -> Result<Vec<Project>> {
    let projects = sqlx::query_as::<_, Project>(
        "SELECT p.* FROM projects p
         INNER JOIN project_members pm ON pm.project_id = p.id
         WHERE pm.user_id = $1
         ORDER BY p.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(projects)
}

pub async fn get(db: &PgPool, project_id: Uuid, user_id: Uuid) -> Result<Project> {
    sqlx::query_as::<_, Project>(
        "SELECT p.* FROM projects p
         INNER JOIN project_members pm ON pm.project_id = p.id
         WHERE p.id = $1 AND pm.user_id = $2",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("Project not found".to_string()))
}

pub async fn create(
    db: &PgPool,
    user_id: Uuid,
    team_id: Uuid,
    req: CreateProjectRequest,
) -> Result<Project> {
    let slug = req.name.to_lowercase().replace(' ', "-");
    let project = sqlx::query_as::<_, Project>(
        "INSERT INTO projects (id, team_id, name, slug, repo_url, openclaw_agent_id, description)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(team_id)
    .bind(&req.name)
    .bind(&slug)
    .bind(&req.repo_url)
    .bind(&req.openclaw_agent_id)
    .bind(&req.description)
    .fetch_one(db)
    .await?;

    sqlx::query(
        "INSERT INTO project_members (id, project_id, user_id, role) VALUES ($1, $2, $3, 'owner')",
    )
    .bind(Uuid::new_v4())
    .bind(project.id)
    .bind(user_id)
    .execute(db)
    .await?;

    Ok(project)
}

pub async fn update(
    db: &PgPool,
    project_id: Uuid,
    req: UpdateProjectRequest,
) -> Result<Project> {
    sqlx::query_as::<_, Project>(
        "UPDATE projects SET
            name = COALESCE($1, name),
            repo_url = COALESCE($2, repo_url),
            openclaw_agent_id = COALESCE($3, openclaw_agent_id),
            description = COALESCE($4, description),
            updated_at = NOW()
         WHERE id = $5 RETURNING *",
    )
    .bind(&req.name)
    .bind(&req.repo_url)
    .bind(&req.openclaw_agent_id)
    .bind(&req.description)
    .bind(project_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("Project not found".to_string()))
}

pub async fn check_member(db: &PgPool, project_id: Uuid, user_id: Uuid) -> Result<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT role FROM project_members WHERE project_id = $1 AND user_id = $2",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::Forbidden("Not a project member".to_string()))
}
