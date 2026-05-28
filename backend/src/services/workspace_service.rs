use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;
use sqlx::PgPool;

use crate::config::Config;
use crate::error::{AppError, Result};

/// Returns a clean HTTPS GitHub URL — no embedded token.
/// SSH and token-embedded URLs are both normalized.
pub fn clean_github_url(repo_url: &str) -> String {
    // SSH: git@github.com:owner/repo.git → https://github.com/owner/repo.git
    if let Some(rest) = repo_url.strip_prefix("git@github.com:") {
        return format!("https://github.com/{}", rest);
    }
    // Strip existing token: https://x-access-token:TOKEN@github.com/... → https://github.com/...
    if let Some(rest) = repo_url.strip_prefix("https://") {
        if let Some(at_pos) = rest.find('@') {
            return format!("https://{}", &rest[at_pos + 1..]);
        }
    }
    repo_url.to_string()
}

/// Returns git env vars to authenticate via HTTP header.
/// Token is passed as an env var — NOT embedded in the URL — so it is
/// not visible in `ps aux` or git logs.
pub fn git_auth_env(token: &str) -> Vec<(String, String)> {
    vec![
        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        ("GIT_CONFIG_KEY_0".to_string(), "http.extraHeader".to_string()),
        ("GIT_CONFIG_VALUE_0".to_string(), format!("Authorization: token {}", token)),
        // Prevent git from hanging waiting for credentials in non-interactive env
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("GIT_ASKPASS".to_string(), "echo".to_string()),
    ]
}

/// Convert any GitHub repo URL to an authenticated HTTPS URL.
/// Kept for backward compat — prefer clean_github_url + git_auth_env for new code.
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
    // Use x-access-token in URL — http.extraHeader does not work with GitHub
    let auth_url = match github_token {
        Some(token) => inject_token_to_url(repo_url, token),
        None => repo_url.to_string(),
    };
    let clean_url = clean_github_url(repo_url); // for remote set-url (no token)
    let workspace_path = PathBuf::from(&config.workspaces_path)
        .join(team_slug)
        .join(project_slug);

    if workspace_path.exists() {
        // Check if it's actually a valid git repo (not a partial/failed clone)
        let git_dir = workspace_path.join(".git");
        if !git_dir.exists() {
            tracing::warn!(path = %workspace_path.display(), "workspace exists but not a git repo, re-cloning");
            let _ = tokio::fs::remove_dir_all(&workspace_path).await;
            // Fall through to clone below
        } else {
        // Update remote URL to use auth token
        if let Some(ref token) = github_token {
            let auth_url_for_remote = inject_token_to_url(repo_url, token);
            let _ = Command::new("git")
                .args(["-C", workspace_path.to_str().unwrap(), "remote", "set-url", "origin", &auth_url_for_remote])
                .status()
                .await;
        }
        let mut fetch_cmd = Command::new("git");
        fetch_cmd.args(["-C", workspace_path.to_str().unwrap(), "fetch", "origin"]);
        fetch_cmd.env("GIT_TERMINAL_PROMPT", "0");
        fetch_cmd.env("GIT_ASKPASS", "echo");
        let fetch_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(60),
            fetch_cmd.status()
        ).await
            .map_err(|_| AppError::Internal(anyhow::anyhow!("git fetch timed out after 60s")))?;
        let status = fetch_result
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
        } // close inner else (valid git repo branch)
    } // close outer if workspace_path.exists()

    if !workspace_path.exists() {
        tokio::fs::create_dir_all(&workspace_path)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir: {}", e)))?;
        let mut clone_cmd = Command::new("git");
        clone_cmd.args(["clone", "--depth=1", &auth_url, workspace_path.to_str().unwrap()]);
        clone_cmd.env("GIT_TERMINAL_PROMPT", "0");
        clone_cmd.env("GIT_ASKPASS", "echo");
        let clone_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(120),
            clone_cmd.status()
        ).await
            .map_err(|_| AppError::Internal(anyhow::anyhow!("git clone timed out after 120s")))?;
        let status = clone_result
            .map_err(|e| AppError::Internal(anyhow::anyhow!("git clone: {}", e)))?;
        if !status.success() {
            return Err(AppError::Internal(anyhow::anyhow!("git clone failed — cek apakah token GitHub punya akses ke repo ini")));
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
    force_new: bool,
) -> Result<String> {
    // Check if this session already has a branch assigned
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT active_branch FROM coding_sessions WHERE id = $1"
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {}", e)))?
    .flatten();

    if let Some(branch) = existing {
        let exists = Command::new("git")
            .args(["-C", workspace_path.to_str().unwrap_or(""), "rev-parse", "--verify", &branch])
            .output().await.map(|o| o.status.success()).unwrap_or(false);
        if exists {
            tracing::info!(session_id = %session_id, branch = %branch, "Reusing existing session branch");
            return Ok(branch);
        }
        tracing::warn!(session_id = %session_id, branch = %branch, "Session branch missing from git, recreating");
    }

    // If not force_new, try to reuse the latest branch from any session for same project+user
    if !force_new {
        let latest_branch: Option<String> = sqlx::query_scalar(
            "SELECT active_branch FROM coding_sessions \
             WHERE project_id = (SELECT project_id FROM coding_sessions WHERE id = $1) \
             AND user_id = (SELECT user_id FROM coding_sessions WHERE id = $1) \
             AND active_branch IS NOT NULL \
             AND id != $1 \
             ORDER BY created_at DESC LIMIT 1"
        )
        .bind(session_id)
        .fetch_optional(db)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {}", e)))?
        .flatten();

        if let Some(branch) = latest_branch {
            // Verify branch exists in git
            let exists = Command::new("git")
                .args(["-C", workspace_path.to_str().unwrap_or(""), "rev-parse", "--verify", &branch])
                .output().await.map(|o| o.status.success()).unwrap_or(false);
            if exists {
                // Assign this branch to current session too
                let _ = sqlx::query(
                    "UPDATE coding_sessions SET active_branch=$1, branch_created_at=NOW() WHERE id=$2"
                )
                .bind(&branch)
                .bind(session_id)
                .execute(db).await;
                tracing::info!(session_id = %session_id, branch = %branch, "Reusing branch from previous session");
                return Ok(branch);
            }
        }
    }

    // Create new branch from main
    let branch_name = format!("ai/session-{}", &session_id.to_string()[..8]);
    let output = Command::new("git")
        .args(["-C", workspace_path.to_str().unwrap_or(""), "branch", &branch_name, "main"])
        .output().await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git branch: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("already exists") {
            return Err(AppError::Internal(anyhow::anyhow!("git branch failed: {}", stderr.trim())));
        }
    }

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
