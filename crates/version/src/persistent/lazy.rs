//! Lazy authenticated reopening and path copy over persisted canonical nodes.

use crate::tree::{anchored_cut_points_children, anchored_cut_points_items};
use crate::{
    CanonicalNode, CanonicalRelation, CanonicalRootAdmissionError, CheckedCanonicalRoot,
    ChildCommitment, CommittedChild, DEFAULT_CUT_POLICY, IdContext, MapChange, NodeError,
    PersistentTree, StateRoot, TreeChange, TreeError, UntrustedId, admit_canonical_root_claim,
    canonical_branch_from_commitments, canonical_empty, canonical_leaf,
};
use std::{borrow::Borrow, cell::Cell};

use super::TreeNodeLoader;

#[path = "lazy/helpers.rs"]
mod helpers;
use helpers::{
    OverlayLoader, child_claim, child_node_from_result, committed_child, leaf_probe_cuts,
    make_branches, make_leaves,
};

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

/// Explicit work performed by one lazy update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LazyTreeWork {
    /// Number of authenticated nodes fetched from the loader.
    pub loaded_nodes: usize,
    /// Number of canonical nodes emitted into the changed frontier.
    pub rebuilt_nodes: usize,
    /// Number of extra nodes created by split propagation.
    pub split_nodes: usize,
    /// Number of logical entries removed.
    pub removed_entries: usize,
    /// Total bytes in the changed frontier.
    pub emitted_bytes: usize,
}

struct RewriteResult<R: CanonicalRelation> {
    roots: Vec<CanonicalNode<R>>,
    changed: Vec<CanonicalNode<R>>,
    split_nodes: usize,
    removed_entries: usize,
    change: MapChange<R>,
}

/// A checked target root produced by a lazy path copy.
#[derive(Debug)]
pub struct LazyPreparedUpdate<R: CanonicalRelation> {
    base: StateRoot<R>,
    target: CheckedCanonicalRoot<R>,
    changed: Vec<CanonicalNode<R>>,
    work: LazyTreeWork,
    changes: Vec<MapChange<R>>,
}

/// A bounded page read from a lazily opened canonical relation.
#[derive(Debug)]
pub struct LazyTreePage<R: CanonicalRelation> {
    entries: Vec<(R::Key, R::Value)>,
    next: Option<R::Key>,
}

impl<R: CanonicalRelation> LazyTreePage<R> {
    /// Returns the owned rows in canonical key order.
    #[must_use]
    pub fn entries(&self) -> &[(R::Key, R::Value)] {
        &self.entries
    }

    /// Returns the last emitted key when another page remains.
    #[must_use]
    pub fn next(&self) -> Option<&R::Key> {
        self.next.as_ref()
    }
}

/// Owned proof that one persisted relation root has passed canonical admission.
///
/// This handle carries the root node and schema-bound evidence without any
/// logical rows. It is the safe handoff between closure admission and a
/// store-backed [`LazyTree`].
#[derive(Debug)]
pub struct PersistedTreeRoot<R: CanonicalRelation> {
    evidence: CheckedCanonicalRoot<R>,
}

impl<R: CanonicalRelation> Clone for PersistedTreeRoot<R> {
    fn clone(&self) -> Self {
        Self {
            evidence: self.evidence.clone(),
        }
    }
}

impl<R: CanonicalRelation> PersistedTreeRoot<R> {
    /// Reuses a canonical root already admitted by a typed node loader.
    #[must_use]
    pub const fn from_checked(evidence: CheckedCanonicalRoot<R>) -> Self {
        Self { evidence }
    }

    /// Admits canonical root bytes against an untrusted relation-root claim.
    ///
    /// # Errors
    /// Returns a canonical grammar, schema, or digest admission error.
    pub fn admit(claim: UntrustedId<R>, bytes: &[u8]) -> Result<Self, CanonicalRootAdmissionError> {
        Ok(Self {
            evidence: admit_canonical_root_claim(claim, bytes)?,
        })
    }

    /// Returns the admitted typed state root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.evidence.root()
    }
    /// Returns the schema identity proven by the relation marker.
    #[must_use]
    pub const fn schema(&self) -> crate::SchemaIdentity {
        crate::SchemaIdentity::new(R::DOMAIN, R::TYPE, R::VERSION)
    }
    /// Returns the admitted canonical node evidence.
    #[must_use]
    pub const fn evidence(&self) -> &CheckedCanonicalRoot<R> {
        &self.evidence
    }
}

