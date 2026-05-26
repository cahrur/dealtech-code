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
        handle_command(client, config, db, &tg_user, chat_id, text).await?;
    } else {
        handle_regular_message(client, config, db, redis, &tg_user, chat_id, text).await?;
    }

    Ok(())
}

async fn handle_command(
    client: &Client,
    config: &Config,
    db: &PgPool,
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
        "/status" => cmd_status(client, config, db, tg_user, chat_id).await?,
        "/help" => cmd_help(client, config, db, tg_user, chat_id).await?,
        "/cost" => cmd_cost(client, config, db, tg_user, chat_id, &parts).await?,
        "/diff" => cmd_diff(client, config, db, tg_user, chat_id).await?,
        "/adduser" => cmd_adduser(client, config, db, tg_user, chat_id, &parts).await?,
        "/removeuser" => cmd_removeuser(client, config, db, tg_user, chat_id, &parts).await?,
        "/newproject" => cmd_newproject(client, config, db, tg_user, chat_id, &parts).await?,
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

async fn cmd_help(client: &Client, config: &Config, db: &PgPool, tg_user: &TelegramDbUser, chat_id: i64) -> anyhow::Result<()> {
    let is_admin = is_admin_user(db, tg_user).await.unwrap_or(false);
    let admin_section = if is_admin {
        "\n\nAdmin:\n\
        /adduser <telegram_id> <nama> — Tambah user\n\
        /removeuser <telegram_id> — Hapus user"
    } else {
        ""
    };
    let reply = format!(
        "🤖 Dealtech Code AI Agent\n\n\
        /start — Mulai\n\
        /projects — Lihat daftar project\n\
        /project <slug> — Pilih project aktif\n\
        /newproject <nama> <repo_url> — Buat project baru\n\
        /status — Lihat status saat ini\n\
        /cost [period] — Lihat biaya (today|yesterday|month|lastmonth|year|all)\n\
        /diff — Lihat diff commit terakhir\n\
        /help — Tampilkan bantuan ini{admin_section}\n\n\
        Kirim pesan biasa untuk memulai coding dengan AI agent.",
        admin_section = admin_section
    );
    send_message(client, &config.telegram_bot_token, chat_id, &reply).await?;
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

    // Auto-create user in users table
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
    .execute(db)
    .await?;

    let actual_user_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM users WHERE username = $1"
    ).bind(&username).fetch_one(db).await?;

    sqlx::query(
        "INSERT INTO telegram_users (telegram_id, user_id, name, added_by) VALUES ($1, $2, $3, $4)"
    )
    .bind(target_tg_id)
    .bind(actual_user_id)
    .bind(&name)
    .bind(tg_user.user_id)
    .execute(db)
    .await?;

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

async fn cmd_newproject(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
    parts: &[&str],
) -> anyhow::Result<()> {
    // Format: /newproject <nama> <repo_url>
    if parts.len() < 3 {
        send_message(client, &config.telegram_bot_token, chat_id,
            "Gunakan: /newproject <nama> <repo_url>\nContoh: /newproject test-martabak https://github.com/user/test-martabak").await?;
        return Ok(());
    }
    let name = parts[1].trim();
    let repo_url = parts[2].trim();

    // Buat slug dari nama (lowercase, spasi jadi dash)
    let slug = name.to_lowercase().replace(' ', "-");

    // Validasi repo_url
    if !repo_url.starts_with("https://") {
        send_message(client, &config.telegram_bot_token, chat_id,
            "⚠️ repo_url harus diawali https://").await?;
        return Ok(());
    }

    // Sanitize: strip token dari URL (https://x-access-token:TOKEN@github.com → https://github.com)
    let repo_url = if let Some(at_pos) = repo_url.find('@') {
        format!("https://{}", &repo_url[at_pos + 1..])
    } else {
        repo_url.to_string()
    };

    // Cek apakah slug sudah ada
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE slug = $1)")
        .bind(&slug)
        .fetch_one(db)
        .await
        .unwrap_or(false);
    if exists {
        send_message(client, &config.telegram_bot_token, chat_id,
            &format!("⚠️ Project dengan slug `{}` sudah ada. Gunakan nama lain.", slug)).await?;
        return Ok(());
    }

    // Buat project
    let project_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO projects (id, team_id, name, slug, repo_url, openclaw_agent_id)
         VALUES (uuid_generate_v4(), uuid_generate_v4(), $1, $2, $3, 'default')
         RETURNING id"
    )
    .bind(name)
    .bind(&slug)
    .bind(repo_url.as_str())
    .fetch_one(db)
    .await
    .map_err(|e| anyhow::anyhow!("Gagal buat project: {}", e))?;

    // Tambah user sebagai admin project
    let _ = sqlx::query(
        "INSERT INTO project_members (project_id, user_id, role) VALUES ($1, $2, 'admin')"
    )
    .bind(project_id)
    .bind(tg_user.user_id)
    .execute(db)
    .await;

    // Set sebagai active project
    let _ = sqlx::query(
        "UPDATE telegram_users SET active_project_id = $1 WHERE telegram_id = $2"
    )
    .bind(project_id)
    .bind(tg_user.telegram_id)
    .execute(db)
    .await;

    send_message(client, &config.telegram_bot_token, chat_id,
        &format!("✅ Project `{}` berhasil dibuat!\nRepo: {}\nSlug: {}\n\nProject ini sudah di-set sebagai project aktif. Langsung kirim prompt untuk mulai coding.",
            name, repo_url, slug)).await?;
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
    let period = parts.get(1).unwrap_or(&"today").trim();
    let (period_filter, period_label) = match period {
        "today" => ("created_at >= CURRENT_DATE", "Hari Ini"),
        "yesterday" => ("created_at >= CURRENT_DATE - INTERVAL '1 day' AND created_at < CURRENT_DATE", "Kemarin"),
        "month" => ("DATE_TRUNC('month', created_at) = DATE_TRUNC('month', NOW())", "Bulan Ini"),
        "lastmonth" => ("DATE_TRUNC('month', created_at) = DATE_TRUNC('month', NOW() - INTERVAL '1 month')", "Bulan Lalu"),
        "year" => ("DATE_TRUNC('year', created_at) = DATE_TRUNC('year', NOW())", "Tahun Ini"),
        "all" => ("1=1", "Semua"),
        _ => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "❓ Period tidak valid. Gunakan: today | yesterday | month | lastmonth | year | all").await?;
            return Ok(());
        }
    };

    let query = format!(
        "SELECT \
            COALESCE(COUNT(*), 0) as total_runs, \
            COALESCE(SUM(tokens_input), 0) as total_input, \
            COALESCE(SUM(tokens_output), 0) as total_output, \
            COALESCE(SUM(cost_usd), 0)::FLOAT8 as total_cost, \
            COALESCE(COUNT(*) FILTER (WHERE status = 'completed'), 0) as completed_runs, \
            COALESCE(COUNT(*) FILTER (WHERE status = 'failed_agent'), 0) as failed_runs \
         FROM agent_runs WHERE user_id = $1 AND {}",
        period_filter
    );

    let row = sqlx::query_as::<_, (i64, i64, i64, f64, i64, i64)>(&query)
        .bind(tg_user.user_id)
        .fetch_one(db)
        .await?;

    let (total_runs, total_input, total_output, total_cost, completed_runs, failed_runs) = row;

    let reply = format!(
        "💰 Cost Report — {}\n\n\
         ✅ Run selesai: {}\n\
         ❌ Run gagal: {}\n\
         📊 Total run: {}\n\n\
         🔤 Token input:  {}\n\
         🔤 Token output: {}\n\
         💵 Estimasi biaya: ${:.4}\n\n\
         Gunakan /cost <period> untuk periode lain:\n\
         today | yesterday | month | lastmonth | year | all",
        period_label, completed_runs, failed_runs, total_runs,
        format_number(total_input), format_number(total_output), total_cost
    );

    send_message(client, &config.telegram_bot_token, chat_id, &reply).await?;
    Ok(())
}

