use redis::aio::ConnectionManager;
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::config::Config;
use crate::services::run_orchestrator;

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    error_code: Option<i64>,
}

#[derive(Debug, Deserialize, Clone)]
struct Update {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize, Clone)]
struct TelegramMessage {
    message_id: i64,
    from: Option<TelegramUser>,
    chat: TelegramChat,
    text: Option<String>,
    caption: Option<String>,
    photo: Option<Vec<TelegramPhotoSize>>,
}

#[derive(Debug, Deserialize, Clone)]
struct TelegramPhotoSize {
    file_id: String,
    #[allow(dead_code)]
    file_unique_id: String,
    #[allow(dead_code)]
    width: i32,
    #[allow(dead_code)]
    height: i32,
    #[serde(default)]
    file_size: Option<i64>,
}

#[derive(Debug, Deserialize, Clone)]
struct TelegramUser {
    id: i64,
    first_name: String,
    last_name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct TelegramDbUser {
    id: Uuid,
    telegram_id: i64,
    user_id: Uuid,
    name: String,
    active_project_id: Option<Uuid>,
}

pub async fn start_polling(config: Arc<Config>, db: PgPool, redis: ConnectionManager) {
    tracing::info!("Telegram bot polling started");
    // polling_client: 35s timeout to accommodate long polling (get_updates timeout=30)
    // api_client: 10s timeout for sendMessage and other API calls
    let polling_client = Client::builder()
        .timeout(std::time::Duration::from_secs(35))
        .build()
        .unwrap_or_default();
    let api_client = Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let mut offset: i64 = 0;

    // Persistent scan-result deliverer: survives backend restarts.
    // Worker pushes finished chat_ids to the `scan:delivery` Redis list; this
    // task drains it and sends the report. If the backend restarts mid-scan,
    // the result stays queued in Redis and is delivered once we're back up.
    {
        let deliver_client = api_client.clone();
        let deliver_config = config.clone();
        let deliver_redis = redis.clone();
        tokio::spawn(async move {
            scan_result_deliverer(deliver_client, deliver_config, deliver_redis).await;
        });
    }

    loop {
        match get_updates(&polling_client, &config.telegram_bot_token, offset).await {
            Ok(updates) => {
                for update in updates {
                    offset = update.update_id + 1;
                    if let Some(msg) = update.message {
                        let db = db.clone();
                        let redis = redis.clone();
                        let config = config.clone();
                        let client = api_client.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_message(&client, &config, &db, redis, &msg).await {
                                tracing::error!(error = %e, "Telegram message handler error");
                            }
                        });
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Telegram getUpdates failed");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    }
}

async fn get_updates(client: &Client, token: &str, offset: i64) -> anyhow::Result<Vec<Update>> {
    let url = format!(
        "https://api.telegram.org/bot{}/getUpdates?offset={}&timeout=30",
        token, offset
    );
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(35))
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    tracing::debug!(status = %status, body_len = %text.len(), "get_updates raw response");
    let body: TelegramResponse<Vec<Update>> = serde_json::from_str(&text)
        .map_err(|e| {
            tracing::error!(error = %e, body = %&text[..text.len().min(200)], "get_updates JSON parse error");
            e
        })?;
    let count = body.result.as_ref().map(|r| r.len()).unwrap_or(0);
    if count > 0 {
        tracing::info!(count = %count, offset = %offset, "get_updates received updates");
    }
    Ok(body.result.unwrap_or_default())
}

async fn handle_message(
    client: &Client,
    config: &Config,
    db: &PgPool,
    redis: ConnectionManager,
    msg: &TelegramMessage,
) -> anyhow::Result<()> {
    let chat_id = msg.chat.id;
    let telegram_id = msg.from.as_ref().map(|u| u.id).unwrap_or(chat_id);
    let text = msg.text.as_deref().unwrap_or("");

    tracing::info!(chat_id = %chat_id, telegram_id = %telegram_id, text = %text, "handle_message called");

    // Check whitelist
    let tg_user = sqlx::query_as::<_, TelegramDbUser>(
        "SELECT id, telegram_id, user_id, name, active_project_id FROM telegram_users WHERE telegram_id = $1"
    )
    .bind(telegram_id)
    .fetch_optional(db)
    .await?;

    let tg_user = match tg_user {
        Some(u) => u,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "⛔ Kamu tidak terdaftar. Hubungi admin untuk mendapatkan akses.").await?;
            return Ok(());
        }
    };

    // Check if message has photo
    let has_photo = msg.photo.as_ref().map(|p| !p.is_empty()).unwrap_or(false);

    if has_photo {
        let caption = msg.caption.as_deref().unwrap_or("Analisis gambar ini");
        handle_photo_message(client, config, db, redis, &tg_user, chat_id, msg, caption).await?;
    } else if text.starts_with('/') {
        handle_command(client, config, db, redis.clone(), &tg_user, chat_id, text).await?;
    } else if !text.is_empty() {
        handle_regular_message(client, config, db, redis, &tg_user, chat_id, text).await?;
    }

    Ok(())
}

async fn handle_photo_message(
    client: &Client,
    config: &Config,
    db: &PgPool,
    redis: ConnectionManager,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    msg: &TelegramMessage,
    caption: &str,
) -> anyhow::Result<()> {
    let photos = msg.photo.as_ref().unwrap();
    // Get largest photo (last in array)
    let photo = photos.last().unwrap();

    // Download photo
    let image_data = match download_telegram_photo(client, &config.telegram_bot_token, &photo.file_id).await {
        Ok(data) => data,
        Err(e) => {
            tracing::error!(error = %e, "Failed to download photo");
            send_message(client, &config.telegram_bot_token, chat_id,
                "❌ Gagal download gambar. Coba lagi.").await?;
            return Ok(());
        }
    };

    // Encode to base64
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&image_data);

    // Build prompt with image reference
    let prompt_with_image = format!(
        "{}

<attached_image>
data:image/jpeg;base64,{}
</attached_image>",
        caption, b64
    );

    tracing::info!(chat_id = %chat_id, caption = %caption, image_size = image_data.len(), "Photo message received");

    // Route to regular message handler with image-augmented prompt
    handle_regular_message(client, config, db, redis, tg_user, chat_id, &prompt_with_image).await
}

async fn download_telegram_photo(client: &Client, token: &str, file_id: &str) -> anyhow::Result<Vec<u8>> {
    // Step 1: getFile to get file_path
    let url = format!("https://api.telegram.org/bot{}/getFile?file_id={}", token, file_id);
    let resp = client.get(&url).send().await?;
    let body: serde_json::Value = resp.json().await?;

    let file_path = body["result"]["file_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No file_path in getFile response"))?;

    // Step 2: Download file
    let download_url = format!("https://api.telegram.org/file/bot{}/{}", token, file_path);
    let resp = client.get(&download_url).send().await?;
    let bytes = resp.bytes().await?;

    Ok(bytes.to_vec())
}

async fn handle_command(
    client: &Client,
    config: &Config,
    db: &PgPool,
    mut redis: ConnectionManager,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let parts: Vec<&str> = text.splitn(3, ' ').collect();
    let cmd = parts[0].split('@').next().unwrap_or(parts[0]);

    match cmd {
        "/start" => cmd_start(client, config, db, tg_user, chat_id).await?,
        "/projects" => cmd_projects(client, config, db, tg_user, chat_id).await?,
        "/project" => cmd_set_project(client, config, db, tg_user, chat_id, &parts).await?,
        "/newproject" => cmd_newproject(client, config, db, tg_user, chat_id, &parts).await?,
        "/cancel" => cmd_cancel(client, config, db, tg_user, chat_id, &mut redis).await?,
        "/retry" => cmd_retry(client, config, db, tg_user, chat_id, &mut redis).await?,
        "/diff" => cmd_diff(client, config, db, tg_user, chat_id).await?,
        "/pr" => cmd_pr(client, config, db, tg_user, chat_id).await?,
        "/backup" => cmd_backup(client, config, db, tg_user, chat_id).await?,
        "/newsession" => cmd_newsession(client, config, db, tg_user, chat_id, &mut redis).await?,
        "/runs" => cmd_runs(client, config, db, tg_user, chat_id).await?,
        "/cost" => cmd_cost(client, config, db, tg_user, chat_id, &parts).await?,
        "/status" => cmd_status(client, config, db, tg_user, chat_id).await?,
        "/scan" => cmd_scan(client, config, db, tg_user, chat_id, &parts, redis.clone()).await?,
        "/setauth" => cmd_setauth(client, config, chat_id, &parts, &mut redis).await?,
        "/clearauth" => cmd_clearauth(client, config, chat_id, &mut redis).await?,
        "/scanstatus" => cmd_scanstatus(client, config, chat_id, &mut redis).await?,
        "/cancelscan" => cmd_cancelscan(client, config, chat_id, &mut redis).await?,
        "/help" => cmd_help(client, config, db, &tg_user, chat_id).await?,
        "/adduser" => cmd_adduser(client, config, db, tg_user, chat_id, &parts).await?,
        "/removeuser" => cmd_removeuser(client, config, db, tg_user, chat_id, &parts).await?,
        _ => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "❓ Perintah tidak dikenal. Gunakan /help untuk melihat daftar perintah.").await?;
        }
    }
    Ok(())
}

async fn cmd_start(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    let project_info = if let Some(pid) = tg_user.active_project_id {
        let name = sqlx::query_scalar::<_, String>("SELECT name FROM projects WHERE id = $1")
            .bind(pid).fetch_optional(db).await?.unwrap_or_default();
        format!("\n\n📂 Project aktif: {}", name)
    } else {
        "\n\nBelum ada project aktif. Gunakan /projects untuk melihat daftar.".to_string()
    };
    let reply = format!(
        "👋 Halo {}! Selamat datang di Dealtech Code AI Agent.{}\n\nGunakan /help untuk melihat daftar perintah.",
        tg_user.name, project_info
    );
    send_message(client, &config.telegram_bot_token, chat_id, &reply).await?;
    Ok(())
}

async fn cmd_projects(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    let projects = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT p.id, p.name, p.slug FROM projects p \
         INNER JOIN project_members pm ON pm.project_id = p.id \
         WHERE pm.user_id = $1 ORDER BY p.name"
    )
    .bind(tg_user.user_id)
    .fetch_all(db)
    .await?;

    if projects.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "Kamu belum punya akses ke project manapun.").await?;
    } else {
        let mut reply = "📂 Daftar Project:\n".to_string();
        for (id, name, slug) in &projects {
            let marker = if tg_user.active_project_id == Some(*id) { " ✅" } else { "" };
            reply.push_str(&format!("\n• {} — {}{}", slug, name, marker));
        }
        reply.push_str("\n\nGunakan /project <slug> untuk memilih project.");
        send_message(client, &config.telegram_bot_token, chat_id, &reply).await?;
    }
    Ok(())
}

