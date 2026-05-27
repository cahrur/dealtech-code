use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use tower_http::cors::{Any, CorsLayer};

use crate::app_state::AppState;
use crate::services::api_key_service;

pub fn app_router(state: AppState) -> Router {
    let cors = if state.config.cors_origin == "*" {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        let origins: Vec<HeaderValue> = state.config.cors_origin
            .split(',')
            .filter_map(|s| s.trim().parse::<HeaderValue>().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any)
    };

    let public = Router::new()
        .route("/health", get(health));

    let protected = Router::new()
        .route("/api/apikeys",
            get(super::api_key_routes::list).post(super::api_key_routes::create))
        .route("/api/apikeys/:key_id/revoke",
            post(super::api_key_routes::revoke))
        .route("/api/projects",
            get(super::project_routes::list).post(super::project_routes::create))
        .route("/api/projects/:project_id",
            get(super::project_routes::get_one).patch(super::project_routes::update))
        .route("/api/projects/:project_id/sessions",
            get(super::session_routes::list).post(super::session_routes::create))
        .route("/api/projects/:project_id/policy",
            get(super::project_routes::get_policy).patch(super::project_routes::update_policy))
        .route("/api/projects/:project_id/audit-logs",
            get(super::project_routes::audit_logs))
        .route("/api/sessions/:session_id",
            get(super::session_routes::get))
        .route("/api/sessions/:session_id/messages",
            get(super::session_routes::messages))
        .route("/api/sessions/:session_id/chat",
            post(super::session_routes::chat))
        .route("/api/sessions/:session_id/agent-runs",
            post(super::run_routes::create))
        .route("/api/agent-runs/:run_id",
            get(super::run_routes::get))
        .route("/api/agent-runs/:run_id/cancel",
            post(super::run_routes::cancel))
        .route("/api/agent-runs/:run_id/diff",
            get(super::run_routes::diff))
        .route("/api/agent-runs/:run_id/events",
            get(super::run_routes::events))
        .route("/api/usage",
            get(super::usage_routes::summary))
        .route("/api/usage/logs",
            get(super::usage_routes::logs))
        .route_layer(middleware::from_fn_with_state(state.clone(), api_key_middleware));

    let ws_route = Router::new().route("/ws", get(super::ws::ws_handler));

    Router::new()
        .merge(public)
        .merge(protected)
        .merge(ws_route)
        .layer(cors)
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    // Check DB
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .is_ok();

    // Check Redis
    let redis_ok = {
        let mut r = state.redis.clone();
        redis::cmd("PING")
            .query_async::<_, String>(&mut r)
            .await
            .is_ok()
    };

    let status = if db_ok && redis_ok { "ok" } else { "degraded" };
    let code = if db_ok && redis_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };

    (code, Json(serde_json::json!({
        "status": status,
        "db": if db_ok { "ok" } else { "error" },
        "redis": if redis_ok { "ok" } else { "error" },
    })))
}

pub async fn api_key_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let raw_key = extract_api_key(&req).ok_or(StatusCode::UNAUTHORIZED)?;

    let claims = api_key_service::validate(&state.db, &raw_key)
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    req.extensions_mut().insert(claims.key_id);
    req.extensions_mut().insert(claims);

    Ok(next.run(req).await)
}

fn extract_api_key<B>(req: &Request<B>) -> Option<String> {
    // Prefer X-API-Key header
    if let Some(key) = req
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
    {
        return Some(key);
    }

    // Fall back to Authorization: Bearer ak_...
    req.headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|s| s.starts_with("ak_"))
        .map(|s| s.to_string())
}
