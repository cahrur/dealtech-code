use sqlx::PgPool;
use std::path::PathBuf;
use time::OffsetDateTime;

pub async fn run(db: PgPool, _worktrees_path: String) {
    loop {
        if let Err(e) = cleanup_old_worktrees(&db).await {
            tracing::error!("Cleanup worker error: {}", e);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
    }
}

async fn cleanup_old_worktrees(db: &PgPool) -> anyhow::Result<()> {
    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(7);

    #[derive(sqlx::FromRow)]
    struct RunRow {
        id: uuid::Uuid,
        worktree_path: Option<String>,
    }

    let old_runs = sqlx::query_as::<_, RunRow>(
        "SELECT id, worktree_path FROM agent_runs
         WHERE status IN ('completed','failed_agent','failed_tests','cancelled','timed_out')
         AND finished_at < $1 AND worktree_path IS NOT NULL",
    )
    .bind(cutoff)
    .fetch_all(db)
    .await?;

    for run in old_runs {
        if let Some(path) = run.worktree_path {
            let p = PathBuf::from(&path);
            if p.exists() {
                if let Err(e) = tokio::fs::remove_dir_all(&p).await {
                    tracing::warn!("Failed to remove worktree {}: {}", path, e);
                } else {
                    tracing::info!("Cleaned up worktree: {}", path);
                }
            }
            sqlx::query("UPDATE agent_runs SET worktree_path = NULL WHERE id = $1")
                .bind(run.id)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}
