use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;
use sqlx::PgPool;

use crate::config::Config;
use crate::error::{AppError, Result};

/// Convert any GitHub repo URL to an authenticated HTTPS URL.
/// Handles:
///   git@github.com:owner/repo.git  → https://x-access-token:TOKEN@github.com/owner/repo.git
///   https://github.com/owner/repo  → https://x-access-token:TOKEN@github.com/owner/repo
///   https://x-access-token:...@github.com/... → unchanged (already has token)
pub fn inject_token_to_url(repo_url: &str, token: &str) -> String {
    // SSH format: git@github.com:owner/repo.git
    if let Some(rest) = repo_url.strip_prefix("git@github.com:") {
        return format!("https://x-access-token:{}@github.com/{}", token, rest);
    }
    // Plain HTTPS without token
    if let Some(rest) = repo_url.strip_prefix("https://github.com/") {
        return format!("https://x-access-token:{}@github.com/{}", token, rest);
    }
    // Already has credentials or unknown format — return as-is
    repo_url.to_string()
}

pub async fn prepare_workspace(
    config: &Config,
    team_slug: &str,
    project_slug: &str,
    repo_url: &str,
    github_token: Option<&str>,
) -> Result<PathBuf> {
    // Use authenticated URL when token is available
    let effective_url = match github_token {
        Some(token) => inject_token_to_url(repo_url, token),
        None => repo_url.to_string(),
    };
    let workspace_path = PathBuf::from(&config.workspaces_path)
        .join(team_slug)
        .join(project_slug);

    if workspace_path.exists() {
        // Update remote URL to use current token (token may have rotated)
        if github_token.is_some() {
            let _ = Command::new("git")
                .args(["-C", workspace_path.to_str().unwrap(), "remote", "set-url", "origin", &effective_url])
                .status()
                .await;
        }
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
            .args(["clone", &effective_url, workspace_path.to_str().unwrap()])
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

        // Handle "already checked out" — stale worktree from a crashed/timed-out run.
        // Find and force-remove the stale worktree, then retry once.
        if stderr.contains("already checked out") {
            tracing::warn!(
                branch = %branch_name,
                "Branch already checked out in stale worktree, force-removing and retrying"
            );
            // git worktree list --porcelain to find which path has the branch
            let list_out = Command::new("git")
                .args(["-C", workspace_path.to_str().unwrap(), "worktree", "list", "--porcelain"])
                .output().await.ok();
            let list_str = list_out
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            // Parse: find worktree path that has this branch
            let mut stale_path: Option<String> = None;
            let mut current_wt: Option<String> = None;
            for line in list_str.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    current_wt = Some(p.to_string());
                } else if line == format!("branch refs/heads/{}", branch_name) {
                    stale_path = current_wt.clone();
                }
            }
            if let Some(stale) = stale_path {
                tracing::warn!(stale = %stale, "Removing stale worktree");
                let _ = Command::new("git")
                    .args(["-C", workspace_path.to_str().unwrap(), "worktree", "remove", "--force", &stale])
                    .status().await;
                let _ = Command::new("git")
                    .args(["-C", workspace_path.to_str().unwrap(), "worktree", "prune"])
                    .status().await;
                // Retry worktree add
                let retry = Command::new("git")
                    .args(&args)
                    .output().await
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("git worktree add retry: {}", e)))?;
                if retry.status.success() {
                    tracing::info!(worktree = %worktree_path.display(), branch = %branch_name, "Worktree created after stale cleanup");
                    return Ok(worktree_path);
                }
                let retry_err = String::from_utf8_lossy(&retry.stderr);
                return Err(AppError::Internal(anyhow::anyhow!(
                    "git worktree add failed after stale cleanup: {}", retry_err.trim()
                )));
            }
        }

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
