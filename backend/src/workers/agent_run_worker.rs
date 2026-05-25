use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::sync::Arc;

use crate::config::Config;
use crate::domain::agent_run::AgentRun;
use crate::domain::policy::PolicyConfig;
use crate::services::run_orchestrator;

pub async fn run(db: Arc<PgPool>, redis: ConnectionManager, config: Arc<Config>) {
    loop {
        if let Err(e) = process_queued(db.clone(), redis.clone(), config.clone()).await {
            tracing::error!("agent_run_worker error: {}", e);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

async fn process_queued(
    db: Arc<PgPool>,
    redis: ConnectionManager,
    config: Arc<Config>,
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

            // Mark as processing immediately to prevent duplicate pickup on next poll
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
            tokio::spawn(async move {
                run_orchestrator::execute_run(
                    db2, redis2, cfg2, run_id,
                    run.user_id.to_string(), p.slug, p.repo_url,
                    policy_config,
                ).await;
            });
        }
    }
    Ok(())
}
