use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn log(
    db: &PgPool,
    user_id: Option<Uuid>,
    project_id: Option<Uuid>,
    run_id: Option<Uuid>,
    action: &str,
    details: Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO audit_logs (id, user_id, project_id, run_id, action, details)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(project_id)
    .bind(run_id)
    .bind(action)
    .bind(details)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list(db: &PgPool, project_id: Uuid, limit: i64) -> anyhow::Result<Vec<Value>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        user_id: Option<Uuid>,
        run_id: Option<Uuid>,
        action: String,
        details: Value,
        created_at: time::OffsetDateTime,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, user_id, run_id, action, details, created_at
         FROM audit_logs WHERE project_id = $1
         ORDER BY created_at DESC LIMIT $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "user_id": r.user_id,
                "run_id": r.run_id,
                "action": r.action,
                "details": r.details,
                "created_at": r.created_at,
            })
        })
        .collect())
}
