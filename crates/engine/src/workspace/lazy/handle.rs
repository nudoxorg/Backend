//! Lazy workspace relation handle operations.

use std::sync::Arc;

use backend_store::{FileStore, RelationNodeRead, TypedObject};
use backend_version::{
    CanonicalRelation, ChildCommitment, IdContext, LazyPreparedUpdate, LazyTree, LazyTreeError,
    LazyTreePage, PersistedTreeRoot, StateRoot, TreeChange, UntrustedId,
};

use super::{
    RelationKeyPrefix, WorkspaceRelationChild, WorkspaceRelationError, WorkspaceRelationHandle,
    WorkspaceRelationNodeHandle, WorkspaceRelationNodePage, map_tree_error, refine_rejection,
};

impl<R: CanonicalRelation> WorkspaceRelationHandle<R> {
    pub(crate) fn open(
        store: Arc<FileStore>,
        root: [u8; 32],
    ) -> Result<Self, WorkspaceRelationError> {
        let claim: UntrustedId<R> = UntrustedId::from_wire(&root, IdContext::relation::<R>())
            .map_err(|_| WorkspaceRelationError::InvalidRoot)?;
        let root = {
            let tree = LazyTree::open(store.as_ref(), claim)
                .map_err(|error| map_tree_error::<R>(error, None))?;
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
                .map_err(|error| map_tree_error::<R>(LazyTreeError::Node(error), None))?;
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
        self.tree()
            .prepare_update(changes)
            .map_err(|error| refine_rejection::<R>(map_tree_error::<R>(error, None), changes))
    }
}
