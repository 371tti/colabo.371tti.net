use std::{env, net::SocketAddr, path::PathBuf};

use url::Url;

#[derive(Clone)]
pub struct Settings {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub static_dir: PathBuf,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub public_origin: String,
    pub secure_cookie: bool,
}

impl Settings {
    pub fn from_env() -> Result<Self, String> {
        let bind = env::var("COLABO_BIND")
            .unwrap_or_else(|_| "127.0.0.1:85".into())
            .parse::<SocketAddr>()
            .map_err(|error| format!("invalid COLABO_BIND: {error}"))?;
        let data_dir =
            PathBuf::from(env::var("COLABO_DATA_DIR").unwrap_or_else(|_| "./data".into()));
        let static_dir = PathBuf::from(
            env::var("COLABO_STATIC_DIR")
                .unwrap_or_else(|_| "./crates/colabo-server/static/build".into()),
        );
        let public_url = Url::parse(
            &env::var("COLABO_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:85".into()),
        )
        .map_err(|error| format!("invalid COLABO_PUBLIC_URL: {error}"))?;
        if !matches!(public_url.scheme(), "http" | "https")
            || public_url.host_str().is_none()
            || !public_url.username().is_empty()
            || public_url.password().is_some()
            || public_url.path() != "/"
            || public_url.query().is_some()
            || public_url.fragment().is_some()
        {
            return Err(
                "COLABO_PUBLIC_URL must be an origin with no path, query, or credentials".into(),
            );
        }
        let redirect_uri = env::var("OIDC_REDIRECT_URI").unwrap_or_else(|_| {
            public_url
                .join("auth/callback")
                .expect("valid public URL")
                .to_string()
        });
        let redirect = Url::parse(&redirect_uri)
            .map_err(|error| format!("invalid OIDC_REDIRECT_URI: {error}"))?;
        if !matches!(redirect.scheme(), "http" | "https")
            || redirect.host_str().is_none()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
            || redirect.origin() != public_url.origin()
            || redirect.path() != "/auth/callback"
            || redirect.query().is_some()
            || redirect.fragment().is_some()
        {
            return Err("OIDC_REDIRECT_URI must be the same-origin /auth/callback URL".into());
        }
        let explicitly_secure = env::var("SESSION_COOKIE_SECURE")
            .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        let secure_cookie = explicitly_secure || public_url.scheme() == "https";
        Ok(Self {
            bind,
            data_dir,
            static_dir,
            issuer: env::var("OIDC_ISSUER").unwrap_or_else(|_| "https://auth.371tti.net".into()),
            client_id: env::var("OIDC_CLIENT_ID").unwrap_or_default(),
            client_secret: env::var("OIDC_CLIENT_SECRET").unwrap_or_default(),
            redirect_uri,
            public_origin: public_url.origin().ascii_serialization(),
            secure_cookie,
        })
    }
}
