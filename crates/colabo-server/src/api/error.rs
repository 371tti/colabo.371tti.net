use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use colabo_domain::{Permission, WorkspaceMetadata, allows};
use colabo_storage::{Store, StoreError};
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: &'static str,
}

impl ApiError {
    pub fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }

    pub fn bad_request(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "permission denied")
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "resource not found")
    }

    pub fn conflict(message: &'static str) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "request could not be completed",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::WorkspaceMissing | StoreError::UserMissing | StoreError::FileMissing => {
                Self::not_found()
            }
            StoreError::AlreadyExists | StoreError::DestinationExists => {
                Self::conflict("destination already exists")
            }
            StoreError::InvalidPath => Self::bad_request("invalid workspace path"),
            StoreError::Symlink => Self::forbidden(),
            StoreError::InvalidCollaboration => {
                Self::bad_request("file is not a UTF-8 text document")
            }
            StoreError::Git(_) => Self::internal(),
            StoreError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::not_found()
            }
            other => {
                tracing::error!(%other, "workspace storage operation failed");
                Self::internal()
            }
        }
    }
}

pub fn workspace_for(
    store: &Store,
    workspace_id: Uuid,
    user_id: Uuid,
    permission: Permission,
) -> Result<WorkspaceMetadata, ApiError> {
    let workspace = store.workspace(workspace_id)?;
    let memberships = store.memberships(workspace_id)?;
    if allows(&workspace, &memberships, user_id, permission) {
        Ok(workspace)
    } else {
        Err(ApiError::forbidden())
    }
}

pub fn owner_for(
    state: &AppState,
    workspace_id: Uuid,
    user_id: Uuid,
) -> Result<WorkspaceMetadata, ApiError> {
    let workspace = state.store.workspace(workspace_id)?;
    if workspace.owner_id == user_id {
        Ok(workspace)
    } else {
        Err(ApiError::forbidden())
    }
}
