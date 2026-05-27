use sqlx::PgPool;
use redis::aio::ConnectionManager;

const CHECK_INTERVAL_SECS: u64 = 3600; // check every hour
const DEFAULT_THRESHOLD_PCT: u64 = 85;  // alert at 85% usage

pub async fn run(
    config: crate::config::Config,
    _db: PgPool,
    redis: ConnectionManager,
) {
    loop {
        if let Err(e) = check_disk(&config, redis.clone()).await {
            tracing::error!("Disk alert worker error: {}", e);
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(CHECK_INTERVAL_SECS)).await;
    }
}

async fn check_disk(
    config: &crate::config::Config,
    mut redis: ConnectionManager,
) -> anyhow::Result<()> {
    let threshold = config.disk_alert_threshold_pct.unwrap_or(DEFAULT_THRESHOLD_PCT);

    let paths = [
        ("/", "Root (/)"),
        ("/srv/ai-platform/workspaces", "Workspaces"),
        ("/srv/ai-platform/worktrees", "Worktrees"),
    ];

    let mut alerts: Vec<String> = Vec::new();

    for (path, label) in &paths {
        if let Ok(usage) = get_disk_usage_pct(path) {
            tracing::debug!("Disk usage {}: {}%", path, usage);
            if usage >= threshold {
                alerts.push(format!("⚠️ *{}*: {}% penuh", label, usage));
            }
        }
    }

    if alerts.is_empty() {
        return Ok(());
    }

    // Rate limit: only alert once per 6 hours per path
    let alert_key = "disk:last_alert";
    let last_alert: Option<i64> = redis::cmd("GET")
        .arg(alert_key)
        .query_async(&mut redis)
        .await
        .unwrap_or(None);

    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if let Some(ts) = last_alert {
        if now - ts < 6 * 3600 {
            tracing::debug!("Disk alert suppressed (cooldown)");
            return Ok(());
        }
    }

    // Set cooldown
    let _: std::result::Result<(), _> = redis::cmd("SETEX")
        .arg(alert_key)
        .arg(6u64 * 3600)
        .arg(now)
        .query_async(&mut redis)
        .await;

    // Send Telegram alert to all admin users
    if config.telegram_enabled && !config.telegram_bot_token.is_empty() {
        let msg = format!(
            "🚨 *Disk Space Alert*\n\n{}\n\nSegera bersihkan workspace atau tambah storage.",
            alerts.join("\n")
        );
        // Get admin chat IDs from config or use a hardcoded admin notification
        // We'll use the notify_telegram helper from run_orchestrator
        if let Some(admin_chat_id) = config.telegram_admin_chat_id {
            crate::services::run_orchestrator::notify_telegram(
                &config.telegram_bot_token,
                admin_chat_id,
                &msg,
            ).await;
            tracing::warn!("Disk alert sent to admin: {}", msg);
        }
    }

    Ok(())
}

/// Returns disk usage percentage for a given path using statvfs syscall.
fn get_disk_usage_pct(path: &str) -> anyhow::Result<u64> {
    use std::ffi::CString;

    let c_path = CString::new(path)?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret != 0 {
        anyhow::bail!("statvfs failed for {}", path);
    }

    let total = stat.f_blocks * stat.f_frsize;
    let free = stat.f_bfree * stat.f_frsize;
    if total == 0 {
        return Ok(0);
    }
    let used = total - free;
    Ok((used * 100 / total) as u64)
}
