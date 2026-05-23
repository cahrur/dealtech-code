use crate::domain::policy::PolicyConfig;

pub struct PolicyEngine {
    pub config: PolicyConfig,
}

impl PolicyEngine {
    pub fn new(config: PolicyConfig) -> Self {
        Self { config }
    }

    pub fn is_command_allowed(&self, command: &str) -> bool {
        let deny_patterns = [
            "sudo",
            "su ",
            "systemctl",
            "docker",
            "kubectl",
            "rm -rf /",
            "chmod 777",
            "cat .env",
            "printenv",
            "> /dev/",
        ];
        for pattern in &deny_patterns {
            if command.contains(pattern) {
                tracing::warn!(command = command, "Command blocked by deny policy");
                return false;
            }
        }
        if self.config.auto_mode == "auto_safe" {
            let safe_prefixes = [
                "git status",
                "git diff",
                "git log",
                "git show",
                "git branch",
                "git checkout",
                "git switch",
                "git add",
                "git commit",
                "cargo test",
                "cargo fmt",
                "cargo clippy",
                "cargo build",
                "npm test",
                "npm run test",
                "npm run lint",
                "npm run build",
                "pnpm test",
                "pnpm run",
                "pytest",
                "go test ./...",
                "go build",
            ];
            return safe_prefixes.iter().any(|p| command.starts_with(p));
        }
        true
    }

    pub fn can_install_dependency(&self) -> bool {
        self.config.auto_mode != "auto_safe"
    }

    pub fn can_commit(&self) -> bool {
        self.config.git.auto_commit && self.config.auto_mode != "auto_safe"
    }

    pub fn can_push_branch(&self) -> bool {
        self.config.git.auto_push_branch && self.config.auto_mode != "auto_safe"
    }

    pub fn can_create_pr(&self) -> bool {
        self.config.git.auto_create_pr && self.config.auto_mode != "auto_safe"
    }

    pub fn max_run_minutes(&self) -> u32 {
        self.config.limits.max_run_minutes
    }

    pub fn max_retries(&self) -> u32 {
        self.config.limits.max_retries
    }
}
