use axum::{
    body::Body,
    extract::State,
    http::{Method, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::state::{AppState, Authenticated};

pub async fn api_auth(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if request.method() == Method::GET && path.starts_with("/api/invites/") {
        return next.run(request).await;
    }

    if let Some(origin) = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        && origin != state.settings.public_origin
    {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({"error":"origin is not allowed"})),
        )
            .into_response();
    }

    let Some(token) = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookie| cookie_value(cookie, state.session_cookie_name()).map(str::to_owned))
    else {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error":"sign in required"})),
        )
            .into_response();
    };
    let session = match state.store.get_session(&token) {
        Ok(Some(session)) => session,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error":"session expired"})),
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!(%error, "could not load application session");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error":"session storage failed"})),
            )
                .into_response();
        }
    };
    let user = match state.store.get_user(session.user_id) {
        Ok(user) => user,
        Err(error) => {
            tracing::error!(%error, "could not load session user");
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error":"session user is unavailable"})),
            )
                .into_response();
        }
    };

    if matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        let csrf = request
            .headers()
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !constant_time_equal(csrf.as_bytes(), session.csrf_token.as_bytes()) {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(json!({"error":"CSRF validation failed"})),
            )
                .into_response();
        }
    }

    request.extensions_mut().insert(Authenticated {
        user,
        session,
        token,
    });
    next.run(request).await
}

pub async fn security_headers(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let websocket_origin = state
        .settings
        .public_origin
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let policy = format!(
        "default-src 'self'; script-src 'self' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' data:; connect-src 'self' {websocket_origin} https://cdn.jsdelivr.net https://packages.typst.org; worker-src 'self' blob:; frame-src 'self' blob:; frame-ancestors 'self'; object-src 'none'; base-uri 'self'; form-action 'self'"
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        policy.parse().expect("valid CSP header"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().expect("valid header"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        "same-origin".parse().expect("valid header"),
    );
    headers.insert(
        "permissions-policy",
        "camera=(), microphone=(), geolocation=()"
            .parse()
            .expect("valid header"),
    );
    response
}

fn cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    cookie_header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
