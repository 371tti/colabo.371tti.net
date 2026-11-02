use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{
    ActorId, CommittedTransaction, Snapshot, TextAnchor, TextSelection, Transaction, TxId,
};
use thiserror::Error;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Presence {
    pub cursor: Option<TextAnchor>,
    pub selection: Option<TextSelection>,
}

impl Presence {
    pub fn at(cursor: Option<TextAnchor>) -> Self {
        Self {
            cursor,
            selection: None,
        }
    }
}

/// Shareable editor identity, separate from the numeric actor used by the
/// transaction and undo protocol. The ID and display name are ephemeral.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EditorIdentity {
    pub id: String,
    pub name: String,
}

/// Arbitrary JSON fields shared among participants in one editing session.
pub type SessionData = BTreeMap<String, Value>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionDataEntry {
    pub actor: ActorId,
    pub identity: EditorIdentity,
    pub data: SessionData,
}

impl EditorIdentity {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Result<Self, IdentityError> {
        let id = id.into();
        let name = name.into();
        if !valid_identity_part(&id, 64) {
            return Err(IdentityError::InvalidId);
        }
        if !valid_identity_part(&name, 80) {
            return Err(IdentityError::InvalidName);
        }
        Ok(Self { id, name })
    }

    pub fn for_actor(actor: ActorId) -> Self {
        let id = actor.0.to_string();
        let name = format!("参加者 {}", actor.0);
        Self { id, name }
    }

    /// Fill omitted fields for older clients, then validate untrusted input.
    pub fn with_actor_defaults(self, actor: ActorId) -> Result<Self, IdentityError> {
        let has_id = !self.id.trim().is_empty();
        let fallback = Self::for_actor(actor);
        let id = if has_id { self.id } else { fallback.id.clone() };
        let name = if self.name.trim().is_empty() {
            if has_id { id.clone() } else { fallback.name }
        } else {
            self.name
        };
        Self::new(id, name)
    }
}

fn valid_identity_part(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control)
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum IdentityError {
    #[error("editor ID must be 1-64 characters and contain no control characters")]
    InvalidId,
    #[error("editor name must be 1-80 characters and contain no control characters")]
    InvalidName,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Boxing each submitted transaction would add a heap allocation.
pub enum ClientMessage {
    Hello {
        actor: ActorId,
        #[serde(default)]
        identity: EditorIdentity,
    },
    Submit {
        transaction: Transaction,
    },
    SessionData {
        data: SessionData,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        snapshot: Snapshot,
        #[serde(default = "default_max_undo_transactions_per_actor")]
        max_undo_transactions_per_actor: usize,
        undo_stack: Vec<TxId>,
        redo_stack: Vec<TxId>,
        #[serde(default)]
        session_data: Vec<SessionDataEntry>,
    },
    Committed {
        committed: CommittedTransaction,
    },
    Rejected {
        tx_id: TxId,
        reason: String,
    },
    SessionDataUpdate {
        actor: ActorId,
        #[serde(default)]
        identity: EditorIdentity,
        data: SessionData,
    },
    SessionDataLeft {
        actor: ActorId,
        #[serde(default)]
        identity: EditorIdentity,
    },
    SessionExpired {
        message: String,
    },
    ProtocolError {
        message: String,
    },
}

fn default_max_undo_transactions_per_actor() -> usize {
    crate::DEFAULT_MAX_UNDO_TRANSACTIONS_PER_ACTOR
}