async fn cmd_set_project(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    parts: &[&str],
) -> anyhow::Result<()> {
    let slug = parts.get(1).unwrap_or(&"").trim();
    if slug.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "Gunakan: /project <slug>\nContoh: /project my-api").await?;
        return Ok(());
    }
    let project = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT p.id, p.name FROM projects p \
         INNER JOIN project_members pm ON pm.project_id = p.id \
         WHERE pm.user_id = $1 AND p.slug = $2"
    )
    .bind(tg_user.user_id)
    .bind(slug)
    .fetch_optional(db)
    .await?;

    match project {
        Some((pid, name)) => {
            sqlx::query("UPDATE telegram_users SET active_project_id = $1, updated_at = NOW() WHERE id = $2")
                .bind(pid).bind(tg_user.id).execute(db).await?;
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("✅ Project aktif diubah ke: {} ({})", name, slug)).await?;
        }
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "❌ Project tidak ditemukan atau kamu tidak punya akses.").await?;
        }
    }
    Ok(())
}

async fn cmd_status(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    let is_admin = is_admin_user(db, tg_user).await.unwrap_or(false);

    // ── Info project aktif ─────────────────────────────────────────────────────────────
    let (project_name, project_slug, active_branch) = if let Some(pid) = tg_user.active_project_id {
        let row = sqlx::query_as::<_, (String, String)>(
            "SELECT name, slug FROM projects WHERE id = $1"
        ).bind(pid).fetch_optional(db).await?;
        let branch = sqlx::query_scalar::<_, String>(
            "SELECT active_branch FROM coding_sessions \
             WHERE project_id=$1 AND user_id=$2 AND active_branch IS NOT NULL \
             ORDER BY created_at DESC LIMIT 1"
        ).bind(pid).bind(tg_user.user_id).fetch_optional(db).await?.unwrap_or_else(|| "-".to_string());
        match row {
            Some((n, s)) => (n, s, branch),
            None => ("(tidak ditemukan)".to_string(), "-".to_string(), "-".to_string()),
        }
    } else {
        ("belum dipilih".to_string(), "-".to_string(), "-".to_string())
    };

    // ── Run stats user ini ─────────────────────────────────────────────────────────────
    let (runs_today, cost_today, active_run_status) = if let Some(pid) = tg_user.active_project_id {
        let runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_runs WHERE user_id=$1 AND project_id=$2 AND created_at > NOW() - INTERVAL '24 hours'"
        ).bind(tg_user.user_id).bind(pid).fetch_one(db).await.unwrap_or(0);

        let cost: f64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM agent_runs WHERE user_id=$1 AND project_id=$2 AND created_at > NOW() - INTERVAL '24 hours'"
        ).bind(tg_user.user_id).bind(pid).fetch_one(db).await.unwrap_or(0.0);

        let active: Option<String> = sqlx::query_scalar(
            "SELECT status FROM agent_runs WHERE user_id=$1 AND project_id=$2 \
             AND status NOT IN ('completed','failed_agent','cancelled','timed_out') \
             ORDER BY created_at DESC LIMIT 1"
        ).bind(tg_user.user_id).bind(pid).fetch_optional(db).await.unwrap_or(None);

        (runs, cost, active)
    } else {
        (0, 0.0, None)
    };

    // ── Build user reply ───────────────────────────────────────────────────────────────
    let run_status_line = match &active_run_status {
        Some(s) => format!("\n⏳ Run aktif: {}", s),
        None => String::new(),
    };

    let mut reply = format!(
        "📊 *Status*\n\n\
        👤 User: {}\n\
        📂 Project: {} ({})\n\
        🌿 Branch: `{}`{}\n\n\
        📅 Runs hari ini: {}\n\
        💰 Cost hari ini: ${:.4}",
        tg_user.name,
        project_name, project_slug,
        active_branch,
        run_status_line,
        runs_today,
        cost_today,
    );

    // ── Admin section ───────────────────────────────────────────────────────────────
    if is_admin {
        let total_users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telegram_users")
            .fetch_one(db).await.unwrap_or(0);
        let total_projects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects")
            .fetch_one(db).await.unwrap_or(0);
        let total_runs_today: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_runs WHERE created_at > NOW() - INTERVAL '24 hours'"
        ).fetch_one(db).await.unwrap_or(0);
        let total_cost_today: f64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM agent_runs WHERE created_at > NOW() - INTERVAL '24 hours'"
        ).fetch_one(db).await.unwrap_or(0.0);
        let active_runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_runs WHERE status NOT IN ('completed','failed_agent','cancelled','timed_out')"
        ).fetch_one(db).await.unwrap_or(0);
        let total_cost_all: f64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM agent_runs"
        ).fetch_one(db).await.unwrap_or(0.0);

        // Disk usage
        let disk_info = get_disk_summary();

        reply.push_str(&format!(
            "\n\n─── *Admin Panel* ───\n\
            👥 Total users: {}\n\
            📁 Total projects: {}\n\
            ⚡ Active runs: {}\n\
            📅 Runs hari ini (semua): {}\n\
            💰 Cost hari ini (semua): ${:.4}\n\
            💳 Total cost all-time: ${:.4}\n\
            {}",
            total_users, total_projects, active_runs,
            total_runs_today, total_cost_today, total_cost_all,
            disk_info,
        ));
    }

    send_message_md(client, &config.telegram_bot_token, chat_id, &reply).await?;
    Ok(())
}

fn get_disk_summary() -> String {
    let paths = [
        ("/srv/ai-platform/workspaces", "Workspaces"),
        ("/srv/ai-platform/worktrees", "Worktrees"),
    ];
    let mut parts = Vec::new();
    for (path, label) in &paths {
        if let Ok(pct) = get_disk_pct(path) {
            let icon = if pct >= 90 { "🔴" } else if pct >= 75 { "🟡" } else { "🟢" };
            parts.push(format!("{} {}: {}%", icon, label, pct));
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("💾 Disk: {}", parts.join(" | "))
}

fn get_disk_pct(path: &str) -> anyhow::Result<u64> {
    use std::ffi::CString;
    let c_path = CString::new(path)?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret != 0 { anyhow::bail!("statvfs failed"); }
    let total = stat.f_blocks * stat.f_frsize;
    if total == 0 { return Ok(0); }
    let used = (stat.f_blocks - stat.f_bfree) * stat.f_frsize;
    Ok((used * 100 / total) as u64)
}

async fn cmd_retry(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    redis: &mut ConnectionManager,
) -> anyhow::Result<()> {
    let project_id = match tg_user.active_project_id {
        Some(pid) => pid,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "Pilih project dulu dengan /project <slug>").await?;
            return Ok(());
        }
    };

    // Find last run for this user+project
    let last = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, prompt, status FROM agent_runs \
         WHERE user_id=$1 AND project_id=$2 \
         ORDER BY created_at DESC LIMIT 1"
    )
    .bind(tg_user.user_id)
    .bind(project_id)
    .fetch_optional(db)
    .await?;

    let (last_id, prompt, status) = match last {
        Some(r) => r,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "Tidak ada run sebelumnya untuk di-retry.").await?;
            return Ok(());
        }
    };

    // Don't retry a run that's still running
    if matches!(status.as_str(), "queued" | "processing" | "running_agent" | "preparing_workspace" | "collecting_diff" | "auto_push_or_pr") {
        send_message(client, &config.telegram_bot_token, chat_id,
            "⏳ Run sebelumnya masih berjalan. Tunggu selesai dulu.").await?;
        return Ok(());
    }

    tracing::info!(last_run_id = %last_id, "Retrying run via Telegram");

    // Get session and project info
    let session_id = get_or_create_session(db, redis, tg_user, project_id).await?;
    let project = match crate::services::project_service::get(db, project_id, tg_user.user_id).await {
        Ok(p) => p,
        Err(_) => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "❌ Gagal mengambil info project.").await?;
            return Ok(());
        }
    };

    let req = crate::domain::agent_run::CreateRunRequest { prompt: prompt.clone(), auto_mode: None, model: None };
    match crate::services::run_orchestrator::create_run(
        db, session_id, project_id, tg_user.user_id, req, &project.openclaw_agent_id, Some(chat_id),
    ).await {
        Ok(_) => {
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("🔄 Retry dimulai!\n\nPrompt: \"{}...\"",
                    &prompt.chars().take(80).collect::<String>())).await?;
        }
        Err(e) => {
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("❌ Gagal retry: {}", e)).await?;
        }
    }
    Ok(())
}

async fn cmd_backup(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    // Admin only
    let is_admin = is_admin_user(db, tg_user).await?;
    if !is_admin {
        send_message(client, &config.telegram_bot_token, chat_id,
            "❌ Hanya admin yang bisa backup.").await?;
        return Ok(());
    }

    send_message(client, &config.telegram_bot_token, chat_id,
        "⏳ Membuat backup database...").await?;

    // Run pg_dump inside postgres container
    let now = time::OffsetDateTime::now_utc();
    let timestamp = format!("{:04}{:02}{:02}_{:02}{:02}{:02}",
        now.year(), now.month() as u8, now.day(),
        now.hour(), now.minute(), now.second());
    let backup_file = format!("/tmp/aicode_backup_{}.sql.gz", timestamp);

    let output = tokio::process::Command::new("docker")
        .args(["exec", "ai-platform-postgres-1",
            "sh", "-c",
            &format!("pg_dump -U postgres aicode | gzip > /tmp/backup_{}.sql.gz && cat /tmp/backup_{}.sql.gz",
                timestamp, timestamp)])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => {
            // Write to temp file
            tokio::fs::write(&backup_file, &out.stdout).await?;
            let size_kb = out.stdout.len() / 1024;

            // Send file via Telegram if < 45MB
            if out.stdout.len() < 45 * 1024 * 1024 {
                let url = format!("https://api.telegram.org/bot{}/sendDocument", config.telegram_bot_token);
                let file_part = reqwest::multipart::Form::new()
                    .text("chat_id", chat_id.to_string())
                    .text("caption", format!("✅ Backup DB — {} KB — {}", size_kb, timestamp))
                    .part("document",
                        reqwest::multipart::Part::bytes(out.stdout)
                            .file_name(format!("aicode_backup_{}.sql.gz", timestamp))
                            .mime_str("application/gzip")?);
                static TG_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
                let tg = TG_CLIENT.get_or_init(reqwest::Client::new);
                let _ = tg.post(&url).multipart(file_part).send().await;
            } else {
                send_message(client, &config.telegram_bot_token, chat_id,
                    &format!("✅ Backup selesai ({} KB) tapi terlalu besar untuk dikirim via Telegram.\nFile tersimpan di server: {}",
                        size_kb, backup_file)).await?;
            }
            // Cleanup temp file
            let _ = tokio::fs::remove_file(&backup_file).await;
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("❌ Backup gagal: {}", err.chars().take(200).collect::<String>())).await?;
        }
        Err(e) => {
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("❌ Backup gagal: {}", e)).await?;
        }
    }
    Ok(())
}

