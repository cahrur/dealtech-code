use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::mpsc;
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
    let route_probe_input = openclaw_service::OpenClawRunInput {
        agent_id: run.openclaw_agent_id.clone(),
        session_key: run.openclaw_session_key.clone(),
        user_id: user_id.to_string(),
        instructions: String::new(),
        prompt: run.prompt.clone(),
        model: run.model.clone(),
    };
    let route = openclaw_service::route_prompt(&config, &route_probe_input)
        .await
        .unwrap_or_else(|_| openclaw_service::fallback_route_prompt(&run.prompt));

    set_status(&db, run_id, "preparing_workspace").await?;
    emit(&db, &mut redis, run_id, session_id, "agent_run.started",
        serde_json::json!({"run_id": run_id})).await?;

    // Persist the user prompt immediately so reopening a session still shows it.
    let _ = crate::services::session_service::add_message(
        db.as_ref(), session_id, "user", &run.prompt
    ).await;

    if route.intent == "smalltalk" {
        set_status(&db, run_id, "running_agent").await?;
        let reply = route
            .reply
            .map(|r| openclaw_service::sanitize_user_facing_response(&r))
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| openclaw_service::fallback_smalltalk_response(&run.prompt));

        let _ = crate::services::session_service::add_message(
            db.as_ref(), session_id, "assistant", &reply
        ).await;
        set_status(&db, run_id, "completed").await?;
        sqlx::query("UPDATE agent_runs SET finished_at=NOW() WHERE id=$1")
            .bind(run_id).execute(db.as_ref()).await?;
        emit(&db, &mut redis, run_id, session_id, "agent_run.completed",
            serde_json::json!({"run_id": run_id, "files_changed": 0})).await?;
        return Ok(());
    }

    let _workspace = workspace_service::prepare_workspace(
        &config, &team_slug, &project_slug, &repo_url,
    ).await?;

    let now = time::OffsetDateTime::now_utc();
    let branch_name = format!("ai/{}{:02}{:02}-{}", now.year(), now.month() as u8, now.day(), &run_id.to_string()[..8]);
    let mut summary_branch_name = branch_name.clone();

    let worktree = workspace_service::create_worktree(
        &config, &team_slug, &project_slug, run_id, &branch_name,
    ).await?;

    sqlx::query("UPDATE agent_runs SET branch_name=$1, worktree_path=$2 WHERE id=$3")
        .bind(&branch_name).bind(worktree.to_str().unwrap()).bind(run_id)
        .execute(db.as_ref()).await?;

    set_status(&db, run_id, "running_agent").await?;

    let planner_input = openclaw_service::OpenClawRunInput {
        agent_id: run.openclaw_agent_id.clone(),
        session_key: format!("{}:planner", run.openclaw_session_key),
        user_id: user_id.to_string(),
        instructions: openclaw_service::build_agent_instructions(&project_slug, &project_slug, &branch_name),
        prompt: run.prompt.clone(),
        model: run.model.clone(),
    };
    let planned_actions = if let Some(local_plan) =
        openclaw_service::fallback_plan_file_actions(&run.prompt)
    {
        Some(local_plan)
    } else {
        openclaw_service::plan_file_actions(&config, &planner_input)
            .await
            .ok()
            .filter(|plan| !plan.actions.is_empty())
    };

    if let Some(plan) = planned_actions {
        let mut applied_actions = Vec::new();
        for action in &plan.actions {
            if action.action_type == "write_file" {
                let _ = file_action_service::write_file(&worktree, &action.path, &action.content).await?;
                applied_actions.push(openclaw_service::AppliedFileAction {
                    action_type: action.action_type.clone(),
                    path: action.path.clone(),
                    content: action.content.clone(),
                });
            }
        }

        set_status(&db, run_id, "collecting_diff").await?;
        let diff = git_service::get_diff(&worktree).await.unwrap_or_default();
        let changed = git_service::changed_files(&worktree).await.unwrap_or_default();
        let mut commit_sha: Option<String> = None;
        let mut pushed_branch = false;
        let mut push_error: Option<String> = None;

        emit(&db, &mut redis, run_id, session_id, "file.changed", serde_json::json!({
            "run_id": run_id, "files": &changed,
            "diff_preview": &diff[..diff.len().min(500)],
        })).await?;

        if policy.can_commit() && !changed.is_empty() {
            set_status(&db, run_id, "auto_commit").await?;
            let msg = plan.commit_message
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| format!("feat: AI agent run {}", &run_id.to_string()[..8]));
            let sha = git_service::commit(&worktree, &msg).await?;
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
                    Err(err) => {
                        push_error = Some(err.to_string());
                    }
                }
            }
        }

        let reply = openclaw_service::synthesize_task_summary_with_plan(
            &run.prompt,
            &changed,
            &applied_actions,
            commit_sha.as_deref(),
            &branch_name,
            pushed_branch,
            false,
            push_error.as_deref(),
        );
        let _ = crate::services::session_service::add_message(
            db.as_ref(), session_id, "assistant", &reply
        ).await;

        set_status(&db, run_id, "completed").await?;
        sqlx::query("UPDATE agent_runs SET finished_at=NOW() WHERE id=$1")
            .bind(run_id).execute(db.as_ref()).await?;
        emit(&db, &mut redis, run_id, session_id, "agent_run.completed",
            serde_json::json!({"run_id": run_id, "files_changed": changed.len()})).await?;
        return Ok(());
    }

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
    let mut final_done_text = String::new();

    while let Some(ev) = rx.recv().await {
        tracing::info!(event_type = %ev.event_type, "OpenClaw event");

        // Map OpenClaw event types to our own
        let mapped_type = match ev.event_type.as_str() {
            "response.output_text.delta" | "content_block_delta" => "assistant.delta",
            other => other,
        };

        // Collect assistant delta — try multiple field paths
        if ev.event_type == "response.output_text.done" {
            if let Some(t) = ev.payload.get("text").and_then(|v| v.as_str()) {
                final_done_text = t.to_string();
            }
        }

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

    let preferred = if !final_done_text.trim().is_empty() { final_done_text } else { assistant_response };
    let mut assistant_response = crate::services::openclaw_service::sanitize_user_facing_response(&preferred);
    if assistant_response.trim().is_empty() || crate::services::openclaw_service::is_response_suspicious(&preferred) {
        let rewrite_input = openclaw_service::OpenClawRunInput {
            agent_id: run.openclaw_agent_id.clone(),
            session_key: format!("{}:rewrite", run.openclaw_session_key),
            user_id: user_id.to_string(),
            instructions: openclaw_service::build_agent_instructions(&project_slug, &project_slug, &branch_name),
            prompt: format!(
                "<user_prompt>{}</user_prompt>\n<draft_answer>{}</draft_answer>\nRewrite into concise user-facing answer only.",
                run.prompt, preferred
            ),
            model: run.model.clone(),
        };
        if let Ok(rewritten) = openclaw_service::run_nonstream(&config, &rewrite_input).await {
            assistant_response = crate::services::openclaw_service::sanitize_user_facing_response(&rewritten);
        }
    }
    let mut stream_failed = false;
    let mut stream_error_message: Option<String> = None;
    if let Ok(err_msg) = err_rx.try_recv() {
        tracing::warn!("OpenClaw stream failed: {}", err_msg);
        stream_failed = true;
        stream_error_message = Some(err_msg);
    }

    set_status(&db, run_id, "collecting_diff").await?;
    let diff = git_service::get_diff(&worktree).await.unwrap_or_default();
    let changed = git_service::changed_files(&worktree).await.unwrap_or_default();
    let mut commit_sha: Option<String> = None;
    let mut pushed_branch = false;
    let mut push_error: Option<String> = None;

    emit(&db, &mut redis, run_id, session_id, "file.changed", serde_json::json!({
        "run_id": run_id, "files": &changed,
        "diff_preview": &diff[..diff.len().min(500)],
    })).await?;

    if policy.can_commit() && !changed.is_empty() {
        set_status(&db, run_id, "auto_commit").await?;
        let msg = format!("feat: AI agent run {}", &run_id.to_string()[..8]);
        let sha = git_service::commit(&worktree, &msg).await?;
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
                Err(err) => {
                    push_error = Some(err.to_string());
                }
            }
        }
    }

    if !pushed_branch && changed.is_empty() && route.intent == "retry_push" {
        if let Some(previous_run) = sqlx::query_as::<_, AgentRun>(
            "SELECT * FROM agent_runs
             WHERE session_id = $1
               AND id <> $2
               AND commit_sha IS NOT NULL
               AND branch_name IS NOT NULL
               AND worktree_path IS NOT NULL
             ORDER BY created_at DESC
             LIMIT 1"
        )
        .bind(session_id)
        .bind(run_id)
        .fetch_optional(db.as_ref())
        .await? {
            if let (Some(prev_branch), Some(prev_worktree), Some(prev_sha)) = (
                previous_run.branch_name.clone(),
                previous_run.worktree_path.clone(),
                previous_run.commit_sha.clone(),
            ) {
                set_status(&db, run_id, "auto_push_or_pr").await?;
                match git_service::push_branch(&std::path::PathBuf::from(prev_worktree), &prev_branch).await {
                    Ok(()) => {
                        pushed_branch = true;
                        summary_branch_name = prev_branch.clone();
                        commit_sha = Some(prev_sha);
                        emit(&db, &mut redis, run_id, session_id, "pr.created", serde_json::json!({
                            "run_id": run_id, "branch": prev_branch, "files_changed": 0,
                        })).await?;
                    }
                    Err(err) => {
                        summary_branch_name = prev_branch;
                        commit_sha = Some(prev_sha);
                        push_error = Some(err.to_string());
                    }
                }
            }
        }
    }

    let synthesized_response = openclaw_service::synthesize_task_summary(
        &run.prompt,
        &changed,
        commit_sha.as_deref(),
        &summary_branch_name,
        pushed_branch,
        stream_failed,
        push_error.as_deref(),
    );

    if changed.is_empty()
        && !stream_failed
        && !openclaw_service::is_write_request(&run.prompt)
        && !openclaw_service::is_push_request(&run.prompt)
        && !assistant_response.trim().is_empty()
    {
        assistant_response = assistant_response.trim().to_string();
    } else {
        assistant_response = synthesized_response;
    }

    if !assistant_response.trim().is_empty() {
        let _ = crate::services::session_service::add_message(
            db.as_ref(), session_id, "assistant", &assistant_response
        ).await;
    }

    if stream_failed && changed.is_empty() {
        set_status(&db, run_id, "failed_agent").await?;
        let reason = stream_error_message
            .unwrap_or_else(|| "Agent stream failed before any file change".to_string());
        sqlx::query("UPDATE agent_runs SET error_message=$1, finished_at=NOW() WHERE id=$2")
            .bind(&reason)
            .bind(run_id)
            .execute(db.as_ref())
            .await?;
        emit(&db, &mut redis, run_id, session_id, "agent_run.failed",
            serde_json::json!({"run_id": run_id, "data": {"reason": reason}})).await?;
        return Ok(());
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
