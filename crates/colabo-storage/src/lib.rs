mod files;
mod git;

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use colabo_domain::{ChatMessage, Invitation, Membership, User, WorkspaceMetadata};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

pub use files::CollaborationFile;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored data is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("workspace does not exist")]
    WorkspaceMissing,
    #[error("user does not exist")]
    UserMissing,
    #[error("file does not exist")]
    FileMissing,
    #[error("file already exists")]
    AlreadyExists,
    #[error("destination already exists")]
    DestinationExists,
    #[error("path is invalid or escapes the workspace")]
    InvalidPath,
    #[error("path contains a symbolic link")]
    Symlink,
    #[error("stored collaboration data is invalid")]
    InvalidCollaboration,
    #[error("Git operation failed: {0}")]
    Git(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionRecord {
    pub user_id: Uuid,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct Store {
    root: Arc<PathBuf>,
    workspace_locks: Arc<Mutex<HashMap<Uuid, Arc<Mutex<()>>>>>,
    identity_lock: Arc<Mutex<()>>,
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        fs::create_dir_all(root.as_ref())?;
        let root = fs::canonicalize(root)?;
        for directory in ["users", "identities", "sessions", "workspaces"] {
            fs::create_dir_all(root.join(directory))?;
        }
        Ok(Self {
            root: Arc::new(root),
            workspace_locks: Arc::new(Mutex::new(HashMap::new())),
            identity_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn lock_workspace(&self, id: Uuid) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.workspace_locks.lock().await;
            locks
                .entry(id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    pub fn get_user(&self, id: Uuid) -> Result<User, StoreError> {
        read_json(&self.user_path(id)).map_err(|error| match error {
            StoreError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                StoreError::UserMissing
            }
            other => other,
        })
    }

    pub fn update_display_name(&self, id: Uuid, display_name: &str) -> Result<User, StoreError> {
        let mut user = self.get_user(id)?;
        user.display_name = clean_display_name(display_name, &user.subject);
        write_json_atomic(&self.user_path(id), &user)?;
        Ok(user)
    }

    pub async fn get_or_create_user(
        &self,
        issuer: &str,
        subject: &str,
        display_name: &str,
        preferred_username: Option<&str>,
        picture_url: Option<&str>,
    ) -> Result<User, StoreError> {
        let _guard = self.identity_lock.lock().await;
        let identity_path = self.identity_path(issuer, subject);
        if identity_path.exists() {
            let id: Uuid = read_json(&identity_path)?;
            let mut user = self.get_user(id)?;
            let updated_name = clean_display_name(display_name, &user.subject);
            let updated_username = preferred_username
                .filter(|username| !username.trim().is_empty())
                .map(str::to_owned);
            let updated_picture = picture_url
                .filter(|url| !url.trim().is_empty())
                .map(str::to_owned);
            let changed = user.display_name != updated_name
                || updated_username
                    .as_ref()
                    .is_some_and(|username| user.preferred_username.as_ref() != Some(username))
                || updated_picture
                    .as_ref()
                    .is_some_and(|picture| user.picture_url.as_ref() != Some(picture));
            if changed {
                user.display_name = updated_name;
                if updated_username.is_some() {
                    user.preferred_username = updated_username;
                }
                if updated_picture.is_some() {
                    user.picture_url = updated_picture;
                }
                write_json_atomic(&self.user_path(id), &user)?;
            }
            return Ok(user);
        }

        let user = User {
            id: Uuid::now_v7(),
            issuer: issuer.to_owned(),
            subject: subject.to_owned(),
            display_name: clean_display_name(display_name, subject),
            preferred_username: preferred_username
                .filter(|username| !username.trim().is_empty())
                .map(str::to_owned),
            picture_url: picture_url
                .filter(|url| !url.trim().is_empty())
                .map(str::to_owned),
            created_at: Utc::now(),
        };
        write_json_atomic(&self.user_path(user.id), &user)?;
        write_json_atomic(&identity_path, &user.id)?;
        Ok(user)
    }

    pub fn create_session(&self, token: &str, record: &SessionRecord) -> Result<(), StoreError> {
        write_json_atomic(&self.session_path(token), record)
    }

    pub fn get_session(&self, token: &str) -> Result<Option<SessionRecord>, StoreError> {
        match read_json::<SessionRecord>(&self.session_path(token)) {
            Ok(record) if record.expires_at > Utc::now() => Ok(Some(record)),
            Ok(_) => {
                let _ = fs::remove_file(self.session_path(token));
                Ok(None)
            }
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn delete_session(&self, token: &str) -> Result<(), StoreError> {
        match fs::remove_file(self.session_path(token)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn create_workspace(
        &self,
        owner_id: Uuid,
        name: String,
    ) -> Result<WorkspaceMetadata, StoreError> {
        let metadata = WorkspaceMetadata {
            id: Uuid::now_v7(),
            name,
            owner_id,
            created_at: Utc::now(),
        };
        let staging = self
            .workspace_root()
            .join(format!(".creating-{}", metadata.id));
        let final_path = self.workspace_path(metadata.id);
        fs::create_dir(&staging)?;
        let result = (|| {
            fs::create_dir(staging.join("files"))?;
            fs::create_dir(staging.join("collaboration"))?;
            fs::create_dir(staging.join("repo.git"))?;
            write_json_atomic(&staging.join("metadata.json"), &metadata)?;
            write_json_atomic::<[Membership]>(&staging.join("members.json"), &[])?;
            write_json_atomic::<[Invitation]>(&staging.join("invitations.json"), &[])?;
            File::create(staging.join("chat.jsonl"))?.sync_all()?;
            git::initialize(&staging.join("repo.git"))?;
            fs::rename(&staging, &final_path)?;
            Ok::<(), StoreError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result?;
        Ok(metadata)
    }

    pub fn workspace(&self, id: Uuid) -> Result<WorkspaceMetadata, StoreError> {
        self.ensure_workspace(id)?;
        read_json(&self.workspace_path(id).join("metadata.json"))
    }

    pub fn workspaces_for(&self, user_id: Uuid) -> Result<Vec<WorkspaceMetadata>, StoreError> {
        let mut result = Vec::new();
        for entry in fs::read_dir(self.workspace_root())? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let metadata_path = entry.path().join("metadata.json");
            let metadata: WorkspaceMetadata = match read_json(&metadata_path) {
                Ok(metadata) => metadata,
                Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if metadata.owner_id == user_id
                || self
                    .memberships(metadata.id)?
                    .iter()
                    .any(|member| member.user_id == user_id)
            {
                result.push(metadata);
            }
        }
        result.sort_by_key(|a| a.name.to_lowercase());
        Ok(result)
    }

    pub fn delete_workspace(&self, id: Uuid) -> Result<(), StoreError> {
        let path = self.workspace_path(id);
        self.ensure_workspace(id)?;
        let canonical = fs::canonicalize(&path)?;
        let parent = fs::canonicalize(self.workspace_root())?;
        if canonical.parent() != Some(parent.as_path()) || canonical.file_name().is_none() {
            return Err(StoreError::InvalidPath);
        }
        fs::remove_dir_all(canonical)?;
        Ok(())
    }

    pub fn memberships(&self, id: Uuid) -> Result<Vec<Membership>, StoreError> {
        self.ensure_workspace(id)?;
        read_json(&self.workspace_path(id).join("members.json"))
    }

    pub fn save_memberships(&self, id: Uuid, members: &[Membership]) -> Result<(), StoreError> {
        self.ensure_workspace(id)?;
        write_json_atomic(&self.workspace_path(id).join("members.json"), members)
    }

    pub fn invitations(&self, id: Uuid) -> Result<Vec<Invitation>, StoreError> {
        self.ensure_workspace(id)?;
        read_json(&self.workspace_path(id).join("invitations.json"))
    }

    pub fn save_invitations(&self, id: Uuid, invitations: &[Invitation]) -> Result<(), StoreError> {
        self.ensure_workspace(id)?;
        write_json_atomic(
            &self.workspace_path(id).join("invitations.json"),
            invitations,
        )
    }

    pub fn chat_messages(
        &self,
        id: Uuid,
        after: u64,
        limit: usize,
    ) -> Result<Vec<ChatMessage>, StoreError> {
        self.ensure_workspace(id)?;
        let file = File::open(self.workspace_path(id).join("chat.jsonl"))?;
        let mut messages = BufReader::new(file)
            .lines()
            .map(|line| {
                let line = line?;
                Ok(serde_json::from_str::<ChatMessage>(&line)?)
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        messages.retain(|message| message.sequence > after);
        let start = messages.len().saturating_sub(limit.min(200));
        Ok(messages.split_off(start))
    }

    pub fn append_chat_message(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
        body: String,
    ) -> Result<ChatMessage, StoreError> {
        self.ensure_workspace(workspace_id)?;
        let path = self.workspace_path(workspace_id).join("chat.jsonl");
        let last = self
            .chat_messages(workspace_id, 0, usize::MAX)?
            .last()
            .map(|m| m.sequence)
            .unwrap_or(0);
        let message = ChatMessage {
            id: Uuid::now_v7(),
            sequence: last.saturating_add(1),
            user_id,
            body,
            sent_at: Utc::now(),
        };
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        serde_json::to_writer(&mut file, &message)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(message)
    }

    pub fn checkpoint(
        &self,
        workspace_id: Uuid,
        message: &str,
        author_id: Uuid,
        author_name: &str,
    ) -> Result<Option<colabo_domain::CommitInfo>, StoreError> {
        self.ensure_workspace(workspace_id)?;
        git::checkpoint(
            &self.workspace_path(workspace_id).join("repo.git"),
            &self.files_root(workspace_id),
            message,
            author_id,
            author_name,
        )
    }

    pub fn commit_history(
        &self,
        workspace_id: Uuid,
        limit: usize,
    ) -> Result<Vec<colabo_domain::CommitInfo>, StoreError> {
        self.ensure_workspace(workspace_id)?;
        git::history(&self.workspace_path(workspace_id).join("repo.git"), limit)
    }

    pub fn read_checkpoint_file(
        &self,
        workspace_id: Uuid,
        commit_id: &str,
        path: &str,
    ) -> Result<Vec<u8>, StoreError> {
        self.ensure_workspace(workspace_id)?;
        let path = colabo_domain::validate_relative_path(path, false)
            .map_err(|_| StoreError::InvalidPath)?;
        git::read_file_at_commit(
            &self.workspace_path(workspace_id).join("repo.git"),
            commit_id,
            &path,
        )
    }

    pub fn checkpoint_files(
        &self,
        workspace_id: Uuid,
        commit_id: &str,
    ) -> Result<Vec<colabo_domain::FileEntry>, StoreError> {
        self.ensure_workspace(workspace_id)?;
        let mut files: Vec<_> = git::files_at_commit(
            &self.workspace_path(workspace_id).join("repo.git"),
            commit_id,
        )?
        .into_iter()
        .map(|(path, bytes)| colabo_domain::FileEntry {
            name: path.rsplit('/').next().unwrap_or(&path).to_owned(),
            path,
            directory: false,
            size: bytes.len() as u64,
        })
        .collect();
        files.sort_by_key(|a| a.path.to_lowercase());
        Ok(files)
    }

    pub fn checkpoint_changes(
        &self,
        workspace_id: Uuid,
        commit_id: &str,
    ) -> Result<Vec<colabo_domain::CommitFileChange>, StoreError> {
        self.ensure_workspace(workspace_id)?;
        git::changes(
            &self.workspace_path(workspace_id).join("repo.git"),
            commit_id,
        )
    }

    pub fn restore_checkpoint(
        &self,
        workspace_id: Uuid,
        commit_id: &str,
    ) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        self.ensure_workspace(workspace_id)?;
        let files = git::files_at_commit(
            &self.workspace_path(workspace_id).join("repo.git"),
            commit_id,
        )?;
        self.replace_file_tree(workspace_id, &files)?;
        Ok(files)
    }

    pub(crate) fn workspace_path(&self, id: Uuid) -> PathBuf {
        self.workspace_root().join(id.to_string())
    }

    pub(crate) fn files_root(&self, id: Uuid) -> PathBuf {
        self.workspace_path(id).join("files")
    }

    fn workspace_root(&self) -> PathBuf {
        self.root.join("workspaces")
    }

    fn user_path(&self, id: Uuid) -> PathBuf {
        self.root.join("users").join(format!("{id}.json"))
    }

    fn identity_path(&self, issuer: &str, subject: &str) -> PathBuf {
        let digest = Sha256::digest(format!("{issuer}\n{subject}").as_bytes());
        self.root
            .join("identities")
            .join(format!("{}.json", hex(&digest)))
    }

    fn session_path(&self, token: &str) -> PathBuf {
        let digest = Sha256::digest(token.as_bytes());
        self.root
            .join("sessions")
            .join(format!("{}.json", hex(&digest)))
    }

    fn ensure_workspace(&self, id: Uuid) -> Result<(), StoreError> {
        let path = self.workspace_path(id);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
            Ok(_) => Err(StoreError::InvalidPath),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::WorkspaceMissing)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn replace_file_tree(
        &self,
        workspace_id: Uuid,
        files: &[(String, Vec<u8>)],
    ) -> Result<(), StoreError> {
        let root = self.files_root(workspace_id);
        let canonical_root = fs::canonicalize(&root)?;
        if fs::symlink_metadata(&root)?.file_type().is_symlink() {
            return Err(StoreError::Symlink);
        }
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() && !entry.file_type()?.is_symlink() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
        for (relative, bytes) in files {
            let relative = colabo_domain::validate_relative_path(relative, false)
                .map_err(|_| StoreError::InvalidPath)?;
            let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
            let parent = path.parent().ok_or(StoreError::InvalidPath)?;
            fs::create_dir_all(parent)?;
            if !fs::canonicalize(parent)?.starts_with(&canonical_root) {
                return Err(StoreError::InvalidPath);
            }
            write_bytes_atomic(&path, bytes)?;
        }
        Ok(())
    }
}

fn clean_display_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        fallback.chars().take(48).collect()
    } else {
        cleaned.to_owned()
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, StoreError> {
    let file = File::open(path)?;
    Ok(serde_json::from_reader(file)?)
}

fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec(value)?;
    write_bytes_atomic(path, &bytes)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().ok_or(StoreError::InvalidPath)?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| StoreError::Io(error.error))?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    result
}