impl<R: CanonicalRelation> LazyPreparedUpdate<R> {
    /// Returns the exact root opened before the update.
    #[must_use]
    pub const fn base(&self) -> StateRoot<R> {
        self.base
    }
    /// Returns the checked target root descriptor.
    #[must_use]
    pub const fn target(&self) -> &CheckedCanonicalRoot<R> {
        &self.target
    }
    /// Consumes the update and returns its checked target descriptor.
    #[must_use]
    pub fn into_target(self) -> CheckedCanonicalRoot<R> {
        self.target
    }

    /// Returns an owned persisted-root handoff for storage publication.
    #[must_use]
    pub fn target_root(&self) -> PersistedTreeRoot<R> {
        PersistedTreeRoot {
            evidence: self.target.clone(),
        }
    }

    /// Clones the exact single-key transition prepared by this path copy.
    ///
    /// The clone is cheap for the canonical byte body and is intended for a
    /// storage adapter that must publish the changed frontier before it
    /// consumes this capability.
    #[must_use]
    pub fn delta(&self) -> super::super::delta::Delta<R> {
        super::super::delta::Delta::from_parts(self.base, self.target.root(), self.changes.clone())
    }

    /// Consumes this path-copy update and returns its exact typed delta.
    ///
    /// The target node remains separately available through
    /// [`Self::into_target`] when a storage adapter needs to write the
    /// changed frontier before selecting the new root.
    #[must_use]
    pub fn into_delta(self) -> super::super::delta::Delta<R> {
        super::super::delta::Delta::from_parts(self.base, self.target.root(), self.changes)
    }
    /// Borrows newly encoded nodes in child-to-root order for store CAS.
    #[must_use]
    pub fn changed_nodes(&self) -> &[CanonicalNode<R>] {
        &self.changed
    }
    /// Returns bounded loader, rebuild, split, and byte counters.
    #[must_use]
    pub const fn work(&self) -> LazyTreeWork {
        self.work
    }
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

    /// Replaces an existing value by loading its root-to-leaf path.
    ///
    /// # Errors
    /// Returns an error if the key is absent, a path node fails admission, or
    /// the loader cannot fetch an affected node.
    pub fn prepare_replace<Q: ?Sized + Ord>(
        &self,
        key: &Q,
        value: R::Value,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        self.finish(key, Some(value), None, false)
    }

    /// Inserts a key, splitting affected leaves and propagating branch splits.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::DuplicateKey`] for an existing key, or a
    /// loader/admission error for an affected persisted node.
    pub fn prepare_insert(
        &self,
        key: &R::Key,
        value: R::Value,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.finish(key, Some(value), Some(key), true)
    }

    /// Applies one replacement or removal and propagates canonical rebalances.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::MissingKey`] when the key is absent, or a
    /// loader/admission error for an affected persisted node.
    pub fn prepare_change(
        &self,
        key: &R::Key,
        after: Option<R::Value>,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.finish(key, after, None, false)
    }

    /// Removes an existing key, shrinking an empty or single-child root.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::MissingKey`] when the key is absent, or a
    /// loader/admission error for an affected persisted node.
    pub fn prepare_remove(
        &self,
        key: &R::Key,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.prepare_change(key, None)
    }

