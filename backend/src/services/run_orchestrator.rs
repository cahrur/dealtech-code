use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::config::Config;
use crate::domain::agent_run::{AgentRun, CreateRunRequest};
use crate::domain::policy::PolicyConfig;
use crate::services::{
    audit_service, git_service, openclaw_service, policy_engine::PolicyEngine,
    realtime_service, workspace_service,
};

pub async fn create_run(
    db: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
    req: CreateRunRequest,
    openclaw_agent_id: &str,
    telegram_chat_id: Option<i64>,
) -> anyhow::Result<AgentRun> {
    let run_id = Uuid::new_v4();
    let auto_mode = req.auto_mode.unwrap_or_else(|| "auto_trusted".to_string());
    let model = req.model.clone().unwrap_or_default();
    // Stable OpenClaw session per coding session keeps continuity and avoids
    // paying full cold-start context repeatedly on every run.
    let session_key = format!("project_{}:session_{}", project_id, session_id);
    let run = sqlx::query_as::<_, AgentRun>(
        "INSERT INTO agent_runs
         (id, session_id, project_id, user_id, prompt, status, auto_mode, openclaw_agent_id, openclaw_session_key, model, timeout_at, telegram_chat_id)
         VALUES ($1,$2,$3,$4,$5,'queued',$6,$7,$8,$9, NOW() + INTERVAL '60 minutes', $10) RETURNING *",
    )
    .bind(run_id).bind(session_id).bind(project_id).bind(user_id)
    .bind(&req.prompt).bind(&auto_mode).bind(openclaw_agent_id).bind(&session_key).bind(&model)
    .bind(telegram_chat_id)
    .fetch_one(db)
    .await?;
    Ok(run)
}

pub async fn execute_run(
    db: Arc<PgPool>,
    redis: ConnectionManager,
    config: Arc<Config>,
    run_id: Uuid,
    team_slug: String,
    project_slug: String,
    repo_url: String,
    policy_config: PolicyConfig,
) {
    let start_time = std::time::Instant::now();
    if let Err(e) = run_inner(db.clone(), redis.clone(), config.clone(), run_id, team_slug.clone(), project_slug.clone(), repo_url, policy_config).await {
        tracing::error!(run_id = %run_id, error = %e, "Agent run failed");
        // Improvement 5: Log run duration on failure
        tracing::info!(run_id = %run_id, duration_ms = start_time.elapsed().as_millis(), status = "failed", "Run finished");
        let error_msg = {
            let s = e.to_string();
            if s.trim().is_empty() { "Unknown error".to_string() } else { s }
        };
        let _ = sqlx::query(
            "UPDATE agent_runs SET status='failed_agent', finished_at=NOW(), error_message=$1 WHERE id=$2"
        )
        .bind(&error_msg)
        .bind(run_id)
        .execute(db.as_ref())
        .await;

        // Cleanup worktree on failure
        let run_info = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT worktree_path, branch_name FROM agent_runs WHERE id=$1"
        )
        .bind(run_id)
        .fetch_optional(db.as_ref())
        .await
        .ok()
        .flatten();
        if let Some((Some(wt_path), Some(_branch))) = run_info {
            let workspace_path = std::path::PathBuf::from(&config.workspaces_path)
                .join(&team_slug)
                .join(&project_slug);
            let _ = workspace_service::cleanup_worktree(
                &std::path::PathBuf::from(&wt_path),
                &workspace_path,
            ).await;
        }
        // Emit failure event so the Android app receives a terminal signal
        let session_id_opt = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT session_id FROM agent_runs WHERE id = $1"
        )
        .bind(run_id)
        .fetch_optional(db.as_ref())
        .await
        .ok()
        .flatten();
        if let Some(session_id) = session_id_opt {
            let mut r = redis;
            let _ = emit(
                db.as_ref(), &mut r, run_id, session_id, "agent_run.failed",
                serde_json::json!({"run_id": run_id, "data": {"reason": e.to_string()}}),
            ).await;
        }
    }
}

