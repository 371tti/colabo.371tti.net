use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Redirect, Response},
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::{
    auth::random_secret,
    state::{AppState, Authenticated},
};

#[derive(Deserialize)]
pub struct LoginQuery {
    #[serde(rename = "returnTo")]
    return_to: Option<String>,
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn login(State(state): State<AppState>, Query(query): Query<LoginQuery>) -> Response {
    match state.auth.begin_login(query.return_to.as_deref()).await {
        Ok((url, state_token)) => {
            let mut response = Redirect::to(&url).into_response();
            if let Ok(value) = state.oidc_state_cookie(&state_token).parse() {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        Err(error) => {
            tracing::warn!(%error, "OIDC login could not start");
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"OIDC login is not configured"})),
            )
                .into_response()
        }
    }
}

pub async fn status(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "configured": state.auth.configured() }))
}

pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if query.error.is_some() {
        let mut response = Redirect::to("/?login=cancelled").into_response();
        clear_state_cookie(&state, &mut response);
        return response;
    }
    let cookie_state = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| cookie_value(value, "colabo_oidc_state"));
    let (Some(state_token), Some(cookie_state), Some(code)) =
        (query.state.as_deref(), cookie_state, query.code.as_deref())
    else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error":"OIDC callback is incomplete"})),
        )
            .into_response();
    };
    let identity = match state
        .auth
        .finish_login(state_token, cookie_state, code)
        .await
    {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(%error, "OIDC callback was rejected");
            let mut response = (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({"error":"sign in could not be completed"})),
            )
                .into_response();
            clear_state_cookie(&state, &mut response);
            return response;
        }
    };
    let user = match state
        .store
        .get_or_create_user(
            &identity.issuer,
            &identity.subject,
            &identity.display_name,
            identity.preferred_username.as_deref(),
            identity.picture_url.as_deref(),
        )
        .await
    {
        Ok(user) => user,
        Err(error) => {
            tracing::error!(%error, "could not store authenticated user");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"account storage failed"})),
            )
                .into_response();
        }
    };
    let token = match random_secret(32) {
        Ok(token) => token,
        Err(error) => {
            tracing::error!(%error, "could not generate session token");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"session could not be created"})),
            )
                .into_response();
        }
    };
    let csrf_token = match random_secret(32) {
        Ok(token) => token,
        Err(error) => {
            tracing::error!(%error, "could not generate CSRF token");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"session could not be created"})),
            )
                .into_response();
        }
    };
    let record = colabo_storage::SessionRecord {
        user_id: user.id,
        csrf_token,
        expires_at: Utc::now() + Duration::days(7),
    };
    if let Err(error) = state.store.create_session(&token, &record) {
        tracing::error!(%error, "could not persist application session");
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"session could not be created"})),
        )
            .into_response();
    }
    let mut response = Redirect::to(&identity.return_to).into_response();
    if let Ok(value) = state.session_cookie(&token, 7 * 24 * 60 * 60).parse() {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    clear_state_cookie(&state, &mut response);
    response
}

pub async fn logout(
    State(state): State<AppState>,
    axum::Extension(auth): axum::Extension<Authenticated>,
) -> Response {
    if let Err(error) = state.store.delete_session(&auth.token) {
        tracing::error!(%error, "could not revoke application session");
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"logout failed"})),
        )
            .into_response();
    }
    let mut response = axum::http::StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = state.clear_session_cookie().parse() {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

fn clear_state_cookie(state: &AppState, response: &mut Response) {
    if let Ok(value) = state.clear_oidc_state_cookie().parse() {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}
