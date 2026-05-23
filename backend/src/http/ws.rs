use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Query, State},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::services::api_key_service;

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    SubscribeSession { session_id: Uuid, after_seq: Option<i64> },
    Ping,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    headers: axum::http::HeaderMap,
) -> Response {
    let raw_key = query.api_key.or_else(|| {
        headers.get("X-API-Key")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    }).or_else(|| {
        headers.get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|s| s.starts_with("ak_"))
            .map(|s| s.to_string())
    });

    ws.on_upgrade(move |socket| handle_socket(socket, state, raw_key))
}

async fn handle_socket(socket: WebSocket, state: AppState, raw_key: Option<String>) {
    let raw_key = match raw_key {
        Some(k) => k,
        None => { let _ = socket.close().await; return; }
    };

    if api_key_service::validate(&state.db, &raw_key).await.is_err() {
        let _ = socket.close().await;
        return;
    }

    let (mut sender, mut receiver) = socket.split();
    while let Some(Ok(msg)) = receiver.next().await {
        if let Message::Text(text) = msg {
            let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) else { continue; };
            match client_msg {
                ClientMessage::SubscribeSession { session_id, after_seq } => {
                    if let Some(seq) = after_seq {
                        if let Ok(events) = crate::services::realtime_service::get_events_after(
                            &state.db, session_id, seq,
                        ).await {
                            for event in events {
                                let _ = sender.send(Message::Text(
                                    serde_json::to_string(&event).unwrap_or_default(),
                                )).await;
                            }
                        }
                    }
                    let client = match redis::Client::open(state.config.redis_url()) {
                        Ok(c) => c, Err(_) => break,
                    };
                    let mut pubsub = match client.get_async_pubsub().await {
                        Ok(p) => p, Err(_) => break,
                    };
                    if pubsub.subscribe(format!("session:{}", session_id)).await.is_err() { break; }
                    let mut msg_stream = pubsub.on_message();
                    while let Some(redis_msg) = msg_stream.next().await {
                        let payload: String = match redis_msg.get_payload() {
                            Ok(p) => p, Err(_) => continue,
                        };
                        if sender.send(Message::Text(payload)).await.is_err() { break; }
                    }
                }
                ClientMessage::Ping => {
                    let _ = sender.send(Message::Text(r#"{"type":"pong"}"#.to_string())).await;
                }
            }
        }
    }
}
