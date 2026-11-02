//! Central in-memory sequencer and per-document WebSocket sessions.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::extract::ws::WebSocket;
use collab_core::{
    ActorId, ApplyError, CommittedTransaction, Document, HistoryAction, ItemId, Operation,
    SessionData, SessionDataEntry, Snapshot, TextAnchor, TextSelection, Transaction, TxId,
    history_action,
};
pub use collab_core::{DocumentId, EditorIdentity};
use futures_util::{FutureExt, future::BoxFuture};
mod config;
mod websocket;
use config::ROOM_BASE_MEMORY_BYTES;
pub use config::{ConfigError, ListenConfig, ResourceLimits, SecurityConfig, ServerConfig};
use thiserror::Error;
use tokio::sync::{Mutex, Notify, RwLock, broadcast};
use uuid::Uuid;

const TRANSACTION_BASE_MEMORY_BYTES: usize = 256;
const INSERT_CHUNK_MEMORY_BYTES: usize = 256;
const DELETED_ITEM_MEMORY_BYTES: usize = 96;
const HISTORY_ACTION_MEMORY_BYTES: usize = 64;
const SELECTION_MEMORY_BYTES: usize = 128;
const MAX_CLIENT_ACTOR: u64 = 1 << 52;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error("invalid transaction history: {0}")]
    History(String),
    #[error("transaction actor does not match the connected actor")]
    ActorMismatch,
    #[error("client actor ID is outside the supported range")]
    InvalidActor,
    #[error("transaction sequence overflow")]
    SequenceOverflow,
    #[error("editing session has expired")]
    SessionExpired,
    #[error("the maximum number of editing sessions has been reached")]
    SessionLimit,
    #[error("the editing session already exists")]
    SessionExists,
    #[error("the document exceeds its {limit_bytes}-byte memory budget")]
    SessionMemoryLimit { limit_bytes: usize },
    #[error("the server has reached its total session memory budget")]
    TotalMemoryLimit,
    #[error("the maximum number of connections for this session has been reached")]
    ConnectionLimit,
    #[error("could not persist collaboration state")]
    Persistence,
    #[error("session data exceeds its {limit_bytes}-byte limit")]
    SessionDataLimit { limit_bytes: usize },
}

fn validate_client_actor(actor: ActorId) -> Result<(), ServerError> {
    (1..=MAX_CLIENT_ACTOR)
        .contains(&actor.0)
        .then_some(())
        .ok_or(ServerError::InvalidActor)
}

#[derive(Clone, Default)]
struct HistoryStacks {
    undo: Vec<TxId>,
    redo: Vec<TxId>,
    selection_history: HashMap<TxId, EditSelectionHistory>,
}

impl HistoryStacks {
    fn record_edit(&mut self, transaction: &Transaction, maximum: usize) {
        if transaction.selection_before.is_some() || transaction.selection_after.is_some() {
            self.selection_history.insert(
                transaction.id,
                EditSelectionHistory {
                    before: transaction.selection_before.clone(),
                    after: transaction.selection_after.clone(),
                },
            );
        }
        self.undo.push(transaction.id);
        let expired = self.undo.len().saturating_sub(maximum);
        for transaction in self.undo.drain(..expired) {
            self.selection_history.remove(&transaction);
        }
        for transaction in self.redo.drain(..) {
            self.selection_history.remove(&transaction);
        }
    }
}

#[derive(Clone)]
struct EditSelectionHistory {
    before: Option<TextSelection>,
    after: Option<TextSelection>,
}

