//! Store-backed lazy relation capabilities for workspace snapshots.
//!
//! A workspace model must be able to inspect and update one selected relation
//! without turning the immutable closure into a `Vec` of every object or row.
//! This module is the narrow engine/version seam: the store owns node lookup,
//! the version crate owns canonical admission and path-copying, and the model
//! receives only an owned checked handle.

use backend_store::{FileStore, ObjectId, RelationNodeRead, StoreError, TypedObject};
use backend_version::{
    CanonicalRelation, ChildCommitment, IdContext, LazyPreparedUpdate, LazyTree, LazyTreeError,
    LazyTreePage, PersistedTreeRoot, StateRoot, TreeChange, UntrustedId,
};
use std::fmt;
use std::sync::Arc;

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
    Tree,
}

impl fmt::Display for WorkspaceRelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreUnavailable => formatter.write_str("workspace relation store unavailable"),
            Self::MissingRelation => formatter.write_str("workspace relation is not selected"),
            Self::InvalidRoot => formatter.write_str("workspace relation root is invalid"),
            Self::Store(error) => write!(formatter, "workspace relation store error: {error:?}"),
            Self::Tree => formatter.write_str("workspace relation node was rejected"),
        }
    }
}

impl std::error::Error for WorkspaceRelationError {}

fn map_tree_error(error: LazyTreeError<StoreError>) -> WorkspaceRelationError {
    match error {
        LazyTreeError::Load(error) => WorkspaceRelationError::Store(error),
        LazyTreeError::Node(_) | LazyTreeError::MissingKey | LazyTreeError::DuplicateKey => {
            WorkspaceRelationError::Tree
        }
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
            let tree = LazyTree::open(store.as_ref(), claim).map_err(map_tree_error)?;
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
            let entries = read
                .node()
                .leaf_entries()
                .map_err(|_| WorkspaceRelationError::Tree)?;
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
        self.tree().lookup(key).map_err(map_tree_error)
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
        self.tree().page(after, limit).map_err(map_tree_error)
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
        self.tree()
            .prepare_change(key, after)
            .map_err(map_tree_error)
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
        self.tree()
            .prepare_insert(key, value)
            .map_err(map_tree_error)
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
        self.tree().prepare_remove(key).map_err(map_tree_error)
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
        self.tree().prepare_update(changes).map_err(map_tree_error)
    }
}
