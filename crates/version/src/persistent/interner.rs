use super::build::{build_tree, empty_node};
use super::update::{
    apply_structural, collect_target_range, get_node, replace_existing, target_shape,
};
use super::{Item, Node, PersistentTree, PreparedUpdate, TreeChange, TreeWork};
use super::{
    TreeNodeClosure, TreeNodeView, TreeRangeIter, TreeZipper, iter::TreeIter, node::TreeNodeHandle,
    work::TreeError,
};
use crate::Relation;
use std::borrow::Borrow;

/// Marker selecting the version kernel's dependency-free node storage.
///
/// Other crates may name the marker in a type parameter, but construction of
/// persistent trees remains available only through checked constructors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoInterner;

/// Safe callback used by the canonical kernel to deduplicate immutable nodes.
///
/// Implementations may return an existing handle with the same commitment
/// (for example from a weak CAS).  Handles can only be obtained from this
/// kernel, and all returned nodes have already passed canonical construction.
pub trait TreeInterner<R: Relation>: Clone {
    /// Retains or deduplicates one checked node handle.
    fn intern(&self, node: TreeNodeHandle<R>) -> TreeNodeHandle<R>;
}

impl<R: Relation> TreeInterner<R> for NoInterner {
    fn intern(&self, node: TreeNodeHandle<R>) -> TreeNodeHandle<R> {
        node
    }
}

/// Runs an adapter callback while preserving the candidate node's checked
/// commitment and structural summary.
pub(super) fn checked_intern<R: Relation, I: TreeInterner<R>>(
    interner: &I,
    node: TreeNodeHandle<R>,
) -> Result<Node<R>, TreeError> {
    let expected = node.clone();
    let retained = interner.intern(node);
    if !expected.same_structure(&retained) {
        return Err(TreeError::InvalidRoot);
    }
    Ok(retained.node)
}

fn interner_empty<R: Relation, I: TreeInterner<R>>(
    interner: &I,
    work: &mut TreeWork,
) -> Result<Node<R>, TreeError> {
    let node = empty_node();
    work.rebuild_node(&node)?;
    checked_intern(interner, TreeNodeHandle { node })
}

impl<R: Relation> PersistentTree<R, NoInterner> {
    /// Creates an empty checked canonical tree.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            root: empty_node(),
            interner: NoInterner,
        }
    }

    /// Builds a canonical tree from strictly ordered rows.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::UnsortedOrDuplicate`] for malformed ordering or
    /// [`TreeError::Canonical`] when a node exceeds canonical limits.
    pub fn from_sorted_items(items: &[(R::Key, R::Value)]) -> Result<Self, TreeError> {
        Self::from_sorted_items_with_work(items).map(|(tree, _)| tree)
    }

    /// Builds a canonical tree and reports exact bulk construction work.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::UnsortedOrDuplicate`] for malformed ordering or
    /// a canonical construction error for an invalid node.
    pub fn from_sorted_items_with_work(
        items: &[(R::Key, R::Value)],
    ) -> Result<(Self, TreeWork), TreeError> {
        if items.windows(2).any(|window| window[0].0 >= window[1].0) {
            return Err(TreeError::UnsortedOrDuplicate);
        }
        let mut work = TreeWork::default();
        let root = build_tree(items, &mut work, &NoInterner)?;
        Ok((
            Self {
                root,
                interner: NoInterner,
            },
            work,
        ))
    }

    pub(crate) fn from_items(items: &[Item<R>]) -> Result<Self, TreeError> {
        Self::from_sorted_items_with_work(items).map(|(tree, _)| tree)
    }

    /// Prepares a checked path-copy update from ordered key changes.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::UnsortedOrDuplicate`] for malformed changes or
    /// the canonical construction errors reported by the update kernel.
    pub fn prepare_update(
        &self,
        changes: &[TreeChange<R>],
    ) -> Result<PreparedUpdate<R>, TreeError> {
        if changes
            .windows(2)
            .any(|window| window[0].key >= window[1].key)
        {
            return Err(TreeError::UnsortedOrDuplicate);
        }
        let (tree, work) = self.update(changes)?;
        Ok(PreparedUpdate { tree, work })
    }

    fn update(&self, changes: &[TreeChange<R>]) -> Result<(Self, TreeWork), TreeError> {
        if changes.is_empty() {
            return Ok((self.clone(), TreeWork::default()));
        }
        let (target_len, single_present, single_noop) = target_shape(&self.root, changes)?;
        if single_noop {
            return Ok((self.clone(), TreeWork::default()));
        }
        if target_len == 0 {
            let mut work = TreeWork::default();
            work.rows(self.root.entry_count())?;
            let root = empty_node();
            work.rebuild_node(&root)?;
            return Ok((
                Self {
                    root,
                    interner: NoInterner,
                },
                work,
            ));
        }
        if self.root.entry_count() == 0 {
            let items = collect_target_range(changes);
            let mut work = TreeWork::default();
            work.rows(items.len())?;
            let root = build_tree(&items, &mut work, &NoInterner)?;
            return Ok((
                Self {
                    root,
                    interner: NoInterner,
                },
                work,
            ));
        }
        if changes.len() == 1 && changes[0].after.is_some() && single_present {
            let mut work = TreeWork::default();
            let root = replace_existing(
                &self.root,
                &changes[0].key,
                changes[0]
                    .after
                    .as_ref()
                    .ok_or(TreeError::InvalidRoot)?
                    .clone(),
                &mut work,
                &NoInterner,
            )?;
            return Ok((
                Self {
                    root,
                    interner: NoInterner,
                },
                work,
            ));
        }
        let mut work = TreeWork::default();
        let root = apply_structural(&self.root, changes, &mut work, &NoInterner)?;
        Ok((
            Self {
                root,
                interner: NoInterner,
            },
            work,
        ))
    }
}

