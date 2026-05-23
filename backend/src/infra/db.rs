use sqlx::postgres::PgPoolOptions;
use crate::config::Config;

pub async fn connect(config: &Config) -> anyhow::Result<sqlx::PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&config.db_url())
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    tracing::info!("Database connected and migrations applied");
    Ok(pool)
}
