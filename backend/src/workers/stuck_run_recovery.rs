use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// Worker that runs every 60 seconds to recover stuck runs.
/// Queries for runs that have exceeded their timeout_at and marks them as failed.
pub async fn run(db: Arc<PgPool>, mut redis: ConnectionManager) {
    loop {
        if let Err(e) = recover_stuck_runs(db.as_ref(), &mut redis).await {
            tracing::error!("stuck_run_recovery error: {}", e);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

async fn recover_stuck_runs(db: &PgPool, redis: &mut ConnectionManager) -> anyhow::Result<()> {
    #[derive(sqlx::FromRow)]
    struct StuckRun {
        id: Uuid,
        session_id: Uuid,
    }

    let stuck_runs = sqlx::query_as::<_, StuckRun>(
        "SELECT id, session_id FROM agent_runs \
         WHERE status IN ('running_agent','processing','queued') \
         AND (timeout_at < NOW() OR (timeout_at IS NULL AND created_at < NOW() - INTERVAL '15 minutes'))"
    )
    .fetch_all(db)
    .await?;

    for run in stuck_runs {
        tracing::warn!(run_id = %run.id, "Recovering stuck run (timed out)");

        sqlx::query(
            "UPDATE agent_runs SET status='failed_agent', error_message='Run timed out (recovered)', finished_at=NOW() WHERE id=$1"
        )
        .bind(run.id)
        .execute(db)
        .await?;

        // Emit failure event to Redis so the client gets notified
        let payload = serde_json::json!({
            "type": "agent_run.failed",
            "run_id": run.id,
            "session_id": run.session_id,
            "data": {"reason": "Run timed out (recovered)"}
        });
        let channel = format!("session:{}", run.session_id);
        let _: Result<(), _> = redis.publish(channel, payload.to_string()).await;
    }

    Ok(())
}
