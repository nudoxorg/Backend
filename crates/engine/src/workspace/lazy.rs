//! Store-backed lazy relation capabilities for workspace snapshots.
//!
//! A workspace model must be able to inspect and update one selected relation
//! without turning the immutable closure into a `Vec` of every object or row.
//! This module is the narrow engine/version seam: the store owns node lookup,
//! the version crate owns canonical admission and path-copying, and the model
//! receives only an owned checked handle.

use backend_store::{FileStore, ObjectId, RelationNodeRead, StoreError, TypedObject};
use backend_version::{
    CanonicalRelation, ChildCommitment, DEFAULT_CUT_POLICY, IdContext, LazyPreparedUpdate,
    LazyTree, LazyTreeError, LazyTreePage, NodeError, PersistedTreeRoot, Relation, StateRoot,
    TreeChange, UntrustedId,
};
use std::fmt;
use std::sync::Arc;

/// The canonical relation a rejected node belongs to.
///
/// A surface renders relation identity, not a Rust type name, so the schema
/// tags travel with the fault instead of being recovered from a generic
/// parameter the caller has already erased.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelationIdentity {
    domain: u8,
    schema: u16,
    version: u8,
}

impl RelationIdentity {
    /// Returns the identity of one canonical relation schema.
    #[must_use]
    pub const fn of<R: Relation>() -> Self {
        Self {
            domain: R::DOMAIN,
            schema: R::TYPE,
            version: R::VERSION,
        }
    }

    /// Returns the relation's domain tag.
    #[must_use]
    pub const fn domain(self) -> u8 {
        self.domain
    }

    /// Returns the relation's schema tag.
    #[must_use]
    pub const fn schema(self) -> u16 {
        self.schema
    }

    /// Returns the relation's schema version.
    #[must_use]
    pub const fn version(self) -> u8 {
        self.version
    }
}

impl fmt::Display for RelationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "relation {:#04x}/{}/v{}",
            self.domain, self.schema, self.version
        )
    }
}

/// The first bytes of one canonical relation key, rendered as hex.
///
/// A key is opaque to a surface but must still identify the offending row, so
/// a bounded prefix travels with the fault rather than the whole key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelationKeyPrefix {
    bytes: [u8; Self::BYTES],
    length: u8,
}

impl RelationKeyPrefix {
    /// Bytes retained from the encoded key.
    pub const BYTES: usize = 8;

    /// Retains the leading bytes of one encoded relation key.
    #[must_use]
    pub fn of<R: Relation>(key: &R::Key) -> Self {
        let mut encoded = Vec::new();
        R::encode_key(key, &mut encoded);
        let mut bytes = [0; Self::BYTES];
        let taken = encoded.len().min(Self::BYTES);
        if let (Some(target), Some(source)) = (bytes.get_mut(..taken), encoded.get(..taken)) {
            target.copy_from_slice(source);
        }
        Self {
            bytes,
            length: u8::try_from(taken).unwrap_or(0),
        }
    }
}

impl fmt::Display for RelationKeyPrefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.bytes.iter().take(usize::from(self.length)) {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str("…")
    }
}

/// Why one canonical relation row or node was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceRelationRejection {
    /// The encoded row exceeds the capacity of one canonical node.
    ///
    /// This is a producer contract failure, not a corrupt store: the value
    /// has no representable row at any tree shape.
    RowTooLarge {
        /// Encoded bytes the value needs.
        encoded_bytes: usize,
        /// Encoded bytes one row may carry beside this key.
        capacity_bytes: usize,
    },
    /// A canonical node failed grammar, ordering, or admission checks.
    Node(NodeError),
    /// A removal or replacement targeted a key that is not present.
    MissingKey,
    /// An insertion targeted a key that is already present.
    DuplicateKey,
}

impl fmt::Display for WorkspaceRelationRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RowTooLarge {
                encoded_bytes,
                capacity_bytes,
            } => write!(
                formatter,
                "row is {encoded_bytes} encoded bytes, above the {capacity_bytes} byte canonical row capacity"
            ),
            Self::Node(error) => write!(formatter, "canonical node rejected: {error}"),
            Self::MissingKey => formatter.write_str("key is not present in the relation"),
            Self::DuplicateKey => formatter.write_str("key is already present in the relation"),
        }
    }
}

/// One rejected relation operation, named precisely enough to render.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceRelationFault {
    relation: RelationIdentity,
    key: Option<RelationKeyPrefix>,
    rejection: WorkspaceRelationRejection,
}

impl WorkspaceRelationFault {
    /// Returns the relation the rejected row belongs to.
    #[must_use]
    pub const fn relation(self) -> RelationIdentity {
        self.relation
    }

