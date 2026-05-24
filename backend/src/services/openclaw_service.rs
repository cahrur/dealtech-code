use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::error::{AppError, Result};

#[derive(Debug, Clone)]
pub struct OpenClawRunInput {
    pub agent_id: String,
    pub session_key: String,
    pub user_id: String,
    pub instructions: String,
    pub prompt: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct OpenClawEvent {
    pub event_type: String,
    pub payload: Value,
}

pub async fn run_stream(
    config: &Config,
    input: OpenClawRunInput,
    tx: mpsc::Sender<OpenClawEvent>,
) -> Result<()> {
    let client = Client::new();
    let body = serde_json::json!({
        "model": "openclaw",
        "stream": true,
        "user": input.user_id,
        "instructions": input.instructions,
        "input": input.prompt,
    });

    let res = client
        .post(format!("{}/v1/responses", config.openclaw_base_url))
        .bearer_auth(&config.openclaw_gateway_token)
        .header("x-openclaw-agent-id", &input.agent_id)
        .header("x-openclaw-session-key", &input.session_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("OpenClaw request: {}", e)))?;

    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(AppError::Internal(anyhow::anyhow!(
            "OpenClaw error {}: {}",
            status,
            text
        )));
    }

    let mut stream = res.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes =
            chunk.map_err(|e| AppError::Internal(anyhow::anyhow!("Stream read: {}", e)))?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(pos) = buffer.find("\n\n") {
            let block = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            for line in block.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        return Ok(());
                    }
                    if let Ok(val) = serde_json::from_str::<Value>(data) {
                        let event_type = val
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        if tx.send(OpenClawEvent { event_type, payload: val }).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub async fn run_chat(config: &Config, input: OpenClawRunInput) -> anyhow::Result<String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<OpenClawEvent>(100);
    let cfg = config.clone();
    let inp = input.clone();
    tokio::spawn(async move {
        let _ = run_stream(&cfg, inp, tx).await;
    });
    let mut response = String::new();
    while let Some(ev) = rx.recv().await {
        let is_delta = matches!(ev.event_type.as_str(),
            "assistant.delta" | "response.output_text.delta" | "content_block_delta");
        if is_delta {
            let delta = ev.payload.get("delta").and_then(|d| d.as_str())
                .or_else(|| ev.payload.get("text").and_then(|d| d.as_str()))
                .or_else(|| ev.payload.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()))
                .unwrap_or("");
            response.push_str(delta);
        }
    }
    Ok(response)
}

pub fn build_agent_instructions(
    project_name: &str,
    repo_slug: &str,
    branch_name: &str,
) -> String {
    format!(
        "You are a helpful AI assistant and coding agent for this team.\n\n\
         Project: {project_name} | Repo: {repo_slug} | Branch: {branch_name}\n\n\
         For casual chat or general questions: respond naturally and helpfully in the same language as the user.\n\n\
         For coding tasks:\n\
         - Work only inside the assigned workspace\n\
         - Never access .env, SSH keys, cloud credentials, or host files\n\
         - Never edit main branch directly\n\
         - Prefer small, reviewable diffs\n\
         - Run relevant tests after changes\n\
         - Do not deploy to production\n\
         - If a command is blocked, choose a safe alternative\n\
         - After coding tasks, briefly summarize what was done"
    )
}
