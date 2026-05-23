use std::sync::Arc;
use sqlx::PgPool;
use redis::aio::ConnectionManager;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub redis: ConnectionManager,
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(db: PgPool, redis: ConnectionManager, config: Config) -> Self {
        Self {
            db,
            redis,
            config: Arc::new(config),
        }
    }
}
