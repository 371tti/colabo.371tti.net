use axum::{
    Extension, Json,
    extract::{Path, State},
};
use chrono::{Duration, Utc};
use colabo_domain::{Invitation, Membership, Permission, Role, User};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::error::{ApiError, owner_for, workspace_for};
use crate::{
    auth::random_secret,
    state::{AppState, Authenticated, WorkspaceEvent},
};

#[derive(Serialize)]
pub struct MemberView {
    user: User,
    role: &'static str,
    owner: bool,
}

pub async fn members(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<MemberView>>, ApiError> {
    let workspace = workspace_for(&state.store, workspace_id, auth.user.id, Permission::Read)?;
    let mut members = Vec::new();
    members.push(MemberView {
        user: state.store.get_user(workspace.owner_id)?,
        role: "owner",
        owner: true,
    });
    for membership in state.store.memberships(workspace_id)? {
        members.push(MemberView {
            user: state.store.get_user(membership.user_id)?,
            role: match membership.role {
                Role::Reader => "reader",
                Role::Writer => "writer",
            },
            owner: false,
        });
    }
    Ok(Json(members))
}

#[derive(Deserialize)]
pub struct UpdateMember {
    role: Role,
}

pub async fn update_member(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, user_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateMember>,
) -> Result<Json<MemberView>, ApiError> {
    owner_for(&state, workspace_id, auth.user.id)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    owner_for(&state, workspace_id, auth.user.id)?;
    let mut memberships = state.store.memberships(workspace_id)?;
    let member = memberships
        .iter_mut()
        .find(|member| member.user_id == user_id)
        .ok_or_else(ApiError::not_found)?;
    member.role = body.role;
    let updated = member.clone();
    state.store.save_memberships(workspace_id, &memberships)?;
    let user = state.store.get_user(user_id)?;
    let sender = state.workspace_event_sender(workspace_id).await;
    let _ = sender.send(WorkspaceEvent {
        kind: "members_changed".into(),
        path: None,
        destination: None,
    });
    state.deactivate_workspace(workspace_id).await?;
    state.close_workspace_streams(workspace_id).await;
    Ok(Json(MemberView {
        user,
        role: match updated.role {
            Role::Reader => "reader",
            Role::Writer => "writer",
        },
        owner: false,
    }))
}

#[derive(Serialize)]
pub struct MemberRemoved {
    removed: bool,
}

pub async fn remove_member(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MemberRemoved>, ApiError> {
    owner_for(&state, workspace_id, auth.user.id)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    owner_for(&state, workspace_id, auth.user.id)?;
    let mut memberships = state.store.memberships(workspace_id)?;
    let previous = memberships.len();
    memberships.retain(|member| member.user_id != user_id);
    if memberships.len() == previous {
        return Err(ApiError::not_found());
    }
    state.store.save_memberships(workspace_id, &memberships)?;
    let sender = state.workspace_event_sender(workspace_id).await;
    let _ = sender.send(WorkspaceEvent {
        kind: "members_changed".into(),
        path: None,
        destination: None,
    });
    state.deactivate_workspace(workspace_id).await?;
    state.close_workspace_streams(workspace_id).await;
    Ok(Json(MemberRemoved { removed: true }))
}

#[derive(Deserialize)]
pub struct NewInvitation {
    role: Role,
}

#[derive(Serialize)]
pub struct InvitationView {
    id: Uuid,
    role: Role,
    issued_by: Uuid,
    created_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
    revoked_at: Option<chrono::DateTime<Utc>>,
}

impl From<Invitation> for InvitationView {
    fn from(value: Invitation) -> Self {
        Self {
            id: value.id,
            role: value.role,
            issued_by: value.issued_by,
            created_at: value.created_at,
            expires_at: value.expires_at,
            revoked_at: value.revoked_at,
        }
    }
}

#[derive(Serialize)]
pub struct CreatedInvitation {
    invitation: InvitationView,
    path: String,
}

pub async fn list_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<InvitationView>>, ApiError> {
    owner_for(&state, workspace_id, auth.user.id)?;
    let invitations = state
        .store
        .invitations(workspace_id)?
        .into_iter()
        .map(InvitationView::from)
        .collect();
    Ok(Json(invitations))
}

pub async fn create_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<NewInvitation>,
) -> Result<Json<CreatedInvitation>, ApiError> {
    owner_for(&state, workspace_id, auth.user.id)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    owner_for(&state, workspace_id, auth.user.id)?;
    let mut invitations = state.store.invitations(workspace_id)?;
    if invitations.len() >= 1000 {
        return Err(ApiError::conflict("workspace invitation limit reached"));
    }
    let token = random_secret(32).map_err(|_| ApiError::internal())?;
    let invitation = Invitation {
        id: Uuid::now_v7(),
        token_hash: hash_token(&token),
        role: body.role,
        issued_by: auth.user.id,
        created_at: Utc::now(),
        expires_at: Utc::now() + Duration::days(7),
        revoked_at: None,
    };
    invitations.push(invitation.clone());
    state.store.save_invitations(workspace_id, &invitations)?;
    Ok(Json(CreatedInvitation {
        invitation: invitation.into(),
        path: format!("/invite/{workspace_id}/{token}"),
    }))
}

pub async fn revoke_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, invitation_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<InvitationView>, ApiError> {
    owner_for(&state, workspace_id, auth.user.id)?;
    let _guard = state.store.lock_workspace(workspace_id).await;
    let mut invitations = state.store.invitations(workspace_id)?;
    let invitation = invitations
        .iter_mut()
        .find(|invite| invite.id == invitation_id)
        .ok_or_else(ApiError::not_found)?;
    invitation.revoked_at = Some(Utc::now());
    let result = InvitationView::from(invitation.clone());
    state.store.save_invitations(workspace_id, &invitations)?;
    Ok(Json(result))
}

#[derive(Serialize)]
pub struct InvitationPreview {
    workspace_id: Uuid,
    workspace_name: String,
    role: Role,
    expires_at: chrono::DateTime<Utc>,
}

pub async fn preview_invitation(
    State(state): State<AppState>,
    Path((workspace_id, token)): Path<(Uuid, String)>,
) -> Result<Json<InvitationPreview>, ApiError> {
    let invitation = find_invitation(&state, workspace_id, &token)?;
    let workspace = state.store.workspace(workspace_id)?;
    Ok(Json(InvitationPreview {
        workspace_id,
        workspace_name: workspace.name,
        role: invitation.role,
        expires_at: invitation.expires_at,
    }))
}

#[derive(Serialize)]
pub struct InvitationAccepted {
    joined: bool,
    role: Role,
}

pub async fn accept_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Path((workspace_id, token)): Path<(Uuid, String)>,
) -> Result<Json<InvitationAccepted>, ApiError> {
    let _guard = state.store.lock_workspace(workspace_id).await;
    let invitation = find_invitation(&state, workspace_id, &token)?;
    let workspace = state.store.workspace(workspace_id)?;
    if workspace.owner_id == auth.user.id {
        return Err(ApiError::conflict(
            "workspace owner cannot join through an invitation",
        ));
    }
    let mut members = state.store.memberships(workspace_id)?;
    if members.iter().any(|member| member.user_id == auth.user.id) {
        return Ok(Json(InvitationAccepted {
            joined: false,
            role: invitation.role,
        }));
    }
    members.push(Membership {
        user_id: auth.user.id,
        role: invitation.role,
        joined_at: Utc::now(),
    });
    state.store.save_memberships(workspace_id, &members)?;
    let sender = state.workspace_event_sender(workspace_id).await;
    let _ = sender.send(WorkspaceEvent {
        kind: "members_changed".into(),
        path: None,
        destination: None,
    });
    Ok(Json(InvitationAccepted {
        joined: true,
        role: invitation.role,
    }))
}

fn find_invitation(
    state: &AppState,
    workspace_id: Uuid,
    token: &str,
) -> Result<Invitation, ApiError> {
    if token.len() < 32 || token.len() > 128 {
        return Err(ApiError::not_found());
    }
    let token_hash = hash_token(token);
    state
        .store
        .invitations(workspace_id)?
        .into_iter()
        .find(|invite| {
            invite.token_hash == token_hash
                && invite.revoked_at.is_none()
                && invite.expires_at > Utc::now()
        })
        .ok_or_else(ApiError::not_found)
}

fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