impl<R: Relation, I: TreeInterner<R> + Clone> PersistentTree<R, I> {
    /// Creates an empty checked tree and reports the one canonical root node
    /// emitted to the interner.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Overflow`] only if work accounting overflows.
    pub fn empty_with_interner(interner: I) -> Result<(Self, TreeWork), TreeError> {
        let mut work = TreeWork::default();
        let root = interner_empty(&interner, &mut work)?;
        Ok((Self { root, interner }, work))
    }

    /// Builds a canonical tree with an interner and reports exact bulk work.
    ///
    /// The callback is invoked once for each newly constructed node.  It is
    /// never called for a reused node during an incremental update.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::UnsortedOrDuplicate`] for malformed ordering or a
    /// canonical construction error for an invalid node.
    pub fn from_sorted_items_with_interner_with_work(
        items: &[(R::Key, R::Value)],
        interner: I,
    ) -> Result<(Self, TreeWork), TreeError> {
        if items.windows(2).any(|window| window[0].0 >= window[1].0) {
            return Err(TreeError::UnsortedOrDuplicate);
        }
        let mut work = TreeWork::default();
        let root = build_tree(items, &mut work, &interner)?;
        Ok((Self { root, interner }, work))
    }

    /// Builds a canonical tree and passes every checked node through an
    /// immutable interner callback.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::UnsortedOrDuplicate`] for malformed ordering or a
    /// canonical construction error for an invalid node.
    pub fn from_sorted_items_with_interner(
        items: &[(R::Key, R::Value)],
        interner: I,
    ) -> Result<Self, TreeError> {
        Self::from_sorted_items_with_interner_with_work(items, interner).map(|(tree, _)| tree)
    }

    /// Returns the exact canonical root node.
    #[must_use]
    pub fn root(&self) -> &crate::CanonicalNode<R> {
        self.root.canonical.as_ref()
    }

    /// Returns the authenticated number of logical rows below the root.
    #[must_use]
    pub fn row_count(&self) -> u64 {
        self.root.canonical.row_count()
    }

    /// Returns an opaque handle to the retained canonical root node.
    #[must_use]
    pub fn root_handle(&self) -> TreeNodeHandle<R> {
        TreeNodeHandle {
            node: self.root.clone(),
        }
    }

