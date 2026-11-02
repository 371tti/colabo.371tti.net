use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Multipart, Path, Query, State, WebSocketUpgrade, multipart::Field},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use colabo_domain::{FileEntry, Permission, allows, validate_relative_path};
use colabo_storage::{CollaborationFile, StoreError};
use collab_core::{ActorId, DocumentId, EditorIdentity};
use serde::{Deserialize, Serialize};
use std::path::{Path as FilePath, PathBuf};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::error::{ApiError, workspace_for};
use crate::state::{AppState, Authenticated, WorkspaceEvent};

pub(super) const MAX_UPLOAD_BYTES: usize = 190_000_000;
pub(super) const MAX_UPLOAD_REQUEST_BYTES: usize = MAX_UPLOAD_BYTES + 8 * 1024 * 1024;
const MAX_MULTIPART_PATH_BYTES: usize = 4096;
const MAX_FILE_BYTES: usize = 24 * 1024 * 1024;
const MAX_COLLABORATIVE_FILE_BYTES: u64 = MAX_FILE_BYTES as u64;

#[derive(Deserialize)]
pub struct DirectoryQuery {
    path: Option<String>,
}

pub async fn list_directory(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<Vec<FileEntry>>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    state.flush_documents(workspace_id).await?;
    Ok(Json(state.store.list_directory(
        workspace_id,
        query.path.as_deref().unwrap_or(""),
    )?))
}

#[derive(Deserialize)]
pub struct FileQuery {
    path: String,
}

pub async fn read_file(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<FileQuery>,
) -> Result<Response, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    state.flush_documents(workspace_id).await?;
    let bytes = state.store.read_file(workspace_id, &query.path)?;
    Ok(file_response(&query.path, bytes))
}

pub async fn download_file(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<FileQuery>,
) -> Result<Response, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    state.flush_documents(workspace_id).await?;
    let bytes = state.store.read_file(workspace_id, &query.path)?;
    Ok(file_response(&query.path, bytes))
}

pub async fn write_file(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<FileQuery>,
    bytes: Bytes,
) -> Result<Json<FileEntry>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "file exceeds the 24 MiB limit",
        ));
    }
    let path = validate_relative_path(&query.path, false)
        .map_err(|_| ApiError::bad_request("invalid workspace path"))?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    let session_replaced = write_one(&state, workspace_id, &path, &bytes).await?;
    let event_kind = if session_replaced {
        "file_replaced"
    } else {
        "file_changed"
    };
    let _ = state
        .workspace_event_sender(workspace_id)
        .await
        .send(WorkspaceEvent {
            kind: event_kind.into(),
            path: Some(path.clone()),
            destination: None,
        });
    file_entry(&state, workspace_id, &path).map(Json)
}

#[derive(Deserialize)]
pub struct NewDirectory {
    path: String,
}

pub async fn create_directory(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<NewDirectory>,
) -> Result<Json<FileEntry>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    let path = validate_relative_path(&body.path, false)
        .map_err(|_| ApiError::bad_request("invalid workspace path"))?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    state.store.create_directory(workspace_id, &path)?;
    notify(
        &state,
        workspace_id,
        "directory_created",
        Some(path.clone()),
        None,
    )
    .await;
    file_entry(&state, workspace_id, &path).map(Json)
}

#[derive(Deserialize)]
pub struct MovePath {
    source: String,
    destination: String,
}

pub async fn move_path(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<MovePath>,
) -> Result<Json<MoveResult>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    let source = validate_relative_path(&body.source, false)
        .map_err(|_| ApiError::bad_request("invalid source path"))?;
    let destination = validate_relative_path(&body.destination, false)
        .map_err(|_| ApiError::bad_request("invalid destination path"))?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    state.store.move_path(workspace_id, &source, &destination)?;
    notify(
        &state,
        workspace_id,
        "path_moved",
        Some(source),
        Some(destination),
    )
    .await;
    Ok(Json(MoveResult { moved: true }))
}

#[derive(Serialize)]
pub struct MoveResult {
    moved: bool,
}

#[derive(Serialize)]
pub struct DeleteResult {
    deleted: bool,
}

