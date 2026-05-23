use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;

use crate::config::Config;
use crate::error::{AppError, Result};

pub struct ContainerInfo {
    pub container_id: String,
    pub container_name: String,
}

pub async fn create(config: &Config, api_key_id: Uuid, api_key_name: &str) -> Result<ContainerInfo> {
    let safe_name: String = api_key_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let container_name = format!("ai-agent-{}-{}", safe_name, &api_key_id.to_string()[..8]);

    let workspace = PathBuf::from(&config.workspaces_path).join(api_key_id.to_string());
    tokio::fs::create_dir_all(&workspace)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir workspace: {}", e)))?;

    let output = Command::new("docker")
        .args([
            "run", "-d",
            "--name", &container_name,
            "--restart", "unless-stopped",
            "--network", "none",
            "--memory", "2g",
            "--cpus", "1.0",
            "--user", "nobody",
            "-v", &format!("{}:/workspace:rw", workspace.display()),
            "-e", &format!("API_KEY_ID={}", api_key_id),
            "openclaw-sandbox-common:bookworm-slim",
        ])
        .output()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("docker run: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Internal(anyhow::anyhow!("docker run failed: {}", stderr)));
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    tracing::info!(container_id = %container_id, name = %container_name, "Container created");
    Ok(ContainerInfo { container_id, container_name })
}

pub async fn destroy(container_id: &str) -> anyhow::Result<()> {
    let _ = Command::new("docker")
        .args(["stop", "--time", "5", container_id])
        .output()
        .await;

    let out = Command::new("docker")
        .args(["rm", "-f", container_id])
        .output()
        .await?;

    if out.status.success() {
        tracing::info!(container_id = %container_id, "Container destroyed");
    } else {
        tracing::warn!("docker rm {} failed: {}", container_id, String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

pub async fn status(container_id: &str) -> anyhow::Result<String> {
    let out = Command::new("docker")
        .args(["inspect", "--format", "{{.State.Status}}", container_id])
        .output()
        .await?;
    if !out.status.success() {
        return Ok("not_found".to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
