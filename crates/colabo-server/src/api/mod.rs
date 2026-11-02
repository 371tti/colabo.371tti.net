mod account;
mod chat;
mod error;
mod files;
mod history;
mod sharing;
mod workspaces;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware::from_fn_with_state,
    routing::{delete, get, patch, post},
};
use tower_http::limit::RequestBodyLimitLayer;

use crate::{middleware::api_auth, state::AppState};

pub fn router(state: AppState) -> Router<AppState> {
    let api = Router::new()
        .route("/api/session", get(account::session))
        .route("/api/account", patch(account::update_account))
        .route("/api/logout", post(crate::auth_handlers::logout))
        .route(
            "/api/workspaces",
            get(workspaces::list).post(workspaces::create),
        )
        .route(
            "/api/workspaces/{workspace_id}",
            get(workspaces::get).delete(workspaces::delete),
        )
        .route(
            "/api/workspaces/{workspace_id}/members",
            get(sharing::members),
        )
        .route(
            "/api/workspaces/{workspace_id}/members/{user_id}",
            patch(sharing::update_member).delete(sharing::remove_member),
        )
        .route(
            "/api/workspaces/{workspace_id}/invitations",
            get(sharing::list_invitations).post(sharing::create_invitation),
        )
        .route(
            "/api/workspaces/{workspace_id}/invitations/{invitation_id}",
            delete(sharing::revoke_invitation),
        )
        .route(
            "/api/invites/{workspace_id}/{token}",
            get(sharing::preview_invitation),
        )
        .route(
            "/api/invites/{workspace_id}/{token}/accept",
            post(sharing::accept_invitation),
        )
        .route(
            "/api/workspaces/{workspace_id}/files",
            get(files::list_directory),
        )
        .route(
            "/api/workspaces/{workspace_id}/file",
            get(files::read_file)
                .put(files::write_file)
                .delete(files::delete_path),
        )
        .route(
            "/api/workspaces/{workspace_id}/download",
            get(files::download_file),
        )
        .route(
            "/api/workspaces/{workspace_id}/directory",
            post(files::create_directory),
        )
        .route(
            "/api/workspaces/{workspace_id}/move",
            post(files::move_path),
        )
        .route(
            "/api/workspaces/{workspace_id}/collaboration",
            get(files::collaboration_info),
        )
        .route(
            "/api/workspaces/{workspace_id}/collaboration/{file_id}/ws",
            get(files::collaboration_socket),
        )
        .route(
            "/api/workspaces/{workspace_id}/events/ws",
            get(files::workspace_events_socket),
        )
        .route(
            "/api/workspaces/{workspace_id}/chat",
            get(chat::history).post(chat::send),
        )
        .route("/api/workspaces/{workspace_id}/chat/ws", get(chat::socket))
        .route(
            "/api/workspaces/{workspace_id}/checkpoints",
            get(history::list).post(history::create),
        )
        .route(
            "/api/workspaces/{workspace_id}/checkpoints/{commit_id}/changes",
            get(history::changes),
        )
        .route(
            "/api/workspaces/{workspace_id}/checkpoints/{commit_id}/files",
            get(history::files),
        )
        .route(
            "/api/workspaces/{workspace_id}/checkpoints/{commit_id}/file",
            get(history::read_file),
        )
        .route(
            "/api/workspaces/{workspace_id}/checkpoints/{commit_id}/restore",
            post(history::restore),
        )
        .route_layer(from_fn_with_state(state.clone(), api_auth))
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(RequestBodyLimitLayer::new(25 * 1024 * 1024));

    let upload = Router::new()
        .route("/api/workspaces/{workspace_id}/upload", post(files::upload))
        .route_layer(from_fn_with_state(state, api_auth))
        .layer(DefaultBodyLimit::max(files::MAX_UPLOAD_REQUEST_BYTES))
        .layer(RequestBodyLimitLayer::new(files::MAX_UPLOAD_REQUEST_BYTES));

    api.merge(upload)
}
