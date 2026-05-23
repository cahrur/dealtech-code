use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::usage::{ModelUsage, UsageLog, UsageSummary};
use crate::error::Result;

pub async fn record(
    db: &PgPool,
    api_key_id: Option<Uuid>,
    run_id: Option<Uuid>,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> anyhow::Result<()> {
    let total = input_tokens + output_tokens;
    sqlx::query(
        "INSERT INTO usage_logs (id, api_key_id, run_id, model, input_tokens, output_tokens, total_tokens)
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6)",
    )
    .bind(api_key_id)
    .bind(run_id)
    .bind(model)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(total)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn summary_for_key(db: &PgPool, api_key_id: Uuid) -> Result<UsageSummary> {
    let total_runs: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT run_id) FROM usage_logs WHERE api_key_id = $1",
    )
    .bind(api_key_id)
    .fetch_one(db)
    .await?;

    let totals = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), COALESCE(SUM(total_tokens),0)
         FROM usage_logs WHERE api_key_id = $1",
    )
    .bind(api_key_id)
    .fetch_one(db)
    .await?;

    #[derive(sqlx::FromRow)]
    struct ModelRow {
        model: String,
        runs: i64,
        input_tokens: i64,
        output_tokens: i64,
    }
    let by_model = sqlx::query_as::<_, ModelRow>(
        "SELECT model,
                COUNT(DISTINCT run_id) AS runs,
                COALESCE(SUM(input_tokens),0)  AS input_tokens,
                COALESCE(SUM(output_tokens),0) AS output_tokens
         FROM usage_logs WHERE api_key_id = $1
         GROUP BY model ORDER BY SUM(total_tokens) DESC",
    )
    .bind(api_key_id)
    .fetch_all(db)
    .await?;

    Ok(UsageSummary {
        total_runs,
        total_input_tokens: totals.0,
        total_output_tokens: totals.1,
        total_tokens: totals.2,
        by_model: by_model
            .into_iter()
            .map(|r| ModelUsage {
                model: r.model,
                runs: r.runs,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
            })
            .collect(),
    })
}

pub async fn list_for_key(db: &PgPool, api_key_id: Uuid, limit: i64) -> Result<Vec<UsageLog>> {
    sqlx::query_as::<_, UsageLog>(
        "SELECT id, api_key_id, run_id, model, input_tokens, output_tokens, total_tokens, created_at
         FROM usage_logs WHERE api_key_id = $1
         ORDER BY created_at DESC LIMIT $2",
    )
    .bind(api_key_id)
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(Into::into)
}