    /// Prepares an ordered set of independent key changes against one exact
    /// persisted root.
    ///
    /// Each step reuses the prior step's immutable frontier through an
    /// in-memory checked-node overlay. Unchanged descendants continue to load
    /// from the original store, so a batch retains O(changed frontier) memory
    /// and never hydrates the full relation. The resulting delta is rooted at
    /// the original source and contains one canonical before/after entry per
    /// effective key.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::Node`] when keys are not strictly ordered or a
    /// constructed frontier fails its own canonical admission, and forwards
    /// lookup/path-copy errors from the underlying loader.
    pub fn prepare_update(
        &self,
        changes: &[TreeChange<R>],
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        if changes
            .windows(2)
            .any(|window| window[0].key >= window[1].key)
        {
            return Err(LazyTreeError::Node(NodeError::UnsortedOrDuplicate));
        }

        // A first ingest is a construction, not a sequence of insertions. Build
        // its canonical tree once from the already sorted relation instead of
        // repeatedly path-copying an ever-growing temporary frontier. Besides
        // removing O(n log n) hashing and admission, this emits every immutable
        // node exactly once in child-to-root storage order.
        if self.root.row_count() == 0 && changes.iter().all(|change| change.after.is_some()) {
            return self.prepare_empty_bulk(changes);
        }

        let mut overlay = OverlayLoader::<R, L>::new(self.loader);
        let mut root = self.root_handle();
        let mut frontier = Vec::new();
        let mut effective = Vec::with_capacity(changes.len());
        let mut work = LazyTreeWork::default();

        for change in changes {
            let tree = LazyTree::from_admitted(&overlay, root);
            let before = tree.lookup(&change.key)?;
            if before == change.after {
                work.loaded_nodes = work.loaded_nodes.saturating_add(tree.loaded_nodes.get());
                root = tree.root_handle();
                continue;
            }
            let update = match (&before, &change.after) {
                (None, Some(after)) => tree.prepare_insert(&change.key, after.clone())?,
                (Some(_), after) => tree.prepare_change(&change.key, after.clone())?,
                (None, None) => {
                    root = tree.root_handle();
                    continue;
                }
            };
            let step = update.work();
            work.loaded_nodes = work.loaded_nodes.saturating_add(tree.loaded_nodes.get());
            work.rebuilt_nodes = work.rebuilt_nodes.saturating_add(step.rebuilt_nodes);
            work.split_nodes = work.split_nodes.saturating_add(step.split_nodes);
            work.removed_entries = work.removed_entries.saturating_add(step.removed_entries);
            work.emitted_bytes = work.emitted_bytes.saturating_add(step.emitted_bytes);
            effective.extend(update.changes.iter().cloned());
            for node in update.changed_nodes() {
                overlay.insert(node)?;
                frontier.push(node.clone());
            }
            root = update.target_root();
        }

        Ok(LazyPreparedUpdate {
            base: self.root.root(),
            target: root.evidence,
            changed: frontier,
            work,
            changes: effective,
        })
    }

