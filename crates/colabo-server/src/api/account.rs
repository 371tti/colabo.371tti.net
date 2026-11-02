use axum::{Extension, Json, extract::State};
use colabo_domain::User;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use crate::state::{AppState, Authenticated};

#[derive(Serialize)]
pub struct SessionView {
    user: User,
    csrf_token: String,
    oidc_configured: bool,
}

pub async fn session(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
) -> Json<SessionView> {
    Json(SessionView {
        user: auth.user,
        csrf_token: auth.session.csrf_token,
        oidc_configured: state.auth.configured(),
    })
}

#[derive(Deserialize)]
pub struct UpdateAccount {
    display_name: String,
}

pub async fn update_account(
    State(state): State<AppState>,
    Extension(auth): Extension<Authenticated>,
    Json(body): Json<UpdateAccount>,
) -> Result<Json<User>, ApiError> {
    let name = body.display_name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "display name must contain 1 to 80 printable characters",
        ));
    }
    let user = state.store.update_display_name(auth.user.id, name)?;
    Ok(Json(user))
}
