mod api;
mod auth;
mod auth_handlers;
mod config;
mod middleware;
mod state;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::get,
};
use colabo_collaboration::{Server as CollaborationServer, ServerConfig};
use colabo_storage::Store;
use tokio::net::TcpListener;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

use crate::{auth::AuthProvider, config::Settings, state::AppState};

const MAX_COLLAB_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let settings = Settings::from_env().map_err(std::io::Error::other)?;
    let store = Store::open(&settings.data_dir)?;
    let auth = AuthProvider::new(&settings)
        .await
        .map_err(std::io::Error::other)?;
    if !auth.configured() {
        tracing::warn!(
            "OIDC_CLIENT_ID and OIDC_CLIENT_SECRET are empty; web sign-in is unavailable and APIs remain closed"
        );
    }
    let mut collaboration_config = ServerConfig::default();
    collaboration_config.security.allowed_origins = vec![settings.public_origin.clone()];
    collaboration_config.limits.max_upload_bytes = MAX_COLLAB_UPLOAD_BYTES;
    let collaboration = CollaborationServer::with_config(collaboration_config)?;
    let address = settings.bind;
    let state = AppState::new(settings, store, collaboration, auth);
    let app = app(state.clone());
    let listener = TcpListener::bind(address).await?;
    tracing::info!(%address, "colabo workspace server is listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn app(state: AppState) -> Router {
    let static_dir = state.settings.static_dir.clone();
    Router::new()
        .route("/auth/login", get(auth_handlers::login))
        .route("/auth/status", get(auth_handlers::status))
        .route("/auth/callback", get(auth_handlers::callback))
        .nest_service("/assets", ServeDir::new(static_dir.join("assets")))
        .nest_service("/vendor", ServeDir::new(static_dir.join("vendor")))
        .nest_service("/static", ServeDir::new(static_dir.join("static")))
        .route("/", get(index))
        .route("/invite/{workspace_id}/{token}", get(index))
        .merge(api::router(state.clone()))
        .fallback(get(index))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<Body>| {
                let path = match request.uri().path() {
                    path if path.starts_with("/invite/") => "/invite/:workspace_id/:token",
                    path if path.starts_with("/api/invites/") => {
                        "/api/invites/:workspace_id/:token"
                    }
                    path => path,
                };
                tracing::info_span!(
                    "http.request",
                    method = %request.method(),
                    path,
                )
            }),
        )
        .layer(from_fn_with_state(
            state.clone(),
            middleware::security_headers,
        ))
        .with_state(state)
}

async fn index(State(state): State<AppState>) -> Response {
    let index_path = state.settings.static_dir.join("index.html");
    match tokio::fs::read(index_path).await {
        Ok(html) => {
            let mut response = Response::new(Body::from(html));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                "text/html; charset=utf-8"
                    .parse()
                    .expect("valid content type"),
            );
            response
        }
        Err(error) => {
            tracing::error!(%error, "web build is missing");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Web interface is not built. Run the documented web build first.",
            )
                .into_response()
        }
    }
}