    fn prepare_empty_bulk(
        &self,
        changes: &[TreeChange<R>],
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        let items = changes
            .iter()
            .map(|change| {
                change
                    .after
                    .clone()
                    .map(|value| (change.key.clone(), value))
                    .ok_or(LazyTreeError::Node(NodeError::InvalidBranch))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (tree, tree_work) = PersistentTree::from_sorted_items_with_work(&items)
            .map_err(|error| LazyTreeError::Node(map_eager_tree_error(error)))?;
        let target_node = tree.root().clone();
        let mut changed_nodes = tree
            .node_closure()
            .map(|node| node.canonical().clone())
            .collect::<Vec<_>>();
        changed_nodes.reverse();
        let changes = changes
            .iter()
            .map(|change| MapChange {
                key: change.key.clone(),
                before: None,
                after: change.after.clone(),
            })
            .collect();
        Ok(LazyPreparedUpdate {
            base: self.root.root(),
            target: CheckedCanonicalRoot::from_parts(target_node.commitment(), target_node),
            changed: changed_nodes,
            work: LazyTreeWork {
                loaded_nodes: 0,
                rebuilt_nodes: tree_work.nodes,
                split_nodes: 0,
                removed_entries: 0,
                emitted_bytes: tree_work.encoded_bytes,
            },
            changes,
        })
    }

    fn load(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<CheckedCanonicalRoot<R>, LazyTreeError<L::Error>> {
        if claim.context() != IdContext::relation::<R>() {
            return Err(LazyTreeError::Node(NodeError::SchemaMismatch));
        }
        let node = self.loader.load(claim).map_err(LazyTreeError::Load)?;
        self.loaded_nodes
            .set(self.loaded_nodes.get().saturating_add(1));
        if node.root().as_bytes() != claim.as_bytes() {
            return Err(LazyTreeError::Node(NodeError::AnchorMismatch));
        }
        Ok(node)
    }

    fn finish<Q: ?Sized + Ord>(
        &self,
        key: &Q,
        after: Option<R::Value>,
        insert_key: Option<&R::Key>,
        insert_only: bool,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        let loaded_before = self.loaded_nodes.get();
        let mut result = self.rewrite(&self.root, key, after, insert_key, insert_only, true)?;
        let mut roots = result.roots;
        let mut level = roots.first().map_or(0, CanonicalNode::level);
        while roots.len() > 1 {
            let summaries = roots
                .iter()
                .map(committed_child)
                .collect::<Result<Vec<_>, _>>()
                .map_err(LazyTreeError::Node)?;
            let keys: Vec<_> = summaries
                .iter()
                .map(|child| child.first_key.clone())
                .collect();
            let cuts = anchored_cut_points_children::<R, _>(
                keys.iter(),
                DEFAULT_CUT_POLICY,
                level.saturating_add(1),
            )
            .map_err(|_| LazyTreeError::Node(NodeError::InvalidBranch))?;
            let mut parents = Vec::new();
            let mut start = 0;
            for end in cuts {
                parents.push(
                    canonical_branch_from_commitments::<R>(
                        level.saturating_add(1),
                        &summaries[start..end],
                    )
                    .map_err(LazyTreeError::Node)?,
                );
                start = end;
            }
            result.split_nodes = result
                .split_nodes
                .saturating_add(parents.len().saturating_sub(1));
            result.changed.extend(parents.iter().cloned());
            roots = parents;
            level = level.saturating_add(1);
        }
        let target = roots
            .pop()
            .ok_or(LazyTreeError::Node(NodeError::InvalidBranch))?;
        let emitted_bytes = result
            .changed
            .iter()
            .map(|node| node.as_bytes().len())
            .sum();
        let work = LazyTreeWork {
            loaded_nodes: self.loaded_nodes.get().saturating_sub(loaded_before),
            rebuilt_nodes: result.changed.len(),
            split_nodes: result.split_nodes,
            removed_entries: result.removed_entries,
            emitted_bytes,
        };
        Ok(LazyPreparedUpdate {
            base: self.root.root(),
            target: CheckedCanonicalRoot::from_parts(target.commitment(), target),
            changed: result.changed,
            work,
            changes: vec![result.change],
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the recursive path-copy state machine is kept together"
    )]
    fn rewrite<Q: ?Sized + Ord>(
        &self,
        node: &CheckedCanonicalRoot<R>,
        key: &Q,
        after: Option<R::Value>,
        insert_key: Option<&R::Key>,
        insert_only: bool,
        is_root: bool,
    ) -> Result<RewriteResult<R>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        if node.node().level() == 0 {
            let mut entries = node.leaf_entries().map_err(LazyTreeError::Node)?;
            let mut removed_entries = 0;
            let change = match (
                entries.binary_search_by(|(candidate, _)| candidate.borrow().cmp(key)),
                after,
            ) {
                (Ok(index), Some(value)) if !insert_only => {
                    let change = MapChange {
                        key: entries[index].0.clone(),
                        before: Some(entries[index].1.clone()),
                        after: Some(value.clone()),
                    };
                    entries[index].1 = value;
                    change
                }
                (Ok(_), Some(_)) => return Err(LazyTreeError::DuplicateKey),
                (Ok(index), None) => {
                    let change = MapChange {
                        key: entries[index].0.clone(),
                        before: Some(entries[index].1.clone()),
                        after: None,
                    };
                    entries.remove(index);
                    removed_entries = 1;
                    change
                }
                (Err(index), Some(value)) => {
                    let owned = insert_key.cloned().ok_or(LazyTreeError::MissingKey)?;
                    let change = MapChange {
                        key: owned.clone(),
                        before: None,
                        after: Some(value.clone()),
                    };
                    entries.insert(index, (owned, value));
                    change
                }
                (Err(_), None) => return Err(LazyTreeError::MissingKey),
            };
            if entries.is_empty() && !is_root {
                return Ok(RewriteResult {
                    roots: Vec::new(),
                    changed: Vec::new(),
                    split_nodes: 0,
                    removed_entries,
                    change,
                });
            }
            let roots = make_leaves::<R>(&entries).map_err(LazyTreeError::Node)?;
            let split_nodes = roots.len().saturating_sub(1);
            return Ok(RewriteResult {
                changed: roots.clone(),
                roots,
                split_nodes,
                removed_entries,
                change,
            });
        }

        let mut summaries = node.child_summaries().map_err(LazyTreeError::Node)?;
        if summaries.is_empty() {
            return Err(LazyTreeError::Node(NodeError::InvalidBranch));
        }
        let index = summaries
            .partition_point(|child| child.first_key.borrow() <= key)
            .saturating_sub(1);
        let claim = child_claim(&summaries[index]).map_err(LazyTreeError::Node)?;
        let child = self.load(claim)?;
        let deleting = after.is_none() && !insert_only;
        let mut result = self.rewrite(&child, key, after, insert_key, insert_only, false)?;

        // A changed leaf can move a content-defined boundary into the next
        // leaf. Load only the adjacent region until a complete boundary is
        // reached.
        if node.node().level() == 1 && (deleting || result.roots.len() > 1) {
            let obsolete = result.roots.len();
            let changed_before = result.changed.len();
            let (replacement, loaded_start, loaded_end) =
                self.spill_leaf_region(&mut result, &summaries, index)?;
            // The leaf region helper replaces the provisional split/merge
            // nodes. Descendant nodes, if any, remain in the frontier.
            result
                .changed
                .truncate(changed_before.saturating_sub(obsolete));
            result.changed.extend(replacement.iter().cloned());
            result.roots = replacement;
            summaries.splice(
                loaded_start..=loaded_end,
                result
                    .roots
                    .iter()
                    .map(committed_child)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(LazyTreeError::Node)?,
            );
        } else if node.node().level() > 1 && (deleting || result.roots.len() > 1) {
            let obsolete = result.roots.len();
            let changed_before = result.changed.len();
            let (replacement, loaded_start, loaded_end) =
                self.spill_branch_region(&mut result, &summaries, index, node.node().level())?;
            result
                .changed
                .truncate(changed_before.saturating_sub(obsolete));
            result.changed.extend(replacement.iter().cloned());
            result.roots = replacement;
            summaries.splice(
                loaded_start..=loaded_end,
                result
                    .roots
                    .iter()
                    .map(committed_child)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(LazyTreeError::Node)?,
            );
        } else {
            summaries.splice(
                index..=index,
                result
                    .roots
                    .iter()
                    .map(committed_child)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(LazyTreeError::Node)?,
            );
        }
        if summaries.is_empty() {
            if !is_root {
                return Ok(RewriteResult {
                    roots: Vec::new(),
                    changed: result.changed,
                    split_nodes: result.split_nodes,
                    removed_entries: result.removed_entries,
                    change: result.change,
                });
            }
            let empty = canonical_empty::<R>();
            result.changed.push(empty.clone());
            return Ok(RewriteResult {
                roots: vec![empty],
                changed: result.changed,
                split_nodes: result.split_nodes,
                removed_entries: result.removed_entries,
                change: result.change,
            });
        }
        if is_root && summaries.len() == 1 {
            let only = child_node_from_result(&result, &summaries[0]);
            if let Some(only) = only {
                result.roots = vec![only];
                return Ok(result);
            }
        }
        let keys: Vec<_> = summaries
            .iter()
            .map(|child| child.first_key.clone())
            .collect();
        let cuts = anchored_cut_points_children::<R, _>(
            keys.iter(),
            DEFAULT_CUT_POLICY,
            node.node().level(),
        )
        .map_err(|_| LazyTreeError::Node(NodeError::InvalidBranch))?;
        let mut branches = Vec::new();
        let mut start = 0;
        for end in cuts {
            branches.push(
                canonical_branch_from_commitments::<R>(node.node().level(), &summaries[start..end])
                    .map_err(LazyTreeError::Node)?,
            );
            start = end;
        }
        result.split_nodes = result
            .split_nodes
            .saturating_add(branches.len().saturating_sub(1));
        result.changed.extend(branches.iter().cloned());
        result.roots = branches;
        Ok(result)
    }

    #[allow(
        clippy::type_complexity,
        reason = "the tuple carries the changed child range"
    )]
    fn spill_leaf_region(
        &self,
        result: &mut RewriteResult<R>,
        summaries: &[CommittedChild<R>],
        index: usize,
    ) -> Result<(Vec<CanonicalNode<R>>, usize, usize), LazyTreeError<L::Error>> {
        let mut entries = result.roots.first().map(|_| Vec::new()).unwrap_or_default();
        for leaf in &result.roots {
            entries.extend(
                CheckedCanonicalRoot::from_parts(leaf.commitment(), leaf.clone())
                    .leaf_entries()
                    .map_err(LazyTreeError::Node)?,
            );
        }
        let mut end = index;
        while end + 1 < summaries.len() {
            let next_claim = child_claim(&summaries[end + 1]).map_err(LazyTreeError::Node)?;
            let next = self.load(next_claim)?;
            entries.extend(next.leaf_entries().map_err(LazyTreeError::Node)?);
            end += 1;
            let probe = summaries.get(end + 1).map(|child| child.first_key.clone());
            let cuts = leaf_probe_cuts::<R>(&entries, probe.as_ref())
                .map_err(|_| LazyTreeError::Node(NodeError::InvalidBranch))?;
            if cuts.contains(&entries.len()) {
                break;
            }
        }
        if entries.is_empty() {
            return Ok((Vec::new(), index, end));
        }
        if end == index && index > 0 {
            let previous_claim = child_claim(&summaries[index - 1]).map_err(LazyTreeError::Node)?;
            let previous = self.load(previous_claim)?;
            let mut combined = previous.leaf_entries().map_err(LazyTreeError::Node)?;
            combined.extend(entries);
            let start = index - 1;
            let leaves = make_leaves::<R>(&combined).map_err(LazyTreeError::Node)?;
            result.split_nodes = result
                .split_nodes
                .saturating_add(leaves.len().saturating_sub(1));
            return Ok((leaves, start, index));
        }
        let leaves = make_leaves::<R>(&entries).map_err(LazyTreeError::Node)?;
        result.split_nodes = result
            .split_nodes
            .saturating_add(leaves.len().saturating_sub(1));
        Ok((leaves, index, end))
    }

    #[allow(
        clippy::type_complexity,
        reason = "the tuple carries the changed child range"
    )]
    fn spill_branch_region(
        &self,
        result: &mut RewriteResult<R>,
        summaries: &[CommittedChild<R>],
        index: usize,
        parent_level: u16,
    ) -> Result<(Vec<CanonicalNode<R>>, usize, usize), LazyTreeError<L::Error>> {
        let child_level = parent_level.saturating_sub(1);
        let mut children = result
            .roots
            .iter()
            .map(|branch| {
                CheckedCanonicalRoot::from_parts(branch.commitment(), branch.clone())
                    .child_summaries()
                    .map_err(LazyTreeError::Node)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let mut end = index;
        while end + 1 < summaries.len() {
            let next = self.load(child_claim(&summaries[end + 1]).map_err(LazyTreeError::Node)?)?;
            children.extend(next.child_summaries().map_err(LazyTreeError::Node)?);
            end += 1;
            let keys: Vec<_> = children
                .iter()
                .map(|child| child.first_key.clone())
                .collect();
            let probe = summaries.get(end + 1).map(|child| child.first_key.clone());
            let mut probe_keys = keys.clone();
            if let Some(probe) = probe {
                probe_keys.push(probe);
            }
            let cuts = anchored_cut_points_children::<R, _>(
                probe_keys.iter(),
                DEFAULT_CUT_POLICY,
                child_level,
            )
            .map_err(|_| LazyTreeError::Node(NodeError::InvalidBranch))?;
            if cuts.contains(&keys.len()) {
                break;
            }
        }
        if children.is_empty() {
            return Ok((Vec::new(), index, end));
        }
        if end == index && index > 0 {
            let previous =
                self.load(child_claim(&summaries[index - 1]).map_err(LazyTreeError::Node)?)?;
            let mut combined = previous.child_summaries().map_err(LazyTreeError::Node)?;
            combined.extend(children);
            children = combined;
            let leaves = make_branches::<R>(child_level, &children).map_err(LazyTreeError::Node)?;
            result.split_nodes = result
                .split_nodes
                .saturating_add(leaves.len().saturating_sub(1));
            return Ok((leaves, index - 1, index));
        }
        let branches = make_branches::<R>(child_level, &children).map_err(LazyTreeError::Node)?;
        result.split_nodes = result
            .split_nodes
            .saturating_add(branches.len().saturating_sub(1));
        Ok((branches, index, end))
    }
}

fn map_eager_tree_error(error: TreeError) -> NodeError {
    match error {
        TreeError::UnsortedOrDuplicate => NodeError::UnsortedOrDuplicate,
        TreeError::Canonical(error) => error,
        TreeError::InvalidRoot | TreeError::Overflow => NodeError::InvalidBranch,
    }
}