async fn cmd_cancel(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    redis: &mut ConnectionManager,
) -> anyhow::Result<()> {
    // Look up active run_id from Redis
    let key = format!("tg:active_run:{}", chat_id);
    let run_id_str: Option<String> = redis::cmd("GET")
        .arg(&key)
        .query_async(redis)
        .await
        .unwrap_or(None);

    let run_id = match run_id_str.as_deref().and_then(|s| uuid::Uuid::parse_str(s).ok()) {
        Some(id) => id,
        None => {
            // Fallback: check DB for active run (Redis key may have expired)
            let row = sqlx::query_scalar::<_, uuid::Uuid>(
                "SELECT id FROM agent_runs WHERE user_id=$1 AND telegram_chat_id=$2                  AND status IN ('queued','processing','running_agent','preparing_workspace','collecting_diff','auto_push_or_pr')                  ORDER BY created_at DESC LIMIT 1"
            )
            .bind(tg_user.user_id)
            .bind(chat_id)
            .fetch_optional(db)
            .await?;

            match row {
                Some(id) => id,
                None => {
                    send_message(client, &config.telegram_bot_token, chat_id,
                        "Tidak ada run yang sedang berjalan.").await?;
                    return Ok(());
                }
            }
        }
    };

    // Mark run as cancelled in DB
    let updated = sqlx::query(
        "UPDATE agent_runs SET status='cancelled', finished_at=NOW(), error_message='Dibatalkan oleh user' \
         WHERE id=$1 AND user_id=$2 AND status IN ('queued','processing','running_agent','preparing_workspace','collecting_diff','auto_push_or_pr')"
    )
    .bind(run_id)
    .bind(tg_user.user_id)
    .execute(db)
    .await?;

    if updated.rows_affected() == 0 {
        send_message(client, &config.telegram_bot_token, chat_id,
            "Run tidak ditemukan atau sudah selesai.").await?;
        return Ok(());
    }

    // Clear Redis key
    let _: std::result::Result<(), _> = redis::cmd("DEL")
        .arg(&key)
        .query_async(redis)
        .await;

    send_message(client, &config.telegram_bot_token, chat_id,
        "❌ Run dibatalkan.").await?;
    Ok(())
}

async fn cmd_newproject(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    parts: &[&str],
) -> anyhow::Result<()> {
    // Usage: /newproject <nama> <repo_url>
    let name = parts.get(1).unwrap_or(&"").trim();
    let repo_url = parts.get(2).unwrap_or(&"").trim();

    if name.is_empty() || repo_url.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "Gunakan: /newproject <nama> <repo_url>\nContoh: /newproject my-api https://github.com/org/my-api").await?;
        return Ok(());
    }

    if !repo_url.starts_with("https://") && !repo_url.starts_with("git@") {
        send_message(client, &config.telegram_bot_token, chat_id,
            "❌ repo_url tidak valid. Gunakan format https://github.com/... atau git@github.com:...").await?;
        return Ok(());
    }

    // Use user_id as team_id (consistent with how workspace paths are built)
    let team_id = tg_user.user_id;
    let slug = name.to_lowercase().replace(' ', "-");

    // Check if slug already exists for this user
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT p.id FROM projects p \
         INNER JOIN project_members pm ON pm.project_id = p.id \
         WHERE pm.user_id = $1 AND p.slug = $2"
    )
    .bind(tg_user.user_id)
    .bind(&slug)
    .fetch_optional(db)
    .await?;

    if existing.is_some() {
        send_message(client, &config.telegram_bot_token, chat_id,
            &format!("❌ Project dengan slug '{}' sudah ada. Gunakan nama lain.", slug)).await?;
        return Ok(());
    }

    let req = crate::domain::project::CreateProjectRequest {
        name: name.to_string(),
        repo_url: Some(repo_url.to_string()),
        openclaw_agent_id: "default".to_string(),
        description: None,
        create_github_repo: Some(false),
        github_private: None,
        github_org: None,
    };

    match crate::services::project_service::create(db, tg_user.user_id, team_id, req).await {
        Ok(project) => {
            // Auto-set as active project
            sqlx::query(
                "UPDATE telegram_users SET active_project_id = $1, updated_at = NOW() WHERE id = $2"
            )
            .bind(project.id)
            .bind(tg_user.id)
            .execute(db)
            .await?;

            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("✅ Project '{}' berhasil dibuat!\n\nSlug: {}\nRepo: {}\n\nProject ini sudah diset sebagai project aktif. Langsung kirim pesan untuk mulai coding!",
                    project.name, project.slug, project.repo_url)).await?;
        }
        Err(e) => {
            tracing::error!("Failed to create project via telegram: {}", e);
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("❌ Gagal membuat project: {}", e)).await?;
        }
    }
    Ok(())
}

async fn cmd_newsession(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    redis: &mut ConnectionManager,
) -> anyhow::Result<()> {
    let project_id = match tg_user.active_project_id {
        Some(pid) => pid,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "Pilih project dulu dengan /project <slug>").await?;
            return Ok(());
        }
    };

    // Force a fresh session+branch once on the next user message.
    // Default behaviour should be session reuse; /newsession is the explicit reset.
    let force_branch_key = format!("tg:force_new_branch:{}:{}", tg_user.user_id, project_id);
    let _: std::result::Result<(), _> = redis::cmd("SETEX")
        .arg(&force_branch_key)
        .arg(86400u64)
        .arg("1")
        .query_async(redis)
        .await;

    let force_session_key = format!("tg:force_new_session:{}:{}", tg_user.user_id, project_id);
    let _: std::result::Result<(), _> = redis::cmd("SETEX")
        .arg(&force_session_key)
        .arg(86400u64)
        .arg("1")
        .query_async(redis)
        .await;

    // Clear the cached pin too so the next lookup cannot accidentally reuse stale state.
    let pin_key = format!("tg:pinned_session:{}:{}", tg_user.user_id, project_id);
    let _: std::result::Result<(), _> = redis::cmd("DEL")
        .arg(&pin_key)
        .query_async(redis)
        .await;

    send_message(client, &config.telegram_bot_token, chat_id,
        "✅ Session baru akan dimulai saat kamu kirim pesan berikutnya.\nBranch baru akan dibuat otomatis.").await?;
    Ok(())
}

async fn cmd_runs(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    // Get active project
    let project_id = match tg_user.active_project_id {
        Some(pid) => pid,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "Pilih project dulu dengan /project <slug>").await?;
            return Ok(());
        }
    };

    // Fetch last 10 runs for this project + user
    let rows = sqlx::query_as::<_, (uuid::Uuid, String, Option<String>, Option<String>, Option<f64>, Option<String>)>(
        "SELECT r.id, r.status, r.commit_sha, r.branch_name, r.cost_usd, \
         LEFT(r.prompt, 60) \
         FROM agent_runs r \
         WHERE r.project_id = $1 AND r.user_id = $2 \
         ORDER BY r.created_at DESC LIMIT 10"
    )
    .bind(project_id)
    .bind(tg_user.user_id)
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "Belum ada run di project ini.").await?;
        return Ok(());
    }

    let mut lines = vec!["📋 10 Run Terakhir\n".to_string()];
    for (id, status, commit_sha, branch, cost, prompt) in &rows {
        let status_icon = match status.as_str() {
            "completed"    => "✅",
            "failed_agent" => "❌",
            "cancelled"    => "🚫",
            "queued" | "processing" | "running_agent" |
            "preparing_workspace" | "collecting_diff" | "auto_push_or_pr" => "⏳",
            _ => "❓",
        };
        let short_id = &id.to_string()[..8];
        let commit = commit_sha.as_deref().map(|s| &s[..s.len().min(7)]).unwrap_or("-");
        let branch_short = branch.as_deref().unwrap_or("-");
        let cost_str = cost.map(|c| format!("${:.4}", c)).unwrap_or_else(|| "-".to_string());
        let prompt_short = prompt.as_deref().unwrap_or("");
        let prompt_display = if prompt_short.len() >= 60 {
            format!("{}...", prompt_short)
        } else {
            prompt_short.to_string()
        };
        lines.push(format!(
            "{} [{}] {}\n   branch: {} | commit: {} | cost: {}\n   \"{}\"\n",
            status_icon, short_id, status,
            branch_short, commit, cost_str,
            prompt_display
        ));
    }

    send_message(client, &config.telegram_bot_token, chat_id, &lines.join("")).await?;
    Ok(())
}

async fn cmd_diff(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    let project_id = match tg_user.active_project_id {
        Some(id) => id,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "⚠️ Pilih project dulu dengan /project <slug>.").await?;
            return Ok(());
        }
    };

    // Get last completed run with a commit for this user+project
    let row = sqlx::query_as::<_, (uuid::Uuid, Option<String>, Option<String>, Option<String>)>(
        "SELECT id, commit_sha, diff_stat, branch_name \
         FROM agent_runs \
         WHERE user_id=$1 AND project_id=$2 AND status='completed' AND commit_sha IS NOT NULL \
         ORDER BY finished_at DESC LIMIT 1"
    )
    .bind(tg_user.user_id)
    .bind(project_id)
    .fetch_optional(db)
    .await?;

    match row {
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "ℹ️ Belum ada run yang menghasilkan commit.").await?;
        }
        Some((run_id, commit_sha, diff_stat, branch_name)) => {
            let sha_short = commit_sha.as_deref().unwrap_or("-");
            let sha_display = if sha_short.len() >= 8 { &sha_short[..8] } else { sha_short };
            let branch = branch_name.as_deref().unwrap_or("-");

            // Header pakai Markdown, diff content kirim plain text chunked
            // (diff punya +/-/*/_/` yang break Telegram Markdown parser)
            let header = format!(
                "📄 *Diff run terakhir*\n🌿 Branch: `{}`\n🔖 Commit: `{}`\n🆔 Run: `{}`",
                branch, sha_display, &run_id.to_string()[..8]
            );
            send_message_md(client, &config.telegram_bot_token, chat_id, &header).await?;

            match diff_stat {
                Some(ref stat) if !stat.trim().is_empty() => {
                    send_long_message(client, &config.telegram_bot_token, chat_id, stat).await?;
                }
                _ => {
                    send_message(client, &config.telegram_bot_token, chat_id,
                        "(diff stat tidak tersedia untuk run ini)").await?;
                }
            }
        }
    }
    Ok(())
}

async fn cmd_pr(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    let project_id = match tg_user.active_project_id {
        Some(id) => id,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "⚠️ Pilih project dulu dengan /project <slug>.").await?;
            return Ok(());
        }
    };

    // Get last 3 runs with a PR for this project
    let rows = sqlx::query_as::<_, (uuid::Uuid, Option<String>, Option<i32>, Option<String>, String)>(
        "SELECT id, pr_url, pr_number, branch_name, prompt \
         FROM agent_runs \
         WHERE project_id=$1 AND pr_url IS NOT NULL \
         ORDER BY finished_at DESC LIMIT 3"
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "ℹ️ Belum ada PR yang dibuat untuk project ini.").await?;
        return Ok(());
    }

    let mut reply = "🔗 *Pull Requests terbaru*\n".to_string();
    for (run_id, pr_url, pr_number, branch_name, prompt) in rows {
        let num = pr_number.map(|n| format!("#{}", n)).unwrap_or_else(|| "-".to_string());
        let branch = branch_name.as_deref().unwrap_or("-");
        let prompt_short: String = prompt.chars().take(50).collect();
        // Escape underscore agar tidak break Telegram Markdown italic parser
        let prompt_escaped = prompt_short.replace('_', "\\_");
        let url = pr_url.as_deref().unwrap_or("-");
        reply.push_str(&format!(
            "\n🟢 PR {} — `{}`\n\
            💬 _{}_\n\
            🔗 {}\n",
            num, branch, prompt_escaped, url
        ));
        let _ = run_id; // suppress unused warning
    }

    send_message_md(client, &config.telegram_bot_token, chat_id, &reply).await?;
    Ok(())
}


