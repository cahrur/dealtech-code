use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::mpsc;
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
) -> anyhow::Result<AgentRun> {
    let run_id = Uuid::new_v4();
    let auto_mode = req.auto_mode.unwrap_or_else(|| "auto_trusted".to_string());
    let model = req.model.clone().unwrap_or_else(|| "claude-sonnet-4-6".to_string());
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

    set_status(&db, run_id, "preparing_workspace").await?;
    emit(&db, &mut redis, run_id, session_id, "agent_run.started",
        serde_json::json!({"run_id": run_id})).await?;

    let _workspace = workspace_service::prepare_workspace(
        &config, &team_slug, &project_slug, &repo_url,
    ).await?;

    let now = time::OffsetDateTime::now_utc();
    let branch_name = format!("ai/{}{:02}{:02}-{}", now.year(), now.month() as u8, now.day(), &run_id.to_string()[..8]);

    let worktree = workspace_service::create_worktree(
        &config, &team_slug, &project_slug, run_id, &branch_name,
    ).await?;

    sqlx::query("UPDATE agent_runs SET branch_name=$1, worktree_path=$2 WHERE id=$3")
        .bind(&branch_name).bind(worktree.to_str().unwrap()).bind(run_id)
        .execute(db.as_ref()).await?;

    // Save user prompt to messages before running
    let _ = crate::services::session_service::add_message(
        db.as_ref(), session_id, "user", &run.prompt
    ).await;

    set_status(&db, run_id, "running_agent").await?;

    let instructions = openclaw_service::build_agent_instructions(&project_slug, &project_slug, &branch_name);
    let (tx, mut rx) = mpsc::channel::<openclaw_service::OpenClawEvent>(100);
    let input = openclaw_service::OpenClawRunInput {
        agent_id: run.openclaw_agent_id.clone(),
        session_key: run.openclaw_session_key.clone(),
        user_id: user_id.to_string(),
        instructions,
        prompt: run.prompt.clone(),
        model: run.model.clone(),
    };

    let (err_tx, mut err_rx) = tokio::sync::oneshot::channel::<String>();
    let cfg = config.clone();
    tokio::spawn(async move {
        if let Err(e) = openclaw_service::run_stream(&cfg, input, tx).await {
            tracing::error!("OpenClaw stream error: {}", e);
            let _ = err_tx.send(e.to_string());
        }
    });

    let mut assistant_response = String::new();

    while let Some(ev) = rx.recv().await {
        tracing::info!(event_type = %ev.event_type, "OpenClaw event");

        // Map OpenClaw event types to our own
        let mapped_type = match ev.event_type.as_str() {
            "response.output_text.delta" | "content_block_delta" => "assistant.delta",
            other => other,
        };

        // Collect assistant delta — try multiple field paths
        if mapped_type == "assistant.delta" {
            let delta = ev.payload.get("delta").and_then(|d| d.as_str())
                .or_else(|| ev.payload.get("text").and_then(|d| d.as_str()))
                .or_else(|| ev.payload.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()))
                .unwrap_or("");
            if !delta.is_empty() {
                assistant_response.push_str(delta);
            }
        }

        emit(&db, &mut redis, run_id, session_id, mapped_type,
            serde_json::json!({"run_id": run_id, "session_id": session_id, "data": ev.payload})).await?;
    }

    // Save assistant response to messages (sanitized to prevent internal prompt leakage)
    let assistant_response = crate::services::openclaw_service::sanitize_user_facing_response(&assistant_response);
    if !assistant_response.is_empty() {
        let _ = crate::services::session_service::add_message(
            db.as_ref(), session_id, "assistant", &assistant_response
        ).await;
    }

    // If OpenClaw errored, mark run as failed instead of completed
    if let Ok(err_msg) = err_rx.try_recv() {
        tracing::warn!("OpenClaw stream failed, trying non-stream fallback: {}", err_msg);
        let fallback_input = openclaw_service::OpenClawRunInput {
            agent_id: run.openclaw_agent_id.clone(),
            session_key: run.openclaw_session_key.clone(),
            user_id: user_id.to_string(),
            instructions: openclaw_service::build_agent_instructions(&project_slug, &project_slug, &branch_name),
            prompt: run.prompt.clone(),
            model: run.model.clone(),
        };

        match openclaw_service::run_nonstream(&config, &fallback_input).await {
            Ok(text) => {
                let safe = openclaw_service::sanitize_user_facing_response(&text);
                if !safe.is_empty() {
                    let _ = crate::services::session_service::add_message(
                        db.as_ref(), session_id, "assistant", &safe
                    ).await;
                }
                emit(&db, &mut redis, run_id, session_id, "agent_run.fallback_nonstream",
                    serde_json::json!({"run_id": run_id})).await?;
            }
            Err(fallback_err) => {
                set_status(&db, run_id, "failed_agent").await?;
                let final_err = format!("{} | fallback failed: {}", err_msg, fallback_err);
                sqlx::query("UPDATE agent_runs SET error_message=$1 WHERE id=$2")
                    .bind(&final_err).bind(run_id).execute(db.as_ref()).await?;
                emit(&db, &mut redis, run_id, session_id, "agent_run.failed",
                    serde_json::json!({"run_id": run_id, "data": {"reason": final_err}})).await?;
                return Ok(());
            }
        }
    }

    set_status(&db, run_id, "collecting_diff").await?;
    let diff = git_service::get_diff(&worktree).await.unwrap_or_default();
    let changed = git_service::changed_files(&worktree).await.unwrap_or_default();

    emit(&db, &mut redis, run_id, session_id, "file.changed", serde_json::json!({
        "run_id": run_id, "files": changed,
        "diff_preview": &diff[..diff.len().min(500)],
    })).await?;

    if policy.can_commit() && !changed.is_empty() {
        set_status(&db, run_id, "auto_commit").await?;
        let msg = format!("feat: AI agent run {}", &run_id.to_string()[..8]);
        let sha = git_service::commit(&worktree, &msg).await?;
        sqlx::query("UPDATE agent_runs SET commit_sha=$1 WHERE id=$2")
            .bind(&sha).bind(run_id).execute(db.as_ref()).await?;
        emit(&db, &mut redis, run_id, session_id, "git.committed",
            serde_json::json!({"run_id": run_id, "sha": sha})).await?;

        if policy.can_push_branch() {
            set_status(&db, run_id, "auto_push_or_pr").await?;
            git_service::push_branch(&worktree, &branch_name).await?;
            emit(&db, &mut redis, run_id, session_id, "pr.created", serde_json::json!({
                "run_id": run_id, "branch": branch_name, "files_changed": changed.len(),
            })).await?;
        }
    }

    set_status(&db, run_id, "completed").await?;
    sqlx::query("UPDATE agent_runs SET finished_at=NOW() WHERE id=$1")
        .bind(run_id).execute(db.as_ref()).await?;
    emit(&db, &mut redis, run_id, session_id, "agent_run.completed",
        serde_json::json!({"run_id": run_id, "files_changed": changed.len()})).await?;

    audit_service::log(db.as_ref(), Some(user_id), Some(project_id), Some(run_id),
        "agent_run.completed",
        serde_json::json!({"branch": branch_name, "files": changed})).await?;

    // Record usage — token counts updated when OpenClaw returns usage in SSE
    let _ = crate::services::usage_service::record(
        db.as_ref(),
        Some(user_id), // user_id == api_key_id in this system
        Some(run_id),
        &run.model,
        0,
        0,
    ).await;

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