#[derive(Clone)]
pub struct Server {
    inner: Arc<ServerInner>,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

struct ServerInner {
    rooms: Mutex<HashMap<DocumentId, Arc<DocumentRoom>>>,
    total_memory_bytes: Arc<AtomicUsize>,
    total_connections: Arc<AtomicUsize>,
    allowed_origins: HashSet<String>,
    config: ServerConfig,
}

struct DocumentRoom {
    room: Mutex<Room>,
    closed: AtomicBool,
    active_connections: AtomicUsize,
    estimated_memory_bytes: AtomicUsize,
    total_memory_bytes: Arc<AtomicUsize>,
    limits: ResourceLimits,
    committed_tx: broadcast::Sender<CommittedTransaction>,
    closed_tx: broadcast::Sender<()>,
    session_data_tx: broadcast::Sender<SessionDataEvent>,
    session_data: Mutex<HashMap<u64, ActiveSessionData>>,
    idle_generation: AtomicU64,
    idle_notify: Arc<Notify>,
    persister: RwLock<Option<SnapshotPersister>>,
    submission_guard: RwLock<Option<SubmissionGuard>>,
}

type SnapshotPersister =
    Arc<dyn Fn(Snapshot) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;
type SubmissionGuard = Arc<dyn Fn() -> BoxFuture<'static, Box<dyn Send>> + Send + Sync>;

struct ActiveSessionData {
    actor: ActorId,
    identity: EditorIdentity,
    data: SessionData,
    estimated_memory_bytes: usize,
}

struct ConnectionSlot {
    total_connections: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct Room {
    seq: u64,
    document: Document,
    histories: HashMap<ActorId, HistoryStacks>,
}

#[derive(Clone)]
struct SessionDataEvent {
    connection_id: u64,
    actor: ActorId,
    identity: EditorIdentity,
    data: Option<SessionData>,
}

const SESSION_DATA_BASE_MEMORY_BYTES: usize = 128;

impl Server {
    pub fn new() -> Self {
        Self::with_config(ServerConfig::default()).expect("default server config is valid")
    }

    pub fn with_config(config: ServerConfig) -> Result<Self, ServerError> {
        config.validate()?;
        let total_memory_bytes = Arc::new(AtomicUsize::new(0));
        let allowed_origins = config.security.allowed_origins.iter().cloned().collect();
        Ok(Self {
            inner: Arc::new(ServerInner {
                rooms: Mutex::new(HashMap::new()),
                total_memory_bytes,
                total_connections: Arc::new(AtomicUsize::new(0)),
                allowed_origins,
                config,
            }),
        })
    }

    /// Create an in-memory document using a stable ID owned by the application.
    pub async fn create_session(&self, text: &str) -> Result<DocumentId, ServerError> {
        let session_id =
            DocumentId::new(Uuid::now_v7().to_string()).expect("a UUID is a valid document ID");
        self.create_session_with_id(session_id.clone(), text)
            .await?;
        Ok(session_id)
    }

    pub async fn create_session_with_id(
        &self,
        session_id: DocumentId,
        text: &str,
    ) -> Result<(), ServerError> {
        let mut rooms = self.inner.rooms.lock().await;
        if rooms.contains_key(&session_id) {
            return Err(ServerError::SessionExists);
        }
        if rooms.len() >= self.inner.config.limits.max_sessions {
            return Err(ServerError::SessionLimit);
        }
        let document_room = Arc::new(new_document_room(
            self.inner.total_memory_bytes.clone(),
            self.inner.config.limits.clone(),
        )?);
        if !text.is_empty() {
            let initial = Transaction::new(
                TxId::new(ActorId(0), 1),
                vec![Operation::TextInsert {
                    start: ItemId::new(ActorId(0), 2),
                    anchor: TextAnchor::default(),
                    text: text.to_owned(),
                }],
            );
            submit_to_room(&document_room, ActorId(0), initial).await?;
        }
        let idle_notify = document_room.idle_notify.clone();
        rooms.insert(session_id.clone(), document_room);
        drop(rooms);
        self.schedule_idle_expiry(session_id, 0, idle_notify);
        Ok(())
    }

    pub async fn restore_session(
        &self,
        session_id: DocumentId,
        snapshot: Snapshot,
    ) -> Result<(), ServerError> {
        let serialized_size = serde_json::to_vec(&snapshot.document)
            .expect("the collaboration document is serializable")
            .len();
        let mut rooms = self.inner.rooms.lock().await;
        if rooms.contains_key(&session_id) {
            return Err(ServerError::SessionExists);
        }
        if rooms.len() >= self.inner.config.limits.max_sessions {
            return Err(ServerError::SessionLimit);
        }
        let document_room = Arc::new(new_document_room(
            self.inner.total_memory_bytes.clone(),
            self.inner.config.limits.clone(),
        )?);
        if serialized_size > 0 {
            document_room.reserve_memory(serialized_size)?;
        }
        {
            let mut room = document_room.room.lock().await;
            room.seq = snapshot.seq;
            room.document = snapshot.document;
        }
        let idle_notify = document_room.idle_notify.clone();
        rooms.insert(session_id.clone(), document_room);
        drop(rooms);
        self.schedule_idle_expiry(session_id, 0, idle_notify);
        Ok(())
    }

