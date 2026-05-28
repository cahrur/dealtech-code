use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::domain::agent_run::AgentRun;
use crate::domain::policy::PolicyConfig;
use crate::services::run_orchestrator;

pub async fn run(db: Arc<PgPool>, redis: ConnectionManager, config: Arc<Config>, semaphore: Option<Arc<Semaphore>>) {
    // Spawn the pub/sub listener alongside the polling loop
    let db2 = db.clone();
    let redis2 = redis.clone();
    let config2 = config.clone();
    let sem2 = semaphore.clone();
    tokio::spawn(async move {
        pubsub_listener(db2, redis2, config2, sem2).await;
    });

    // Polling loop as fallback (every 5 seconds)
    loop {
        if let Err(e) = process_queued(db.clone(), redis.clone(), config.clone(), semaphore.clone()).await {
            tracing::error!("agent_run_worker error: {}", e);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

/// Listens to Redis pub/sub channel "agent_run:queued" for immediate processing
async fn pubsub_listener(db: Arc<PgPool>, redis: ConnectionManager, config: Arc<Config>, semaphore: Option<Arc<Semaphore>>) {
    use redis::Client;

    // We need a separate connection for pub/sub (can't reuse ConnectionManager)
    let redis_url = config.redis_url();
    let client = match Client::open(redis_url.as_str()) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create Redis client for pubsub: {}", e);
            return;
        }
    };

    loop {
        match client.get_async_pubsub().await {
            Ok(mut pubsub) => {
                if let Err(e) = pubsub.subscribe("agent_run:queued").await {
                    tracing::error!("Failed to subscribe to agent_run:queued: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    continue;
                }
                tracing::info!("Subscribed to agent_run:queued channel");

                use futures_util::StreamExt;
                let mut msg_stream = pubsub.on_message();

                while let Some(msg) = msg_stream.next().await {
                    let payload: String = match msg.get_payload() {
                        Ok(p) => p,
                        Err(_) => continue,
                    };

                    // Parse run_id from payload
                    let run_id: Option<uuid::Uuid> = serde_json::from_str::<serde_json::Value>(&payload)
                        .ok()
                        .and_then(|v| v.get("run_id")?.as_str().map(|s| s.to_string()))
                        .and_then(|s| s.parse().ok());

                    if let Some(_run_id) = run_id {
                        // Process queued runs immediately
                        let db3 = db.clone();
                        let redis3 = redis.clone();
                        let config3 = config.clone();
                        let sem3 = semaphore.clone();
                        tokio::spawn(async move {
                            if let Err(e) = process_queued(db3, redis3, config3, sem3).await {
                                tracing::error!("agent_run_worker (pubsub trigger) error: {}", e);
                            }
                        });
                    }
                }
                tracing::warn!("pubsub stream ended, reconnecting...");
            }
            Err(e) => {
                tracing::error!("Failed to get pubsub connection: {}", e);
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

async fn process_queued(
    db: Arc<PgPool>,
    redis: ConnectionManager,
    config: Arc<Config>,
    semaphore: Option<Arc<Semaphore>>,
) -> anyhow::Result<()> {
    #[derive(sqlx::FromRow)]
    struct ProjectInfo { slug: String, repo_url: String }

    let queued = sqlx::query_as::<_, AgentRun>(
        "SELECT * FROM agent_runs WHERE status = 'queued' ORDER BY created_at ASC LIMIT 5",
    )
    .fetch_all(db.as_ref())
    .await?;

    for run in queued {
        let proj = sqlx::query_as::<_, ProjectInfo>(
            "SELECT slug, repo_url FROM projects WHERE id = $1",
        )
        .bind(run.project_id)
        .fetch_optional(db.as_ref())
        .await?;

        if let Some(p) = proj {
            // Load policy from DB; fall back to defaults if project has no policy row yet
            #[derive(sqlx::FromRow)]
            struct PolicyRow { auto_mode: String, policy: serde_json::Value }

            let policy_config = sqlx::query_as::<_, PolicyRow>(
                "SELECT auto_mode, policy FROM project_policies WHERE project_id = $1",
            )
            .bind(run.project_id)
            .fetch_optional(db.as_ref())
            .await
            .ok()
            .flatten()
            .and_then(|row| {
                let mut cfg: PolicyConfig = serde_json::from_value(row.policy).ok()?;
                cfg.auto_mode = row.auto_mode;
                Some(cfg)
            })
            .unwrap_or_default();

            // Mark as processing immediately to prevent duplicate pickup
            let updated = sqlx::query_scalar::<_, i64>(
                "UPDATE agent_runs SET status='processing' WHERE id=$1 AND status='queued' RETURNING 1"
            )
            .bind(run.id)
            .fetch_optional(db.as_ref())
            .await
            .ok()
            .flatten();

            if updated.is_none() {
                // Another worker already picked this up
                continue;
            }

            let db2 = db.clone();
            let redis2 = redis.clone();
            let cfg2 = config.clone();
            let run_id = run.id;
            let sem_clone = semaphore.clone();
            tracing::info!(run_id = %run_id, "worker: spawning task");
            tokio::spawn(async move {
                tracing::info!(run_id = %run_id, "worker: task started, acquiring semaphore");
                let _permit = match &sem_clone {
                    Some(sem) => Some(sem.acquire().await.expect("semaphore closed")),
                    None => None,
                };
                tracing::info!(run_id = %run_id, "worker: semaphore acquired, calling execute_run");
                run_orchestrator::execute_run(
                    db2, redis2, cfg2, run_id,
                    "default".to_string(), p.slug, p.repo_url,
                    policy_config,
                ).await;
                tracing::info!(run_id = %run_id, "worker: execute_run finished");
            });
        }
    }
    Ok(())
}
