use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;
use sqlx::PgPool;

use crate::config::Config;
use crate::error::{AppError, Result};

pub async fn prepare_workspace(
    config: &Config,
    team_slug: &str,
    project_slug: &str,
    repo_url: &str,
) -> Result<PathBuf> {
    let workspace_path = PathBuf::from(&config.workspaces_path)
        .join(team_slug)
        .join(project_slug);

    if workspace_path.exists() {
        let status = Command::new("git")
            .args(["-C", workspace_path.to_str().unwrap(), "fetch", "origin"])
            .status()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("git fetch: {}", e)))?;
        if !status.success() {
            return Err(AppError::Internal(anyhow::anyhow!("git fetch failed")));
        }
        let _ = Command::new("git")
            .args(["-C", workspace_path.to_str().unwrap(), "checkout", "main"])
            .status()
            .await;
        let _ = Command::new("git")
            .args(["-C", workspace_path.to_str().unwrap(), "pull", "--ff-only", "origin", "main"])
            .status()
            .await;
    } else {
        tokio::fs::create_dir_all(&workspace_path)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir: {}", e)))?;
        let status = Command::new("git")
            .args(["clone", repo_url, workspace_path.to_str().unwrap()])
            .status()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("git clone: {}", e)))?;
        if !status.success() {
            return Err(AppError::Internal(anyhow::anyhow!("git clone failed")));
        }
        // Empty repo has no HEAD — create initial commit so worktrees work
        let has_head = Command::new("git")
            .args(["-C", workspace_path.to_str().unwrap(), "rev-parse", "HEAD"])
            .output().await.map(|o| o.status.success()).unwrap_or(false);
        if !has_head {
            let _ = Command::new("git")
                .args(["-C", workspace_path.to_str().unwrap(),
                    "commit", "--allow-empty", "-m", "chore: initial commit"])
                .output().await;
            let _ = Command::new("git")
                .args(["-C", workspace_path.to_str().unwrap(), "push", "origin", "HEAD"])
                .output().await;
        }
    }
    Ok(workspace_path)
}

/// Get the active branch for a coding session, or create a new one.
/// Branch name format: ai/<session_id_short> — stable per session.
/// Persists the branch name back to coding_sessions.active_branch.
pub async fn get_or_create_session_branch(
    db: &PgPool,
    workspace_path: &PathBuf,
    session_id: Uuid,
) -> Result<String> {
    // Check if session already has an active branch
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT active_branch FROM coding_sessions WHERE id = $1"
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {}", e)))?
    .flatten();

    if let Some(branch) = existing {
        // Verify branch actually exists in git
        let exists = Command::new("git")
            .args(["-C", workspace_path.to_str().unwrap_or(""), "rev-parse", "--verify", &branch])
            .output().await.map(|o| o.status.success()).unwrap_or(false);
        if exists {
            tracing::info!(session_id = %session_id, branch = %branch, "Reusing existing session branch");
            return Ok(branch);
        }
        tracing::warn!(session_id = %session_id, branch = %branch, "Session branch missing from git, recreating");
    }

    // Create new branch from main
    let branch_name = format!("ai/session-{}", &session_id.to_string()[..8]);
    let output = Command::new("git")
        .args(["-C", workspace_path.to_str().unwrap_or(""), "branch", &branch_name, "main"])
        .output().await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git branch: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Branch may already exist but wasn't in DB — that's fine
        if !stderr.contains("already exists") {
            return Err(AppError::Internal(anyhow::anyhow!("git branch failed: {}", stderr.trim())));
        }
    }

    // Persist to DB
    sqlx::query(
        "UPDATE coding_sessions SET active_branch=$1, branch_created_at=NOW() WHERE id=$2"
    )
    .bind(&branch_name)
    .bind(session_id)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB update: {}", e)))?;

    tracing::info!(session_id = %session_id, branch = %branch_name, "Created new session branch");
    Ok(branch_name)
}

pub async fn create_worktree(
    config: &Config,
    team_slug: &str,
    project_slug: &str,
    run_id: Uuid,
    branch_name: &str,
) -> Result<PathBuf> {
    let workspace_path = PathBuf::from(&config.workspaces_path)
        .join(team_slug)
        .join(project_slug);
    let worktree_path = PathBuf::from(&config.worktrees_path)
        .join(team_slug)
        .join(project_slug)
        .join(format!("run_{}", run_id));

    tokio::fs::create_dir_all(worktree_path.parent().unwrap())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir worktree parent: {}", e)))?;

    // First, force-remove any existing worktree at the target path (from failed runs)
    tracing::debug!(
        workspace = %workspace_path.display(),
        branch = %branch_name,
        "Cleaning up any existing worktree/branch before worktree creation"
    );
    let _ = Command::new("git")
        .args(["-C", workspace_path.to_str().unwrap(), "worktree", "remove", "--force", worktree_path.to_str().unwrap()])
        .status()
        .await;

    // Prune stale worktree entries
    let _ = Command::new("git")
        .args(["-C", workspace_path.to_str().unwrap(), "worktree", "prune"])
        .status()
        .await;

    tracing::info!(
        workspace = %workspace_path.display(),
        worktree = %worktree_path.display(),
        branch = %branch_name,
        run_id = %run_id,
        "Creating worktree"
    );

    // If branch already exists, use it directly; otherwise create new
    let branch_exists = Command::new("git")
        .args(["-C", workspace_path.to_str().unwrap(), "rev-parse", "--verify", branch_name])
        .output().await.map(|o| o.status.success()).unwrap_or(false);

    let mut args = vec![
        "-C", workspace_path.to_str().unwrap(),
        "worktree", "add",
        worktree_path.to_str().unwrap(),
    ];
    let branch_flag;
    if branch_exists {
        args.push(branch_name);
        branch_flag = String::new(); // unused
    } else {
        branch_flag = branch_name.to_string();
        args.push("-b");
        args.push(&branch_flag);
    }

    let output = Command::new("git")
        .args(&args)
        .output()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git worktree add: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(
            workspace = %workspace_path.display(),
            branch = %branch_name,
            stderr = %stderr,
            "git worktree add failed"
        );
        return Err(AppError::Internal(anyhow::anyhow!(
            "git worktree add failed for branch {}: {}",
            branch_name,
            stderr.trim()
        )));
    }
    Ok(worktree_path)
}

/// Cleanup worktree after a run — removes the worktree dir but KEEPS the branch.
/// Branch persists per session so the next run in the same session can reuse it.
pub async fn cleanup_worktree(
    worktree_path: &PathBuf,
    workspace_path: &PathBuf,
) -> anyhow::Result<()> {
    // Unregister worktree from git
    let _ = Command::new("git")
        .args(["-C", workspace_path.to_str().unwrap_or(""), "worktree", "remove", "--force", worktree_path.to_str().unwrap_or("")])
        .status()
        .await;

    // Prune stale entries
    let _ = Command::new("git")
        .args(["-C", workspace_path.to_str().unwrap_or(""), "worktree", "prune"])
        .status()
        .await;

    // Remove directory if still present
    if worktree_path.exists() {
        let _ = tokio::fs::remove_dir_all(worktree_path).await;
    }

    tracing::info!(worktree = %worktree_path.display(), "Worktree cleaned up (branch kept)");
    Ok(())
}
