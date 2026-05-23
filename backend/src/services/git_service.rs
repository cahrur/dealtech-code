use std::path::PathBuf;
use tokio::process::Command;

use crate::error::{AppError, Result};

pub async fn get_diff(worktree_path: &PathBuf) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", worktree_path.to_str().unwrap(), "diff", "HEAD"])
        .output()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git diff: {}", e)))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn get_status(worktree_path: &PathBuf) -> Result<String> {
    let output = Command::new("git")
        .args(["-C", worktree_path.to_str().unwrap(), "status", "--porcelain"])
        .output()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git status: {}", e)))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn commit(worktree_path: &PathBuf, message: &str) -> Result<String> {
    let add = Command::new("git")
        .args(["-C", worktree_path.to_str().unwrap(), "add", "-A"])
        .status()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git add: {}", e)))?;
    if !add.success() {
        return Err(AppError::Internal(anyhow::anyhow!("git add failed")));
    }
    let out = Command::new("git")
        .args([
            "-C", worktree_path.to_str().unwrap(),
            "commit", "-m", message,
            "--author", "AI Agent <agent@ai-platform>",
        ])
        .output()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git commit: {}", e)))?;
    if !out.status.success() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "git commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let sha = Command::new("git")
        .args(["-C", worktree_path.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git rev-parse: {}", e)))?;
    Ok(String::from_utf8_lossy(&sha.stdout).trim().to_string())
}

pub async fn push_branch(worktree_path: &PathBuf, branch_name: &str) -> Result<()> {
    let status = Command::new("git")
        .args(["-C", worktree_path.to_str().unwrap(), "push", "origin", branch_name])
        .status()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git push: {}", e)))?;
    if !status.success() {
        return Err(AppError::Internal(anyhow::anyhow!("git push failed")));
    }
    Ok(())
}

pub async fn changed_files(worktree_path: &PathBuf) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["-C", worktree_path.to_str().unwrap(), "diff", "--name-only", "HEAD"])
        .output()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("git diff --name-only: {}", e)))?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect())
}