    /// Returns the offending key prefix when one row could be blamed.
    #[must_use]
    pub const fn key(self) -> Option<RelationKeyPrefix> {
        self.key
    }

    /// Returns why the operation was rejected.
    #[must_use]
    pub const fn rejection(self) -> WorkspaceRelationRejection {
        self.rejection
    }
}

impl fmt::Display for WorkspaceRelationFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.key {
            Some(key) => write!(
                formatter,
                "{} key {key}: {}",
                self.relation, self.rejection
            ),
            None => write!(formatter, "{}: {}", self.relation, self.rejection),
        }
    }
}

/// Failure while opening or using a checked workspace relation capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceRelationError {
    /// The snapshot was created from a standalone head and has no store.
    StoreUnavailable,
    /// The checked workspace manifest does not select the requested relation.
    MissingRelation,
    /// The selected root is not valid for the requested relation schema.
    InvalidRoot,
    /// The store could not admit or load the requested node.
    Store(StoreError),
    /// A canonical node or exact path-copy operation was rejected.
    Tree(WorkspaceRelationFault),
}

impl fmt::Display for WorkspaceRelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreUnavailable => formatter.write_str("workspace relation store unavailable"),
            Self::MissingRelation => formatter.write_str("workspace relation is not selected"),
            Self::InvalidRoot => formatter.write_str("workspace relation root is invalid"),
            Self::Store(error) => write!(formatter, "workspace relation store error: {error:?}"),
            Self::Tree(fault) => write!(formatter, "workspace relation rejected {fault}"),
        }
    }
}

impl std::error::Error for WorkspaceRelationError {}

/// Returns the encoded value size of one relation row and the capacity it had.
fn row_capacity<R: Relation>(key: &R::Key, value: &R::Value) -> (usize, usize) {
    let mut encoded_key = Vec::new();
    R::encode_key(key, &mut encoded_key);
    let mut encoded_value = Vec::new();
    R::encode_value(value, &mut encoded_value);
    let capacity = DEFAULT_CUT_POLICY
        .max_row_value_bytes(encoded_key.len())
        .unwrap_or(0);
    (encoded_value.len(), capacity)
}

/// Blames the first change whose row cannot fit a canonical node.
///
/// A batch rejection names only the failing node, so the offending row is
/// recovered by measuring the same bound the cut kernel applies.  When no row
/// is oversized the node error itself is the whole story.
fn blame_oversized_row<R: Relation>(changes: &[TreeChange<R>]) -> Option<WorkspaceRelationFault> {
    changes.iter().find_map(|change| {
        let value = change.after.as_ref()?;
        let (encoded_bytes, capacity_bytes) = row_capacity::<R>(&change.key, value);
        (encoded_bytes > capacity_bytes).then(|| WorkspaceRelationFault {
            relation: RelationIdentity::of::<R>(),
            key: Some(RelationKeyPrefix::of::<R>(&change.key)),
            rejection: WorkspaceRelationRejection::RowTooLarge {
                encoded_bytes,
                capacity_bytes,
            },
        })
    })
}

fn map_tree_error<R: Relation>(
    error: LazyTreeError<StoreError>,
    key: Option<RelationKeyPrefix>,
) -> WorkspaceRelationError {
    let rejection = match error {
        LazyTreeError::Load(error) => return WorkspaceRelationError::Store(error),
        LazyTreeError::Node(error) => WorkspaceRelationRejection::Node(error),
        LazyTreeError::MissingKey => WorkspaceRelationRejection::MissingKey,
        LazyTreeError::DuplicateKey => WorkspaceRelationRejection::DuplicateKey,
    };
    WorkspaceRelationError::Tree(WorkspaceRelationFault {
        relation: RelationIdentity::of::<R>(),
        key,
        rejection,
    })
}

/// Names the offending row when a rejection is really a capacity failure.
fn refine_rejection<R: Relation>(
    error: WorkspaceRelationError,
    changes: &[TreeChange<R>],
) -> WorkspaceRelationError {
    match (&error, blame_oversized_row::<R>(changes)) {
        (WorkspaceRelationError::Tree(_), Some(fault)) => WorkspaceRelationError::Tree(fault),
        _ => error,
    }
}

/// An owned, checked lazy view of one relation selected by a workspace head.
///
/// The handle owns an `Arc` clone of the filesystem store and the admitted
/// root evidence. It can therefore outlive the owner borrow that produced the
/// snapshot. Cloning it is O(1), and all operations fetch only the affected
/// root-to-leaf path.
#[derive(Debug)]
pub struct WorkspaceRelationHandle<R: CanonicalRelation> {
    store: Arc<FileStore>,
    root: PersistedTreeRoot<R>,
}

