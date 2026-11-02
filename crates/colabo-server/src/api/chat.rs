use axum::{
    Extension, Json,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::HeaderMap,
    response::Response,
};
use colabo_domain::{ChatMessage, Permission};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    error::{ApiError, workspace_for},
    files::require_origin,
};
use crate::state::{AppState, Authenticated};

#[derive(Deserialize)]
pub struct HistoryQuery {
    after: Option<u64>,
    limit: Option<usize>,
}

pub async fn history(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Vec<ChatMessage>>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    Ok(Json(state.store.chat_messages(
        workspace_id,
        query.after.unwrap_or(0),
        query.limit.unwrap_or(100),
    )?))
}

#[derive(Deserialize)]
pub struct NewMessage {
    body: String,
}

pub async fn send(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<NewMessage>,
) -> Result<Json<ChatMessage>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let body = body.body.trim();
    if body.is_empty()
        || body.chars().count() > 4000
        || body
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
    {
        return Err(ApiError::bad_request(
            "message must contain 1 to 4000 printable characters",
        ));
    }
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let message = state
        .store
        .append_chat_message(workspace_id, auth.user.id, body.to_owned())?;
    let _ = state.chat_sender(workspace_id).await.send(message.clone());
    Ok(Json(message))
}

#[derive(Serialize)]
struct SocketMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    message: &'a ChatMessage,
}

pub async fn socket(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    require_origin(&state, &headers)?;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let sender = state.chat_sender(workspace_id).await;
    let mut messages = sender.subscribe();
    drop(_guard);
    Ok(upgrade.on_upgrade(move |mut socket| async move {
        loop {
            tokio::select! {
                message = messages.recv() => match message {
                    Ok(message) => {
                        let event = SocketMessage { kind: "message", message: &message };
                        if let Ok(text) = serde_json::to_string(&event)
                            && socket.send(axum::extract::ws::Message::Text(text.into())).await.is_err() { break; }
                    }
                    Err(_) => break,
                },
                frame = socket.recv() => match frame {
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(axum::extract::ws::Message::Ping(payload))) => {
                        if socket.send(axum::extract::ws::Message::Pong(payload)).await.is_err() { break; }
                    }
                    _ => {}
                }
            }
        }
    }))
}
