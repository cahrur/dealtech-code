use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProjectPolicy {
    pub id: Uuid,
    pub project_id: Uuid,
    pub auto_mode: String,
    pub policy: serde_json::Value,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    pub auto_mode: String,
    pub limits: PolicyLimits,
    pub network: NetworkPolicy,
    pub git: GitPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyLimits {
    pub max_run_minutes: u32,
    pub max_command_minutes: u32,
    pub max_changed_files: u32,
    pub max_diff_lines: u32,
    pub max_retries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub default: String,
    pub dependency_install_window: bool,
    pub allowed_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitPolicy {
    pub auto_commit: bool,
    pub auto_push_branch: bool,
    pub auto_create_pr: bool,
    pub auto_merge: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            auto_mode: "auto_trusted".to_string(),
            limits: PolicyLimits {
                max_run_minutes: 30,
                max_command_minutes: 5,
                max_changed_files: 30,
                max_diff_lines: 3000,
                max_retries: 3,
            },
            network: NetworkPolicy {
                default: "none".to_string(),
                dependency_install_window: true,
                allowed_hosts: vec![
                    "registry.npmjs.org".to_string(),
                    "pypi.org".to_string(),
                    "crates.io".to_string(),
                ],
            },
            git: GitPolicy {
                auto_commit: true,
                auto_push_branch: true,
                auto_create_pr: true,
                auto_merge: false,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdatePolicyRequest {
    pub policy: serde_json::Value,
}