    /// Submit a transaction to an existing in-memory session.
    pub async fn submit_to(
        &self,
        document_id: &DocumentId,
        connected_actor: ActorId,
        transaction: Transaction,
    ) -> Result<CommittedTransaction, ServerError> {
        validate_client_actor(connected_actor)?;
        let room = self.document_room(document_id).await?;
        submit_to_room(&room, connected_actor, transaction).await
    }

    /// Read the latest confirmed snapshot for a document.
    pub async fn snapshot(&self, document_id: &DocumentId) -> Result<Snapshot, ServerError> {
        let room = self.document_room(document_id).await?;
        let room = room.room.lock().await;
        Ok(Snapshot {
            seq: room.seq,
            document: room.document.clone(),
        })
    }

    pub async fn set_snapshot_persister<F, Fut>(
        &self,
        document_id: &DocumentId,
        persister: F,
    ) -> Result<(), ServerError>
    where
        F: Fn(Snapshot) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let room = self.document_room(document_id).await?;
        let persister = Arc::new(persister);
        let wrapped: SnapshotPersister = Arc::new(move |snapshot| persister(snapshot).boxed());
        *room.persister.write().await = Some(wrapped);
        Ok(())
    }

    /// 編集を共有状態へ適用する前に取得するロックを設定する。
    pub async fn set_submission_guard<G, F, Fut>(
        &self,
        document_id: &DocumentId,
        guard_factory: F,
    ) -> Result<(), ServerError>
    where
        G: Send + 'static,
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = G> + Send + 'static,
    {
        let room = self.document_room(document_id).await?;
        let guard_factory = Arc::new(guard_factory);
        let wrapped: SubmissionGuard = Arc::new(move || {
            let guard_factory = guard_factory.clone();
            async move { Box::new(guard_factory().await) as Box<dyn Send> }.boxed()
        });
        *room.submission_guard.write().await = Some(wrapped);
        Ok(())
    }

    pub async fn has_session(&self, document_id: &DocumentId) -> bool {
        self.find_room(document_id).await.is_some()
    }

    pub async fn subscribe(
        &self,
        document_id: &DocumentId,
    ) -> Result<broadcast::Receiver<CommittedTransaction>, ServerError> {
        let room = self.document_room(document_id).await?;
        Ok(room.committed_tx.subscribe())
    }

    pub async fn replace_text(
        &self,
        document_id: &DocumentId,
        text: &str,
    ) -> Result<(), ServerError> {
        self.replace_text_inner(document_id, text, true).await
    }

    /// 既に送信ロックを保持している呼び出し元から本文を置き換える。
    pub async fn replace_text_with_guard_held(
        &self,
        document_id: &DocumentId,
        text: &str,
    ) -> Result<(), ServerError> {
        self.replace_text_inner(document_id, text, false).await
    }

    async fn replace_text_inner(
        &self,
        document_id: &DocumentId,
        text: &str,
        acquire_submission_guard: bool,
    ) -> Result<(), ServerError> {
        let document_room = self.document_room(document_id).await?;
        let submission_guard = if acquire_submission_guard {
            acquire_room_submission_guard(&document_room).await
        } else {
            None
        };
        let snapshot = {
            let room = document_room.room.lock().await;
            Snapshot {
                seq: room.seq,
                document: room.document.clone(),
            }
        };
        if snapshot.document.text() == text {
            return Ok(());
        }
        let actor = ActorId(0);
        let clock = snapshot
            .document
            .actor_max_clock(actor)
            .checked_add(1)
            .ok_or(ServerError::SequenceOverflow)?;
        let mut operations = Vec::new();
        let visible_ids = snapshot.document.visible_ids();
        if !visible_ids.is_empty() {
            operations.push(Operation::TextDelete {
                targets: visible_ids,
            });
        }
        if !text.is_empty() {
            let item_clock = clock.checked_add(1).ok_or(ServerError::SequenceOverflow)?;
            operations.push(Operation::TextInsert {
                start: ItemId::new(actor, item_clock),
                anchor: TextAnchor::default(),
                text: text.to_owned(),
            });
        }
        let transaction = Transaction::new(TxId::new(actor, clock), operations);
        submit_to_room_inner(&document_room, actor, transaction, false).await?;
        drop(submission_guard);
        Ok(())
    }