pub async fn delete_path(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<FileQuery>,
) -> Result<Json<DeleteResult>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    let path = validate_relative_path(&query.path, false)
        .map_err(|_| ApiError::bad_request("invalid workspace path"))?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    let removed = state.store.delete_path(workspace_id, &path)?;
    for file_id in &removed {
        state.deactivate_document(*file_id).await;
    }
    state
        .store
        .remove_collaboration_files(workspace_id, &removed)?;
    notify(&state, workspace_id, "path_deleted", Some(path), None).await;
    Ok(Json(DeleteResult { deleted: true }))
}

pub async fn upload(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<Json<Vec<FileEntry>>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    let staging_dir = tempfile::tempdir().map_err(|_| ApiError::internal())?;
    let mut folder = String::new();
    let mut next_file_path: Option<String> = None;
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut total = 0usize;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::bad_request("invalid multipart upload"))?
    {
        match field.name() {
            Some("folder") => {
                folder =
                    read_multipart_text(field, MAX_MULTIPART_PATH_BYTES, "invalid upload folder")
                        .await?;
                folder = validate_relative_path(&folder, true)
                    .map_err(|_| ApiError::bad_request("invalid upload folder"))?;
            }
            Some("file_path") => {
                if next_file_path.is_some() {
                    return Err(ApiError::bad_request(
                        "file path was not followed by a file",
                    ));
                }
                let path =
                    read_multipart_text(field, MAX_MULTIPART_PATH_BYTES, "invalid uploaded path")
                        .await?;
                next_file_path = Some(
                    validate_relative_path(&path, false)
                        .map_err(|_| ApiError::bad_request("invalid uploaded path"))?,
                );
            }
            Some("files") => {
                let filename = field
                    .file_name()
                    .map(str::to_owned)
                    .ok_or_else(|| ApiError::bad_request("uploaded file has no name"))?;
                let filename = next_file_path
                    .take()
                    .map_or_else(|| validate_relative_path(&filename, false), Ok)
                    .map_err(|_| ApiError::bad_request("invalid uploaded filename"))?;
                let path = if folder.is_empty() {
                    filename
                } else {
                    format!("{folder}/{filename}")
                };
                let path = validate_relative_path(&path, false)
                    .map_err(|_| ApiError::bad_request("invalid uploaded path"))?;
                let temporary_path = staging_dir.path().join(files.len().to_string());
                let mut temporary_file = tokio::fs::File::create(&temporary_path)
                    .await
                    .map_err(|_| ApiError::internal())?;
                let mut field = field;
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|_| ApiError::bad_request("could not read uploaded file"))?
                {
                    total = total.saturating_add(chunk.len());
                    if total > MAX_UPLOAD_BYTES {
                        return Err(ApiError::new(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "upload exceeds the 190 MB file data limit",
                        ));
                    }
                    temporary_file
                        .write_all(&chunk)
                        .await
                        .map_err(|_| ApiError::internal())?;
                }
                temporary_file
                    .flush()
                    .await
                    .map_err(|_| ApiError::internal())?;
                temporary_file
                    .sync_all()
                    .await
                    .map_err(|_| ApiError::internal())?;
                files.push((path, temporary_path));
            }
            _ => return Err(ApiError::bad_request("unexpected multipart field")),
        }
    }
    if next_file_path.is_some() {
        return Err(ApiError::bad_request(
            "file path was not followed by a file",
        ));
    }
    if files.is_empty() {
        return Err(ApiError::bad_request("no files were uploaded"));
    }
    let _guard = state.store.lock_workspace(workspace_id).await;
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Write)?;
    let mut result = Vec::with_capacity(files.len());
    for (path, temporary_path) in files {
        let session_replaced =
            write_one_from_path(&state, workspace_id, &path, &temporary_path).await?;
        let event_kind = if session_replaced {
            "file_replaced"
        } else {
            "file_changed"
        };
        notify(&state, workspace_id, event_kind, Some(path.clone()), None).await;
        result.push(file_entry(&state, workspace_id, &path)?);
    }
    Ok(Json(result))
}

