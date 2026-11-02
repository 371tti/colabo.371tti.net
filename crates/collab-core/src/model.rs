use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_SAFE_ID: u64 = (1 << 53) - 1;

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct ActorId(pub u64);

/// A URL-safe identifier for one independently sequenced document.
///
/// IDs are limited to ASCII letters, digits, `-`, and `_` so they can be used
/// safely as one path segment and as a directory name by the server.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DocumentId(String);

impl DocumentId {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidDocumentId> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if valid {
            Ok(Self(value))
        } else {
            Err(InvalidDocumentId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
#[error("document ID must be 1-64 ASCII letters, digits, hyphens, or underscores")]
pub struct InvalidDocumentId;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TxId {
    pub actor: ActorId,
    pub clock: u64,
}

impl TxId {
    pub const fn new(actor: ActorId, clock: u64) -> Self {
        Self { actor, clock }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ItemId {
    pub actor: ActorId,
    pub clock: u64,
}

impl ItemId {
    pub const fn new(actor: ActorId, clock: u64) -> Self {
        Self { actor, clock }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextAnchor {
    /// The stable item immediately before the insertion point.
    pub left: Option<ItemId>,
    /// The stable item immediately after the insertion point.
    pub right: Option<ItemId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextSelection {
    pub anchor: TextAnchor,
    pub head: TextAnchor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Operation {
    TextInsert {
        start: ItemId,
        anchor: TextAnchor,
        text: String,
    },
    TextDelete {
        targets: Vec<ItemId>,
    },
    /// Enable or disable one of this actor's earlier edit transactions.
    SetActive {
        target: TxId,
        active: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Transaction {
    pub id: TxId,
    pub actor: ActorId,
    pub ops: Vec<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_selection: Option<TextSelection>,
    /// Selection immediately before this edit, used when the edit is undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_before: Option<TextSelection>,
    /// Selection immediately after this edit, used when the edit is redone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_after: Option<TextSelection>,
}

impl Transaction {
    pub fn new(id: TxId, ops: Vec<Operation>) -> Self {
        Self {
            id,
            actor: id.actor,
            ops,
            restore_selection: None,
            selection_before: None,
            selection_after: None,
        }
    }

    pub fn with_restore_selection(mut self, selection: Option<TextSelection>) -> Self {
        self.restore_selection = selection;
        self
    }

    pub fn with_selection_history(
        mut self,
        before: Option<TextSelection>,
        after: Option<TextSelection>,
    ) -> Self {
        self.selection_before = before;
        self.selection_after = after;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommittedTransaction {
    pub seq: u64,
    pub transaction: Transaction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextChunk {
    /// IDs for each Unicode scalar are derived from `start` and the scalar offset.
    pub start: ItemId,
    pub anchor: TextAnchor,
    pub text: String,
    pub transaction: TxId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DeletionTag {
    target: ItemId,
    transaction: TxId,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Document {
    chunks: Vec<TextChunk>,
    deletion_tags: Vec<DeletionTag>,
    inactive_transactions: BTreeSet<TxId>,
    transactions: BTreeSet<TxId>,
    edit_transactions: BTreeSet<TxId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Snapshot {
    pub seq: u64,
    pub document: Document,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryAction {
    Edit,
    SetActive { target: TxId, active: bool },
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ApplyError {
    #[error("transaction actor does not match its ID")]
    ActorMismatch,
    #[error("actor and clock IDs must be JavaScript-safe integers")]
    IdOutOfRange,
    #[error("transaction must contain at least one operation")]
    EmptyTransaction,
    #[error("transaction ID has already been used")]
    DuplicateTransaction,
    #[error("a SetActive operation must be the only operation in its transaction")]
    MixedHistoryOperation,
    #[error("transaction selection metadata refers to an invalid position")]
    InvalidSelectionMetadata,
    #[error("text insertion cannot be empty")]
    EmptyInsert,
    #[error("text insertion IDs must belong to the transaction actor")]
    InsertActorMismatch,
    #[error("text insertion IDs must start after the transaction ID")]
    InsertClockMismatch,
    #[error("text insertion ID range overflows")]
    InsertClockOverflow,
    #[error("text insertion would reuse an existing item ID")]
    DuplicateItem,
    #[error("an insertion anchor refers to an unknown item")]
    UnknownAnchor,
    #[error("an insertion cannot use the same item as both anchors")]
    InvalidAnchor,
    #[error("deletion must target at least one item")]
    EmptyDelete,
    #[error("deletion refers to an unknown item")]
    UnknownDeleteTarget,
    #[error("SetActive can target only one of the actor's own edit transactions")]
    InvalidActiveTarget,
    #[error("cannot allocate another local ID")]
    ClockExhausted,
}

#[derive(Clone, Copy)]
struct Node {
    value: char,
    parent: Option<ItemId>,
    created_by: TxId,
}

impl Document {
    pub fn text(&self) -> String {
        self.visible_items().into_iter().map(|(_, ch)| ch).collect()
    }

    pub fn visible_ids(&self) -> Vec<ItemId> {
        self.visible_items().into_iter().map(|(id, _)| id).collect()
    }

    /// Resolve a stable text anchor to a Unicode-scalar offset in the current
    /// visible text. Deleted or undone neighbors are handled by counting only
    /// visible items before the surviving anchor boundary.
    pub fn position_of_anchor(&self, anchor: TextAnchor) -> usize {
        let (mut visible, mut left, mut right) = (0, None, None);
        for (id, _, is_visible) in self.ordered_items() {
            if anchor.left == Some(id) {
                left = Some(visible + is_visible as usize);
            }
            if anchor.right == Some(id) {
                right = Some(visible);
            }
            visible += is_visible as usize;
        }
        left.or(right).unwrap_or(visible)
    }

    fn contains_item(&self, id: ItemId) -> bool {
        self.chunks.iter().any(|chunk| {
            if chunk.start.actor != id.actor {
                return false;
            }
            let count = chunk.text.chars().count() as u64;
            id.clock >= chunk.start.clock && id.clock < chunk.start.clock.saturating_add(count)
        })
    }

    pub fn actor_max_clock(&self, actor: ActorId) -> u64 {
        let mut max_clock = self
            .transactions
            .iter()
            .filter(|id| id.actor == actor)
            .map(|id| id.clock)
            .max()
            .unwrap_or(0);
        for chunk in &self.chunks {
            if chunk.start.actor == actor {
                let count = chunk.text.chars().count() as u64;
                max_clock =
                    max_clock.max(chunk.start.clock.saturating_add(count.saturating_sub(1)));
            }
        }
        max_clock
    }

    fn validate_transaction(&self, transaction: &Transaction) -> Result<HistoryAction, ApplyError> {
        if transaction.id.actor != transaction.actor {
            return Err(ApplyError::ActorMismatch);
        }
        if transaction.actor.0 > MAX_SAFE_ID || transaction.id.clock > MAX_SAFE_ID {
            return Err(ApplyError::IdOutOfRange);
        }
        let action = history_action(transaction)?;
        if self.transactions.contains(&transaction.id) {
            return Err(ApplyError::DuplicateTransaction);
        }
        match action {
            HistoryAction::Edit if transaction.restore_selection.is_some() => {
                return Err(ApplyError::InvalidSelectionMetadata);
            }
            HistoryAction::SetActive { .. }
                if transaction.selection_before.is_some()
                    || transaction.selection_after.is_some() =>
            {
                return Err(ApplyError::InvalidSelectionMetadata);
            }
            _ => {}
        }
        let mut staged_ids = HashSet::new();
        for op in &transaction.ops {
            match op {
                Operation::TextInsert {
                    start,
                    anchor,
                    text,
                } => {
                    if text.is_empty() {
                        return Err(ApplyError::EmptyInsert);
                    }
                    if start.actor != transaction.actor {
                        return Err(ApplyError::InsertActorMismatch);
                    }
                    if start.clock <= transaction.id.clock {
                        return Err(ApplyError::InsertClockMismatch);
                    }
                    let count = text.chars().count() as u64;
                    let last_clock = start
                        .clock
                        .checked_add(count - 1)
                        .ok_or(ApplyError::InsertClockOverflow)?;
                    if last_clock > MAX_SAFE_ID {
                        return Err(ApplyError::IdOutOfRange);
                    }
                    for offset in 0..count {
                        let id = ItemId::new(start.actor, start.clock + offset);
                        if self.contains_item(id) || !staged_ids.insert(id) {
                            return Err(ApplyError::DuplicateItem);
                        }
                    }
                    if anchor.left.is_some() && anchor.left == anchor.right {
                        return Err(ApplyError::InvalidAnchor);
                    }
                    for anchor in [anchor.left, anchor.right].into_iter().flatten() {
                        if !self.contains_item(anchor) {
                            return Err(ApplyError::UnknownAnchor);
                        }
                    }
                }
                Operation::TextDelete { targets } => {
                    if targets.is_empty() {
                        return Err(ApplyError::EmptyDelete);
                    }
                    for target in targets {
                        if !self.contains_item(*target) && !staged_ids.contains(target) {
                            return Err(ApplyError::UnknownDeleteTarget);
                        }
                    }
                }
                Operation::SetActive { target, .. } => {
                    if target.actor != transaction.actor || !self.edit_transactions.contains(target)
                    {
                        return Err(ApplyError::InvalidActiveTarget);
                    }
                }
            }
        }
        if let Some(selection) = &transaction.restore_selection
            && !self.selection_items_exist(selection, &HashSet::new())
        {
            return Err(ApplyError::InvalidSelectionMetadata);
        }
        if let Some(selection) = &transaction.selection_before
            && !self.selection_items_exist(selection, &HashSet::new())
        {
            return Err(ApplyError::InvalidSelectionMetadata);
        }
        if let Some(selection) = &transaction.selection_after
            && !self.selection_items_exist(selection, &staged_ids)
        {
            return Err(ApplyError::InvalidSelectionMetadata);
        }
        Ok(action)
    }

    fn selection_items_exist(
        &self,
        selection: &TextSelection,
        staged_ids: &HashSet<ItemId>,
    ) -> bool {
        [selection.anchor, selection.head]
            .into_iter()
            .flat_map(|anchor| [anchor.left, anchor.right])
            .flatten()
            .all(|item| self.contains_item(item) || staged_ids.contains(&item))
    }

    pub fn apply(&mut self, transaction: &Transaction) -> Result<(), ApplyError> {
        let action = self.validate_transaction(transaction)?;
        for op in &transaction.ops {
            match op {
                Operation::TextInsert {
                    start,
                    anchor,
                    text,
                } => {
                    self.chunks.push(TextChunk {
                        start: *start,
                        anchor: *anchor,
                        text: text.clone(),
                        transaction: transaction.id,
                    });
                }
                Operation::TextDelete { targets } => {
                    for target in targets {
                        let tag = DeletionTag {
                            target: *target,
                            transaction: transaction.id,
                        };
                        if !self.deletion_tags.contains(&tag) {
                            self.deletion_tags.push(tag);
                        }
                    }
                }
                Operation::SetActive { target, active } => {
                    if *active {
                        self.inactive_transactions.remove(target);
                    } else {
                        self.inactive_transactions.insert(*target);
                    }
                }
            }
        }
        self.transactions.insert(transaction.id);
        if matches!(action, HistoryAction::Edit) {
            self.edit_transactions.insert(transaction.id);
        }
        Ok(())
    }

    fn visible_items(&self) -> Vec<(ItemId, char)> {
        self.ordered_items()
            .into_iter()
            .filter_map(|(id, value, visible)| visible.then_some((id, value)))
            .collect()
    }

    fn ordered_items(&self) -> Vec<(ItemId, char, bool)> {
        let mut nodes = HashMap::<ItemId, Node>::new();
        let mut children = HashMap::<Option<ItemId>, Vec<ItemId>>::new();
        let mut sibling_groups = HashMap::<(Option<ItemId>, Option<ItemId>), Vec<ItemId>>::new();

        for chunk in &self.chunks {
            let mut parent = chunk.anchor.left;
            for (offset, value) in chunk.text.chars().enumerate() {
                let id = ItemId::new(chunk.start.actor, chunk.start.clock + offset as u64);
                nodes.insert(
                    id,
                    Node {
                        value,
                        parent,
                        created_by: chunk.transaction,
                    },
                );
                children.entry(parent).or_default().push(id);
                parent = Some(id);
            }
            sibling_groups
                .entry((chunk.anchor.left, chunk.anchor.right))
                .or_default()
                .push(chunk.start);
        }

        let mut edges = HashMap::<Option<ItemId>, HashMap<ItemId, BTreeSet<ItemId>>>::new();
        for chunk in &self.chunks {
            if let Some(target) = top_child(chunk.anchor.left, chunk.anchor.right, &nodes)
                && target != chunk.start
            {
                edges
                    .entry(chunk.anchor.left)
                    .or_default()
                    .entry(chunk.start)
                    .or_default()
                    .insert(target);
            }
        }
        for ((parent, _), mut roots) in sibling_groups {
            roots.sort_unstable();
            roots.dedup();
            for pair in roots.windows(2) {
                edges
                    .entry(parent)
                    .or_default()
                    .entry(pair[0])
                    .or_default()
                    .insert(pair[1]);
            }
        }

        for (parent, siblings) in &mut children {
            *siblings = topological_siblings(siblings, edges.get(parent));
        }

        let mut ordered = Vec::new();
        let mut stack = children.get(&None).cloned().unwrap_or_default();
        stack.reverse();
        while let Some(id) = stack.pop() {
            let Some(node) = nodes.get(&id) else {
                continue;
            };
            let inserted = !self.inactive_transactions.contains(&node.created_by);
            let deleted = self.deletion_tags.iter().any(|tag| {
                tag.target == id && !self.inactive_transactions.contains(&tag.transaction)
            });
            ordered.push((id, node.value, inserted && !deleted));
            if let Some(descendants) = children.get(&Some(id)) {
                stack.extend(descendants.iter().rev().copied());
            }
        }
        ordered
    }
}

fn top_child(
    parent: Option<ItemId>,
    before: Option<ItemId>,
    nodes: &HashMap<ItemId, Node>,
) -> Option<ItemId> {
    let mut current = before?;
    loop {
        let node = nodes.get(&current)?;
        if node.parent == parent {
            return Some(current);
        }
        current = node.parent?;
    }
}

fn topological_siblings(
    siblings: &[ItemId],
    edges: Option<&HashMap<ItemId, BTreeSet<ItemId>>>,
) -> Vec<ItemId> {
    let members = siblings.iter().copied().collect::<BTreeSet<_>>();
    let mut indegree = members
        .iter()
        .copied()
        .map(|id| (id, 0usize))
        .collect::<BTreeMap<_, _>>();
    if let Some(edges) = edges {
        for (from, targets) in edges {
            if !members.contains(from) {
                continue;
            }
            for target in targets.iter().filter(|target| members.contains(target)) {
                *indegree.entry(*target).or_default() += 1;
            }
        }
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(siblings.len());
    while let Some(id) = ready.pop_first() {
        ordered.push(id);
        if let Some(targets) = edges.and_then(|edges| edges.get(&id)) {
            for target in targets.iter().filter(|target| members.contains(target)) {
                if let Some(degree) = indegree.get_mut(target) {
                    *degree -= 1;
                    if *degree == 0 {
                        ready.insert(*target);
                    }
                }
            }
        }
    }
    if ordered.len() != members.len() {
        // A malformed snapshot should still render deterministically.
        return members.into_iter().collect();
    }
    ordered
}

pub fn history_action(transaction: &Transaction) -> Result<HistoryAction, ApplyError> {
    match transaction.ops.as_slice() {
        [] => Err(ApplyError::EmptyTransaction),
        [Operation::SetActive { target, active }] => Ok(HistoryAction::SetActive {
            target: *target,
            active: *active,
        }),
        operations
            if operations
                .iter()
                .any(|operation| matches!(operation, Operation::SetActive { .. })) =>
        {
            Err(ApplyError::MixedHistoryOperation)
        }
        _ => Ok(HistoryAction::Edit),
    }
}
