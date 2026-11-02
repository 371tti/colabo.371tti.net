use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use chrono::Utc;
use colabo_domain::{CommitFileChange, CommitInfo, FileEntry, Permission};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    error::{ApiError, owner_for, workspace_for},
    files::file_response,
};
use crate::state::{AppState, Authenticated, WorkspaceEvent};

#[derive(Deserialize)]
pub struct HistoryQuery {
    limit: Option<usize>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Vec<CommitInfo>>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    Ok(Json(state.store.commit_history(
        workspace_id,
        query.limit.unwrap_or(100),
    )?))
}

#[derive(Deserialize)]
pub struct NewCheckpoint {
    message: String,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<NewCheckpoint>,
) -> Result<Json<Option<CommitInfo>>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    let message = body.message.trim();
    if message.is_empty() || message.chars().count() > 160 || message.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "checkpoint message must contain 1 to 160 printable characters",
        ));
    }
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    state.flush_documents(workspace_id).await?;
    let commit =
        state
            .store
            .checkpoint(workspace_id, message, auth.user.id, &auth.user.display_name)?;
    if commit.is_some() {
        let sender = state.workspace_event_sender(workspace_id).await;
        let _ = sender.send(WorkspaceEvent {
            kind: "checkpoint_created".into(),
            path: None,
            destination: None,
        });
    }
    Ok(Json(commit))
}

pub async fn read_file(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, commit_id)): Path<(Uuid, String)>,
    Query(query): Query<FileQuery>,
) -> Result<axum::response::Response, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let bytes = state
        .store
        .read_checkpoint_file(workspace_id, &commit_id, &query.path)?;
    Ok(file_response(&query.path, bytes))
}

pub async fn files(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, commit_id)): Path<(Uuid, String)>,
) -> Result<Json<Vec<FileEntry>>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    Ok(Json(
        state.store.checkpoint_files(workspace_id, &commit_id)?,
    ))
}

pub async fn changes(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, commit_id)): Path<(Uuid, String)>,
) -> Result<Json<Vec<CommitFileChange>>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    Ok(Json(
        state.store.checkpoint_changes(workspace_id, &commit_id)?,
    ))
}

#[derive(Deserialize)]
pub struct FileQuery {
    path: String,
}

#[derive(Serialize)]
pub struct RestoreResult {
    files_restored: usize,
    checkpoint: Option<CommitInfo>,
}

pub async fn restore(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, commit_id)): Path<(Uuid, String)>,
) -> Result<Json<RestoreResult>, ApiError> {
    owner_for(&state, workspace_id, auth.user.id)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    owner_for(&state, workspace_id, auth.user.id)?;
    state.flush_documents(workspace_id).await?;
    state.store.checkpoint(
        workspace_id,
        &format!("Before restore {}", Utc::now().format("%Y-%m-%d %H:%M UTC")),
        auth.user.id,
        &auth.user.display_name,
    )?;
    state.deactivate_workspace(workspace_id).await?;
    let files = state.store.restore_checkpoint(workspace_id, &commit_id)?;
    state.store.reset_collaboration(workspace_id)?;
    let checkpoint = state.store.checkpoint(
        workspace_id,
        &format!("Restore to {}", &commit_id[..commit_id.len().min(12)]),
        auth.user.id,
        &auth.user.display_name,
    )?;
    let sender = state.workspace_event_sender(workspace_id).await;
    let _ = sender.send(WorkspaceEvent {
        kind: "file_tree_restored".into(),
        path: None,
        destination: None,
    });
    Ok(Json(RestoreResult {
        files_restored: files.len(),
        checkpoint,
    }))
}
