use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::session::{CodingSession, CreateSessionRequest, Message};
use crate::error::{AppError, Result};

pub async fn create(
    db: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
    req: CreateSessionRequest,
) -> Result<CodingSession> {
    let title = req.title.unwrap_or_else(|| "New Session".to_string());
    sqlx::query_as::<_, CodingSession>(
        "INSERT INTO coding_sessions (id, project_id, user_id, title)
         VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(project_id)
    .bind(user_id)
    .bind(&title)
    .fetch_one(db)
    .await
    .map_err(Into::into)
}

pub async fn list(db: &PgPool, project_id: Uuid) -> Result<Vec<CodingSession>> {
    sqlx::query_as::<_, CodingSession>(
        "SELECT * FROM coding_sessions WHERE project_id = $1 ORDER BY created_at DESC",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(Into::into)
}

pub async fn get(db: &PgPool, session_id: Uuid) -> Result<CodingSession> {
    sqlx::query_as::<_, CodingSession>("SELECT * FROM coding_sessions WHERE id = $1")
        .bind(session_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("Session not found".to_string()))
}

pub async fn messages(db: &PgPool, session_id: Uuid) -> Result<Vec<Message>> {
    sqlx::query_as::<_, Message>(
        "SELECT * FROM messages WHERE session_id = $1 ORDER BY created_at ASC",
    )
    .bind(session_id)
    .fetch_all(db)
    .await
    .map_err(Into::into)
}

pub async fn add_message(
    db: &PgPool,
    session_id: Uuid,
    role: &str,
    content: &str,
) -> Result<Message> {
    sqlx::query_as::<_, Message>(
        "INSERT INTO messages (id, session_id, role, content) VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(session_id)
    .bind(role)
    .bind(content)
    .fetch_one(db)
    .await
    .map_err(Into::into)
}
