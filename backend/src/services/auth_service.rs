use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::Config;
use crate::domain::auth::{
    AuthResponse, LoginRequest, RefreshRequest, RegisterRequest, User, UserSession,
};
use crate::error::{AppError, Result};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub role: String,
    pub exp: i64,
    pub iat: i64,
}

pub async fn register(db: &PgPool, config: &Config, req: RegisterRequest) -> Result<AuthResponse> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_one(db)
        .await?;
    if count > 0 {
        return Err(AppError::Conflict("Email already registered".to_string()));
    }
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Hash error: {}", e)))?
        .to_string();
    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (id, email, password_hash, name, role)
         VALUES ($1, $2, $3, $4, 'developer') RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(&req.email)
    .bind(&password_hash)
    .bind(&req.name)
    .fetch_one(db)
    .await?;
    create_auth_response(db, config, user).await
}

pub async fn login(db: &PgPool, config: &Config, req: LoginRequest) -> Result<AuthResponse> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::Unauthorized("Invalid credentials".to_string()))?;
    let parsed_hash = PasswordHash::new(&user.password_hash)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Hash parse: {}", e)))?;
    Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .map_err(|_| AppError::Unauthorized("Invalid credentials".to_string()))?;
    create_auth_response(db, config, user).await
}

pub async fn refresh(db: &PgPool, config: &Config, req: RefreshRequest) -> Result<AuthResponse> {
    let token_hash = format!("{:x}", Sha256::digest(req.refresh_token.as_bytes()));
    let session = sqlx::query_as::<_, UserSession>(
        "SELECT * FROM user_sessions WHERE refresh_token_hash = $1 AND expires_at > NOW()",
    )
    .bind(&token_hash)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::Unauthorized("Invalid or expired refresh token".to_string()))?;
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(session.user_id)
        .fetch_one(db)
        .await?;
    sqlx::query("DELETE FROM user_sessions WHERE id = $1")
        .bind(session.id)
        .execute(db)
        .await?;
    create_auth_response(db, config, user).await
}

pub async fn logout(db: &PgPool, user_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM user_sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

pub fn verify_token(config: &Config, token: &str) -> Result<Claims> {
    let key = DecodingKey::from_secret(config.jwt_secret.as_bytes());
    decode::<Claims>(token, &key, &Validation::default())
        .map(|d| d.claims)
        .map_err(|_| AppError::Unauthorized("Invalid token".to_string()))
}

async fn create_auth_response(db: &PgPool, config: &Config, user: User) -> Result<AuthResponse> {
    let now = OffsetDateTime::now_utc();
    let exp = now + time::Duration::hours(config.jwt_expiry_hours);
    let claims = Claims {
        sub: user.id.to_string(),
        email: user.email.clone(),
        role: user.role.clone(),
        exp: exp.unix_timestamp(),
        iat: now.unix_timestamp(),
    };
    let access_token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("JWT encode: {}", e)))?;
    let refresh_token: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();
    let token_hash = format!("{:x}", Sha256::digest(refresh_token.as_bytes()));
    let expires_at = now + time::Duration::days(config.refresh_token_expiry_days);
    sqlx::query(
        "INSERT INTO user_sessions (id, user_id, refresh_token_hash, expires_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(&token_hash)
    .bind(expires_at)
    .execute(db)
    .await?;
    Ok(AuthResponse {
        access_token,
        refresh_token,
        user: user.into(),
    })
}