async fn read_multipart_text(
    mut field: Field<'_>,
    max_bytes: usize,
    error_message: &'static str,
) -> Result<String, ApiError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| ApiError::bad_request(error_message))?
    {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ApiError::bad_request(error_message));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| ApiError::bad_request(error_message))
}

#[derive(Serialize)]
pub struct CollaborationInfo {
    document_id: String,
    file_id: Uuid,
    actor_id: u64,
    identity: EditorIdentity,
    websocket_path: String,
    can_write: bool,
    max_message_bytes: usize,
}

pub async fn collaboration_info(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<FileQuery>,
) -> Result<Json<CollaborationInfo>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    let workspace = workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let file = state.store.collaboration_file(workspace_id, &query.path)?;
    state.ensure_document(workspace_id, file.clone()).await?;
    let actor_id = state
        .store
        .editor_actor_id(workspace_id, file.id, auth.user.id)?;
    let identity = editor_identity(&auth.user);
    let can_write = allows(
        &workspace,
        &state.store.memberships(workspace_id)?,
        auth.user.id,
        Permission::Write,
    );
    Ok(Json(CollaborationInfo {
        document_id: file.id.to_string(),
        file_id: file.id,
        actor_id,
        identity,
        websocket_path: format!(
            "/api/workspaces/{workspace_id}/collaboration/{}/ws",
            file.id
        ),
        can_write,
        max_message_bytes: state.collaboration.websocket_max_message_size(),
    }))
}

pub async fn collaboration_socket(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, file_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::forbidden)?;
    if !state.collaboration.origin_allowed(origin) {
        return Err(ApiError::forbidden());
    }
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    let workspace = workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let path = state.store.collaboration_file_path(workspace_id, file_id)?;
    let file = CollaborationFile { id: file_id, path };
    let document = state.ensure_document(workspace_id, file).await?;
    let actor = ActorId(
        state
            .store
            .editor_actor_id(workspace_id, file_id, auth.user.id)?,
    );
    let identity = editor_identity(&auth.user);
    let writable = allows(
        &workspace,
        &state.store.memberships(workspace_id)?,
        auth.user.id,
        Permission::Write,
    );
    let server = state.collaboration.clone();
    let max_message = server.websocket_max_message_size();
    drop(_guard);
    Ok(upgrade
        .max_message_size(max_message)
        .on_upgrade(move |socket| async move {
            if let Err(error) = server.connect_socket(&document, socket, actor, identity, writable).await {
                tracing::warn!(%error, %workspace_id, %file_id, "collaboration websocket ended with an error");
            }
        }))
}

