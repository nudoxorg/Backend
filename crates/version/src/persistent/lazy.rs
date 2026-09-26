//! Lazy authenticated reopening and path copy over persisted canonical nodes.

use crate::tree::{anchored_cut_points_children, anchored_cut_points_items};
use crate::{
    CanonicalNode, CanonicalRelation, CheckedCanonicalRoot, ChildCommitment, CommittedChild,
    DEFAULT_CUT_POLICY, IdContext, MapChange, NodeError, UntrustedId,
    canonical_branch_from_commitments, canonical_empty, canonical_leaf,
};
use std::{borrow::Borrow, cell::Cell};

use super::TreeNodeLoader;

#[path = "lazy/helpers.rs"]
mod helpers;
mod types;
mod rewrite;
use helpers::child_claim;
pub use types::{LazyPreparedUpdate, LazyTreePage, LazyTreeWork, PersistedTreeRoot};

/// Error while opening or path copying a lazily loaded canonical tree.
#[derive(Debug, Eq, PartialEq)]
pub enum LazyTreeError<E> {
    /// The loader could not provide a requested node.
    Load(E),
    /// A loaded node failed canonical grammar or path validation.
    Node(NodeError),
    /// A removal targeted a key that was absent.
    MissingKey,
    /// An insertion targeted a key already present in the tree.
    DuplicateKey,
}

impl<E: std::fmt::Display> std::fmt::Display for LazyTreeError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(error) => write!(f, "lazy tree node load failed: {error}"),
            Self::Node(error) => write!(f, "lazy tree node rejected: {error}"),
            Self::MissingKey => f.write_str("lazy tree change targeted an absent key"),
            Self::DuplicateKey => f.write_str("lazy tree insertion targeted an existing key"),
        }
    }
}
impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for LazyTreeError<E> {}

struct RewriteResult<R: CanonicalRelation> {
    roots: Vec<CanonicalNode<R>>,
    changed: Vec<CanonicalNode<R>>,
    split_nodes: usize,
    removed_entries: usize,
    change: MapChange<R>,
}

/// A lazily opened canonical root with a store supplied node loader.
pub struct LazyTree<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> {
    root: CheckedCanonicalRoot<R>,
    loader: &'a L,
    loaded_nodes: Cell<usize>,
}
impl<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> LazyTree<'a, R, L> {
    /// Opens a root through the loader after preserving its typed claim.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::Load`] for loader failures.
    pub fn open(loader: &'a L, claim: UntrustedId<R>) -> Result<Self, LazyTreeError<L::Error>> {
        // A loader is allowed to use the claim as its storage lookup key, but
        // a matching digest alone is not enough to establish the relation
        // identity.  Reject a caller-supplied object/delta/foreign-schema
        // context before invoking storage so the typed `R` marker is also
        // enforced at this admission boundary.
        if claim.context() != IdContext::relation::<R>() {
            return Err(LazyTreeError::Node(NodeError::SchemaMismatch));
        }
        let root = loader.load(claim).map_err(LazyTreeError::Load)?;
        if root.root().as_bytes() != claim.as_bytes() {
            return Err(LazyTreeError::Node(NodeError::AnchorMismatch));
        }
        Ok(Self {
            root,
            loader,
            loaded_nodes: Cell::new(1),
        })
    }

    /// Opens an already admitted persisted root without fetching its node.
    ///
    /// This is O(1) in relation rows and is useful when closure admission has
    /// already retained the root evidence.
    #[must_use]
    pub fn from_admitted(loader: &'a L, root: PersistedTreeRoot<R>) -> Self {
        Self {
            root: root.evidence,
            loader,
            loaded_nodes: Cell::new(0),
        }
    }

    /// Returns the checked root loaded at open time.
    #[must_use]
    pub const fn root(&self) -> &CheckedCanonicalRoot<R> {
        &self.root
    }

    /// Returns an owned checked root handle for a later loader handoff.
    #[must_use]
    pub fn root_handle(&self) -> PersistedTreeRoot<R> {
        PersistedTreeRoot {
            evidence: self.root.clone(),
        }
    }

    /// Looks up one key by loading only its authenticated root-to-leaf path.
    ///
    /// # Errors
    /// Returns a loader or canonical-node error if an affected path node
    /// cannot be fetched or admitted.
    pub fn lookup<Q: ?Sized + Ord>(
        &self,
        key: &Q,
    ) -> Result<Option<R::Value>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        let mut node = self.root.clone();
        loop {
            if node.node().level() == 0 {
                let entries = node.leaf_entries().map_err(LazyTreeError::Node)?;
                return Ok(entries
                    .binary_search_by(|(candidate, _)| candidate.borrow().cmp(key))
                    .ok()
                    .map(|index| entries[index].1.clone()));
            }
            let summaries = node.child_summaries().map_err(LazyTreeError::Node)?;
            if summaries.is_empty() {
                return Err(LazyTreeError::Node(NodeError::InvalidBranch));
            }
            let index = summaries
                .partition_point(|child| child.first_key.borrow() <= key)
                .saturating_sub(1);
            let claim = child_claim(&summaries[index]).map_err(LazyTreeError::Node)?;
            node = self.load(claim)?;
        }
    }

    /// Reads at most `limit` rows after an optional canonical key.  Branches
    /// and leaves are fetched only until the requested page is full, so a
    /// status cursor performs O(tree height + page size) authenticated work.
    ///
    /// # Errors
    /// Returns a loader or canonical-node error if an affected path node
    /// cannot be fetched or admitted.
    pub fn page(
        &self,
        after: Option<&R::Key>,
        limit: usize,
    ) -> Result<LazyTreePage<R>, LazyTreeError<L::Error>> {
        if limit == 0 {
            return Ok(LazyTreePage {
                entries: Vec::new(),
                next: None,
            });
        }
        let mut stack = vec![self.root.clone()];
        let mut entries: Vec<(R::Key, R::Value)> = Vec::with_capacity(limit.min(256));
        while let Some(node) = stack.pop() {
            if node.node().level() == 0 {
                let leaf = node.leaf_entries().map_err(LazyTreeError::Node)?;
                for (key, value) in leaf {
                    if after.is_some_and(|after| key <= *after) {
                        continue;
                    }
                    if entries.len() == limit {
                        let next = entries.last().map(|(key, _)| key.clone());
                        return Ok(LazyTreePage { entries, next });
                    }
                    entries.push((key.clone(), value.clone()));
                }
                continue;
            }
            let children = node.child_summaries().map_err(LazyTreeError::Node)?;
            for child in children.into_iter().rev() {
                stack.push(self.load(child_claim(&child).map_err(LazyTreeError::Node)?)?);
            }
        }
        Ok(LazyTreePage {
            entries,
            next: None,
        })
    }
}