    pub async fn remove_session(&self, document_id: &DocumentId) {
        let room = self.inner.rooms.lock().await.remove(document_id);
        if let Some(room) = room {
            room.closed.store(true, Ordering::Release);
            let _ = room.closed_tx.send(());
            let _room = room.room.lock().await;
        }
    }

    pub fn websocket_max_message_size(&self) -> usize {
        self.inner.config.limits.max_websocket_message_bytes
    }

    pub fn origin_allowed(&self, origin: &str) -> bool {
        self.inner.allowed_origins.contains(origin)
    }

    pub async fn connect_socket(
        &self,
        document_id: &DocumentId,
        socket: WebSocket,
        actor: ActorId,
        identity: EditorIdentity,
        writable: bool,
    ) -> Result<(), ServerError> {
        let document_room = self.document_room(document_id).await?;
        let connection_slot = reserve_connection_slot(self)?;
        websocket::handle_authorized_socket(
            socket,
            websocket::AuthorizedSocket {
                server: self.clone(),
                document_id: document_id.clone(),
                document_room,
                connection_slot,
                actor,
                identity,
                writable,
            },
        )
        .await;
        Ok(())
    }

    async fn find_room(&self, document_id: &DocumentId) -> Option<Arc<DocumentRoom>> {
        self.inner.rooms.lock().await.get(document_id).cloned()
    }

    async fn document_room(
        &self,
        document_id: &DocumentId,
    ) -> Result<Arc<DocumentRoom>, ServerError> {
        self.find_room(document_id)
            .await
            .ok_or(ServerError::SessionExpired)
    }

    async fn join_session(
        &self,
        document_id: &DocumentId,
        document_room: &Arc<DocumentRoom>,
    ) -> Result<(), ServerError> {
        let rooms = self.inner.rooms.lock().await;
        let Some(active_room) = rooms.get(document_id) else {
            return Err(ServerError::SessionExpired);
        };
        if !Arc::ptr_eq(active_room, document_room) {
            return Err(ServerError::SessionExpired);
        }
        let was_idle = document_room.add_connection()?;
        if was_idle {
            document_room.idle_generation.fetch_add(1, Ordering::AcqRel);
            document_room.idle_notify.notify_one();
        }
        Ok(())
    }

    async fn leave_session(&self, document_id: &DocumentId, document_room: &Arc<DocumentRoom>) {
        let rooms = self.inner.rooms.lock().await;
        let Some(current_room) = rooms.get(document_id) else {
            return;
        };
        if !Arc::ptr_eq(current_room, document_room) {
            return;
        }
        let previous = document_room
            .active_connections
            .fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0, "session connection count underflow");
        if previous == 1 {
            let generation = document_room.idle_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let idle_notify = document_room.idle_notify.clone();
            drop(rooms);
            self.schedule_idle_expiry(document_id.clone(), generation, idle_notify);
        }
    }

    fn schedule_idle_expiry(
        &self,
        document_id: DocumentId,
        generation: u64,
        idle_notify: Arc<Notify>,
    ) {
        let ttl = Duration::from_secs(self.inner.config.limits.session_ttl_seconds);
        let server = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(ttl) => {}
                _ = idle_notify.notified() => return,
            }
            let Some(server) = server.upgrade() else {
                return;
            };
            let expired_room = {
                let mut rooms = server.rooms.lock().await;
                let expired = rooms.get(&document_id).is_some_and(|room| {
                    room.active_connections.load(Ordering::Acquire) == 0
                        && room.idle_generation.load(Ordering::Acquire) == generation
                });
                if expired {
                    let room = rooms.remove(&document_id);
                    if let Some(room) = &room {
                        room.closed.store(true, Ordering::Release);
                        let _ = room.closed_tx.send(());
                    }
                    room
                } else {
                    None
                }
            };
            if let Some(room) = expired_room {
                let _room = room.room.lock().await;
            }
        });
    }
}