    /// Borrows a canonical root view for the lifetime of this tree.
    ///
    /// The view retains no ownership and cannot outlive the persistent tree;
    /// adapters that need to publish immutable bytes should consume it before
    /// releasing the tree generation.
    #[must_use]
    pub fn root_view(&self) -> TreeNodeView<'_, R> {
        TreeNodeView::from_node(self.root.as_ref())
    }

    /// Traverses the complete retained node closure in canonical pre-order.
    ///
    /// Every yielded view borrows this tree.  Unchanged path-copied nodes are
    /// therefore exposed directly, without rebuilding from logical rows.
    #[must_use]
    pub fn node_closure(&self) -> TreeNodeClosure<'_, R> {
        TreeNodeClosure::from_root(self.root.as_ref())
    }

    /// Creates a borrowed zipper rooted at this tree's canonical root.
    #[must_use]
    pub fn zipper(&self) -> TreeZipper<'_, R> {
        TreeZipper::from_root(self.root.as_ref())
    }

    /// Looks up one logical key.
    #[must_use]
    pub fn get<Q: ?Sized + Ord>(&self, key: &Q) -> Option<&R::Value>
    where
        R::Key: Borrow<Q>,
    {
        get_node(&self.root, key)
    }

    /// Iterates rows in canonical key order.
    #[must_use]
    pub fn iter(&self) -> TreeIter<'_, R> {
        TreeIter::new(&self.root)
    }

    /// Returns rows in an inclusive/exclusive ordered range without scanning
    /// keys before the lower bound.
    #[must_use]
    pub fn range<Q: ?Sized + Ord, B: std::ops::RangeBounds<Q>>(
        &self,
        bounds: B,
    ) -> TreeRangeIter<'_, R>
    where
        R::Key: Borrow<Q>,
    {
        TreeRangeIter::new(&self.root, bounds, None)
    }

    /// Returns a range iterator and records the nodes visited to seek its
    /// bounds. Each bounded side touches at most one root-to-leaf path; the
    /// iterator itself retains only the resulting row count.
    pub fn range_with_work<Q: ?Sized + Ord, B: std::ops::RangeBounds<Q>>(
        &self,
        bounds: B,
        work: &mut TreeWork,
    ) -> TreeRangeIter<'_, R>
    where
        R::Key: Borrow<Q>,
    {
        TreeRangeIter::new(&self.root, bounds, Some(work))
    }

    /// Prepares a path-copy update and interns its newly checked nodes.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::UnsortedOrDuplicate`] for malformed changes or a
    /// canonical construction error for an invalid resulting tree.
    pub fn prepare_update_with_interner(
        &self,
        changes: &[TreeChange<R>],
    ) -> Result<PreparedUpdate<R, I>, TreeError> {
        if changes
            .windows(2)
            .any(|window| window[0].key >= window[1].key)
        {
            return Err(TreeError::UnsortedOrDuplicate);
        }
        let (tree, work) = self.update_with_interner(changes)?;
        Ok(PreparedUpdate { tree, work })
    }

    fn update_with_interner(
        &self,
        changes: &[TreeChange<R>],
    ) -> Result<(Self, TreeWork), TreeError> {
        if changes.is_empty() {
            return Ok((self.clone(), TreeWork::default()));
        }
        let (target_len, single_present, single_noop) = target_shape(&self.root, changes)?;
        if single_noop {
            return Ok((self.clone(), TreeWork::default()));
        }
        if target_len == 0 {
            let mut work = TreeWork::default();
            work.rows(self.root.entry_count())?;
            let root = interner_empty(&self.interner, &mut work)?;
            return Ok((
                Self {
                    root,
                    interner: self.interner.clone(),
                },
                work,
            ));
        }
        if self.root.entry_count() == 0 {
            let items = collect_target_range(changes);
            let mut work = TreeWork::default();
            work.rows(items.len())?;
            let root = build_tree(&items, &mut work, &self.interner)?;
            return Ok((
                Self {
                    root,
                    interner: self.interner.clone(),
                },
                work,
            ));
        }
        if changes.len() == 1 && changes[0].after.is_some() && single_present {
            let mut work = TreeWork::default();
            let root = replace_existing(
                &self.root,
                &changes[0].key,
                changes[0].after.clone().ok_or(TreeError::InvalidRoot)?,
                &mut work,
                &self.interner,
            )?;
            return Ok((
                Self {
                    root,
                    interner: self.interner.clone(),
                },
                work,
            ));
        }
        let mut work = TreeWork::default();
        let root = apply_structural(&self.root, changes, &mut work, &self.interner)?;
        Ok((
            Self {
                root,
                interner: self.interner.clone(),
            },
            work,
        ))
    }
}
