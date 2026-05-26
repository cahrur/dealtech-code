mod app_state;
mod config;
mod domain;
mod error;
mod http;
mod infra;
mod services;
mod workers;

use std::sync::Arc;
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

    let state = AppState::new(db.clone(), redis.clone(), config.clone());

    let db_arc = Arc::new(db.clone());
    let config_arc = Arc::new(config.clone());

    tokio::spawn(workers::cleanup_worker::run(
        db.clone(),
        config.worktrees_path.clone(),
    ));

    tokio::spawn(workers::agent_run_worker::run(
        db_arc.clone(),
        redis.clone(),
        config_arc.clone(),
    ));

    tokio::spawn(workers::stuck_run_recovery::run(
        db_arc,
        redis.clone(),
    ));

    let app = http::routes::app_router(state);
    let addr = format!("0.0.0.0:{}", config.app_port);
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
