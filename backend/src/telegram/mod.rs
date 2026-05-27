use redis::aio::ConnectionManager;
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::config::Config;
use crate::domain::policy::PolicyConfig;
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
    let client = Client::new();
    let mut offset: i64 = 0;

    loop {
        match get_updates(&client, &config.telegram_bot_token, offset).await {
            Ok(updates) => {
                for update in updates {
                    offset = update.update_id + 1;
                    if let Some(msg) = update.message {
                        let db = db.clone();
                        let redis = redis.clone();
                        let config = config.clone();
                        let client = client.clone();
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
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
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
    let body: TelegramResponse<Vec<Update>> = resp.json().await?;
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
        "/backup" => cmd_backup(client, config, db, tg_user, chat_id).await?,
        "/newsession" => cmd_newsession(client, config, db, tg_user, chat_id, &mut redis).await?,
        "/runs" => cmd_runs(client, config, db, tg_user, chat_id).await?,
        "/status" => cmd_status(client, config, db, tg_user, chat_id).await?,
        "/help" => cmd_help(client, config, chat_id).await?,
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
    let project_info = if let Some(pid) = tg_user.active_project_id {
        let row = sqlx::query_as::<_, (String, String)>(
            "SELECT name, slug FROM projects WHERE id = $1"
        ).bind(pid).fetch_optional(db).await?;
        match row {
            Some((name, slug)) => format!("📂 Project: {} ({})\n", name, slug),
            None => "📂 Project: (tidak ditemukan)\n".to_string(),
        }
    } else {
        "📂 Project: belum dipilih\n".to_string()
    };
    let reply = format!("📊 Status\n\n{}👤 User: {}", project_info, tg_user.name);
    send_message(client, &config.telegram_bot_token, chat_id, &reply).await?;
    Ok(())
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
    let session_id = get_or_create_session(db, tg_user, project_id).await?;
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
            send_message(client, &config.telegram_bot_token, chat_id,
                "Tidak ada run yang sedang berjalan.").await?;
            return Ok(());
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
        "SELECT id FROM projects p \
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
        lines.push(format!(
            "{} [{}] {}\n   branch: {} | commit: {} | cost: {}\n   \"{}...\"\n",
            status_icon, short_id, status,
            branch_short, commit, cost_str,
            prompt_short
        ));
    }

    send_message(client, &config.telegram_bot_token, chat_id, &lines.join("")).await?;
    Ok(())
}

async fn cmd_help(client: &Client, config: &Config, chat_id: i64) -> anyhow::Result<()> {
    let reply = "🤖 Dealtech Code AI Agent\n\n\
        /start — Mulai\n\
        /projects — Lihat daftar project\n\
        /project <slug> — Pilih project aktif\n\
        /newproject <nama> <repo_url> — Tambah project baru\n\
        /runs — Lihat 10 run terakhir\n\
        /newsession — Mulai session baru (branch baru)\n\
        /retry — Ulangi run terakhir yang gagal\n\
        /cancel — Batalkan run yang sedang berjalan\n\
        /status — Lihat status saat ini\n\
        /help — Tampilkan bantuan ini\n\n\
        Admin:\n\
        /adduser <telegram_id> <nama> — Tambah user\n\
        /removeuser <telegram_id> — Hapus user\n\
        /backup — Backup database (kirim via Telegram)\n\n\
        Kirim pesan biasa untuk memulai coding dengan AI agent.";
    send_message(client, &config.telegram_bot_token, chat_id, reply).await?;
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
    redis: ConnectionManager,
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

    let (project_slug, repo_url, openclaw_agent_id) = match project {
        Some(p) => p,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "❌ Project tidak ditemukan. Pilih ulang dengan /project <slug>").await?;
            return Ok(());
        }
    };

    // Auto-create or reuse today's coding session (checks Redis pinned session first)
    let session_id = get_or_create_session(db, tg_user, project_id).await?;

    // Create run request
    let req = crate::domain::agent_run::CreateRunRequest {
        prompt: text.to_string(),
        auto_mode: Some("auto_trusted".to_string()),
        model: None,
    };

    let run = run_orchestrator::create_run(
        db, session_id, project_id, tg_user.user_id, req, &openclaw_agent_id, Some(chat_id),
    ).await?;

    // Load policy
    let policy_config = {
        let row = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT policy FROM project_policies WHERE project_id = $1"
        ).bind(project_id).fetch_optional(db).await?;
        match row {
            Some(val) => serde_json::from_value::<PolicyConfig>(val).unwrap_or_default(),
            None => PolicyConfig::default(),
        }
    };

    // Execute run
    let db_arc = Arc::new(db.clone());
    let run_id = run.id;
    run_orchestrator::execute_run(
        db_arc.clone(), redis, config.clone().into(), run_id,
        "default".to_string(), project_slug, repo_url, policy_config,
    ).await;

    // Get the assistant reply from messages
    let reply = sqlx::query_scalar::<_, String>(
        "SELECT content FROM messages WHERE session_id = $1 AND role = 'assistant' ORDER BY created_at DESC LIMIT 1"
    ).bind(session_id).fetch_optional(db).await?.unwrap_or_else(|| "Selesai.".to_string());

    // Send reply, split if too long
    send_long_message(client, &config.telegram_bot_token, chat_id, &reply).await?;

    Ok(())
}

async fn get_or_create_session(
    db: &PgPool,
    tg_user: &TelegramDbUser,
    project_id: Uuid,
) -> anyhow::Result<Uuid> {
    // Check for existing session today for this telegram user + project (fresh context per day)
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
        return Ok(sid);
    }

    // Create new session
    let session_id = Uuid::new_v4();
    let title = format!("Telegram - {} - {}", tg_user.name, chrono_today());
    sqlx::query(
        "INSERT INTO coding_sessions (id, project_id, user_id, title) VALUES ($1, $2, $3, $4)"
    )
    .bind(session_id)
    .bind(project_id)
    .bind(tg_user.user_id)
    .bind(&title)
    .execute(db)
    .await?;

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