async fn cmd_cost(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    parts: &[&str],
) -> anyhow::Result<()> {
    tracing::info!(chat_id = chat_id, "cmd_cost called");
    let period = parts.get(1).copied().unwrap_or("today");

    let (interval_label, interval_sql) = match period {
        "week" | "minggu" => ("7 hari terakhir", "7 days"),
        "month" | "bulan" => ("30 hari terakhir", "30 days"),
        "all" | "semua" => ("semua waktu", "100 years"),
        _ => ("hari ini", "24 hours"),
    };

    // Per-project breakdown
    let rows: Vec<(String, i64, f64, i64, i64)> = match sqlx::query_as(
        &format!(
            "SELECT p.name, COUNT(r.id)::bigint, COALESCE(SUM(r.cost_usd), 0)::float8, \
             COALESCE(SUM(r.tokens_input), 0)::bigint, COALESCE(SUM(r.tokens_output), 0)::bigint \
             FROM agent_runs r JOIN projects p ON r.project_id = p.id \
             WHERE r.user_id=$1 AND r.created_at > NOW() - INTERVAL '{}' \
             GROUP BY p.name ORDER BY SUM(r.cost_usd) DESC",
            interval_sql
        )
    )
    .bind(tg_user.user_id)
    .fetch_all(db)
    .await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "cmd_cost query failed");
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("❌ Error query cost: {}", e)).await?;
            return Ok(());
        }
    };

    if rows.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            &format!("💰 Tidak ada penggunaan untuk periode: {}", interval_label)).await?;
        return Ok(());
    }

    let mut total_cost: f64 = 0.0;
    let mut total_runs: i64 = 0;
    let mut total_input: i64 = 0;
    let mut total_output: i64 = 0;
    let mut lines = Vec::new();

    for (name, runs, cost, input_tok, output_tok) in &rows {
        total_cost += cost;
        total_runs += runs;
        total_input += input_tok;
        total_output += output_tok;
        lines.push(format!(
            "  {} — {} runs, ${:.4} ({}/{}k tok)",
            name, runs, cost, input_tok / 1000, output_tok / 1000
        ));
    }

    let reply = format!(
        "💰 *Cost Report* ({})

        📊 Total: {} runs | ${:.4}
        🔤 Tokens: {}k input / {}k output

        📂 Per project:
{}\n
        _Gunakan: /cost today|week|month|all_",
        interval_label,
        total_runs, total_cost,
        total_input / 1000, total_output / 1000,
        lines.join("\n"),
    );

    send_message(client, &config.telegram_bot_token, chat_id, &reply).await?;
    Ok(())
}

// Mask a secret for display so the bot never echoes the real value back:
// keep only the last 4 chars, e.g. "...a1b2". Empty/short -> fully masked.
fn mask_secret(s: &str) -> String {
    let n = s.chars().count();
    if n <= 4 { "****".to_string() } else { format!("...{}", &s[s.len().saturating_sub(4)..]) }
}

// /setauth <type> <value...> — store scan auth for THIS chat in Redis (TTL 2h).
// type: cookie | bearer | header   (header value form: "Name: Value")
// SECURITY: the raw value is never echoed back (only masked) and is never
// written to logs. Stored short-lived in Redis, auto-expires after 2h.
async fn cmd_setauth(
    client: &Client,
    config: &Config,
    chat_id: i64,
    parts: &[&str],
    redis: &mut ConnectionManager,
) -> anyhow::Result<()> {
    let kind = parts.get(1).unwrap_or(&"").trim().to_lowercase();
    let is_refresh = kind == "refresh";
    let refresh_url = if is_refresh {
        parts.get(3).map(|s| s.trim().to_string()).unwrap_or_default()
    } else { String::new() };
    let value: String = if is_refresh {
        parts.get(2).map(|s| s.trim().to_string()).unwrap_or_default()
    } else {
        parts.get(2..).map(|p| p.join(" ")).unwrap_or_default().trim().to_string()
    };

    let valid_kind = matches!(kind.as_str(), "cookie" | "bearer" | "header" | "refresh");
    if !valid_kind || value.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "🔐 Set kredensial untuk authenticated scan (berlaku 2 jam)\n\n\
            Gunakan: /setauth <type> <value>\n\n\
            Type:\n\
            • cookie — /setauth cookie SESSIONID=abc123; role=admin\n\
            • bearer — /setauth bearer eyJhbGci...\n\
            • header — /setauth header X-Api-Key: rahasia\n\
            • refresh — /setauth refresh <refresh_token> [refresh_url]\n\
               (tukar refresh→access token tiap scan; url default <target>/api/auth/refresh)\n\n\
            Setelah diset, /scan <url> <mode> otomatis pakai auth ini \
            (nuclei/dalfox/sqlmap). Hapus dengan /clearauth.\n\n\
            ⚠️ Nilai kredensial akan terlihat di history chat Telegram & \
            tersimpan sementara di server. Jangan pakai kredensial produksi \
            yang sensitif kalau tidak perlu.").await?;
        return Ok(());
    }

    let mut payload = serde_json::json!({ "kind": kind, "value": value });
    if is_refresh && !refresh_url.is_empty() {
        payload["refresh_url"] = serde_json::json!(refresh_url);
    }
    let key = format!("scan:auth:{}", chat_id);
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(payload.to_string())
        .arg("EX").arg(7200) // 2 hours
        .query_async(redis)
        .await
        .unwrap_or(());

    send_message(client, &config.telegram_bot_token, chat_id,
        &format!("🔐 Auth tersimpan untuk scan kamu (berlaku 2 jam).\n\n\
        Type: {}\nValue: {}\n\n\
        Scan berikutnya otomatis pakai auth ini. /clearauth untuk hapus.",
        kind, mask_secret(&value))).await?;
    Ok(())
}

// /clearauth — remove stored scan auth for this chat.
async fn cmd_clearauth(
    client: &Client,
    config: &Config,
    chat_id: i64,
    redis: &mut ConnectionManager,
) -> anyhow::Result<()> {
    let key = format!("scan:auth:{}", chat_id);
    let _: () = redis::cmd("DEL").arg(&key).query_async(redis).await.unwrap_or(());
    send_message(client, &config.telegram_bot_token, chat_id,
        "🔓 Auth scan dihapus. Scan berikutnya jalan sebagai anonim.").await?;
    Ok(())
}

async fn cmd_scan(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    parts: &[&str],
    mut redis: ConnectionManager,
) -> anyhow::Result<()> {
    // Usage: /scan <url> [mode]
    // Modes: quick (default), full, recon, cves, misconfig, exposure
    let url = parts.get(1).unwrap_or(&"").trim();
    if url.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "🔒 Security Scanner\n\n\
            Gunakan: /scan <url> [mode]\n\n\
            Mode:\n\
            • quick — Top critical checks (default, ~2-5 min)\n\
            • full — Semua templates (~15-30 min)\n\
            • recon — Reconnaissance only\n\
            • cves — Known CVEs\n\
            • misconfig — Misconfigurations\n\
            • exposure — Exposed files/panels\n\
            • sqli — Active SQL injection test (sqlmap)\n\
            • xss — Active XSS test (dalfox)\n\
            • tls — TLS/SSL config audit (testssl.sh)\n\
            • deps — Dependency CVE + secret + IaC scan repo git (trivy)\n\
            • discovery — Petakan subdomain & host hidup (subfinder+httpx)\n\n\
            Contoh:\n\
            /scan https://myapp.com\n\
            /scan https://myapp.com full\n\
            /scan https://myapp.com/page?id=1 sqli\n\
            /scan https://github.com/user/repo deps\n\
            /scan siwanu.com discovery").await?;
        return Ok(());
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        send_message(client, &config.telegram_bot_token, chat_id,
            "❌ URL harus dimulai dengan http:// atau https://").await?;
        return Ok(());
    }

    let mode = parts.get(2).unwrap_or(&"quick").trim();
    let valid_modes = ["quick", "full", "recon", "cves", "misconfig", "exposure", "sqli", "xss", "tls", "deps", "discovery"];
    if !valid_modes.contains(&mode) {
        send_message(client, &config.telegram_bot_token, chat_id,
            &format!("❌ Mode tidak valid: {}\nPilih: quick, full, recon, cves, misconfig, exposure, sqli, xss, tls, deps, discovery", mode)).await?;
        return Ok(());
    }

    // Prevent the same chat from queueing two scans at once
    let active_self: Option<String> = redis::cmd("GET")
        .arg(format!("scan:active:{}", chat_id))
        .query_async(&mut redis)
        .await
        .unwrap_or(None);
    if active_self.is_some() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "⚠️ Kamu sudah punya scan yang sedang berjalan atau mengantri.\n\
            Gunakan /scanstatus untuk cek, atau /cancelscan untuk membatalkan.").await?;
        return Ok(());
    }

    // Check if another scan is already running (single-scan policy)
    let running_for: Option<String> = redis::cmd("GET")
        .arg("scan:running")
        .query_async(&mut redis)
        .await
        .unwrap_or(None);
    let queue_len: i64 = redis::cmd("LLEN")
        .arg("scan:queue")
        .query_async(&mut redis)
        .await
        .unwrap_or(0);

    // Load stored auth for this chat (if any) so the scan runs authenticated.
    let auth_key = format!("scan:auth:{}", chat_id);
    let auth_raw: Option<String> = redis::cmd("GET")
        .arg(&auth_key)
        .query_async(&mut redis)
        .await
        .unwrap_or(None);
    let auth_val: Option<serde_json::Value> =
        auth_raw.as_deref().and_then(|s| serde_json::from_str(s).ok());
    let auth_note = if auth_val.is_some() { "\n🔐 Mode: authenticated" } else { "" };

    if running_for.is_some() {
        // Position = scans already queued ahead + the one currently running
        let position = queue_len + 1;
        send_message(client, &config.telegram_bot_token, chat_id,
            &format!("⏳ Ada scan lain yang sedang berjalan.\n\n\
            🎯 Target kamu: {}\n📋 Mode: {}{}\n🔢 Posisi antrian: #{}\n\n\
            Scan kamu akan diproses otomatis setelah antrian selesai. \
            Aku kabari kalau sudah jalan dan setelah selesai.", url, mode, auth_note, position)).await?;
    } else {
        send_message(client, &config.telegram_bot_token, chat_id,
            &format!("🔒 Memulai security scan...\n\n🎯 Target: {}\n📋 Mode: {}{}\n\n⏳ Ini bisa memakan waktu beberapa menit.", url, mode, auth_note)).await?;
    }

    // Store scan state in Redis
    let scan_key = format!("scan:active:{}", chat_id);
    let scan_info = serde_json::json!({
        "url": url,
        "mode": mode,
        "started_at": format!("{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_secs()),
        "status": "running"
    });
    let _: () = redis::cmd("SET")
        .arg(&scan_key)
        .arg(scan_info.to_string())
        .arg("EX").arg(3600)
        .query_async(&mut redis)
        .await
        .unwrap_or(());

    // Push scan job to queue (worker on host picks it up). Include auth when set.
    let mut job = serde_json::json!({
        "url": url,
        "mode": mode,
        "chat_id": chat_id
    });
    if let Some(a) = auth_val {
        job["auth"] = a;
    }
    let _: () = redis::cmd("LPUSH")
        .arg("scan:queue")
        .arg(job.to_string())
        .query_async(&mut redis)
        .await
        .unwrap_or(());

    // Delivery is handled by the persistent scan_result_deliverer task (see
    // start_polling). The worker pushes the chat_id to `scan:delivery` when the
    // scan finishes, so results survive backend restarts.

    Ok(())
}