async fn run_inner(
    db: Arc<PgPool>,
    mut redis: ConnectionManager,
    config: Arc<Config>,
    run_id: Uuid,
    team_slug: String,
    project_slug: String,
    repo_url: String,
    policy_config: PolicyConfig,
) -> anyhow::Result<()> {
    let start_time = std::time::Instant::now();
    tracing::info!(run_id = %run_id, project = %project_slug, "run_inner started");

    // Set started_at immediately so we can confirm run_inner is being called
    let _ = sqlx::query("UPDATE agent_runs SET started_at=NOW() WHERE id=$1 AND started_at IS NULL")
        .bind(run_id).execute(db.as_ref()).await;

    let run = sqlx::query_as::<_, AgentRun>("SELECT * FROM agent_runs WHERE id = $1")
        .bind(run_id).fetch_one(db.as_ref()).await?;

    let policy = PolicyEngine::new(policy_config);
    let (session_id, project_id, user_id) = (run.session_id, run.project_id, run.user_id);

    // Improvement 2: Concurrency guard — only one active run per session
    let concurrent_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_runs WHERE session_id=$1 AND status IN ('queued','processing','running_agent') AND id != $2"
    )
    .bind(session_id)
    .bind(run_id)
    .fetch_one(db.as_ref())
    .await
    .unwrap_or(0);

    if concurrent_count > 0 {
        let reply = "Masih ada run yang sedang berjalan di sesi ini. Tunggu sebentar lalu coba lagi.";
        return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, reply, false, &config.telegram_bot_token).await;
    }

    // Persist user message immediately so reopening a session still shows it
    let _ = crate::services::session_service::add_message(
        db.as_ref(), session_id, "user", &run.prompt,
    ).await;

    set_status(&db, run_id, "preparing_workspace").await?;
    emit(&db, &mut redis, run_id, session_id, "agent_run.started",
        serde_json::json!({"run_id": run_id})).await?;

    // Notify Telegram user that agent is working
    if let Some(chat_id) = run.telegram_chat_id {
        notify_telegram(&config.telegram_bot_token, chat_id,
            "⏳ Agent sedang bekerja... Saya akan kabari kalau sudah selesai.").await;
        // Store active run_id in Redis so /cancel can find it
        let key = format!("tg:active_run:{}", chat_id);
        let _: std::result::Result<(), _> = redis::cmd("SETEX")
            .arg(&key)
            .arg(7500u64)
            .arg(run_id.to_string())
            .query_async(&mut redis)
            .await;
    }

    let route = openclaw_service::classify_prompt(&run.prompt);
    tracing::info!(run_id = %run_id, route = ?route, "Prompt classified for run");

    // Prepare workspace — graceful error: tell user instead of crashing
    if let Some(chat_id) = run.telegram_chat_id {
        notify_telegram(&config.telegram_bot_token, chat_id,
            "📂 Menyiapkan workspace...").await;
    }
    let github_token = config.github_token.as_deref();
    let workspace_path = match workspace_service::prepare_workspace(
        &config, &team_slug, &project_slug, &repo_url, github_token,
    ).await {
        Ok(p) => p,
        Err(e) => {
            let reply = format!(
                "Tidak bisa mengakses repository `{}`. Pastikan URL repo benar dan credentials sudah dikonfigurasi.\n\nDetail: {}",
                repo_url, e
            );
            return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, &reply, false, &config.telegram_bot_token).await;
        }
    };

    // Get or create persistent branch for this session (1 branch per session)
    // Check if user requested a new branch via /newsession
    let force_new_key = format!("tg:force_new_branch:{}:{}", user_id, run.project_id);
    let force_new: bool = redis::cmd("GETDEL")
        .arg(&force_new_key)
        .query_async(&mut redis)
        .await
        .unwrap_or(None::<String>)
        .is_some();

    let branch_name = match workspace_service::get_or_create_session_branch(
        db.as_ref(), &workspace_path, session_id, force_new,
    ).await {
        Ok(b) => b,
        Err(e) => {
            let reply = format!("Gagal menyiapkan branch untuk sesi ini. Detail: {}", e);
            return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, &reply, false, &config.telegram_bot_token).await;
        }
    };

    // Create worktree — graceful error
    let worktree = match workspace_service::create_worktree(
        &config, &team_slug, &project_slug, run_id, &branch_name,
    ).await {
        Ok(w) => w,
        Err(e) => {
            let reply = format!("Gagal membuat branch kerja `{}`. Detail: {}", branch_name, e);
            return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, &reply, false, &config.telegram_bot_token).await;
        }
    };

    sqlx::query("UPDATE agent_runs SET branch_name=$1, worktree_path=$2 WHERE id=$3")
        .bind(&branch_name).bind(worktree.to_str().unwrap_or("")).bind(run_id)
        .execute(db.as_ref()).await?;

    set_status(&db, run_id, "running_agent").await?;

    // Progress update: agent is now running
    if let Some(chat_id) = run.telegram_chat_id {
        notify_telegram(&config.telegram_bot_token, chat_id,
            "🤖 Agent sedang menganalisis dan menulis kode...").await;
    }

    // Build instructions: tell OpenClaw the worktree path so it can use its own tools
    let git_status = git_service::get_status(&worktree).await.unwrap_or_default();
    let worktree_str = worktree.to_string_lossy().to_string();
    let task_summary = sqlx::query_scalar::<_, Option<String>>(
        "SELECT task_summary FROM coding_sessions WHERE id = $1"
    )
    .bind(session_id)
    .fetch_optional(db.as_ref())
    .await
    .unwrap_or(None)
    .flatten();
    let instructions = openclaw_service::build_full_agent_instructions(
        &repo_url,
        &branch_name,
        &worktree_str,
        &git_status,
        task_summary.as_deref(),
    );

    let sanitized_prompt = openclaw_service::sanitize_user_prompt(&run.prompt);
    let prompt_len = sanitized_prompt.len();
    let trimmed_prompt = sanitized_prompt.trim().to_lowercase();
    let history_limit: i64 = if trimmed_prompt.contains("lanjut") || trimmed_prompt.contains("yang tadi") {
        4
    } else {
        8
    };

    // Fetch session history — keep it lean and let task summary carry older state.
    let history: Vec<(String, String)> = crate::services::session_service::recent_messages(db.as_ref(), session_id, history_limit)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|m| !(m.role == "user" && m.content == run.prompt))
        .map(|m| (m.role, m.content))
        .collect();

    // Single OpenClaw call — streaming, OpenClaw writes files directly via its own tools
    let input = openclaw_service::OpenClawRunInput {
        agent_id: run.openclaw_agent_id.clone(),
        session_key: run.openclaw_session_key.clone(),
        user_id: user_id.to_string(),
        instructions,
        prompt: sanitized_prompt,
        model: run.model.clone(),
        history,
    };

    let history_len = input.history.len();

    let agent_response = match tokio::time::timeout(
        tokio::time::Duration::from_secs(7200),
        openclaw_service::run_agent_full(&config, &input),
    ).await {
        Ok(Ok(r)) => {
            tracing::info!(reply_len = r.reply.len(), actions = r.actions.len(), input_tokens = r.usage.input_tokens, output_tokens = r.usage.output_tokens, reply_preview = %&r.reply[..r.reply.len().min(200)], "OpenClaw reply");
            r
        }
        Ok(Err(e)) => {
            tracing::error!("OpenClaw call failed: {:#}", e);
            let reply = "Agent tidak bisa diproses saat ini. Silakan coba lagi.".to_string();
            return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, &reply, true, &config.telegram_bot_token).await;
        }
        Err(_elapsed) => {
            tracing::error!(run_id = %run_id, "OpenClaw call timed out after 60 minutes");
            sqlx::query("UPDATE agent_runs SET error_message='Run timed out after 120 minutes' WHERE id=$1")
                .bind(run_id).execute(db.as_ref()).await?;
            let reply = "Run timed out after 120 minutes. Silakan coba lagi dengan prompt yang lebih sederhana.";
            return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, reply, true, &config.telegram_bot_token).await;
        }
    };
    let agent_reply = agent_response.reply.clone();

    set_status(&db, run_id, "collecting_diff").await?;
    let diff = git_service::get_diff(&worktree).await.unwrap_or_default();
    let changed = git_service::changed_files(&worktree).await.unwrap_or_default();

    if !changed.is_empty() {
        emit(&db, &mut redis, run_id, session_id, "file.changed", serde_json::json!({
            "run_id": run_id, "files": &changed,
            "diff_preview": &diff[..diff.len().min(500)],
        })).await?;
    }

    let mut commit_sha: Option<String> = None;
    let mut pushed_branch = false;
    let mut push_error: Option<String> = None;

    if policy.can_commit() && !changed.is_empty() {
        set_status(&db, run_id, "auto_commit").await?;
        let msg = format!("feat: AI agent run {}", &run_id.to_string()[..8]);
        match git_service::commit(&worktree, &msg).await {
            Ok(sha) => {
                commit_sha = Some(sha.clone());
                sqlx::query("UPDATE agent_runs SET commit_sha=$1 WHERE id=$2")
                    .bind(&sha).bind(run_id).execute(db.as_ref()).await?;
                emit(&db, &mut redis, run_id, session_id, "git.committed",
                    serde_json::json!({"run_id": run_id, "sha": sha})).await?;

                if policy.can_push_branch() {
                    set_status(&db, run_id, "auto_push_or_pr").await?;
                    // Progress update: pushing
                    if let Some(chat_id) = run.telegram_chat_id {
                        notify_telegram(&config.telegram_bot_token, chat_id,
                            "🔀 Commit dibuat, sedang push ke branch...").await;
                    }
                    match git_service::push_branch(&worktree, &branch_name, config.github_token.as_deref()).await {
                        Ok(()) => {
                            pushed_branch = true;
                            emit(&db, &mut redis, run_id, session_id, "pr.created", serde_json::json!({
                                "run_id": run_id, "branch": &branch_name, "files_changed": changed.len(),
                            })).await?;
                        }
                        Err(e) => { push_error = Some(e.to_string()); }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("git commit failed: {}", e);
                push_error = Some(format!("Commit gagal: {}", e));
            }
        }
    }

    // Build final reply: OpenClaw's reply augmented with git status
    let task_summary_text = openclaw_service::synthesize_task_summary(
        &run.prompt,
        &changed,
        commit_sha.as_deref(),
        &branch_name,
        pushed_branch,
        false,
        push_error.as_deref(),
    );
    let _ = crate::services::session_service::update_task_summary(db.as_ref(), session_id, &task_summary_text).await;

    let mut final_reply = agent_reply.clone();
    if final_reply.trim().is_empty() {
        final_reply = if changed.is_empty() {
            "Tidak ada perubahan file.".to_string()
        } else {
            format!("Selesai. {} file diubah.", changed.len())
        };
    }
    if pushed_branch {
        final_reply.push_str(&format!("\n\n✅ Push ke branch `{}` berhasil.", branch_name));

        // Auto-create PR if github_token available and repo_url is GitHub
        if let Some(token) = config.github_token.as_deref() {
            if !repo_url.is_empty() {
                let pr_title = format!("[AI] {}", run.prompt.chars().take(60).collect::<String>());
                // Build informative PR description
                let files_list = changed.iter()
                    .map(|f| format!("- `{}`", f))
                    .collect::<Vec<_>>()
                    .join("\n");
                let pr_body = format!(
                    "## 🤖 AI Agent Changes\n\n\
                    **Prompt:**\n> {}\n\n\
                    **Files changed ({}):**\n{}\n\n\
                    **Agent summary:**\n{}\n\n\
                    ---\n*Auto-generated by Dealtech Code Agent • Run `{}`*",
                    run.prompt,
                    changed.len(),
                    if files_list.is_empty() { "_(none)_".to_string() } else { files_list },
                    agent_reply,
                    &run_id.to_string()[..8],
                );
                match crate::infra::github::create_pr(
                    token, &repo_url, &branch_name, "main", &pr_title, &pr_body,
                ).await {
                    Ok(pr) => {
                        // Save pr_url and pr_number to DB
                        let _ = sqlx::query(
                            "UPDATE agent_runs SET pr_url=$1, pr_number=$2 WHERE id=$3"
                        )
                        .bind(&pr.html_url)
                        .bind(pr.number as i32)
                        .bind(run_id)
                        .execute(db.as_ref())
                        .await;
                        final_reply.push_str(&format!("\n🔗 PR dibuat: [#{} {}]({})", pr.number, pr.title, pr.html_url));
                    }
                    Err(e) => {
                        tracing::warn!("Auto PR failed (non-fatal): {}", e);
                    }
                }
            }
        }
    } else if let Some(ref err) = push_error {
        final_reply.push_str(&format!("\n\n⚠️ {}", err));
    }

    let _ = crate::services::session_service::add_message(
        db.as_ref(), session_id, "assistant", &final_reply,
    ).await;

    // Notify Telegram user that run is complete
    if let Some(chat_id) = run.telegram_chat_id {
        // Clear active run key
        let key = format!("tg:active_run:{}", chat_id);
        let _: std::result::Result<(), _> = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut redis)
            .await;
        let tg_msg = format!("{}", final_reply);
        notify_telegram(&config.telegram_bot_token, chat_id, &tg_msg).await;
    }

    // Cost tracking: use real token usage from OpenClaw response
    let tokens_input = agent_response.usage.input_tokens;
    let tokens_output = agent_response.usage.output_tokens;
    // Fallback to char estimate if OpenClaw didn't return usage
    let (tokens_input, tokens_output) = if tokens_input == 0 && tokens_output == 0 {
        tracing::warn!("No token usage from OpenClaw, falling back to char estimate");
        ((run.prompt.len() as i32) / 4, (agent_reply.len() as i32) / 4)
    } else {
        (tokens_input, tokens_output)
    };
    let cost_usd = agent_response.usage.cost_usd(&run.model);

    // Diff stat: get git diff --stat for completed run
    let diff_stat_output = if commit_sha.is_some() {
        let stat_out = tokio::process::Command::new("git")
            .args(["-C", worktree.to_str().unwrap_or(""), "diff", "HEAD~1", "HEAD", "--stat"])
            .output()
            .await
            .ok();
        stat_out.map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .filter(|s| !s.trim().is_empty())
    } else {
        None
    };

    set_status(&db, run_id, "completed").await?;
    sqlx::query(
        "UPDATE agent_runs SET finished_at=NOW(), tokens_input=$1, tokens_output=$2, cost_usd=$3, diff_stat=$4 WHERE id=$5"
    )
    .bind(tokens_input)
    .bind(tokens_output)
    .bind(cost_usd)
    .bind(&diff_stat_output)
    .bind(run_id)
    .execute(db.as_ref())
    .await?;

    emit(&db, &mut redis, run_id, session_id, "agent_run.completed",
        serde_json::json!({"run_id": run_id, "files_changed": changed.len()})).await?;

    // Publish to Redis channel for Telegram notification
    if let Some(ref diff_stat_str) = diff_stat_output {
        let tg_chat_id = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT telegram_chat_id FROM agent_runs WHERE id = $1"
        ).bind(run_id).fetch_one(db.as_ref()).await.unwrap_or(None);
        if let Some(chat_id) = tg_chat_id {
            let notify_payload = serde_json::json!({
                "run_id": run_id.to_string(),
                "chat_id": chat_id,
                "diff_stat": diff_stat_str,
            });
            let _: std::result::Result<(), _> = redis::cmd("PUBLISH")
                .arg("agent_run:completed")
                .arg(notify_payload.to_string())
                .query_async(&mut redis)
                .await;
        }
    }

    audit_service::log(db.as_ref(), Some(user_id), Some(project_id), Some(run_id),
        "agent_run.completed",
        serde_json::json!({"branch": branch_name, "files": changed})).await?;

    let _ = crate::services::usage_service::record(
        db.as_ref(), Some(user_id), Some(run_id), &run.model, tokens_input as i64, tokens_output as i64,
    ).await;

    // Cleanup worktree after run completes (branch is kept for session reuse)
    let _ = workspace_service::cleanup_worktree(&worktree, &workspace_path).await;

    // Improvement 5: Log run duration on success
    tracing::info!(
        run_id = %run_id,
        session_id = %session_id,
        route = ?route,
        history_len,
        prompt_len,
        changed_files = changed.len(),
        pushed_branch,
        input_tokens = tokens_input,
        output_tokens = tokens_output,
        cost_usd,
        duration_ms = start_time.elapsed().as_millis(),
        status = "completed",
        "Run finished"
    );

    Ok(())
}

