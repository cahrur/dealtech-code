use redis::aio::ConnectionManager;
use crate::config::Config;

pub async fn connect(config: &Config) -> anyhow::Result<ConnectionManager> {
    let client = redis::Client::open(config.redis_url())?;
    let manager = ConnectionManager::new(client).await?;
    tracing::info!("Redis connected");
    Ok(manager)
}
