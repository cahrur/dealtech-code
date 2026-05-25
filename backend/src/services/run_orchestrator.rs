use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::config::Config;
use crate::domain::agent_run::{AgentRun, CreateRunRequest};
use crate::domain::policy::PolicyConfig;
use crate::services::{
    audit_service, file_action_service, git_service, openclaw_service, policy_engine::PolicyEngine,
    realtime_service, workspace_service,
};

pub async fn create_run(
    db: &PgPool,
    session_id: Uuid,
    project_id: Uuid,
    user_id: Uuid,
    req: CreateRunRequest,
    openclaw_agent_id: &str,
) -> anyhow::Result<AgentRun> {
    let run_id = Uuid::new_v4();
    let auto_mode = req.auto_mode.unwrap_or_else(|| "auto_trusted".to_string());
    let model = req.model.clone().unwrap_or_default();
    let session_key = format!("project_{}:session_{}", project_id, session_id);
    let run = sqlx::query_as::<_, AgentRun>(
        "INSERT INTO agent_runs
         (id, session_id, project_id, user_id, prompt, status, auto_mode, openclaw_agent_id, openclaw_session_key, model)
         VALUES ($1,$2,$3,$4,$5,'queued',$6,$7,$8,$9) RETURNING *",
    )
    .bind(run_id).bind(session_id).bind(project_id).bind(user_id)
    .bind(&req.prompt).bind(&auto_mode).bind(openclaw_agent_id).bind(&session_key).bind(&model)
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
    if let Err(e) = run_inner(db.clone(), redis.clone(), config, run_id, team_slug, project_slug, repo_url, policy_config).await {
        tracing::error!(run_id = %run_id, error = %e, "Agent run failed");
        let _ = sqlx::query(
            "UPDATE agent_runs SET status='failed_agent', finished_at=NOW(), error_message=$1 WHERE id=$2"
        )
        .bind(e.to_string())
        .bind(run_id)
        .execute(db.as_ref())
        .await;
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
    let run = sqlx::query_as::<_, AgentRun>("SELECT * FROM agent_runs WHERE id = $1")
        .bind(run_id).fetch_one(db.as_ref()).await?;

    let policy = PolicyEngine::new(policy_config);
    let (session_id, project_id, user_id) = (run.session_id, run.project_id, run.user_id);

    // Persist user message immediately so reopening a session still shows it
    let _ = crate::services::session_service::add_message(
        db.as_ref(), session_id, "user", &run.prompt,
    ).await;

    set_status(&db, run_id, "preparing_workspace").await?;
    emit(&db, &mut redis, run_id, session_id, "agent_run.started",
        serde_json::json!({"run_id": run_id})).await?;

    // Prepare workspace — graceful error: tell user instead of crashing
    if let Err(e) = workspace_service::prepare_workspace(
        &config, &team_slug, &project_slug, &repo_url,
    ).await {
        let reply = format!(
            "Tidak bisa mengakses repository `{}`. Pastikan URL repo benar dan credentials sudah dikonfigurasi.\n\nDetail: {}",
            repo_url, e
        );
        return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, &reply, false).await;
    }

    let now = time::OffsetDateTime::now_utc();
    let branch_name = format!(
        "ai/{}{:02}{:02}-{}",
        now.year(), now.month() as u8, now.day(),
        &run_id.to_string()[..8]
    );

    // Create worktree — graceful error
    let worktree = match workspace_service::create_worktree(
        &config, &team_slug, &project_slug, run_id, &branch_name,
    ).await {
        Ok(w) => w,
        Err(e) => {
            let reply = format!("Gagal membuat branch kerja `{}`. Detail: {}", branch_name, e);
            return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, &reply, false).await;
        }
    };

    sqlx::query("UPDATE agent_runs SET branch_name=$1, worktree_path=$2 WHERE id=$3")
        .bind(&branch_name).bind(worktree.to_str().unwrap_or("")).bind(run_id)
        .execute(db.as_ref()).await?;

    set_status(&db, run_id, "running_agent").await?;

    // Build workspace context so OpenClaw knows what files exist and git state
    let file_list = openclaw_service::build_worktree_context(&worktree).await;
    let git_status = git_service::get_status(&worktree).await.unwrap_or_default();
    let instructions = openclaw_service::build_full_agent_instructions(
        &repo_url, &branch_name, &file_list, &git_status,
    );

    // Single OpenClaw call with full context + same session key (preserves chat history)
    let input = openclaw_service::OpenClawRunInput {
        agent_id: run.openclaw_agent_id.clone(),
        session_key: run.openclaw_session_key.clone(),
        user_id: user_id.to_string(),
        instructions,
        prompt: run.prompt.clone(),
        model: run.model.clone(),
    };

    let agent_response = match openclaw_service::run_agent_full(&config, &input).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("OpenClaw call failed: {:#}", e);
            let reply = "Agent tidak bisa diproses saat ini. Silakan coba lagi.".to_string();
            return finish_with_reply(db.as_ref(), &mut redis, run_id, session_id, &reply, true).await;
        }
    };

    // Execute file actions returned by OpenClaw
    for action in &agent_response.actions {
        if action.action_type == "write_file" {
            match file_action_service::write_file(&worktree, &action.path, &action.content).await {
                Ok(_) => tracing::info!(path = %action.path, "Wrote file"),
                Err(e) => tracing::warn!(path = %action.path, error = %e, "Failed to write file"),
            }
        }
    }

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
        let msg = agent_response.commit_message.clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| format!("feat: AI agent run {}", &run_id.to_string()[..8]));
        match git_service::commit(&worktree, &msg).await {
            Ok(sha) => {
                commit_sha = Some(sha.clone());
                sqlx::query("UPDATE agent_runs SET commit_sha=$1 WHERE id=$2")
                    .bind(&sha).bind(run_id).execute(db.as_ref()).await?;
                emit(&db, &mut redis, run_id, session_id, "git.committed",
                    serde_json::json!({"run_id": run_id, "sha": sha})).await?;

                if policy.can_push_branch() {
                    set_status(&db, run_id, "auto_push_or_pr").await?;
                    match git_service::push_branch(&worktree, &branch_name).await {
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
    let mut final_reply = agent_response.reply.clone();
    if final_reply.trim().is_empty() {
        final_reply = if changed.is_empty() {
            "Tidak ada perubahan file.".to_string()
        } else {
            format!("Selesai. {} file diubah.", changed.len())
        };
    }
    if pushed_branch {
        final_reply.push_str(&format!("\n\n✅ Push ke branch `{}` berhasil.", branch_name));
    } else if let Some(ref err) = push_error {
        final_reply.push_str(&format!("\n\n⚠️ {}", err));
    }

    let _ = crate::services::session_service::add_message(
        db.as_ref(), session_id, "assistant", &final_reply,
    ).await;

    set_status(&db, run_id, "completed").await?;
    sqlx::query("UPDATE agent_runs SET finished_at=NOW() WHERE id=$1")
        .bind(run_id).execute(db.as_ref()).await?;
    emit(&db, &mut redis, run_id, session_id, "agent_run.completed",
        serde_json::json!({"run_id": run_id, "files_changed": changed.len()})).await?;

    audit_service::log(db.as_ref(), Some(user_id), Some(project_id), Some(run_id),
        "agent_run.completed",
        serde_json::json!({"branch": branch_name, "files": changed})).await?;

    let _ = crate::services::usage_service::record(
        db.as_ref(), Some(user_id), Some(run_id), &run.model, 0, 0,
    ).await;

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
) -> anyhow::Result<()> {
    let _ = crate::services::session_service::add_message(db, session_id, "assistant", reply).await;
    let status = if is_failure { "failed_agent" } else { "completed" };
    set_status(db, run_id, status).await?;
    sqlx::query("UPDATE agent_runs SET finished_at=NOW() WHERE id=$1")
        .bind(run_id).execute(db).await?;
    let event = if is_failure { "agent_run.failed" } else { "agent_run.completed" };
    emit(db, redis, run_id, session_id, event,
        serde_json::json!({"run_id": run_id, "files_changed": 0})).await?;
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
