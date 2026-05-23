use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::domain::api_key::{ApiKeyClaims, ApiKeyPublic, CreateApiKeyRequest};
use crate::error::{AppError, Result};
use crate::services::api_key_service;

fn require_admin(claims: &ApiKeyClaims) -> Result<()> {
    if claims.role != "admin" {
        return Err(AppError::Forbidden("Admin role required".to_string()));
    }
    Ok(())
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<ApiKeyClaims>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse> {
    require_admin(&claims)?;
    let created = api_key_service::create(&state.db, &state.config, req, &claims.name).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<ApiKeyClaims>,
) -> Result<impl IntoResponse> {
    require_admin(&claims)?;
    let keys = api_key_service::list(&state.db).await?;
    let public: Vec<ApiKeyPublic> = keys.into_iter().map(Into::into).collect();
    Ok(Json(public))
}

pub async fn revoke(
    State(state): State<AppState>,
    Extension(claims): Extension<ApiKeyClaims>,
    Path(key_id): Path<Uuid>,
) -> Result<impl IntoResponse> {
    require_admin(&claims)?;
    api_key_service::revoke(&state.db, key_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
