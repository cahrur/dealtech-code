use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

/// Row returned from the api_keys table (key_hash is never exposed to callers).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ApiKey {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub key_hash: String,
    pub key_prefix: String,
    pub role: String,
    pub created_by: String,
    pub last_used_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
    pub container_id: Option<String>,
    pub container_name: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl ApiKey {
    pub fn is_active(&self) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        if let Some(exp) = self.expires_at {
            return exp > OffsetDateTime::now_utc();
        }
        true
    }
}

/// Returned once on creation — the only time the plain-text key is visible.
#[derive(Debug, Serialize)]
pub struct ApiKeyCreated {
    pub id: Uuid,
    pub name: String,
    pub key: String,        // plain-text, shown once
    pub key_prefix: String,
    pub role: String,
    pub created_at: OffsetDateTime,
}

/// Public view of an API key (no secrets).
#[derive(Debug, Serialize)]
pub struct ApiKeyPublic {
    pub id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub role: String,
    pub created_by: String,
    pub last_used_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

impl From<ApiKey> for ApiKeyPublic {
    fn from(k: ApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name,
            key_prefix: k.key_prefix,
            role: k.role,
            created_by: k.created_by,
            last_used_at: k.last_used_at,
            expires_at: k.expires_at,
            revoked_at: k.revoked_at,
            created_at: k.created_at,
        }
    }
}

/// Request body for creating a new API key.
#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub role: String,
    pub expires_at: Option<OffsetDateTime>,
}

/// Claims injected into request extensions after successful API key auth.
#[derive(Debug, Clone)]
pub struct ApiKeyClaims {
    pub key_id: Uuid,
    pub role: String,
    pub name: String,
}
