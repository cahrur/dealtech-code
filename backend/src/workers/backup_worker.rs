use redis::aio::ConnectionManager;

/// Runs a daily DB backup at BACKUP_HOUR (default 02:00 UTC).
/// Sends the backup file to TELEGRAM_ADMIN_CHAT_ID.
pub async fn run(config: crate::config::Config, redis: ConnectionManager) {
    loop {
        let wait_secs = secs_until_next_run(config.backup_hour_utc.unwrap_or(2));
        tracing::info!("Next scheduled backup in {}s ({:.1}h)", wait_secs, wait_secs as f64 / 3600.0);
        tokio::time::sleep(tokio::time::Duration::from_secs(wait_secs)).await;

        if let Err(e) = run_backup(&config, redis.clone()).await {
            tracing::error!("Scheduled backup failed: {}", e);
        }
    }
}

async fn run_backup(
    config: &crate::config::Config,
    mut redis: ConnectionManager,
) -> anyhow::Result<()> {
    let admin_chat_id = match config.telegram_admin_chat_id {
        Some(id) => id,
        None => {
            tracing::warn!("Scheduled backup: TELEGRAM_ADMIN_CHAT_ID not set, skipping");
            return Ok(());
        }
    };

    // Rate limit: skip if backup already ran in last 23h (prevents double-run on restart)
    let lock_key = "backup:last_run";
    let last: Option<i64> = redis::cmd("GET")
        .arg(lock_key)
        .query_async(&mut redis)
        .await
        .unwrap_or(None);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if let Some(ts) = last {
        if now - ts < 23 * 3600 {
            tracing::info!("Scheduled backup skipped (ran {}s ago)", now - ts);
            return Ok(());
        }
    }

    tracing::info!("Starting scheduled DB backup");

    let ts = {
        let t = time::OffsetDateTime::now_utc();
        format!("{:04}{:02}{:02}_{:02}{:02}{:02}",
            t.year(), t.month() as u8, t.day(),
            t.hour(), t.minute(), t.second())
    };

    let output = tokio::process::Command::new("docker")
        .args([
            "exec", "ai-platform-postgres-1",
            "sh", "-c",
            &format!("pg_dump -U postgres aicode | gzip"),
        ])
        .output()
        .await?;

    if !output.status.success() || output.stdout.is_empty() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("pg_dump failed: {}", err.chars().take(200).collect::<String>());
    }

    let size_kb = output.stdout.len() / 1024;
    tracing::info!("Backup created: {} KB", size_kb);

    // Mark backup ran
    let _: std::result::Result<(), _> = redis::cmd("SETEX")
        .arg(lock_key)
        .arg(25u64 * 3600) // 25h TTL
        .arg(now)
        .query_async(&mut redis)
        .await;

    if !config.telegram_enabled || config.telegram_bot_token.is_empty() {
        tracing::warn!("Telegram not configured, backup not sent");
        return Ok(());
    }

    // Send via Telegram
    if output.stdout.len() < 45 * 1024 * 1024 {
        let url = format!("https://api.telegram.org/bot{}/sendDocument", config.telegram_bot_token);
        let form = reqwest::multipart::Form::new()
            .text("chat_id", admin_chat_id.to_string())
            .text("caption", format!(
                "🗄️ *Scheduled Backup* — {} KB\n📅 {}\n✅ Backup otomatis harian",
                size_kb, ts
            ))
            .text("parse_mode", "Markdown")
            .part(
                "document",
                reqwest::multipart::Part::bytes(output.stdout)
                    .file_name(format!("aicode_backup_{}.sql.gz", ts))
                    .mime_str("application/gzip")?,
            );

        static TG_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        let client = TG_CLIENT.get_or_init(reqwest::Client::new);
        let resp = client.post(&url).multipart(form).send().await?;
        if resp.status().is_success() {
            tracing::info!("Scheduled backup sent to Telegram admin ({}KB)", size_kb);
        } else {
            tracing::error!("Failed to send backup to Telegram: {}", resp.status());
        }
    } else {
        // Too large — just notify
        crate::services::run_orchestrator::notify_telegram(
            &config.telegram_bot_token,
            admin_chat_id,
            &format!("🗄️ *Scheduled Backup selesai* — {} KB\n⚠️ File terlalu besar untuk dikirim via Telegram.", size_kb),
        ).await;
    }

    Ok(())
}

/// Returns seconds until next HH:00:00 UTC.
fn secs_until_next_run(hour: u8) -> u64 {
    let now = time::OffsetDateTime::now_utc();
    let hour = hour.min(23) as u8;

    let mut target = now.replace_time(
        time::Time::from_hms(hour, 0, 0).unwrap_or(time::Time::MIDNIGHT)
    );

    if target <= now {
        target += time::Duration::days(1);
    }

    (target - now).whole_seconds().max(60) as u64
}