fn new_document_room(
    total_memory_bytes: Arc<AtomicUsize>,
    limits: ResourceLimits,
) -> Result<DocumentRoom, ServerError> {
    let (committed_tx, _) = broadcast::channel(limits.broadcast_capacity);
    let (closed_tx, _) = broadcast::channel(1);
    let (session_data_tx, _) = broadcast::channel(limits.broadcast_capacity);
    let room = DocumentRoom {
        room: Mutex::new(Room {
            seq: 0,
            document: Document::default(),
            histories: HashMap::new(),
        }),
        closed: AtomicBool::new(false),
        active_connections: AtomicUsize::new(0),
        estimated_memory_bytes: AtomicUsize::new(0),
        total_memory_bytes,
        limits,
        committed_tx,
        closed_tx,
        session_data_tx,
        session_data: Mutex::new(HashMap::new()),
        idle_generation: AtomicU64::new(0),
        idle_notify: Arc::new(Notify::new()),
        persister: RwLock::new(None),
        submission_guard: RwLock::new(None),
    };
    room.reserve_memory(ROOM_BASE_MEMORY_BYTES)?;
    Ok(room)
}

impl DocumentRoom {
    fn reserve_memory(&self, bytes: usize) -> Result<(), ServerError> {
        if self
            .estimated_memory_bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.limits.max_session_memory_bytes)
            })
            .is_err()
        {
            return Err(ServerError::SessionMemoryLimit {
                limit_bytes: self.limits.max_session_memory_bytes,
            });
        }

        let total_limit = self.limits.max_total_session_memory_bytes;
        if self
            .total_memory_bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= total_limit)
            })
            .is_err()
        {
            self.estimated_memory_bytes
                .fetch_sub(bytes, Ordering::AcqRel);
            return Err(ServerError::TotalMemoryLimit);
        }
        Ok(())
    }

    fn release_memory(&self, bytes: usize) {
        let previous = self
            .estimated_memory_bytes
            .fetch_sub(bytes, Ordering::AcqRel);
        debug_assert!(previous >= bytes, "session memory accounting underflow");
        self.total_memory_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }

    fn add_connection(&self) -> Result<bool, ServerError> {
        let maximum = self.limits.max_connections_per_session;
        self.active_connections
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < maximum).then_some(current + 1)
            })
            .map(|previous| previous == 0)
            .map_err(|_| ServerError::ConnectionLimit)
    }
}

impl Drop for DocumentRoom {
    fn drop(&mut self) {
        let bytes = self.estimated_memory_bytes.load(Ordering::Acquire);
        if bytes > 0 {
            self.total_memory_bytes.fetch_sub(bytes, Ordering::AcqRel);
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        let previous = self.total_connections.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "global connection count underflow");
    }
}

fn reserve_connection_slot(server: &Server) -> Result<ConnectionSlot, ServerError> {
    let total_connections = server.inner.total_connections.clone();
    let maximum = server.inner.config.limits.max_total_connections;
    total_connections
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < maximum).then_some(current + 1)
        })
        .map_err(|_| ServerError::ConnectionLimit)?;
    Ok(ConnectionSlot { total_connections })
}

async fn submit_to_room(
    document_room: &DocumentRoom,
    connected_actor: ActorId,
    transaction: Transaction,
) -> Result<CommittedTransaction, ServerError> {
    submit_to_room_inner(document_room, connected_actor, transaction, true).await
}

