use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn publish_event(
    redis: &mut ConnectionManager,
    session_id: Uuid,
    event: &Value,
) -> anyhow::Result<()> {
    let channel = format!("session:{}", session_id);
    let payload = serde_json::to_string(event)?;
    redis.publish::<_, _, ()>(&channel, &payload).await?;
    Ok(())
}

pub async fn save_event(
    db: &PgPool,
    run_id: Uuid,
    session_id: Uuid,
    event_type: &str,
    payload: Value,
) -> anyhow::Result<i64> {
    let mut tx = db.begin().await?;
    // Lock session row to prevent concurrent seq generation (race condition fix)
    sqlx::query("SELECT id FROM coding_sessions WHERE id = $1 FOR UPDATE")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    let seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_events WHERE session_id = $1",
    )
    .bind(session_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO run_events (id, run_id, session_id, seq, event_type, payload)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(run_id)
    .bind(session_id)
    .bind(seq)
    .bind(event_type)
    .bind(&payload)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(seq)
}

pub async fn get_events_after(
    db: &PgPool,
    session_id: Uuid,
    after_seq: i64,
) -> anyhow::Result<Vec<Value>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        seq: i64,
        event_type: String,
        payload: Value,
        created_at: time::OffsetDateTime,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT seq, event_type, payload, created_at FROM run_events
         WHERE session_id = $1 AND seq > $2
         ORDER BY seq ASC LIMIT 500",
    )
    .bind(session_id)
    .bind(after_seq)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "seq": r.seq,
                "type": r.event_type,
                "payload": r.payload,
                "created_at": r.created_at,
            })
        })
        .collect())
}
