use std::{
    fs,
    net::{AddrParseError, Ipv4Addr, SocketAddr, SocketAddrV4},
    path::Path,
};

use axum::http::Uri;
use collab_core::DEFAULT_MAX_UNDO_TRANSACTIONS_PER_ACTOR;
use serde::Deserialize;
use thiserror::Error;

const MIB: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: ListenConfig,
    pub limits: ResourceLimits,
    pub security: SecurityConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListenConfig {
    pub ipv4: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimits {
    pub max_sessions: usize,
    pub max_session_memory_bytes: usize,
    pub max_total_session_memory_bytes: usize,
    pub max_undo_transactions_per_actor: usize,
    pub session_ttl_seconds: u64,
    pub max_session_data_bytes: usize,
    pub max_upload_bytes: usize,
    pub max_concurrent_uploads: usize,
    pub max_websocket_message_bytes: usize,
    pub max_connections_per_session: usize,
    pub max_total_connections: usize,
    pub broadcast_capacity: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read config file: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid YAML config: {0}")]
    Yaml(#[from] yaml_serde::Error),
    #[error("invalid listen IPv4 address or socket address: {0}")]
    ListenAddress(#[from] AddrParseError),
    #[error("invalid config: {0}")]
    Invalid(String),
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            ipv4: "0.0.0.0".into(),
            port: 85,
        }
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_sessions: 100,
            max_session_memory_bytes: 64 * MIB,
            max_total_session_memory_bytes: 512 * MIB,
            max_undo_transactions_per_actor: DEFAULT_MAX_UNDO_TRANSACTIONS_PER_ACTOR,
            session_ttl_seconds: 60 * 60,
            max_session_data_bytes: 8 * 1024,
            max_upload_bytes: 10 * MIB,
            max_concurrent_uploads: 8,
            max_websocket_message_bytes: 256 * 1024,
            max_connections_per_session: 64,
            max_total_connections: 256,
            broadcast_capacity: 32,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            allowed_origins: vec!["http://127.0.0.1:85".into(), "http://localhost:85".into()],
        }
    }
}

impl ServerConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let source = fs::read_to_string(path)?;
        let config: Self = yaml_serde::from_str(&source)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.socket_addr()?;

        let limits = &self.limits;
        for (name, value) in [
            ("limits.max_sessions", limits.max_sessions),
            (
                "limits.max_session_memory_bytes",
                limits.max_session_memory_bytes,
            ),
            (
                "limits.max_total_session_memory_bytes",
                limits.max_total_session_memory_bytes,
            ),
            (
                "limits.max_undo_transactions_per_actor",
                limits.max_undo_transactions_per_actor,
            ),
            (
                "limits.max_session_data_bytes",
                limits.max_session_data_bytes,
            ),
            ("limits.max_upload_bytes", limits.max_upload_bytes),
            (
                "limits.max_concurrent_uploads",
                limits.max_concurrent_uploads,
            ),
            (
                "limits.max_websocket_message_bytes",
                limits.max_websocket_message_bytes,
            ),
            (
                "limits.max_connections_per_session",
                limits.max_connections_per_session,
            ),
            ("limits.max_total_connections", limits.max_total_connections),
            ("limits.broadcast_capacity", limits.broadcast_capacity),
        ] {
            if value == 0 {
                return Err(ConfigError::Invalid(format!(
                    "{name} must be greater than 0"
                )));
            }
        }

        if limits.max_total_session_memory_bytes < ROOM_BASE_MEMORY_BYTES {
            return Err(ConfigError::Invalid(format!(
                "limits.max_total_session_memory_bytes must be at least {ROOM_BASE_MEMORY_BYTES}"
            )));
        }
        if limits.max_session_memory_bytes < ROOM_BASE_MEMORY_BYTES {
            return Err(ConfigError::Invalid(format!(
                "limits.max_session_memory_bytes must be at least {ROOM_BASE_MEMORY_BYTES}"
            )));
        }
        if limits.max_upload_bytes > limits.max_session_memory_bytes {
            return Err(ConfigError::Invalid(
                "limits.max_upload_bytes cannot exceed limits.max_session_memory_bytes".into(),
            ));
        }
        if limits.max_session_data_bytes > limits.max_websocket_message_bytes {
            return Err(ConfigError::Invalid(
                "limits.max_session_data_bytes cannot exceed limits.max_websocket_message_bytes"
                    .into(),
            ));
        }
        if limits.session_ttl_seconds == 0 || limits.session_ttl_seconds > 31_536_000 {
            return Err(ConfigError::Invalid(
                "limits.session_ttl_seconds must be between 1 and 31536000".into(),
            ));
        }
        if limits.max_websocket_message_bytes > limits.max_session_memory_bytes {
            return Err(ConfigError::Invalid(
                "limits.max_websocket_message_bytes cannot exceed limits.max_session_memory_bytes"
                    .into(),
            ));
        }
        if limits.broadcast_capacity > 65_536 {
            return Err(ConfigError::Invalid(
                "limits.broadcast_capacity cannot exceed 65536".into(),
            ));
        }
        if limits.max_concurrent_uploads > tokio::sync::Semaphore::MAX_PERMITS {
            return Err(ConfigError::Invalid(format!(
                "limits.max_concurrent_uploads must not exceed {}",
                tokio::sync::Semaphore::MAX_PERMITS
            )));
        }

        for origin in &self.security.allowed_origins {
            validate_origin(origin)?;
        }
        Ok(())
    }

    pub fn socket_addr(&self) -> Result<SocketAddr, ConfigError> {
        let ipv4 = self.listen.ipv4.parse::<Ipv4Addr>()?;
        if self.listen.port == 0 {
            return Err(ConfigError::Invalid(
                "listen.port must be between 1 and 65535".into(),
            ));
        }
        Ok(SocketAddr::V4(SocketAddrV4::new(ipv4, self.listen.port)))
    }
}

pub(crate) const ROOM_BASE_MEMORY_BYTES: usize = 8 * 1024;

fn validate_origin(origin: &str) -> Result<(), ConfigError> {
    let Some((scheme, authority_text)) = origin.split_once("://") else {
        return Err(ConfigError::Invalid(format!(
            "security.allowed_origins entries must be origins only (for example http://localhost:85): {origin:?}"
        )));
    };
    let uri = origin.parse::<Uri>().map_err(|error| {
        ConfigError::Invalid(format!(
            "invalid security.allowed_origins value {origin:?}: {error}"
        ))
    })?;
    let valid_scheme = matches!(scheme, "http" | "https");
    let valid_authority = uri
        .authority()
        .is_some_and(|authority| !authority.as_str().contains('@'));
    let authority_has_suffix = authority_text
        .chars()
        .any(|character| matches!(character, '/' | '?' | '#'));
    if !valid_scheme || !valid_authority || authority_text.is_empty() || authority_has_suffix {
        return Err(ConfigError::Invalid(format!(
            "security.allowed_origins entries must be origins only (for example http://localhost:85): {origin:?}"
        )));
    }
    Ok(())
}