async fn submit_to_room_inner(
    document_room: &DocumentRoom,
    connected_actor: ActorId,
    mut transaction: Transaction,
    acquire_submission_guard: bool,
) -> Result<CommittedTransaction, ServerError> {
    if transaction.actor != connected_actor || transaction.id.actor != connected_actor {
        return Err(ServerError::ActorMismatch);
    }

    let submission_guard = if acquire_submission_guard {
        acquire_room_submission_guard(document_room).await
    } else {
        None
    };
    let persister = document_room.persister.read().await.clone();
    let mut room = document_room.room.lock().await;
    if document_room.closed.load(Ordering::Acquire) {
        return Err(ServerError::SessionExpired);
    }
    let action = room.prepare_history_selection(&mut transaction)?;
    room.validate_submission(&transaction, action)?;
    let memory_growth = estimate_transaction_memory(&transaction, &document_room.limits)?;
    document_room.reserve_memory(memory_growth)?;
    let previous = persister.as_ref().map(|_| room.clone());
    let seq = room.seq.checked_add(1).ok_or_else(|| {
        document_room.release_memory(memory_growth);
        ServerError::SequenceOverflow
    })?;
    let committed = CommittedTransaction { seq, transaction };

    if let Err(error) = room.apply_committed(
        &committed,
        action,
        document_room.limits.max_undo_transactions_per_actor,
    ) {
        document_room.release_memory(memory_growth);
        return Err(error);
    }
    if let Some(persister) = persister {
        let snapshot = Snapshot {
            seq: room.seq,
            document: room.document.clone(),
        };
        if persister(snapshot).await.is_err() {
            if let Some(previous) = previous {
                *room = previous;
            }
            document_room.release_memory(memory_growth);
            return Err(ServerError::Persistence);
        }
    }
    let _ = document_room.committed_tx.send(committed.clone());
    drop(room);
    drop(submission_guard);
    Ok(committed)
}

async fn acquire_room_submission_guard(document_room: &DocumentRoom) -> Option<Box<dyn Send>> {
    let guard_factory = document_room.submission_guard.read().await.clone();
    match guard_factory {
        Some(guard_factory) => Some(guard_factory().await),
        None => None,
    }
}

/// Conservative retained-state accounting; transient request/snapshot copies are
/// separately bounded by the upload, WebSocket message, and connection limits.
fn estimate_transaction_memory(
    transaction: &Transaction,
    limits: &ResourceLimits,
) -> Result<usize, ServerError> {
    let mut bytes = TRANSACTION_BASE_MEMORY_BYTES;
    for operation in &transaction.ops {
        let growth = match operation {
            Operation::TextInsert { text, .. } => text.len().checked_add(INSERT_CHUNK_MEMORY_BYTES),
            Operation::TextDelete { targets } => {
                targets.len().checked_mul(DELETED_ITEM_MEMORY_BYTES)
            }
            Operation::SetActive { .. } => Some(HISTORY_ACTION_MEMORY_BYTES),
        }
        .ok_or(ServerError::SessionMemoryLimit {
            limit_bytes: limits.max_session_memory_bytes,
        })?;
        bytes = bytes
            .checked_add(growth)
            .ok_or(ServerError::SessionMemoryLimit {
                limit_bytes: limits.max_session_memory_bytes,
            })?;
    }
    for _ in [
        &transaction.restore_selection,
        &transaction.selection_before,
        &transaction.selection_after,
    ]
    .into_iter()
    .flatten()
    {
        bytes =
            bytes
                .checked_add(SELECTION_MEMORY_BYTES)
                .ok_or(ServerError::SessionMemoryLimit {
                    limit_bytes: limits.max_session_memory_bytes,
                })?;
    }
    Ok(bytes)
}

async fn welcome_from_room(
    document_room: &DocumentRoom,
    actor: ActorId,
) -> (Snapshot, Vec<TxId>, Vec<TxId>) {
    let room = document_room.room.lock().await;
    let history = room.histories.get(&actor);
    (
        Snapshot {
            seq: room.seq,
            document: room.document.clone(),
        },
        history
            .map(|history| history.undo.clone())
            .unwrap_or_default(),
        history
            .map(|history| history.redo.clone())
            .unwrap_or_default(),
    )
}

