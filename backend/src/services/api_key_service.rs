use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::Config;
use crate::domain::api_key::{ApiKey, ApiKeyClaims, ApiKeyCreated, CreateApiKeyRequest};
use crate::error::{AppError, Result};

const KEY_RANDOM_LEN: usize = 48;
const KEY_PREFIX_STR: &str = "ak_";
const KEY_DISPLAY_PREFIX_LEN: usize = 12;

fn generate_key() -> String {
    let random: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(KEY_RANDOM_LEN)
        .map(char::from)
        .collect();
    format!("{}{}", KEY_PREFIX_STR, random)
}

fn hash_key(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}

fn display_prefix(key: &str) -> String {
    key.chars().take(KEY_DISPLAY_PREFIX_LEN).collect()
}

pub async fn create(
    db: &PgPool,
    config: &Config,
    req: CreateApiKeyRequest,
    created_by: &str,
) -> Result<ApiKeyCreated> {
    if !["admin", "developer", "viewer"].contains(&req.role.as_str()) {
        return Err(AppError::BadRequest("Invalid role. Must be admin, developer, or viewer".to_string()));
    }
    let plain_key = generate_key();
    let key_hash = hash_key(&plain_key);
    let prefix = display_prefix(&plain_key);
    let id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();

    sqlx::query(
        "INSERT INTO api_keys (id, name, key_hash, key_prefix, role, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(&req.name)
    .bind(&key_hash)
    .bind(&prefix)
    .bind(&req.role)
    .bind(created_by)
    .bind(req.expires_at)
    .execute(db)
    .await?;

    // Spawn container for this API key (fire-and-forget on failure)
    match crate::services::container_service::create(config, id, &req.name).await {
        Ok(info) => {
            let _ = sqlx::query(
                "UPDATE api_keys SET container_id = $1, container_name = $2 WHERE id = $3",
            )
            .bind(&info.container_id)
            .bind(&info.container_name)
            .bind(id)
            .execute(db)
            .await;
            tracing::info!(container = %info.container_name, "Container linked to API key");
        }
        Err(e) => tracing::warn!("Container creation skipped for {}: {}", id, e),
    }

    Ok(ApiKeyCreated {
        id,
        name: req.name,
        key: plain_key,
        key_prefix: prefix,
        role: req.role,
        created_at: now,
    })
}

pub async fn list(db: &PgPool) -> Result<Vec<ApiKey>> {
    let keys = sqlx::query_as::<_, ApiKey>(
        "SELECT * FROM api_keys ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(keys)
}

pub async fn revoke(db: &PgPool, key_id: Uuid) -> Result<()> {
    let key = sqlx::query_as::<_, ApiKey>(
        "SELECT * FROM api_keys WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(key_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("API key not found or already revoked".to_string()))?;

    if let Some(ref cid) = key.container_id {
        let _ = crate::services::container_service::destroy(cid).await;
    }

    sqlx::query(
        "UPDATE api_keys SET revoked_at = NOW(), updated_at = NOW() WHERE id = $1",
    )
    .bind(key_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn validate(db: &PgPool, raw_key: &str) -> Result<ApiKeyClaims> {
    let key_hash = hash_key(raw_key);

    let key = sqlx::query_as::<_, ApiKey>(
        "SELECT * FROM api_keys WHERE key_hash = $1",
    )
    .bind(&key_hash)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::Unauthorized("Invalid API key".to_string()))?;

    if !key.is_active() {
        return Err(AppError::Unauthorized("API key is revoked or expired".to_string()));
    }

    // Fire-and-forget last_used_at update — do not fail the request on error
    let _ = sqlx::query(
        "UPDATE api_keys SET last_used_at = NOW(), updated_at = NOW() WHERE id = $1",
    )
    .bind(key.id)
    .execute(db)
    .await;

    Ok(ApiKeyClaims {
        key_id: key.id,
        role: key.role,
        name: key.name,
    })
}

/// Called once at startup. If no admin key exists and ADMIN_API_KEY env var is set,
/// registers that key as the bootstrap admin key.
pub async fn bootstrap_admin_key(db: &PgPool, config: &Config) -> Result<()> {
    let admin_key = match &config.admin_api_key {
        Some(k) if !k.is_empty() => k.clone(),
        _ => return Ok(()),
    };

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM api_keys WHERE role = 'admin' AND revoked_at IS NULL",
    )
    .fetch_one(db)
    .await?;

    if count > 0 {
        return Ok(());
    }

    let key_hash = hash_key(&admin_key);
    let prefix = display_prefix(&admin_key);

    sqlx::query(
        "INSERT INTO api_keys (id, name, key_hash, key_prefix, role, created_by)
         VALUES ($1, 'Bootstrap Admin', $2, $3, 'admin', 'system')
         ON CONFLICT (key_hash) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(&key_hash)
    .bind(&prefix)
    .execute(db)
    .await?;

    tracing::info!("Bootstrap admin API key registered (prefix: {}...)", prefix);
    Ok(())
}