impl<R: CanonicalRelation> Clone for WorkspaceRelationHandle<R> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            root: self.root.clone(),
        }
    }
}

/// An opaque node claim issued by a [`WorkspaceRelationHandle`].
///
/// The selected workspace root is retained in the claim, so a node obtained
/// from one snapshot cannot be replayed through a different snapshot even
/// when both snapshots use the same relation schema.  Callers can only obtain
/// claims from [`WorkspaceRelationHandle::root_node`] or a node page.
#[derive(Debug, Eq, PartialEq)]
pub struct WorkspaceRelationNodeHandle<R: CanonicalRelation> {
    selected: StateRoot<R>,
    /// A checked parent only supplies this as an opaque child commitment.
    /// `node_page` re-admits the corresponding canonical node before use.
    root: ChildCommitment<R>,
    object: ObjectId,
    level: u16,
    row_count: u64,
}

impl<R: CanonicalRelation> Copy for WorkspaceRelationNodeHandle<R> {}

impl<R: CanonicalRelation> Clone for WorkspaceRelationNodeHandle<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: CanonicalRelation> WorkspaceRelationNodeHandle<R> {
    /// Returns the opaque node commitment issued by an admitted parent page.
    /// The corresponding node bytes are re-admitted when the handle is used.
    #[must_use]
    pub const fn root(self) -> ChildCommitment<R> {
        self.root
    }

    /// Returns the immutable object identity containing this node.
    #[must_use]
    pub const fn object(self) -> ObjectId {
        self.object
    }

    /// Returns the authenticated node level, with leaves at zero.
    #[must_use]
    pub const fn level(self) -> u16 {
        self.level
    }

    /// Returns the authenticated number of rows below this node.
    #[must_use]
    pub const fn row_count(self) -> u64 {
        self.row_count
    }
}

/// One authenticated child summary in a workspace relation branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRelationChild<R: CanonicalRelation> {
    handle: WorkspaceRelationNodeHandle<R>,
    first_key: R::Key,
    level: u16,
    row_count: u64,
}

impl<R: CanonicalRelation> WorkspaceRelationChild<R> {
    /// Returns the child node claim that may be followed through the same
    /// workspace relation handle.
    #[must_use]
    pub const fn handle(&self) -> WorkspaceRelationNodeHandle<R> {
        self.handle
    }

    /// Returns the authenticated first-key anchor.
    #[must_use]
    pub fn first_key(&self) -> &R::Key {
        &self.first_key
    }

    /// Returns the child level, where leaves are level zero.
    #[must_use]
    pub const fn level(&self) -> u16 {
        self.level
    }

    /// Returns the authenticated number of rows below this child.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }
}

/// One authenticated relation node page.
///
/// A page owns at most the requested number of direct children or leaf rows;
/// all other subtrees remain represented only by their parent commitment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceRelationNodePage<R: CanonicalRelation> {
    /// A bounded page of leaf rows.
    Leaf {
        /// Node claim for the page.
        node: WorkspaceRelationNodeHandle<R>,
        /// Exact canonical proof bytes for the node.
        proof: Arc<[u8]>,
        /// Rows in canonical key order.
        entries: Box<[(R::Key, R::Value)]>,
        /// Whether more rows remain after this page.
        has_more: bool,
    },
    /// A bounded page of authenticated child summaries.
    Branch {
        /// Node claim for the page.
        node: WorkspaceRelationNodeHandle<R>,
        /// Exact canonical proof bytes for the node.
        proof: Arc<[u8]>,
        /// Children in canonical anchor order.
        children: Box<[WorkspaceRelationChild<R>]>,
        /// Whether more children remain after this page.
        has_more: bool,
    },
}

impl<R: CanonicalRelation> WorkspaceRelationNodePage<R> {
    /// Returns the node claim for this page.
    #[must_use]
    pub const fn node(&self) -> WorkspaceRelationNodeHandle<R> {
        match self {
            Self::Leaf { node, .. } | Self::Branch { node, .. } => *node,
        }
    }

    /// Returns the exact canonical proof bytes admitted for this page.
    #[must_use]
    pub fn proof_bytes(&self) -> &[u8] {
        match self {
            Self::Leaf { proof, .. } | Self::Branch { proof, .. } => proof,
        }
    }
}

