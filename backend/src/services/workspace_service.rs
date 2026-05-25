use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;

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

    // First, try to delete any existing branch with same name (from failed runs)
    tracing::debug!(
        workspace = %workspace_path.display(),
        branch = %branch_name,
        "Cleaning up any existing branch before worktree creation"
    );
    let _ = Command::new("git")
        .args(["-C", workspace_path.to_str().unwrap(), "branch", "-D", branch_name])
        .status()
        .await;

    // Also remove any stale worktree entry
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

    let output = Command::new("git")
        .args([
            "-C",
            workspace_path.to_str().unwrap(),
            "worktree",
            "add",
            worktree_path.to_str().unwrap(),
            "-b",
            branch_name,
        ])
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

pub async fn cleanup_worktree(worktree_path: &PathBuf) -> anyhow::Result<()> {
    if worktree_path.exists() {
        tokio::fs::remove_dir_all(worktree_path).await?;
    }
    Ok(())
}
