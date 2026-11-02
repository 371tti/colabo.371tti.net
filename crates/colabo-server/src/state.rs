use std::{collections::HashMap, sync::Arc};

use colabo_collaboration::Server as CollaborationServer;
use colabo_domain::{ChatMessage, User};
use colabo_storage::{CollaborationFile, SessionRecord, Store, StoreError};
use collab_core::{DocumentId, Snapshot};
use serde::Serialize;
use tokio::{
    sync::{Mutex, broadcast},
    task::JoinHandle,
};
use tracing::warn;
use uuid::Uuid;

use crate::{auth::AuthProvider, config::Settings};

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub store: Store,
    pub collaboration: CollaborationServer,
    pub auth: Arc<AuthProvider>,
    pub(crate) active_documents: Arc<Mutex<HashMap<Uuid, ActiveDocument>>>,
    chat_rooms: Arc<Mutex<HashMap<Uuid, broadcast::Sender<ChatMessage>>>>,
    workspace_events: Arc<Mutex<HashMap<Uuid, broadcast::Sender<WorkspaceEvent>>>>,
}

pub(crate) struct ActiveDocument {
    workspace_id: Uuid,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct Authenticated {
    pub user: User,
    pub session: SessionRecord,
    pub token: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceEvent {
    pub kind: String,
    pub path: Option<String>,
    pub destination: Option<String>,
}

impl AppState {
    pub fn new(
        settings: Settings,
        store: Store,
        collaboration: CollaborationServer,
        auth: AuthProvider,
    ) -> Self {
        Self {
            settings,
            store,
            collaboration,
            auth: Arc::new(auth),
            active_documents: Arc::new(Mutex::new(HashMap::new())),
            chat_rooms: Arc::new(Mutex::new(HashMap::new())),
            workspace_events: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn ensure_document(
        &self,
        workspace_id: Uuid,
        file: CollaborationFile,
    ) -> Result<DocumentId, StoreError> {
        let mut active = self.active_documents.lock().await;
        let document_id =
            DocumentId::new(file.id.to_string()).map_err(|_| StoreError::InvalidCollaboration)?;
        if active.contains_key(&file.id) {
            if self.collaboration.has_session(&document_id).await {
                return Ok(document_id);
            }
            if let Some(expired) = active.remove(&file.id) {
                expired.task.abort();
            }
        }

        let snapshot = self
            .store
            .load_collaboration_snapshot(workspace_id, file.id)?;
        if let Some(snapshot) = snapshot {
            let text = snapshot.document.text();
            self.collaboration
                .restore_session(document_id.clone(), snapshot.clone())
                .await
                .map_err(|_| StoreError::InvalidCollaboration)?;
            if let Err(error) =
                self.store
                    .save_collaboration_state(workspace_id, file.id, &snapshot, &text)
            {
                self.collaboration.remove_session(&document_id).await;
                return Err(error);
            }
        } else {
            let bytes = self.store.read_file(workspace_id, &file.path)?;
            let text = String::from_utf8(bytes).map_err(|_| StoreError::InvalidCollaboration)?;
            self.collaboration
                .create_session_with_id(document_id.clone(), &text)
                .await
                .map_err(|_| StoreError::InvalidCollaboration)?;
            let snapshot = self
                .collaboration
                .snapshot(&document_id)
                .await
                .map_err(|_| StoreError::InvalidCollaboration)?;
            if let Err(error) =
                self.store
                    .save_collaboration_state(workspace_id, file.id, &snapshot, &text)
            {
                self.collaboration.remove_session(&document_id).await;
                return Err(error);
            }
        }

        let persister_store = self.store.clone();
        if self
            .collaboration
            .set_snapshot_persister(&document_id, move |snapshot| {
                let store = persister_store.clone();
                async move {
                    let result = tokio::task::spawn_blocking(move || {
                        store.save_collaboration_snapshot(workspace_id, file.id, &snapshot)
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                    result.map_err(|error| error.to_string())
                }
            })
            .await
            .is_err()
        {
            self.collaboration.remove_session(&document_id).await;
            return Err(StoreError::InvalidCollaboration);
        }

        let guard_store = self.store.clone();
        if self
            .collaboration
            .set_submission_guard(&document_id, move || {
                let store = guard_store.clone();
                async move { store.lock_workspace(workspace_id).await }
            })
            .await
            .is_err()
        {
            self.collaboration.remove_session(&document_id).await;
            return Err(StoreError::InvalidCollaboration);
        }

        let mut events = match self.collaboration.subscribe(&document_id).await {
            Ok(events) => events,
            Err(_) => {
                self.collaboration.remove_session(&document_id).await;
                return Err(StoreError::InvalidCollaboration);
            }
        };
        let collab = self.collaboration.clone();
        let store = self.store.clone();
        let listener_document_id = document_id.clone();
        let task = tokio::spawn(async move {
            while let Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) = events.recv().await {
                let _workspace_guard = store.lock_workspace(workspace_id).await;
                match collab.snapshot(&listener_document_id).await {
                    Ok(snapshot) => {
                        let text = snapshot.document.text();
                        if let Err(error) =
                            store.save_collaboration_state(workspace_id, file.id, &snapshot, &text)
                        {
                            warn!(%workspace_id, file_id = %file.id, %error, "could not persist collaborative document");
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        active.insert(file.id, ActiveDocument { workspace_id, task });
        Ok(document_id)
    }

    pub async fn deactivate_document(&self, file_id: Uuid) {
        let mut active_documents = self.active_documents.lock().await;
        if let Some(active) = active_documents.remove(&file_id) {
            if let Ok(document_id) = DocumentId::new(file_id.to_string()) {
                self.collaboration.remove_session(&document_id).await;
            }
            active.task.abort();
        }
    }

    pub async fn deactivate_workspace(&self, workspace_id: Uuid) -> Result<(), StoreError> {
        let ids: Vec<Uuid> = self
            .active_documents
            .lock()
            .await
            .iter()
            .filter_map(|(file_id, active)| {
                (active.workspace_id == workspace_id).then_some(*file_id)
            })
            .collect();
        for file_id in ids {
            self.deactivate_document(file_id).await;
        }
        Ok(())
    }

    pub async fn flush_documents(&self, workspace_id: Uuid) -> Result<(), StoreError> {
        let ids: Vec<Uuid> = self
            .active_documents
            .lock()
            .await
            .iter()
            .filter_map(|(file_id, active)| {
                (active.workspace_id == workspace_id).then_some(*file_id)
            })
            .collect();
        for file_id in ids {
            let document_id = DocumentId::new(file_id.to_string())
                .map_err(|_| StoreError::InvalidCollaboration)?;
            let snapshot: Snapshot = if self.collaboration.has_session(&document_id).await {
                self.collaboration
                    .snapshot(&document_id)
                    .await
                    .map_err(|_| StoreError::InvalidCollaboration)?
            } else {
                self.deactivate_document(file_id).await;
                let Some(snapshot) = self
                    .store
                    .load_collaboration_snapshot(workspace_id, file_id)?
                else {
                    continue;
                };
                snapshot
            };
            let text = snapshot.document.text();
            if self
                .store
                .collaboration_file_path(workspace_id, file_id)
                .is_ok()
            {
                self.store
                    .save_collaboration_state(workspace_id, file_id, &snapshot, &text)?;
            }
        }
        self.store.sync_collaboration_files(workspace_id)?;
        Ok(())
    }

    pub async fn is_document_active(&self, file_id: Uuid) -> bool {
        self.active_documents.lock().await.contains_key(&file_id)
    }

    pub async fn chat_sender(&self, workspace_id: Uuid) -> broadcast::Sender<ChatMessage> {
        let mut rooms = self.chat_rooms.lock().await;
        rooms
            .entry(workspace_id)
            .or_insert_with(|| broadcast::channel(128).0)
            .clone()
    }

    pub async fn workspace_event_sender(
        &self,
        workspace_id: Uuid,
    ) -> broadcast::Sender<WorkspaceEvent> {
        let mut rooms = self.workspace_events.lock().await;
        rooms
            .entry(workspace_id)
            .or_insert_with(|| broadcast::channel(64).0)
            .clone()
    }

    pub async fn close_workspace_streams(&self, workspace_id: Uuid) {
        self.chat_rooms.lock().await.remove(&workspace_id);
        self.workspace_events.lock().await.remove(&workspace_id);
    }

    pub fn session_cookie_name(&self) -> &'static str {
        if self.settings.secure_cookie {
            "__Host-colabo_session"
        } else {
            "colabo_session"
        }
    }

    pub fn session_cookie(&self, token: &str, max_age: i64) -> String {
        let secure = if self.settings.secure_cookie {
            "; Secure"
        } else {
            ""
        };
        format!(
            "{}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}",
            self.session_cookie_name()
        )
    }

    pub fn clear_session_cookie(&self) -> String {
        let secure = if self.settings.secure_cookie {
            "; Secure"
        } else {
            ""
        };
        format!(
            "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}",
            self.session_cookie_name()
        )
    }

    pub fn oidc_state_cookie(&self, state: &str) -> String {
        let secure = if self.settings.secure_cookie {
            "; Secure"
        } else {
            ""
        };
        format!(
            "colabo_oidc_state={state}; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=600{secure}"
        )
    }

    pub fn clear_oidc_state_cookie(&self) -> String {
        let secure = if self.settings.secure_cookie {
            "; Secure"
        } else {
            ""
        };
        format!("colabo_oidc_state=; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=0{secure}")
    }
}