pub async fn workspace_events_socket(
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
    let mut events = state.workspace_event_sender(workspace_id).await.subscribe();
    drop(_guard);
    Ok(upgrade.on_upgrade(move |mut socket| async move {
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => {
                        if let Ok(text) = serde_json::to_string(&event)
                            && socket.send(axum::extract::ws::Message::Text(text.into())).await.is_err() { break; }
                    }
                    Err(_) => break,
                },
                message = socket.recv() => match message {
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

async fn write_one_from_path(
    state: &AppState,
    workspace_id: Uuid,
    path: &str,
    temporary_path: &FilePath,
) -> Result<bool, ApiError> {
    let mut session_replaced = false;
    let collaboration_file = match state.store.collaboration_file(workspace_id, path) {
        Ok(file) => Some(file),
        Err(StoreError::FileMissing) => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(file) = collaboration_file {
        if state.is_document_active(file.id).await {
            let size = tokio::fs::metadata(temporary_path)
                .await
                .map_err(|_| ApiError::internal())?
                .len();
            if size <= MAX_COLLABORATIVE_FILE_BYTES {
                let bytes = tokio::fs::read(temporary_path)
                    .await
                    .map_err(|_| ApiError::internal())?;
                if let Ok(text) = std::str::from_utf8(&bytes)
                    && !text.contains('\0')
                {
                    let document =
                        DocumentId::new(file.id.to_string()).map_err(|_| ApiError::internal())?;
                    if !state.collaboration.has_session(&document).await {
                        state.deactivate_document(file.id).await;
                        state.ensure_document(workspace_id, file.clone()).await?;
                        session_replaced = true;
                    }
                    state
                        .collaboration
                        .replace_text_with_guard_held(&document, text)
                        .await
                        .map_err(|_| ApiError::internal())?;
                    let snapshot = state
                        .collaboration
                        .snapshot(&document)
                        .await
                        .map_err(|_| ApiError::internal())?;
                    state
                        .store
                        .save_collaboration_state(workspace_id, file.id, &snapshot, text)?;
                    return Ok(session_replaced);
                }
            }
            state.deactivate_document(file.id).await;
            session_replaced = true;
        }
        state
            .store
            .clear_collaboration_snapshot(workspace_id, file.id)?;
    }

    let store = state.store.clone();
    let path = path.to_owned();
    let temporary_path = temporary_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        match store.replace_file_from_path(workspace_id, &path, &temporary_path) {
            Ok(()) => Ok(()),
            Err(StoreError::FileMissing) => {
                store.create_file_from_path(workspace_id, &path, &temporary_path)
            }
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(session_replaced)
}

async fn write_one(
    state: &AppState,
    workspace_id: Uuid,
    path: &str,
    bytes: &[u8],
) -> Result<bool, ApiError> {
    let mut session_replaced = false;
    match state.store.read_file(workspace_id, path) {
        Ok(_) => {
            if let Ok(file) = state.store.collaboration_file(workspace_id, path) {
                if state.is_document_active(file.id).await {
                    match std::str::from_utf8(bytes) {
                        Ok(text) if !text.contains('\0') => {
                            let document = DocumentId::new(file.id.to_string())
                                .map_err(|_| ApiError::internal())?;
                            if !state.collaboration.has_session(&document).await {
                                state.deactivate_document(file.id).await;
                                state.ensure_document(workspace_id, file.clone()).await?;
                                session_replaced = true;
                            }
                            state
                                .collaboration
                                .replace_text_with_guard_held(&document, text)
                                .await
                                .map_err(|_| ApiError::internal())?;
                            let snapshot = state
                                .collaboration
                                .snapshot(&document)
                                .await
                                .map_err(|_| ApiError::internal())?;
                            state.store.save_collaboration_state(
                                workspace_id,
                                file.id,
                                &snapshot,
                                text,
                            )?;
                            return Ok(session_replaced);
                        }
                        _ => {
                            state.deactivate_document(file.id).await;
                            session_replaced = true;
                        }
                    }
                }
                state
                    .store
                    .clear_collaboration_snapshot(workspace_id, file.id)?;
            }
            state.store.replace_file(workspace_id, path, bytes)?;
        }
        Err(StoreError::FileMissing) => {
            state.store.create_file(workspace_id, path, bytes)?;
        }
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            state.store.create_file(workspace_id, path, bytes)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(session_replaced)
}

fn file_entry(state: &AppState, workspace_id: Uuid, path: &str) -> Result<FileEntry, ApiError> {
    let parent = path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
    state
        .store
        .list_directory(workspace_id, parent)?
        .into_iter()
        .find(|entry| entry.name == name)
        .ok_or_else(ApiError::not_found)
}

pub(super) fn file_response(path: &str, bytes: Vec<u8>) -> Response {
    let content_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let mut response = Response::new(axum::body::Body::from(bytes));
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        "attachment".parse().expect("valid header"),
    );
    response
}

fn editor_identity(user: &colabo_domain::User) -> EditorIdentity {
    EditorIdentity::new(user.id.to_string(), user.display_name.clone())
        .unwrap_or_else(|_| EditorIdentity::for_actor(ActorId(0)))
}

async fn notify(
    state: &AppState,
    workspace_id: Uuid,
    kind: &str,
    path: Option<String>,
    destination: Option<String>,
) {
    let sender = state.workspace_event_sender(workspace_id).await;
    let _ = sender.send(WorkspaceEvent {
        kind: kind.into(),
        path,
        destination,
    });
}

pub(super) fn require_origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::forbidden)?;
    if state.collaboration.origin_allowed(origin) {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}