// Persistent scan-result deliverer. Drains the `scan:delivery` Redis list
// (chat_ids pushed by the host worker when a scan finishes) and sends the
// formatted report. Because it reads from Redis rather than per-request memory,
// results survive a backend restart/crash that happens mid-scan.
async fn scan_result_deliverer(
    client: Client,
    config: Arc<Config>,
    mut redis: ConnectionManager,
) {
    tracing::info!("Scan result deliverer started");
    loop {
        // BRPOP blocks up to 30s; returns [list_name, value] on hit.
        let popped: Option<(String, String)> = redis::cmd("BRPOP")
            .arg("scan:delivery")
            .arg(30)
            .query_async(&mut redis)
            .await
            .unwrap_or(None);

        let chat_id_str = match popped {
            Some((_, v)) => v,
            None => continue, // timeout, loop again
        };

        let chat_id: i64 = match chat_id_str.trim().parse() {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!(value = %chat_id_str, "deliverer: bad chat_id");
                continue;
            }
        };

        let result_key = format!("scan:result:{}", chat_id);
        let scan_key = format!("scan:active:{}", chat_id);

        let data: Option<String> = redis::cmd("GET")
            .arg(&result_key)
            .query_async(&mut redis)
            .await
            .unwrap_or(None);

        let data = match data {
            Some(d) => d,
            None => {
                tracing::warn!(chat_id = %chat_id, "deliverer: no result payload");
                continue;
            }
        };

        let report = format_scan_report(&data);
        if report.is_empty() {
            let _ = send_message(&client, &config.telegram_bot_token, chat_id,
                "✅ Scan selesai!\n\nTidak ditemukan vulnerability.\n\n\
                ⚠️ Note: Automated scanner hanya mendeteksi ~30-40% vulnerability.").await;
        } else {
            let _ = send_long_message(&client, &config.telegram_bot_token, chat_id, &report).await;
            // Also send downloadable HTML + JSON reports (actionable/auditable).
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs();
            let html = build_html_report(&data);
            if !html.is_empty() {
                let _ = send_scan_document(&client, &config.telegram_bot_token, chat_id,
                    html.into_bytes(), &format!("scan_report_{}.html", ts),
                    "text/html", "📄 Report HTML (buka di browser)").await;
            }
            // Pretty-print JSON for the machine-readable export.
            let json_pretty = serde_json::from_str::<serde_json::Value>(&data)
                .ok().and_then(|v| serde_json::to_vec_pretty(&v).ok())
                .unwrap_or_else(|| data.clone().into_bytes());
            let _ = send_scan_document(&client, &config.telegram_bot_token, chat_id,
                json_pretty, &format!("scan_report_{}.json", ts),
                "application/json", "🗂 Report JSON (machine-readable)").await;
        }

        // Clean up markers
        let _: () = redis::cmd("DEL").arg(&result_key)
            .query_async(&mut redis).await.unwrap_or(());
        let _: () = redis::cmd("DEL").arg(&scan_key)
            .query_async(&mut redis).await.unwrap_or(());

        tracing::info!(chat_id = %chat_id, "deliverer: report sent");
    }
}