fn format_number(n: i64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

async fn cmd_diff(
    client: &Client,
    config: &Config,
    db: &PgPool,
    tg_user: &TelegramDbUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    // Find last completed run for this user
    let last_run = sqlx::query_as::<_, (Uuid, Option<String>, Option<String>)>(
        "SELECT id, worktree_path, branch_name FROM agent_runs \
         WHERE user_id = $1 AND status = 'completed' AND commit_sha IS NOT NULL \
         ORDER BY finished_at DESC LIMIT 1"
    )
    .bind(tg_user.user_id)
    .fetch_optional(db)
    .await?;

    let (_run_id, worktree_path, _branch) = match last_run {
        Some(r) => r,
        None => {
            send_message(client, &config.telegram_bot_token, chat_id,
                "Belum ada run yang selesai dengan commit.").await?;
            return Ok(());
        }
    };

    // Get workspace path from project config
    let workspace_from_project = if let Some(pid) = tg_user.active_project_id {
        let slug = sqlx::query_scalar::<_, String>(
            "SELECT slug FROM projects WHERE id = $1"
        ).bind(pid).fetch_optional(db).await?.unwrap_or_default();
        if !slug.is_empty() {
            Some(format!("{}/default/{}", config.workspaces_path, slug))
        } else {
            None
        }
    } else {
        None
    };

    // Try worktree_path first, then workspace from project
    let diff_path = worktree_path
        .or(workspace_from_project)
        .unwrap_or_default();

    if diff_path.is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "Tidak bisa menemukan workspace untuk diff.").await?;
        return Ok(());
    }

    // Run git diff
    let output = tokio::process::Command::new("git")
        .args(["-C", &diff_path, "diff", "HEAD~1", "HEAD"])
        .output()
        .await;

    let diff_text = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("Git diff gagal: {}", err)).await?;
            return Ok(());
        }
        Err(e) => {
            send_message(client, &config.telegram_bot_token, chat_id,
                &format!("Error menjalankan git: {}", e)).await?;
            return Ok(());
        }
    };

    if diff_text.trim().is_empty() {
        send_message(client, &config.telegram_bot_token, chat_id,
            "Tidak ada diff untuk commit terakhir.").await?;
        return Ok(());
    }

    if diff_text.len() <= 3000 {
        let msg = format!("```diff\n{}\n```", diff_text);
        send_message(client, &config.telegram_bot_token, chat_id, &msg).await?;
    } else {
        // Send as file
        send_document(client, &config.telegram_bot_token, chat_id, "diff.patch", diff_text.as_bytes()).await?;
    }
    Ok(())
}

async fn send_document(
    client: &Client,
    token: &str,
    chat_id: i64,
    filename: &str,
    content: &[u8],
) -> anyhow::Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendDocument", token);
    let part = reqwest::multipart::Part::bytes(content.to_vec())
        .file_name(filename.to_string())
        .mime_str("text/plain")?;
    let form = reqwest::multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part("document", part);
    client.post(&url).multipart(form).send().await?;
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

    // Auto-create or reuse today's coding session
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
    // Check for existing session today for this telegram user + project
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
