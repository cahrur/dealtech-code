mod app_state;
mod config;
mod domain;
mod error;
mod http;
mod infra;
mod services;
mod telegram;
mod workers;

use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use app_state::AppState;
use config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(&config.log_level))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    tracing::info!("Starting AI Coding Agent Platform backend");

    let db = infra::db::connect(&config).await?;
    let redis = infra::redis::connect(&config).await?;

    services::api_key_service::bootstrap_admin_key(&db, &config)
        .await
        .map_err(|e| anyhow::anyhow!("Bootstrap admin key failed: {}", e))?;

    // On startup: reset any runs stuck in processing/running_agent back to queued
    // (happens when backend restarts mid-run)
    let reset_count = sqlx::query_scalar::<_, i64>(
        "UPDATE agent_runs SET status='queued', started_at=NULL \
         WHERE status IN ('processing','running_agent','preparing_workspace','collecting_diff','auto_push_or_pr') \
         AND finished_at IS NULL \
         RETURNING 1"
    )
    .fetch_all(&db)
    .await
    .map(|rows| rows.len())
    .unwrap_or(0);
    if reset_count > 0 {
        tracing::warn!(count = reset_count, "Reset stuck runs to queued on startup");
    }

    let state = AppState::new(db.clone(), redis.clone(), config.clone());

    let db_arc = Arc::new(db.clone());
    let config_arc = Arc::new(config.clone());

    // Improvement 2: Create semaphore for max concurrent runs
    let semaphore = if config.max_concurrent_runs > 0 {
        Some(Arc::new(Semaphore::new(config.max_concurrent_runs)))
    } else {
        None
    };

    tokio::spawn(workers::cleanup_worker::run(
        db.clone(),
        config.worktrees_path.clone(),
    ));

    tokio::spawn(workers::agent_run_worker::run(
        db_arc.clone(),
        redis.clone(),
        config_arc.clone(),
        semaphore,
    ));

    tokio::spawn(workers::stuck_run_recovery::run(
        db_arc,
        redis.clone(),
        config_arc.clone(),
    ));

    tokio::spawn(workers::disk_alert_worker::run(
        config.clone(),
        db.clone(),
        redis.clone(),
    ));

    tokio::spawn(workers::backup_worker::run(
        config.clone(),
        redis.clone(),
    ));

    // Telegram bot polling
    if config.telegram_enabled && !config.telegram_bot_token.is_empty() {
        let tg_config = Arc::new(config.clone());
        let tg_db = db.clone();
        let tg_redis = redis.clone();
        tokio::spawn(async move {
            telegram::start_polling(tg_config, tg_db, tg_redis).await;
        });
        tracing::info!("Telegram bot enabled");
    }

    let app = http::routes::app_router(state);
    let addr = format!("0.0.0.0:{}", config.app_port);
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // Improvement 1: Graceful shutdown
    if config.graceful_shutdown {
        let shutdown_signal = async {
            let ctrl_c = async {
                tokio::signal::ctrl_c()
                    .await
                    .expect("failed to install CTRL+C handler");
            };
            #[cfg(unix)]
            let terminate = async {
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to install SIGTERM handler")
                    .recv()
                    .await;
            };
            #[cfg(not(unix))]
            let terminate = std::future::pending::<()>();

            tokio::select! {
                _ = ctrl_c => {},
                _ = terminate => {},
            }
            tracing::info!("Shutting down gracefully");
        };
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal)
            .await?;
        // Wait for running tasks to finish (max 30 seconds)
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
    } else {
        axum::serve(listener, app).await?;
    }

    Ok(())
}
