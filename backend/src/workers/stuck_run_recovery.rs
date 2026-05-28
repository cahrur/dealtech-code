use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::config::Config;
use crate::services::run_orchestrator::notify_telegram;

/// Worker that runs every 60 seconds to recover stuck runs.
/// Queries for runs that have exceeded their timeout_at and marks them as failed.
pub async fn run(db: Arc<PgPool>, mut redis: ConnectionManager, config: Arc<Config>) {
    loop {
        if let Err(e) = recover_stuck_runs(db.as_ref(), &mut redis, &config).await {
            tracing::error!("stuck_run_recovery error: {}", e);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

async fn recover_stuck_runs(db: &PgPool, redis: &mut ConnectionManager, config: &Config) -> anyhow::Result<()> {
    #[derive(sqlx::FromRow)]
    struct StuckRun {
        id: Uuid,
        session_id: Uuid,
        telegram_chat_id: Option<i64>,
    }

    let stuck_runs = sqlx::query_as::<_, StuckRun>(
        "SELECT id, session_id, telegram_chat_id FROM agent_runs \
         WHERE status IN ('running_agent','processing','queued') \
         AND timeout_at < NOW()"
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

        // Notify Telegram user so they're not left hanging
        if let Some(chat_id) = run.telegram_chat_id {
            // Clear active run key in Redis
            let key = format!("tg:active_run:{}", chat_id);
            let _: Result<(), _> = redis.del(&key).await;
            notify_telegram(
                &config.telegram_bot_token,
                chat_id,
                "⏰ Run kamu tadi timeout dan dihentikan otomatis. Silakan coba lagi.",
            ).await;
        }
    }

    Ok(())
}
