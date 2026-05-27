use sqlx::PgPool;
use std::path::PathBuf;
use time::OffsetDateTime;

pub async fn run(db: PgPool, worktrees_path: String) {
    loop {
        if let Err(e) = cleanup_old_worktrees(&db, &worktrees_path).await {
            tracing::error!("Cleanup worker error: {}", e);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
    }
}

async fn cleanup_old_worktrees(db: &PgPool, worktrees_path: &str) -> anyhow::Result<()> {
    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(7);
    let base = PathBuf::from(worktrees_path).canonicalize().unwrap_or_else(|_| PathBuf::from(worktrees_path));

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
        if let Some(path_str) = run.worktree_path {
            let p = PathBuf::from(&path_str);

            // Security: only remove paths that are under the expected worktrees base
            let canonical = match p.canonicalize() {
                Ok(c) => c,
                Err(_) => {
                    // Path doesn't exist — just clear DB
                    sqlx::query("UPDATE agent_runs SET worktree_path = NULL WHERE id = $1")
                        .bind(run.id).execute(db).await?;
                    continue;
                }
            };
            if !canonical.starts_with(&base) {
                tracing::warn!(run_id = %run.id, path = %path_str, "Skipping cleanup: path outside worktrees_path");
                continue;
            }

            // Remove directory if still exists
            if canonical.exists() {
                if let Err(e) = tokio::fs::remove_dir_all(&canonical).await {
                    tracing::warn!("Failed to remove worktree {}: {}", path_str, e);
                } else {
                    tracing::info!("Cleaned up worktree: {}", path_str);
                }
            }

            sqlx::query("UPDATE agent_runs SET worktree_path = NULL WHERE id = $1")
                .bind(run.id).execute(db).await?;
        }
    }
    Ok(())
}