/// Complete a run with a user-facing reply, skipping all git operations.
/// Used for workspace errors and OpenClaw call failures.
async fn finish_with_reply(
    db: &PgPool,
    redis: &mut ConnectionManager,
    run_id: Uuid,
    session_id: Uuid,
    reply: &str,
    is_failure: bool,
    bot_token: &str,
) -> anyhow::Result<()> {
    let _ = crate::services::session_service::add_message(db, session_id, "assistant", reply).await;
    let _ = crate::services::session_service::update_task_summary(db, session_id, reply).await;
    let status = if is_failure { "failed_agent" } else { "completed" };
    if is_failure {
        let error_msg = if reply.trim().is_empty() { "Unknown error" } else { reply };
        sqlx::query("UPDATE agent_runs SET status=$1, finished_at=NOW(), error_message=$2 WHERE id=$3")
            .bind(status).bind(error_msg).bind(run_id).execute(db).await?;
    } else {
        sqlx::query("UPDATE agent_runs SET status=$1, finished_at=NOW() WHERE id=$2")
            .bind(status).bind(run_id).execute(db).await?;
    }
    let event = if is_failure { "agent_run.failed" } else { "agent_run.completed" };
    emit(db, redis, run_id, session_id, event,
        serde_json::json!({"run_id": run_id, "files_changed": 0})).await?;

    // Notify Telegram if this run came from a Telegram message
    // Notify Telegram if this run came from a Telegram message — clear active run key
    let chat_id: Option<i64> = sqlx::query_scalar(
        "SELECT telegram_chat_id FROM agent_runs WHERE id = $1"
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None)
    .flatten();

    if let Some(cid) = chat_id {
        let key = format!("tg:active_run:{}", cid);
        let _: std::result::Result<(), _> = redis::cmd("DEL")
            .arg(&key)
            .query_async(redis)
            .await;
        // Send the reply/error text to Telegram so user isn't left hanging
        notify_telegram(bot_token, cid, reply).await;
    }

    Ok(())
}

