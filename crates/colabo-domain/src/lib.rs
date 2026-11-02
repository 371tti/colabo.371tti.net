use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct User {
    pub id: Uuid,
    pub issuer: String,
    pub subject: String,
    pub display_name: String,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub picture_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkspaceMetadata {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Reader,
    Writer,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Membership {
    pub user_id: Uuid,
    pub role: Role,
    pub joined_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    Read,
    Write,
    Manage,
}

pub fn allows(
    workspace: &WorkspaceMetadata,
    memberships: &[Membership],
    user_id: Uuid,
    permission: Permission,
) -> bool {
    if workspace.owner_id == user_id {
        return true;
    }
    let Some(member) = memberships.iter().find(|member| member.user_id == user_id) else {
        return false;
    };
    match permission {
        Permission::Read => true,
        Permission::Write => member.role == Role::Writer,
        Permission::Manage => false,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Invitation {
    pub id: Uuid,
    pub token_hash: String,
    pub role: Role,
    pub issued_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatMessage {
    pub id: Uuid,
    pub sequence: u64,
    pub user_id: Uuid,
    pub body: String,
    pub sent_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub directory: bool,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommitInfo {
    pub id: String,
    pub message: String,
    pub author: String,
    pub committed_at: DateTime<Utc>,
    pub parent_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommitFileChange {
    pub path: String,
    pub kind: CommitChangeKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PathError {
    #[error("path must be workspace-relative")]
    NotRelative,
    #[error("path contains an invalid component")]
    InvalidComponent,
    #[error("path is too long")]
    TooLong,
}

pub fn validate_workspace_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err("workspace name must contain 1 to 80 printable characters");
    }
    Ok(name.to_owned())
}

pub fn validate_relative_path(path: &str, allow_root: bool) -> Result<String, PathError> {
    if path.is_empty() {
        return if allow_root {
            Ok(String::new())
        } else {
            Err(PathError::NotRelative)
        };
    }
    if path.len() > 1024 {
        return Err(PathError::TooLong);
    }
    if path.starts_with('/') || path.starts_with('\\') || path.contains('\\') {
        return Err(PathError::NotRelative);
    }

    let mut components = Vec::new();
    for component in path.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > 240
            || component.ends_with('.')
            || component.ends_with(' ')
            || component
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
            || is_reserved_windows_name(component)
        {
            return Err(PathError::InvalidComponent);
        }
        components.push(component);
    }
    Ok(components.join("/"))
}

fn is_reserved_windows_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let uppercase = stem.to_ascii_uppercase();
    matches!(uppercase.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            uppercase
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.len() == 1 && "123456789".contains(suffix))
        })
}