impl<R: CanonicalRelation> WorkspaceRelationHandle<R> {
    pub(crate) fn open(
        store: Arc<FileStore>,
        root: [u8; 32],
    ) -> Result<Self, WorkspaceRelationError> {
        let claim: UntrustedId<R> = UntrustedId::from_wire(&root, IdContext::relation::<R>())
            .map_err(|_| WorkspaceRelationError::InvalidRoot)?;
        let root = {
            let tree =
                LazyTree::open(store.as_ref(), claim).map_err(|error| map_tree_error::<R>(error, None))?;
            tree.root_handle()
        };
        Ok(Self { store, root })
    }

    /// Returns the authenticated relation root.
    #[must_use]
    pub fn root(&self) -> StateRoot<R> {
        self.root.root()
    }

    /// Returns an O(1) cloneable handoff of the authenticated root evidence.
    #[must_use]
    pub fn root_handle(&self) -> PersistedTreeRoot<R> {
        self.root.clone()
    }

    /// Loads the selected root object without traversing descendants.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn root_object(&self) -> Result<TypedObject, WorkspaceRelationError> {
        self.store
            .read_relation_node(self.root.root())
            .map_err(WorkspaceRelationError::Store)
    }

    /// Opens the selected root and returns its first opaque node claim.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn root_node(&self) -> Result<WorkspaceRelationNodeHandle<R>, WorkspaceRelationError> {
        let node = self
            .store
            .read_checked_relation_node_with_children(self.root.root())
            .map_err(WorkspaceRelationError::Store)?;
        Ok(self.claim_for(&node))
    }

    /// Returns the authenticated first-key anchor of the selected root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn root_first_key(&self) -> Result<Option<R::Key>, WorkspaceRelationError> {
        let node = self
            .store
            .read_checked_relation_node_with_children(self.root.root())
            .map_err(WorkspaceRelationError::Store)?;
        Ok(node.node().node().first_key().cloned())
    }

    /// Reads one node claim previously issued by this handle.
    ///
    /// The returned page contains only the requested direct-child or leaf-row
    /// range.  The canonical proof is copied once into an immutable byte
    /// owner, while untouched descendants are never loaded.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn node_page(
        &self,
        node: WorkspaceRelationNodeHandle<R>,
        offset: usize,
        limit: usize,
    ) -> Result<WorkspaceRelationNodePage<R>, WorkspaceRelationError> {
        if node.selected != self.root.root() || limit == 0 {
            return Err(WorkspaceRelationError::InvalidRoot);
        }
        let claim: UntrustedId<R> =
            UntrustedId::from_wire(node.root.as_bytes(), IdContext::relation::<R>())
                .map_err(|_| WorkspaceRelationError::InvalidRoot)?;
        let read = self
            .store
            .read_relation_node_with_children(claim)
            .map_err(WorkspaceRelationError::Store)?;
        if read.object() != node.object {
            return Err(WorkspaceRelationError::InvalidRoot);
        }
        if read.node().root().to_bytes() != node.root.to_bytes() {
            return Err(WorkspaceRelationError::InvalidRoot);
        }
        self.page_from_read(&read, node, offset, limit)
    }

    /// Returns the canonical proof bytes for one node claim.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn node_proof(
        &self,
        node: WorkspaceRelationNodeHandle<R>,
    ) -> Result<Arc<[u8]>, WorkspaceRelationError> {
        if node.selected != self.root.root() {
            return Err(WorkspaceRelationError::InvalidRoot);
        }
        let claim: UntrustedId<R> =
            UntrustedId::from_wire(node.root.as_bytes(), IdContext::relation::<R>())
                .map_err(|_| WorkspaceRelationError::InvalidRoot)?;
        let read = self
            .store
            .read_relation_node_with_children(claim)
            .map_err(WorkspaceRelationError::Store)?;
        if read.object() != node.object {
            return Err(WorkspaceRelationError::InvalidRoot);
        }
        if read.node().root().to_bytes() != node.root.to_bytes() {
            return Err(WorkspaceRelationError::InvalidRoot);
        }
        Ok(Arc::from(read.node().bytes().to_vec().into_boxed_slice()))
    }

    fn claim_for(&self, read: &RelationNodeRead<R>) -> WorkspaceRelationNodeHandle<R> {
        WorkspaceRelationNodeHandle {
            selected: self.root.root(),
            root: ChildCommitment::from(read.node().root()),
            object: read.object(),
            level: read.node().node().level(),
            row_count: read.node().row_count(),
        }
    }

    fn page_from_read(
        &self,
        read: &RelationNodeRead<R>,
        node: WorkspaceRelationNodeHandle<R>,
        offset: usize,
        limit: usize,
    ) -> Result<WorkspaceRelationNodePage<R>, WorkspaceRelationError> {
        let proof = Arc::from(read.node().bytes().to_vec().into_boxed_slice());
        if read.node().node().level() == 0 {
            let entries = read.node().leaf_entries().map_err(|error| {
                map_tree_error::<R>(LazyTreeError::Node(error), None)
            })?;
            if offset > entries.len() {
                return Err(WorkspaceRelationError::InvalidRoot);
            }
            let end = offset.saturating_add(limit).min(entries.len());
            return Ok(WorkspaceRelationNodePage::Leaf {
                node,
                proof,
                entries: entries[offset..end].to_vec().into_boxed_slice(),
                has_more: end < entries.len(),
            });
        }
        let children = read.children();
        if offset > children.len() {
            return Err(WorkspaceRelationError::InvalidRoot);
        }
        let end = offset.saturating_add(limit).min(children.len());
        let page = children[offset..end]
            .iter()
            .map(|child| WorkspaceRelationChild {
                handle: WorkspaceRelationNodeHandle {
                    selected: self.root.root(),
                    root: child.summary().commitment,
                    object: child.object(),
                    level: child.summary().level,
                    row_count: child.summary().row_count,
                },
                first_key: child.summary().first_key.clone(),
                level: child.summary().level,
                row_count: child.summary().row_count,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(WorkspaceRelationNodePage::Branch {
            node,
            proof,
            children: page,
            has_more: end < children.len(),
        })
    }

    fn tree(&self) -> LazyTree<'_, R, FileStore> {
        LazyTree::from_admitted(self.store.as_ref(), self.root.clone())
    }

    /// Looks up one logical key by loading only its authenticated path.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn lookup(&self, key: &R::Key) -> Result<Option<R::Value>, WorkspaceRelationError> {
        self.tree()
            .lookup(key)
            .map_err(|error| map_tree_error::<R>(error, Some(RelationKeyPrefix::of::<R>(key))))
    }

    /// Reads one bounded page in canonical key order.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn page(
        &self,
        after: Option<&R::Key>,
        limit: usize,
    ) -> Result<LazyTreePage<R>, WorkspaceRelationError> {
        self.tree()
            .page(after, limit)
            .map_err(|error| map_tree_error::<R>(error, None))
    }

    /// Prepares one exact replacement/removal by path-copying affected nodes.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn prepare_change(
        &self,
        key: &R::Key,
        after: Option<R::Value>,
    ) -> Result<LazyPreparedUpdate<R>, WorkspaceRelationError> {
        let changes = [TreeChange {
            key: key.clone(),
            after,
        }];
        let Some(change) = changes.first() else {
            return Err(WorkspaceRelationError::InvalidRoot);
        };
        self.tree()
            .prepare_change(&change.key, change.after.clone())
            .map_err(|error| {
                refine_rejection::<R>(
                    map_tree_error::<R>(error, Some(RelationKeyPrefix::of::<R>(key))),
                    &changes,
                )
            })
    }

    /// Prepares an insertion with canonical duplicate-key admission.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn prepare_insert(
        &self,
        key: &R::Key,
        value: R::Value,
    ) -> Result<LazyPreparedUpdate<R>, WorkspaceRelationError> {
        let changes = [TreeChange {
            key: key.clone(),
            after: Some(value),
        }];
        let Some(change) = changes.first() else {
            return Err(WorkspaceRelationError::InvalidRoot);
        };
        let Some(value) = change.after.clone() else {
            return Err(WorkspaceRelationError::InvalidRoot);
        };
        self.tree()
            .prepare_insert(&change.key, value)
            .map_err(|error| {
                refine_rejection::<R>(
                    map_tree_error::<R>(error, Some(RelationKeyPrefix::of::<R>(key))),
                    &changes,
                )
            })
    }

    /// Prepares removal of one existing key.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn prepare_remove(
        &self,
        key: &R::Key,
    ) -> Result<LazyPreparedUpdate<R>, WorkspaceRelationError> {
        self.tree()
            .prepare_remove(key)
            .map_err(|error| map_tree_error::<R>(error, Some(RelationKeyPrefix::of::<R>(key))))
    }

    /// Prepares one canonical multi-key delta while loading only affected
    /// paths and retaining a checked in-memory overlay for the evolving
    /// frontier.
    /// # Errors
    ///
    /// Returns an error when changes are unordered, a node fails admission,
    /// or the durable loader cannot fetch an affected path.
    pub fn prepare_update(
        &self,
        changes: &[TreeChange<R>],
    ) -> Result<LazyPreparedUpdate<R>, WorkspaceRelationError> {
        self.tree().prepare_update(changes).map_err(|error| {
            refine_rejection::<R>(map_tree_error::<R>(error, None), changes)
        })
    }
}
