use axum::{
    Extension, Json,
    extract::{Path, State},
};
use colabo_domain::{Permission, Role, WorkspaceMetadata, allows, validate_workspace_name};
use colabo_storage::StoreError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::{ApiError, owner_for, workspace_for};
use crate::state::{AppState, Authenticated, WorkspaceEvent};

#[derive(Serialize)]
pub struct WorkspaceListItem {
    #[serde(flatten)]
    workspace: WorkspaceMetadata,
    role: &'static str,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
) -> Result<Json<Vec<WorkspaceListItem>>, ApiError> {
    let workspaces = state.store.workspaces_for(auth.user.id)?;
    let mut result = Vec::with_capacity(workspaces.len());
    for listed_workspace in workspaces {
        let _guard = state.store.lock_workspace(listed_workspace.id).await;
        let workspace = match state.store.workspace(listed_workspace.id) {
            Ok(workspace) => workspace,
            Err(StoreError::WorkspaceMissing) => continue,
            Err(error) => return Err(error.into()),
        };
        let memberships = state.store.memberships(workspace.id)?;
        if !allows(&workspace, &memberships, auth.user.id, Permission::Read) {
            continue;
        }
        let role = if workspace.owner_id == auth.user.id {
            "owner"
        } else {
            memberships
                .into_iter()
                .find(|membership| membership.user_id == auth.user.id)
                .map(|membership| match membership.role {
                    Role::Reader => "reader",
                    Role::Writer => "writer",
                })
                .unwrap_or("reader")
        };
        result.push(WorkspaceListItem { workspace, role });
    }
    Ok(Json(result))
}

#[derive(Deserialize)]
pub struct CreateWorkspace {
    name: String,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Json(body): Json<CreateWorkspace>,
) -> Result<Json<WorkspaceMetadata>, ApiError> {
    let name = validate_workspace_name(&body.name).map_err(ApiError::bad_request)?;
    let workspace = state.store.create_workspace(auth.user.id, name)?;
    Ok(Json(workspace))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<WorkspaceMetadata>, ApiError> {
    workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    let workspace = workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    Ok(Json(workspace))
}

#[derive(Serialize)]
pub struct WorkspaceDeleted {
    deleted: bool,
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<WorkspaceDeleted>, ApiError> {
    owner_for(&state, workspace_id, auth.user.id)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    owner_for(&state, workspace_id, auth.user.id)?;
    let sender = state.workspace_event_sender(workspace_id).await;
    let _ = sender.send(WorkspaceEvent {
        kind: "workspace_deleted".into(),
        path: None,
        destination: None,
    });
    state.deactivate_workspace(workspace_id).await?;
    state.close_workspace_streams(workspace_id).await;
    state.store.delete_workspace(workspace_id)?;
    Ok(Json(WorkspaceDeleted { deleted: true }))
}