// Build a downloadable HTML report from the raw scan result JSON.
// Self-contained (inline CSS), grouped by severity, safe-escaped.
fn build_html_report(data: &str) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
         .replace('"', "&quot;")
    }
    let parsed: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let empty = vec![];
    let findings = parsed["findings"].as_array().unwrap_or(&empty);
    let order = ["critical", "high", "medium", "low", "info"];
    let mut rows = String::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for sev in order {
        for f in findings {
            let fsev = f["info"]["severity"].as_str().unwrap_or("info");
            if fsev != sev { continue; }
            let name = f["info"]["name"].as_str().unwrap_or("Unknown");
            let tid = f["template-id"].as_str().unwrap_or("unknown");
            let key = format!("{}|{}", name, tid);
            if !seen.insert(key) { continue; }
            let matched = f["matched-at"].as_str()
                .or_else(|| f["host"].as_str()).unwrap_or("N/A");
            let desc = f["info"]["description"].as_str().unwrap_or("");
            rows.push_str(&format!(
                "<tr class=\"{}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                esc(sev), esc(&sev.to_uppercase()), esc(name), esc(tid),
                esc(matched), esc(desc)));
        }
    }
    let total = seen.len();
    format!(r####"<!DOCTYPE html><html lang="id"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Security Scan Report</title><style>
body{{font-family:system-ui,Arial,sans-serif;margin:24px;color:#1a1a1a;background:#fafafa}}
h1{{font-size:20px}} .meta{{color:#666;font-size:13px;margin-bottom:16px}}
table{{border-collapse:collapse;width:100%;background:#fff;box-shadow:0 1px 3px rgba(0,0,0,.1)}}
th,td{{padding:8px 10px;text-align:left;border-bottom:1px solid #eee;font-size:13px;vertical-align:top}}
th{{background:#222;color:#fff;position:sticky;top:0}}
td:first-child{{font-weight:700;white-space:nowrap}}
tr.critical td:first-child{{color:#c0392b}} tr.high td:first-child{{color:#e67e22}}
tr.medium td:first-child{{color:#b7950b}} tr.low td:first-child{{color:#2980b9}}
tr.info td:first-child{{color:#7f8c8d}}
</style></head><body>
<h1>&#128274; Security Scan Report</h1>
<div class="meta">Total temuan: <b>{}</b> &middot; Dibuat oleh Dealtech Code Scanner</div>
<table><thead><tr><th>Severity</th><th>Nama</th><th>Template</th><th>Lokasi</th><th>Detail</th></tr></thead>
<tbody>{}</tbody></table>
<p class="meta">&#9888;&#65039; Automated scanner mendeteksi ~30-40% vulnerability. Hasil ini bukan jaminan aman; tetap perlu review manual.</p>
</body></html>"####, total, rows)
}

fn format_scan_report(data: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };

    let findings = match parsed["findings"].as_array() {
        Some(f) if !f.is_empty() => f,
        _ => return String::new(),
    };

    let mut critical = 0u32;
    let mut high = 0u32;
    let mut medium = 0u32;
    let mut low = 0u32;
    let mut info = 0u32;
    let mut finding_lines: Vec<String> = Vec::new();
    // Collapse duplicate findings (same name+template repeated across many URLs)
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for f in findings {
        let name = f["info"]["name"].as_str().unwrap_or("Unknown");
        let sev = f["info"]["severity"].as_str().unwrap_or("unknown");
        let matched = f["matched-at"].as_str()
            .or_else(|| f["host"].as_str())
            .unwrap_or("N/A");
        let template_id = f["template-id"].as_str().unwrap_or("unknown");

        // Dedupe: same name + template = same issue type, show once
        let key = format!("{}|{}", name, template_id);
        if !seen.insert(key) {
            continue;
        }

        match sev {
            "critical" => critical += 1,
            "high" => high += 1,
            "medium" => medium += 1,
            "low" => low += 1,
            _ => info += 1,
        }

        let icon = match sev {
            "critical" => "🔴",
            "high" => "🟠",
            "medium" => "🟡",
            "low" => "🔵",
            _ => "⚪",
        };

        finding_lines.push(format!(
            "{} [{}] {}\n   Template: {}\n   URL: {}",
            icon, sev.to_uppercase(), name, template_id, matched
        ));
    }

    let total = critical + high + medium + low + info;
    let mut report = format!("🔒 Security Scan Report\n\n📊 Total: {} finding(s)\n", total);
    if critical > 0 { report.push_str(&format!("🔴 Critical: {}\n", critical)); }
    if high > 0 { report.push_str(&format!("🟠 High: {}\n", high)); }
    if medium > 0 { report.push_str(&format!("🟡 Medium: {}\n", medium)); }
    if low > 0 { report.push_str(&format!("🔵 Low: {}\n", low)); }
    if info > 0 { report.push_str(&format!("⚪ Info: {}\n", info)); }
    report.push_str("\n━━━━━━━━━━━━━━━━━━━━\n\n");
    report.push_str(&finding_lines.join("\n\n"));
    report.push_str("\n\n━━━━━━━━━━━━━━━━━━━━\n");
    // Footer hint depends on what was scanned. If this report already contains
    // an active SQLi finding (sqlmap), the signature-mode disclaimer would be
    // contradictory — so only show the "use sqli mode" hint for nuclei scans.
    let tid = |id: &str| findings.iter().any(|f| {
        f["template-id"].as_str().map(|t| t.starts_with(id)).unwrap_or(false)
    });
    let has_sqli = tid("sqlmap-sqli");
    let has_xss = tid("dalfox-xss");
    let has_tls = tid("testssl-");
    let has_deps = tid("trivy-");
    if has_deps {
        report.push_str("⚠️ Temuan dependency/secret/IaC dari repo — update library ke versi fixed, rotasi & cabut secret yang bocor, perbaiki misconfig.\n");
        report.push_str("💡 Cek lainnya: /scan <url> full  |  /scan <url> xss  |  /scan <url> tls");
    } else if has_sqli {
        report.push_str("⚠️ SQL injection terdeteksi — perbaiki dengan parameterized query / prepared statement.\n");
        report.push_str("💡 Cek lainnya: /scan <url> full  |  /scan <url> xss  |  /scan <url> tls");
    } else if has_xss {
        report.push_str("⚠️ XSS terdeteksi — sanitasi/escape output & pakai Content-Security-Policy.\n");
        report.push_str("💡 Cek lainnya: /scan <url> full  |  /scan <url>?param=nilai sqli  |  /scan <url> tls");
    } else if has_tls {
        report.push_str("⚠️ Masalah TLS/SSL terdeteksi — perbaiki cipher/protokol lemah & sertifikat.\n");
        report.push_str("💡 Cek lainnya: /scan <url> full  |  /scan <url> xss  |  /scan <url>?param=nilai sqli");
    } else {
        report.push_str("⚠️ Mode signature (quick/full/cves/misconfig/exposure) cek misconfig, CVE, exposed files — BUKAN uji SQLi/XSS aktif.\n");
        report.push_str("💡 Uji aktif: /scan <url> sqli  |  /scan <url> xss  |  /scan <url> tls");
    }

    report
}

async fn cmd_scanstatus(
    client: &Client,
    config: &Config,
    chat_id: i64,
    redis: &mut ConnectionManager,
) -> anyhow::Result<()> {
    let scan_key = format!("scan:active:{}", chat_id);
    let scan_data: Option<String> = redis::cmd("GET")
        .arg(&scan_key)
        .query_async(redis)
        .await
        .unwrap_or(None);

    match scan_data {
        Some(data) => {
            if let Ok(info) = serde_json::from_str::<serde_json::Value>(&data) {
                let url = info["url"].as_str().unwrap_or("unknown");
                let mode = info["mode"].as_str().unwrap_or("unknown");
                let started_secs = info["started_at"].as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_secs();
                let elapsed = now_secs.saturating_sub(started_secs);
                let elapsed_str = if started_secs == 0 {
                    "tidak diketahui".to_string()
                } else if elapsed < 60 {
                    format!("{} detik lalu", elapsed)
                } else {
                    format!("{} menit {} detik lalu", elapsed / 60, elapsed % 60)
                };
                send_message(client, &config.telegram_bot_token, chat_id,
                    &format!("🔄 Scan sedang berjalan\n\n🎯 Target: {}\n📋 Mode: {}\n⏱ Mulai: {}", url, mode, elapsed_str)).await?;
            } else {
                send_message(client, &config.telegram_bot_token, chat_id,
                    "🔄 Ada scan yang sedang berjalan.").await?;
            }
        }
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "✅ Tidak ada scan yang sedang berjalan.").await?;
        }
    }
    Ok(())
}

async fn cmd_cancelscan(
    client: &Client,
    config: &Config,
    chat_id: i64,
    redis: &mut ConnectionManager,
) -> anyhow::Result<()> {
    let scan_key = format!("scan:active:{}", chat_id);
    let scan_data: Option<String> = redis::cmd("GET")
        .arg(&scan_key)
        .query_async(redis)
        .await
        .unwrap_or(None);

    if scan_data.is_none() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "✅ Tidak ada scan yang sedang berjalan.").await?;
        return Ok(());
    }

    // Kill nuclei processes
    let _ = tokio::process::Command::new("pkill")
        .args(&["-f", "nuclei.*-u"])
        .output()
        .await;

    // Clear Redis key
    let _: () = redis::cmd("DEL").arg(&scan_key)
        .query_async(redis).await.unwrap_or(());

    send_message(client, &config.telegram_bot_token, chat_id,
        "⛔ Scan dibatalkan.").await?;
    Ok(())
}

async fn cmd_help(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    let is_admin = is_admin_user(db, tg_user).await.unwrap_or(false);

    let mut reply = "🤖 *Dealtech Code AI Agent*\n\n\
        /start — Mulai\n\
        /projects — Lihat daftar project\n\
        /project <slug> — Pilih project aktif\n\
        /newproject <nama> <repo_url> — Tambah project baru\n\
        /runs — Lihat 10 run terakhir\n\
        /cost — Lihat penggunaan token & biaya\n\
        /diff — Lihat file yang diubah di run terakhir\n\
        /pr — Lihat PR terbaru project ini\n\
        /scan <url> [mode] — Security scan website\n\
        /setauth <type> <value> — Set login utk authenticated scan\n\
        /clearauth — Hapus kredensial scan\n\
        /scanstatus — Cek status scan yang berjalan\n\
        /cancelscan — Batalkan scan yang berjalan\n\
        /newsession — Mulai session baru (branch baru)\n\
        /retry — Ulangi run terakhir yang gagal\n\
        /cancel — Batalkan run yang sedang berjalan\n\
        /status — Lihat status saat ini\n\
        /help — Tampilkan bantuan ini\n\n\
        Kirim pesan biasa untuk memulai coding dengan AI agent."
        .to_string();

    if is_admin {
        reply.push_str("\n\n─── *Admin* ───\n\
            /adduser <telegram_id> <nama> — Tambah user\n\
            /removeuser <telegram_id> — Hapus user\n\
            /backup — Backup database (kirim via Telegram)");
    }

    send_message_md(client, &config.telegram_bot_token, chat_id, &reply).await?;
    Ok(())
}

async fn cmd_adduser(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    parts: &[&str],
) -> anyhow::Result<()> {
    let is_admin = is_admin_user(db, tg_user).await?;
    if !is_admin {
        send_message(client, &config.telegram_bot_token, chat_id,
            "⛔ Hanya admin yang bisa menambah user.").await?;
        return Ok(());
    }

    let target_tg_id: i64 = match parts.get(1).and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "Gunakan: /adduser <telegram_id> <nama>").await?;
            return Ok(());
        }
    };
    let name = parts.get(2).unwrap_or(&"User").to_string();

    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM telegram_users WHERE telegram_id = $1)"
    ).bind(target_tg_id).fetch_one(db).await?;

    if exists {
        send_message(client, &config.telegram_bot_token, chat_id,
            "User sudah terdaftar.").await?;
        return Ok(());
    }

    // Wrap both inserts in a transaction — if telegram_users insert fails,
    // the users row is also rolled back (no orphaned records)
    let mut tx = db.begin().await?;

    let new_user_id = Uuid::new_v4();
    let username = format!("tg_{}", target_tg_id);
    sqlx::query(
        "INSERT INTO users (id, username, email, password_hash, role) \
         VALUES ($1, $2, $3, 'telegram_auth', 'developer') \
         ON CONFLICT (username) DO NOTHING"
    )
    .bind(new_user_id)
    .bind(&username)
    .bind(format!("{}@telegram.local", target_tg_id))
    .execute(&mut *tx)
    .await?;

    let actual_user_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM users WHERE username = $1"
    ).bind(&username).fetch_one(&mut *tx).await?;

    sqlx::query(
        "INSERT INTO telegram_users (telegram_id, user_id, name, added_by) VALUES ($1, $2, $3, $4)"
    )
    .bind(target_tg_id)
    .bind(actual_user_id)
    .bind(&name)
    .bind(tg_user.user_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    send_message(client, &config.telegram_bot_token, chat_id,
        &format!("✅ User {} (ID: {}) berhasil ditambahkan.", name, target_tg_id)).await?;
    Ok(())
}

async fn cmd_removeuser(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    parts: &[&str],
) -> anyhow::Result<()> {
    let is_admin = is_admin_user(db, tg_user).await?;
    if !is_admin {
        send_message(client, &config.telegram_bot_token, chat_id,
            "⛔ Hanya admin yang bisa menghapus user.").await?;
        return Ok(());
    }

    let target_tg_id: i64 = match parts.get(1).and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "Gunakan: /removeuser <telegram_id>").await?;
            return Ok(());
        }
    };

    let deleted = sqlx::query("DELETE FROM telegram_users WHERE telegram_id = $1")
        .bind(target_tg_id).execute(db).await?.rows_affected();

    if deleted > 0 {
        send_message(client, &config.telegram_bot_token, chat_id,
            &format!("✅ User dengan Telegram ID {} berhasil dihapus.", target_tg_id)).await?;
    } else {
        send_message(client, &config.telegram_bot_token, chat_id,
            "User tidak ditemukan.").await?;
    }
    Ok(())
}

async fn is_admin_user(db: &PgPool, tg_user: &TelegramDbUser) -> anyhow::Result<bool> {
    let role = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE id = $1")
        .bind(tg_user.user_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_default();
    if role == "admin" {
        return Ok(true);
    }
    let first_id = sqlx::query_scalar::<_, i64>(
        "SELECT telegram_id FROM telegram_users ORDER BY created_at ASC LIMIT 1"
    ).fetch_optional(db).await?;
    Ok(first_id == Some(tg_user.telegram_id))
}

async fn handle_regular_message(
    client: &Client,
    config: &Config,
    db: &PgPool,
    mut redis: ConnectionManager,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let project_id = match tg_user.active_project_id {
        Some(pid) => pid,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "Pilih project dulu dengan /project <slug> atau /projects untuk lihat daftar").await?;
            return Ok(());
        }
    };

    // Rate limiting: max user_rate_limit_per_minute runs per user per minute
    let rate_limit = config.user_rate_limit_per_minute;
    if rate_limit > 0 {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_runs WHERE user_id=$1 AND created_at > NOW() - INTERVAL '1 minute'"
        )
        .bind(tg_user.user_id)
        .fetch_one(db)
        .await
        .unwrap_or(0);
        if count >= rate_limit as i64 {
            send_message(client, &config.telegram_bot_token, chat_id,
                "⏳ Terlalu banyak request. Tunggu sebentar sebelum kirim lagi.").await?;
            return Ok(());
        }
    }

    let started_at = std::time::Instant::now();

    // Non-command messages are routed by AI first; fallback classifier is only a safety net.
    let early_router_placeholder_started = std::time::Instant::now();
    let router_placeholder_id = send_message_with_id(
        client,
        &config.telegram_bot_token,
        chat_id,
        "💭 Lagi mikir...",
    ).await.ok();
    tracing::info!(
        chat_id,
        elapsed_ms = started_at.elapsed().as_millis(),
        step_ms = early_router_placeholder_started.elapsed().as_millis(),
        has_placeholder = router_placeholder_id.is_some(),
        "Telegram fast chat step: sent router placeholder"
    );

    let router_started = std::time::Instant::now();
    let route_input = crate::services::openclaw_service::OpenClawRunInput {
        agent_id: "default".to_string(),
        session_key: format!("telegram_route_{}_{}", tg_user.user_id, project_id),
        user_id: tg_user.user_id.to_string(),
        instructions: String::new(),
        prompt: text.to_string(),
        model: "openclaw".to_string(),
        history: vec![],
    };
    let route_decision = crate::services::openclaw_service::route_prompt(config, &route_input).await.ok();
    let mut route = match route_decision.as_ref().map(|d| d.intent.as_str()) {
        Some("smalltalk") => crate::services::openclaw_service::PromptRoute::Smalltalk,
        Some("chat") => crate::services::openclaw_service::PromptRoute::Chat,
        Some("retry_push") => crate::services::openclaw_service::PromptRoute::RetryPush,
        Some("coding_task") => crate::services::openclaw_service::PromptRoute::CodingTask,
        _ => crate::services::openclaw_service::classify_prompt(text),
    };
    tracing::info!(
        chat_id,
        prompt_len = text.len(),
        route = ?route,
        elapsed_ms = started_at.elapsed().as_millis(),
        step_ms = router_started.elapsed().as_millis(),
        used_ai_router = route_decision.is_some(),
        "Telegram fast chat step: classified prompt"
    );

    if matches!(route, crate::services::openclaw_service::PromptRoute::Smalltalk) {
        let typing_started = std::time::Instant::now();
        let _ = send_chat_action(client, &config.telegram_bot_token, chat_id, "typing").await;
        tracing::info!(
            chat_id,
            elapsed_ms = started_at.elapsed().as_millis(),
            step_ms = typing_started.elapsed().as_millis(),
            "Telegram fast chat step: sent typing action"
        );

        let reply_started = std::time::Instant::now();
        let reply = crate::services::openclaw_service::fallback_smalltalk_response(text);
        tracing::info!(
            chat_id,
            elapsed_ms = started_at.elapsed().as_millis(),
            step_ms = reply_started.elapsed().as_millis(),
            reply_len = reply.len(),
            "Telegram fast chat step: built smalltalk reply"
        );

        let finalize_started = std::time::Instant::now();
        if let Some(message_id) = router_placeholder_id {
            if let Err(e) = edit_message_text(client, &config.telegram_bot_token, chat_id, message_id, &reply).await {
                tracing::error!(chat_id, message_id, error = %e, "Telegram router placeholder finalize edit failed");
                send_long_message(client, &config.telegram_bot_token, chat_id, &reply).await?;
            }
        } else {
            send_placeholder_then_finalize(
                client,
                &config.telegram_bot_token,
                chat_id,
                "💭 Lagi mikir...",
                &reply,
            ).await?;
        }
        tracing::info!(
            chat_id,
            elapsed_ms = started_at.elapsed().as_millis(),
            step_ms = finalize_started.elapsed().as_millis(),
            "Telegram fast chat step: finalized smalltalk reply"
        );

        let project_lookup_started = std::time::Instant::now();
        let project = sqlx::query_as::<_, (String, String, String)>(
            "SELECT slug, repo_url, openclaw_agent_id FROM projects WHERE id = $1"
        ).bind(project_id).fetch_optional(db).await?;
        tracing::info!(
            chat_id,
            elapsed_ms = started_at.elapsed().as_millis(),
            step_ms = project_lookup_started.elapsed().as_millis(),
            "Telegram fast chat step: loaded project"
        );

        if project.is_some() {
            let session_lookup_started = std::time::Instant::now();
            if let Ok(session_id) = get_or_create_session(db, &mut redis, tg_user, project_id).await {
                tracing::info!(
                    chat_id,
                    session_id = %session_id,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    step_ms = session_lookup_started.elapsed().as_millis(),
                    "Telegram fast chat step: got session"
                );
                let add_user_msg_started = std::time::Instant::now();
                let _ = crate::services::session_service::add_message(db, session_id, "user", text).await;
                tracing::info!(
                    chat_id,
                    session_id = %session_id,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    step_ms = add_user_msg_started.elapsed().as_millis(),
                    "Telegram fast chat step: stored user message"
                );
                let add_assistant_msg_started = std::time::Instant::now();
                let _ = crate::services::session_service::add_message(db, session_id, "assistant", &reply).await;
                tracing::info!(
                    chat_id,
                    session_id = %session_id,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    step_ms = add_assistant_msg_started.elapsed().as_millis(),
                    "Telegram fast chat step: stored assistant message"
                );
                tracing::info!(
                    chat_id,
                    user_id = %tg_user.user_id,
                    project_id = %project_id,
                    session_id = %session_id,
                    route = ?route,
                    history_len = 0,
                    prompt_len = text.len(),
                    reply_len = reply.len(),
                    latency_ms = started_at.elapsed().as_millis(),
                    "Telegram fast chat completed"
                );
            } else {
                tracing::warn!(chat_id, "Telegram fast chat step: session persistence skipped for smalltalk");
            }
        }

        return Ok(());
    }

    if matches!(route, crate::services::openclaw_service::PromptRoute::Chat) {
        let early_placeholder_id = router_placeholder_id;
        let project_lookup_started = std::time::Instant::now();
        let project = sqlx::query_as::<_, (String, String, String)>(
            "SELECT slug, repo_url, openclaw_agent_id FROM projects WHERE id = $1"
        ).bind(project_id).fetch_optional(db).await?;
        tracing::info!(
            chat_id,
            elapsed_ms = started_at.elapsed().as_millis(),
            step_ms = project_lookup_started.elapsed().as_millis(),
            "Telegram fast chat step: loaded project"
        );

        let (_project_slug, _repo_url, openclaw_agent_id) = match project {
            Some(p) => p,
            None => {
                send_message(client, &config.telegram_bot_token, chat_id,
                    "❌ Project tidak ditemukan. Pilih ulang dengan /project <slug>").await?;
                return Ok(());
            }
        };

        let history: Vec<(String, String)> = Vec::new();
        let history_len = history.len();
        let input = crate::services::openclaw_service::OpenClawRunInput {
            agent_id: openclaw_agent_id,
            session_key: format!("telegram_chat_user_{}_project_{}", tg_user.user_id, project_id),
            user_id: tg_user.user_id.to_string(),
            instructions: crate::services::openclaw_service::build_chat_instructions(),
            prompt: text.to_string(),
            model: "openclaw".to_string(),
            history,
        };
        let stream_started = std::time::Instant::now();
        let reply = stream_chat_reply_with_placeholder(
            client,
            &config.telegram_bot_token,
            chat_id,
            config,
            input,
            early_placeholder_id,
        ).await?;
        tracing::info!(
            chat_id,
            elapsed_ms = started_at.elapsed().as_millis(),
            step_ms = stream_started.elapsed().as_millis(),
            reply_len = reply.len(),
            "Telegram fast chat step: streamed chat reply"
        );

        let persist_started = std::time::Instant::now();
        if let Ok(real_session_id) = get_or_create_session(db, &mut redis, tg_user, project_id).await {
            let _ = crate::services::session_service::add_message(db, real_session_id, "user", text).await;
            let _ = crate::services::session_service::add_message(db, real_session_id, "assistant", &reply).await;
            tracing::info!(
                chat_id,
                session_id = %real_session_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                step_ms = persist_started.elapsed().as_millis(),
                "Telegram fast chat step: persisted chat exchange"
            );
        } else {
            tracing::warn!(chat_id, "Telegram fast chat step: session persistence skipped for chat");
        }
        tracing::info!(
            chat_id,
            user_id = %tg_user.user_id,
            project_id = %project_id,
            route = ?route,
            history_len,
            prompt_len = text.len(),
            reply_len = reply.len(),
            latency_ms = started_at.elapsed().as_millis(),
            "Telegram fast chat completed"
        );
        return Ok(());
    }

    // Send processing indicator
    send_message(client, &config.telegram_bot_token, chat_id, "⏳ Sedang diproses...").await?;

    // Get project info
    let project = sqlx::query_as::<_, (String, String, String)>(
        "SELECT slug, repo_url, openclaw_agent_id FROM projects WHERE id = $1"
    ).bind(project_id).fetch_optional(db).await?;

    let (_project_slug, _repo_url, openclaw_agent_id) = match project {
        Some(p) => p,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "❌ Project tidak ditemukan. Pilih ulang dengan /project <slug>").await?;
            return Ok(());
        }
    };

    // Auto-create or reuse today's coding session (checks Redis pinned session first)
    let session_id = get_or_create_session(db, &mut redis, tg_user, project_id).await?;

    // Create run request
    let req = crate::domain::agent_run::CreateRunRequest {
        prompt: text.to_string(),
        auto_mode: Some("auto_trusted".to_string()),
        model: None,
    };

    let run = run_orchestrator::create_run(
        db, session_id, project_id, tg_user.user_id, req, &openclaw_agent_id, Some(chat_id),
    ).await?;

    // Trigger worker immediately via Redis pub/sub (worker handles execution + Telegram reply)
    let mut r = redis;
    let payload = serde_json::json!({"run_id": run.id.to_string()});
    let _: std::result::Result<(), _> = redis::cmd("PUBLISH")
        .arg("agent_run:queued")
        .arg(payload.to_string())
        .query_async(&mut r)
        .await;

    Ok(())
}

