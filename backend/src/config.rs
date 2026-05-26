use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub app_port: u16,
    pub app_env: String,
    pub log_level: String,

    pub db_host: String,
    pub db_port: u16,
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,

    pub redis_host: String,
    pub redis_port: u16,

    pub admin_api_key: Option<String>,
    pub github_token: Option<String>,

    pub openclaw_base_url: String,
    pub openclaw_gateway_token: String,

    pub workspaces_path: String,
    pub worktrees_path: String,
    pub logs_path: String,

    // Production-readiness settings
    pub graceful_shutdown: bool,
    pub max_concurrent_runs: usize,
    pub openclaw_max_retries: u32,
    pub user_rate_limit_per_minute: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();
        Ok(Self {
            app_port: env::var("APP_PORT").unwrap_or_else(|_| "8080".into()).parse()?,
            app_env: env::var("APP_ENV").unwrap_or_else(|_| "development".into()),
            log_level: env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into()),

            db_host: env::var("DB_HOST").unwrap_or_else(|_| "localhost".into()),
            db_port: env::var("DB_PORT").unwrap_or_else(|_| "5432".into()).parse()?,
            db_name: env::var("DB_NAME").unwrap_or_else(|_| "aicode".into()),
            db_user: env::var("DB_USER").unwrap_or_else(|_| "postgres".into()),
            db_password: env::var("DB_PASSWORD")
                .map_err(|_| anyhow::anyhow!("DB_PASSWORD required"))?,

            redis_host: env::var("REDIS_HOST").unwrap_or_else(|_| "localhost".into()),
            redis_port: env::var("REDIS_PORT").unwrap_or_else(|_| "6379".into()).parse()?,

            admin_api_key: env::var("ADMIN_API_KEY").ok(),
            github_token: env::var("GITHUB_TOKEN").ok(),

            openclaw_base_url: env::var("OPENCLAW_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:18789".into()),
            openclaw_gateway_token: env::var("OPENCLAW_GATEWAY_TOKEN")
                .map_err(|_| anyhow::anyhow!("OPENCLAW_GATEWAY_TOKEN required"))?,

            workspaces_path: env::var("WORKSPACES_PATH")
                .unwrap_or_else(|_| "/srv/ai-platform/workspaces".into()),
            worktrees_path: env::var("WORKTREES_PATH")
                .unwrap_or_else(|_| "/srv/ai-platform/worktrees".into()),
            logs_path: env::var("LOGS_PATH")
                .unwrap_or_else(|_| "/srv/ai-platform/logs".into()),

            graceful_shutdown: env::var("GRACEFUL_SHUTDOWN")
                .unwrap_or_else(|_| "true".into())
                .parse()
                .unwrap_or(true),
            max_concurrent_runs: env::var("MAX_CONCURRENT_RUNS")
                .unwrap_or_else(|_| "5".into())
                .parse()
                .unwrap_or(5),
            openclaw_max_retries: env::var("OPENCLAW_MAX_RETRIES")
                .unwrap_or_else(|_| "3".into())
                .parse()
                .unwrap_or(3),
            user_rate_limit_per_minute: env::var("USER_RATE_LIMIT_PER_MINUTE")
                .unwrap_or_else(|_| "10".into())
                .parse()
                .unwrap_or(10),
        })
    }

    pub fn db_url(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            self.db_user, self.db_password, self.db_host, self.db_port, self.db_name
        )
    }

    pub fn redis_url(&self) -> String {
        format!("redis://{}:{}", self.redis_host, self.redis_port)
    }
}
