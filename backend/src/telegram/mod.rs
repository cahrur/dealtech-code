use redis::aio::ConnectionManager;
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::config::Config;
use crate::services::run_orchestrator;

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
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

    if text.starts_with('/') {
        handle_command(client, config, db, redis.clone(), &tg_user, chat_id, text).await?;
    } else {
        handle_regular_message(client, config, db, redis, &tg_user, chat_id, text).await?;
    }

    Ok(())
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

    // Set force_new_branch flag in Redis — consumed once by next run
    let force_key = format!("tg:force_new_branch:{}:{}", tg_user.user_id, project_id);
    let _: std::result::Result<(), _> = redis::cmd("SETEX")
        .arg(&force_key)
        .arg(86400u64)
        .arg("1")
        .query_async(redis)
        .await;

    // Also clear any pinned session so get_or_create_session makes a fresh one
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

    // Check Redis for pinned session — cleared by /newsession to force a fresh one
    let pinned: Option<String> = redis::cmd("GET")
        .arg(&pin_key)
        .query_async(redis)
        .await
        .unwrap_or(None);

    if let Some(sid_str) = pinned {
        if let Ok(sid) = sid_str.parse::<Uuid>() {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM coding_sessions WHERE id = $1)"
            )
            .bind(sid)
            .fetch_one(db)
            .await
            .unwrap_or(false);
            if exists {
                return Ok(sid);
            }
        }
    }

    // Check for existing session today for this user + project
    let today_session = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM coding_sessions \
         WHERE project_id = $1 AND user_id = $2 \
         AND created_at::date = CURRENT_DATE \
         ORDER BY created_at DESC LIMIT 1"
    )
    .bind(project_id)
    .bind(tg_user.user_id)
    .fetch_optional(db)
    .await?;

    if let Some(sid) = today_session {
        // Pin so /newsession can clear it next time
        let _: std::result::Result<(), _> = redis::cmd("SETEX")
            .arg(&pin_key).arg(86400u64).arg(sid.to_string())
            .query_async(redis).await;
        return Ok(sid);
    }

    // Create new session
    let session_id = Uuid::new_v4();
    let title = format!("Telegram - {} - {}", tg_user.name, chrono_today());
    sqlx::query(
        "INSERT INTO coding_sessions (id, project_id, user_id, title) VALUES ($1, $2, $3, $4)"
    )
    .bind(session_id).bind(project_id).bind(tg_user.user_id).bind(&title)
    .execute(db).await?;

    // Pin the new session in Redis (24h TTL)
    let _: std::result::Result<(), _> = redis::cmd("SETEX")
        .arg(&pin_key).arg(86400u64).arg(session_id.to_string())
        .query_async(redis).await;

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

async fn send_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
    });

    let mut retries = 0u32;
    loop {
        let resp = client.post(&url).json(&body).send().await?;
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
        break;
    }
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