async fn session_data_snapshot(
    document_room: &DocumentRoom,
    actor: ActorId,
    identity_id: &str,
) -> Vec<SessionDataEntry> {
    // A reconnect can overlap its old socket; hide that identity's live position from itself.
    document_room
        .session_data
        .lock()
        .await
        .values()
        .filter(|peer| peer.actor != actor)
        .map(|peer| SessionDataEntry {
            actor: peer.actor,
            identity: peer.identity.clone(),
            data: if peer.identity.id == identity_id {
                without_live_position(peer.data.clone())
            } else {
                peer.data.clone()
            },
        })
        .collect()
}

fn without_live_position(mut data: SessionData) -> SessionData {
    data.remove("cursor");
    data.remove("selection");
    data
}

async fn update_session_data(
    document_room: &DocumentRoom,
    connection_id: u64,
    actor: ActorId,
    identity: EditorIdentity,
    data: SessionData,
) -> Result<(), ServerError> {
    let data_bytes = serde_json::to_vec(&data)
        .expect("a serde_json::Value map is always serializable")
        .len();
    if data_bytes > document_room.limits.max_session_data_bytes {
        return Err(ServerError::SessionDataLimit {
            limit_bytes: document_room.limits.max_session_data_bytes,
        });
    }
    let estimated_memory_bytes = data_bytes
        .checked_add(SESSION_DATA_BASE_MEMORY_BYTES)
        .ok_or(ServerError::SessionDataLimit {
            limit_bytes: document_room.limits.max_session_data_bytes,
        })?;
    let mut session_data = document_room.session_data.lock().await;
    let previous_bytes = session_data
        .get(&connection_id)
        .map(|peer| peer.estimated_memory_bytes)
        .unwrap_or(0);
    if estimated_memory_bytes > previous_bytes {
        document_room.reserve_memory(estimated_memory_bytes - previous_bytes)?;
    } else if previous_bytes > estimated_memory_bytes {
        document_room.release_memory(previous_bytes - estimated_memory_bytes);
    }
    session_data.insert(
        connection_id,
        ActiveSessionData {
            actor,
            identity,
            data,
            estimated_memory_bytes,
        },
    );
    Ok(())
}

impl Room {
    fn prepare_history_selection(
        &self,
        transaction: &mut Transaction,
    ) -> Result<HistoryAction, ServerError> {
        let action = history_action(transaction)?;
        if let HistoryAction::SetActive { target, active } = action
            && let Some(history) = self
                .histories
                .get(&transaction.actor)
                .and_then(|stacks| stacks.selection_history.get(&target))
        {
            transaction.restore_selection = if active {
                history.after.clone()
            } else {
                history.before.clone()
            };
        }
        Ok(action)
    }

    fn validate_submission(
        &self,
        transaction: &Transaction,
        action: HistoryAction,
    ) -> Result<(), ServerError> {
        let stacks = self.histories.get(&transaction.actor);
        match action {
            HistoryAction::Edit => {}
            HistoryAction::SetActive {
                target,
                active: false,
            } => {
                if stacks.and_then(|history| history.undo.last()) != Some(&target) {
                    return Err(ServerError::History(
                        "undo target is not the actor's latest active edit".into(),
                    ));
                }
            }
            HistoryAction::SetActive {
                target,
                active: true,
            } => {
                if stacks.and_then(|history| history.redo.last()) != Some(&target) {
                    return Err(ServerError::History(
                        "redo target is not the actor's latest undone edit".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn apply_committed(
        &mut self,
        committed: &CommittedTransaction,
        action: HistoryAction,
        max_undo_transactions_per_actor: usize,
    ) -> Result<(), ServerError> {
        let transaction = &committed.transaction;
        self.document.apply(transaction)?;
        let stacks = self.histories.entry(transaction.actor).or_default();
        match action {
            HistoryAction::Edit => {
                stacks.record_edit(transaction, max_undo_transactions_per_actor);
            }
            HistoryAction::SetActive {
                target,
                active: false,
            } => {
                stacks.undo.pop();
                stacks.redo.push(target);
            }
            HistoryAction::SetActive {
                target,
                active: true,
            } => {
                stacks.redo.pop();
                stacks.undo.push(target);
            }
        }
        self.seq = committed.seq;
        Ok(())
    }
}