async fn set_status(db: &PgPool, run_id: Uuid, status: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE agent_runs SET status=$1,
         started_at=CASE WHEN started_at IS NULL THEN NOW() ELSE started_at END
         WHERE id=$2",
    )
    .bind(status).bind(run_id).execute(db).await?;
    Ok(())
}

async fn emit(
    db: &PgPool,
    redis: &mut ConnectionManager,
    run_id: Uuid,
    session_id: Uuid,
    event_type: &str,
    mut payload: serde_json::Value,
) -> anyhow::Result<()> {
    let seq = realtime_service::save_event(db, run_id, session_id, event_type, payload.clone()).await?;
    payload["type"] = serde_json::json!(event_type);
    payload["seq"] = serde_json::json!(seq);
    payload["session_id"] = serde_json::json!(session_id);
    realtime_service::publish_event(redis, session_id, &payload).await?;
    Ok(())
}

/// Send a Telegram message directly via Bot API (fire-and-forget).
pub async fn notify_telegram(bot_token: &str, chat_id: i64, text: &str) {
    static TG_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = TG_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    });
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);

    // Convert CommonMark **bold** → *bold* for Telegram legacy Markdown
    // Also strip heading markers (# ## ###) that Telegram doesn't support
    let converted = convert_markdown_for_telegram(text);

    let mut remaining: &str = &converted;
    let mut first = true;
    loop {
        if remaining.is_empty() { break; }
        if !first {
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        }
        first = false;
        let end = remaining.len().min(4000);
        let split_at = if end < remaining.len() {
            remaining[..end].rfind('\n').unwrap_or(end)
        } else {
            end
        };
        let chunk = &remaining[..split_at];
        // Try with Markdown first, fall back to plain text if Telegram rejects
        let body_md = serde_json::json!({ "chat_id": chat_id, "text": chunk, "parse_mode": "Markdown" });
        let resp = client.post(&url).json(&body_md).send().await;
        let ok = resp.map(|r| r.status().is_success()).unwrap_or(false);
        if !ok {
            // Fallback: send as plain text (no parse_mode)
            let body_plain = serde_json::json!({ "chat_id": chat_id, "text": chunk });
            let _ = client.post(&url).json(&body_plain).send().await;
        }
        remaining = remaining[split_at..].trim_start();
    }
}

/// Convert CommonMark/GitHub Markdown to Telegram legacy Markdown.
/// Telegram legacy Markdown: *bold*, _italic_, `code`, ```pre```
/// Does NOT support: **bold**, __italic__, # headings, ~~strikethrough~~
fn convert_markdown_for_telegram(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        // Strip heading markers
        let line = if line.starts_with("### ") { &line[4..] }
            else if line.starts_with("## ") { &line[3..] }
            else if line.starts_with("# ") { &line[2..] }
            else { line };
        // Convert **bold** → *bold* (simple replace, safe for UTF-8/emoji)
        let line = line.replace("**", "*");
        // Convert __italic__ → _italic_
        let line = line.replace("__", "_");
        // Strip ~~strikethrough~~
        let line = line.replace("~~", "");
        result.push_str(&line);
        result.push('\n');
    }
    if result.ends_with('\n') { result.pop(); }
    result
}