async fn get_or_create_session(
    db: &PgPool,
    redis: &mut ConnectionManager,
    tg_user: &TelegramDbUser,
    project_id: Uuid,
) -> anyhow::Result<Uuid> {
    let pin_key = format!("tg:pinned_session:{}:{}", tg_user.user_id, project_id);
    let force_session_key = format!("tg:force_new_session:{}:{}", tg_user.user_id, project_id);

    // /newsession should be the only thing that rotates the coding session.
    // Consume this one-shot flag first before checking any cached/default session.
    let force_new_session: bool = redis::cmd("GETDEL")
        .arg(&force_session_key)
        .query_async(redis)
        .await
        .unwrap_or(None::<String>)
        .is_some();

    if !force_new_session {
        // Fast path: cached pinned session for this user+project.
        let pinned: Option<String> = redis::cmd("GET")
            .arg(&pin_key)
            .query_async(redis)
            .await
            .unwrap_or(None);

        if let Some(sid_str) = pinned {
            if let Ok(sid) = sid_str.parse::<Uuid>() {
                let exists = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM coding_sessions WHERE id = $1 AND project_id = $2 AND user_id = $3)"
                )
                .bind(sid)
                .bind(project_id)
                .bind(tg_user.user_id)
                .fetch_one(db)
                .await
                .unwrap_or(false);
                if exists {
                    return Ok(sid);
                }
            }
        }

        // Fallback: reuse the latest existing session for this user+project,
        // regardless of day. Daily rollover was causing unnecessary cold starts.
        let existing_session = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM coding_sessions \
             WHERE project_id = $1 AND user_id = $2 \
             ORDER BY created_at DESC LIMIT 1"
        )
        .bind(project_id)
        .bind(tg_user.user_id)
        .fetch_optional(db)
        .await?;

        if let Some(sid) = existing_session {
            let _: std::result::Result<(), _> = redis::cmd("SET")
                .arg(&pin_key)
                .arg(sid.to_string())
                .query_async(redis)
                .await;
            return Ok(sid);
        }
    }

    // Create new session only when no reusable one exists, or after explicit /newsession.
    let session_id = Uuid::new_v4();
    let title = format!("Telegram - {} - {}", tg_user.name, chrono_today());
    sqlx::query(
        "INSERT INTO coding_sessions (id, project_id, user_id, title) VALUES ($1, $2, $3, $4)"
    )
    .bind(session_id).bind(project_id).bind(tg_user.user_id).bind(&title)
    .execute(db).await?;

    let _: std::result::Result<(), _> = redis::cmd("SET")
        .arg(&pin_key)
        .arg(session_id.to_string())
        .query_async(redis)
        .await;

    Ok(session_id)
}

fn chrono_today() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!("{}-{:02}-{:02}", now.year(), now.month() as u8, now.day())
}

async fn send_long_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    if text.len() <= 4000 {
        send_message(client, token, chat_id, text).await?;
    } else {
        // Split into chunks of 4000 chars
        let mut remaining = text;
        while !remaining.is_empty() {
            let end = remaining.len().min(4000);
            // Try to split at newline
            let split_at = if end < remaining.len() {
                remaining[..end].rfind('\n').unwrap_or(end)
            } else {
                end
            };
            let chunk = &remaining[..split_at];
            send_message(client, token, chat_id, chunk).await?;
            remaining = &remaining[split_at..].trim_start();
            if !remaining.is_empty() {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
        }
    }
    Ok(())
}

async fn send_placeholder_then_finalize(
    client: &Client,
    token: &str,
    chat_id: i64,
    placeholder: &str,
    final_text: &str,
) -> anyhow::Result<()> {
    let placeholder_id = match send_message_with_id(client, token, chat_id, placeholder).await {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::error!(chat_id, error = %e, "Telegram placeholder send failed");
            None
        }
    };
    if let Some(message_id) = placeholder_id {
        if final_text.len() <= 4000 {
            if let Err(e) = edit_message_text(client, token, chat_id, message_id, final_text).await {
                tracing::error!(chat_id, message_id, error = %e, "Telegram placeholder finalize edit failed");
                send_long_message(client, token, chat_id, final_text).await?;
            }
        } else {
            let first_window = final_text.len().min(4000);
            let first_chunk_end = final_text[..first_window].rfind('\n').unwrap_or(first_window);
            let first_chunk = &final_text[..first_chunk_end];
            if let Err(e) = edit_message_text(client, token, chat_id, message_id, first_chunk).await {
                tracing::error!(chat_id, message_id, error = %e, "Telegram placeholder first chunk edit failed");
                send_message(client, token, chat_id, first_chunk).await?;
            }
            let remaining = final_text[first_chunk_end..].trim_start();
            if !remaining.is_empty() {
                send_long_message(client, token, chat_id, remaining).await?;
            }
        }
    } else {
        send_long_message(client, token, chat_id, final_text).await?;
    }
    Ok(())
}

