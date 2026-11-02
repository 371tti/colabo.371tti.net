mod model;
mod protocol;

/// Default retained undoable edit count per actor.
pub const DEFAULT_MAX_UNDO_TRANSACTIONS_PER_ACTOR: usize = 100;

pub use model::{
    ActorId, ApplyError, CommittedTransaction, Document, DocumentId, HistoryAction,
    InvalidDocumentId, ItemId, Operation, Snapshot, TextAnchor, TextChunk, TextSelection,
    Transaction, TxId, history_action,
};
pub use protocol::{
    ClientMessage, EditorIdentity, IdentityError, Presence, ServerMessage, SessionData,
    SessionDataEntry,
};