async fn stream_chat_reply(
    client: &Client,
    token: &str,
    chat_id: i64,
    config: &Config,
    input: crate::services::openclaw_service::OpenClawRunInput,
) -> anyhow::Result<String> {
    stream_chat_reply_with_placeholder(client, token, chat_id, config, input, None).await
}

async fn stream_chat_reply_with_placeholder(
    client: &Client,
    token: &str,
    chat_id: i64,
    config: &Config,
    input: crate::services::openclaw_service::OpenClawRunInput,
    existing_placeholder_id: Option<i64>,
) -> anyhow::Result<String> {
    let placeholder_id = if let Some(id) = existing_placeholder_id {
        Some(id)
    } else {
        match send_message_with_id(client, token, chat_id, "💭 Lagi mikir...").await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::error!(chat_id, error = %e, "Telegram stream placeholder send failed");
                None
            }
        }
    };
    let mut last_typing_at = Instant::now() - Duration::from_secs(10);
    let mut last_edit_at = Instant::now() - Duration::from_secs(10);
    let mut last_sent_text = String::new();
    let mut stream_text = String::new();
    let mut final_done_text = String::new();
    let mut stream_broken = false;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::services::openclaw_service::OpenClawEvent>(100);
    let cfg = config.clone();
    let inp = input.clone();
    tokio::spawn(async move {
        let _ = crate::services::openclaw_service::run_stream(&cfg, inp, tx).await;
    });

    while let Some(ev) = rx.recv().await {
        if last_typing_at.elapsed() >= Duration::from_secs(4) {
            let _ = send_chat_action(client, token, chat_id, "typing").await;
            last_typing_at = Instant::now();
        }

        if matches!(ev.event_type.as_str(), "response.output_text.done" | "message.completed" | "response.completed") {
            if let Some(t) = ev.payload.get("text").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    final_done_text = t.to_string();
                }
            }
            if final_done_text.is_empty() {
                if let Some(arr) = ev.payload.get("response")
                    .and_then(|r| r.get("output"))
                    .and_then(|o| o.as_array())
                {
                    for msg in arr.iter().rev() {
                        if let Some(parts) = msg.get("content").and_then(|c| c.as_array()) {
                            for part in parts {
                                let is_text = part.get("type").and_then(|t| t.as_str()) == Some("text");
                                if is_text {
                                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                        if !t.is_empty() {
                                            final_done_text = t.to_string();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        if !final_done_text.is_empty() {
                            break;
                        }
                    }
                }
            }
        }

        let is_delta = matches!(ev.event_type.as_str(),
            "assistant.delta" | "response.output_text.delta" | "content_block_delta");
        if is_delta {
            let delta = ev.payload.get("delta").and_then(|d| d.as_str())
                .or_else(|| ev.payload.get("text").and_then(|d| d.as_str()))
                .or_else(|| ev.payload.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()))
                .unwrap_or("");
            if !delta.is_empty() {
                stream_text.push_str(delta);
            }
        }

        if stream_broken {
            continue;
        }

        let Some(message_id) = placeholder_id else { continue; };
        if stream_text.trim().is_empty() {
            continue;
        }
        if last_edit_at.elapsed() < Duration::from_millis(900)
            && stream_text.len().saturating_sub(last_sent_text.len()) < 80
        {
            continue;
        }

        let preview = crate::services::openclaw_service::sanitize_user_facing_response(&stream_text);
        let preview = preview.trim();
        if preview.is_empty() {
            continue;
        }
        let preview = if preview.len() > 3800 {
            &preview[..3800]
        } else {
            preview
        };
        if preview == last_sent_text {
            continue;
        }

        if edit_message_text(client, token, chat_id, message_id, preview).await.is_ok() {
            last_edit_at = Instant::now();
            last_sent_text = preview.to_string();
        } else {
            tracing::error!(chat_id, message_id, "Telegram stream preview edit failed; falling back to final send");
            stream_broken = true;
        }
    }

    let chosen = if !final_done_text.trim().is_empty() {
        final_done_text
    } else {
        stream_text
    };
    let mut safe = crate::services::openclaw_service::sanitize_user_facing_response(&chosen);
    if safe.trim().is_empty() {
        let fallback = crate::services::openclaw_service::run_chat(config, input).await?;
        safe = crate::services::openclaw_service::sanitize_user_facing_response(&fallback);
    }
    if safe.trim().is_empty() {
        safe = "Siap. Coba kirim ulang dengan sedikit detail tambahan ya.".to_string();
    }

    if let Some(message_id) = placeholder_id {
        if safe.len() <= 4000 {
            if let Err(e) = edit_message_text(client, token, chat_id, message_id, &safe).await {
                tracing::error!(chat_id, message_id, error = %e, "Telegram final stream edit failed");
                send_long_message(client, token, chat_id, &safe).await?;
            }
        } else {
            let first_window = safe.len().min(4000);
            let first_chunk_end = safe[..first_window].rfind('\n').unwrap_or(first_window);
            let first_chunk = &safe[..first_chunk_end];
            if let Err(e) = edit_message_text(client, token, chat_id, message_id, first_chunk).await {
                tracing::error!(chat_id, message_id, error = %e, "Telegram final first chunk edit failed");
                send_message(client, token, chat_id, first_chunk).await?;
            }
            let remaining = safe[first_chunk_end..].trim_start();
            if !remaining.is_empty() {
                send_long_message(client, token, chat_id, remaining).await?;
            }
        }
    } else {
        send_placeholder_then_finalize(client, token, chat_id, "💭 Lagi mikir...", &safe).await?;
    }

    Ok(safe)
}

async fn send_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let _ = send_message_with_id(client, token, chat_id, text).await?;
    Ok(())
}

fn convert_markdown_for_telegram(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        let line = if line.starts_with("### ") { &line[4..] }
            else if line.starts_with("## ") { &line[3..] }
            else if line.starts_with("# ") { &line[2..] }
            else { line };
        let line = line.replace("**", "*");
        let line = line.replace("__", "_");
        let line = line.replace("~~", "");
        result.push_str(&line);
        result.push('\n');
    }
    if result.ends_with('\n') { result.pop(); }
    result
}

async fn send_message_with_id(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<i64> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let converted = convert_markdown_for_telegram(text);
    let body_md = serde_json::json!({
        "chat_id": chat_id,
        "text": converted,
        "parse_mode": "Markdown",
    });
    let body_plain = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
    });

    let mut retries = 0u32;
    loop {
        let resp = client.post(&url).json(&body_md).send().await?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            retries += 1;
            if retries > 5 {
                anyhow::bail!("Telegram rate limit exceeded after 5 retries");
            }
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(retries as u64 * 2);
            tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
            continue;
        }
        let status = resp.status();
        let parsed: TelegramResponse<TelegramMessage> = resp.json().await?;
        if status.is_success() && parsed.ok {
            let message_id = parsed
                .result
                .map(|m| m.message_id)
                .ok_or_else(|| anyhow::anyhow!("Telegram sendMessage missing message_id"))?;
            return Ok(message_id);
        }

        let markdown_desc = parsed.description.unwrap_or_else(|| "unknown error".to_string());
        tracing::warn!(chat_id, status = %status, error = %markdown_desc, "Telegram Markdown send failed; retrying as plain text");
        let resp_plain = client.post(&url).json(&body_plain).send().await?;
        let status_plain = resp_plain.status();
        let parsed_plain: TelegramResponse<TelegramMessage> = resp_plain.json().await?;
        if !status_plain.is_success() || !parsed_plain.ok {
            anyhow::bail!(
                "Telegram sendMessage failed with status {} code {:?}: {}",
                status_plain,
                parsed_plain.error_code,
                parsed_plain.description.unwrap_or_else(|| "unknown error".to_string())
            );
        }
        let message_id = parsed_plain
            .result
            .map(|m| m.message_id)
            .ok_or_else(|| anyhow::anyhow!("Telegram sendMessage missing message_id"))?;
        return Ok(message_id);
    }
}

async fn send_chat_action(
    client: &Client,
    token: &str,
    chat_id: i64,
    action: &str,
) -> anyhow::Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendChatAction", token);
    let body = serde_json::json!({
        "chat_id": chat_id,
        "action": action,
    });
    let _ = client.post(&url).json(&body).send().await?;
    Ok(())
}

async fn edit_message_text(
    client: &Client,
    token: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!("https://api.telegram.org/bot{}/editMessageText", token);
    let converted = convert_markdown_for_telegram(text);
    let body_md = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": converted,
        "parse_mode": "Markdown",
    });
    let resp = client.post(&url).json(&body_md).send().await?;
    if resp.status().is_success() {
        return Ok(());
    }

    let err_md = resp.text().await.unwrap_or_default();
    tracing::warn!(chat_id, message_id, error = %err_md, "Telegram Markdown edit failed; retrying as plain text");

    let body_plain = serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": text,
    });
    let resp_plain = client.post(&url).json(&body_plain).send().await?;
    if !resp_plain.status().is_success() {
        let err = resp_plain.text().await.unwrap_or_default();
        tracing::error!(chat_id, message_id, error = %err, "Telegram editMessageText failed");
        anyhow::bail!("Telegram editMessageText failed: {}", err);
    }
    Ok(())
}

// Send an in-memory file as a Telegram document (used for scan report export).
// Skips silently if too large for Telegram (50MB). Best-effort, never panics.
async fn send_scan_document(
    client: &Client,
    token: &str,
    chat_id: i64,
    bytes: Vec<u8>,
    file_name: &str,
    mime: &str,
    caption: &str,
) -> anyhow::Result<()> {
    if bytes.is_empty() || bytes.len() >= 50 * 1024 * 1024 {
        return Ok(());
    }
    let url = format!("https://api.telegram.org/bot{}/sendDocument", token);
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name.to_string())
        .mime_str(mime)?;
    let form = reqwest::multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .text("caption", caption.to_string())
        .part("document", part);
    let _ = client.post(&url).multipart(form).send().await?;
    Ok(())
}

async fn send_message_md(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "Markdown",
    });

    let mut retries = 0u32;
    loop {
        let resp = client.post(&url).json(&body).send().await?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            retries += 1;
            if retries > 5 {
                anyhow::bail!("Telegram rate limit exceeded after 5 retries");
            }
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(retries as u64 * 2);
            tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
            continue;
        }
        if status.is_success() {
            break;
        }
        // If Telegram rejects Markdown (400), fallback to plain text
        if status == reqwest::StatusCode::BAD_REQUEST {
            tracing::warn!(chat_id = %chat_id, "send_message_md: Markdown rejected, falling back to plain text");
            let plain_body = serde_json::json!({
                "chat_id": chat_id,
                "text": text,
            });
            let fallback_resp = client.post(&url).json(&plain_body).send().await?;
            if !fallback_resp.status().is_success() {
                let fb_text = fallback_resp.text().await.unwrap_or_default();
                tracing::error!(chat_id = %chat_id, response = %fb_text, "send_message_md: plain text fallback also failed");
                anyhow::bail!("Telegram sendMessage failed: {}", fb_text);
            }
        } else {
            let err_text = resp.text().await.unwrap_or_default();
            tracing::error!(chat_id = %chat_id, status = %status, response = %err_text, "send_message_md: unexpected status");
            anyhow::bail!("Telegram sendMessage failed with status {}: {}", status, err_text);
        }
        break;
    }
    Ok(())
}
