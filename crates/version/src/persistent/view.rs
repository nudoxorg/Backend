//! Borrowed views and authenticated traversals over retained tree nodes.
//!
//! The persistent tree already owns every node needed to describe a checked
//! relation root.  Rebuilding a tree from its rows merely to hand those nodes
//! to a storage or replication adapter throws away the most useful property of
//! path copying: the unchanged subtrees are already there.  This module is the
//! narrow borrowed seam for those adapters.  A view can only be obtained from
//! a live [`PersistentTree`](super::PersistentTree), carries the tree's
//! lifetime, and exposes canonical bytes without cloning keys or values.

use super::node::{TreeNode, TreeNodeId, node_id};
use super::{Node, TreeWork};
use crate::{CanonicalNode, CheckedStateObjectRef, Relation, StateRoot, TreeError};
use smallvec::SmallVec;
use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::fmt;

/// A borrowed view of one node retained by a checked persistent tree.
///
/// The private node reference makes this type unforgeable outside the version
/// kernel: callers cannot pair arbitrary bytes with a root claim.  Views are
/// `Copy` because copying one is only copying a reference; canonical keys,
/// values, and bytes remain borrowed from the owning tree.
pub struct TreeNodeView<'a, R: Relation> {
    pub(super) node: &'a TreeNode<R>,
}

impl<R: Relation> Copy for TreeNodeView<'_, R> {}

impl<R: Relation> Clone for TreeNodeView<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: Relation> fmt::Debug for TreeNodeView<'_, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeNodeView")
            .field("id", &self.id())
            .field("level", &self.level())
            .field("len", &self.len())
            .finish()
    }
}

impl<'a, R: Relation> TreeNodeView<'a, R> {
    pub(super) const fn from_node(node: &'a TreeNode<R>) -> Self {
        Self { node }
    }

    /// Returns the authenticated content identity of this node.
    #[must_use]
    pub fn id(self) -> TreeNodeId<R> {
        node_id(self.node)
    }

    /// Returns the node commitment as its typed relation-state root.
    ///
    /// Every view came from a checked tree constructor, so this root is bound
    /// to the canonical bytes returned by [`Self::canonical`].
    #[must_use]
    pub fn state_root(self) -> StateRoot<R> {
        self.node.canonical.commitment()
    }

    /// Returns the exact canonical node retained by the tree.
    #[must_use]
    pub fn canonical(self) -> &'a CanonicalNode<R> {
        self.node.canonical.as_ref()
    }

    /// Returns canonical node bytes without making an ownership copy.
    #[must_use]
    pub fn canonical_bytes(self) -> &'a [u8] {
        self.node.canonical.as_bytes()
    }

    /// Returns the node level, where leaves are zero.
    #[must_use]
    pub fn level(self) -> u16 {
        self.node.level()
    }

    /// Returns the number of visible rows below this node.
    #[must_use]
    pub const fn len(self) -> usize {
        self.node.len
    }

    /// Returns the authenticated row count encoded by this node.
    #[must_use]
    pub fn row_count(self) -> u64 {
        self.node.canonical.row_count()
    }

    /// Returns whether this node contains no visible rows.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.node.len == 0
    }

    /// Returns the first key anchor without cloning it.
    #[must_use]
    pub fn first_key(self) -> Option<&'a R::Key> {
        self.node.first_key()
    }

    /// Returns leaf rows without cloning keys or values.
    #[must_use]
    pub fn entries(self) -> Option<&'a [(R::Key, R::Value)]> {
        self.node.entries()
    }

    /// Returns whether this node is a leaf.
    #[must_use]
    pub const fn is_leaf(self) -> bool {
        self.node.leaf_items.is_some()
    }

    /// Returns the number of direct child nodes.
    #[must_use]
    pub fn child_count(self) -> usize {
        self.node.children().map_or(0, <[_]>::len)
    }

    /// Returns direct children in their authenticated canonical order.
    #[must_use]
    pub fn children(self) -> TreeNodeChildren<'a, R> {
        TreeNodeChildren {
            inner: self.node.children().map(<[_]>::iter),
        }
    }

    /// Converts this borrowed node into a typed, lifetime-bound state object.
    ///
    /// This is the object-store hand-off: the receiver may copy the canonical
    /// bytes once, but cannot substitute a different root or node.  The
    /// returned object remains borrowed from the original tree.
    #[must_use]
    pub fn state_object(self) -> CheckedStateObjectRef<'a, R> {
        CheckedStateObjectRef::from_canonical_node(self.state_root(), self.canonical())
    }
}

/// Borrowing iterator over a node's direct children.
pub struct TreeNodeChildren<'a, R: Relation> {
    inner: Option<std::slice::Iter<'a, Node<R>>>,
}

impl<'a, R: Relation> Iterator for TreeNodeChildren<'a, R> {
    type Item = TreeNodeView<'a, R>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .as_mut()
            .and_then(Iterator::next)
            .map(|node| TreeNodeView::from_node(node.as_ref()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner
            .as_ref()
            .map_or((0, Some(0)), Iterator::size_hint)
    }
}

impl<R: Relation> ExactSizeIterator for TreeNodeChildren<'_, R> {
    fn len(&self) -> usize {
        self.inner.as_ref().map_or(0, std::slice::Iter::len)
    }
}

impl<R: Relation> std::iter::FusedIterator for TreeNodeChildren<'_, R> {}

/// Exact accounting for one borrowed transitive node traversal.
///
/// `visited_nodes` counts each distinct commitment yielded by the traversal.
/// `visited_edges` counts every direct child edge examined, including edges
/// to a subtree that was already visited.  Such repeated edges are counted in
/// `shared_edges` and are not yielded a second time.  `canonical_bytes` is the
/// total number of bytes borrowed from yielded node records; it is a measure
/// of the authenticated closure exposed to a writer, not an allocation count.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TreeNodeClosureWork {
    /// Distinct canonical nodes yielded in pre-order.
    pub visited_nodes: usize,
    /// Direct child edges examined while expanding yielded nodes.
    pub visited_edges: usize,
    /// Edges whose child commitment had already been yielded.
    pub shared_edges: usize,
    /// Total canonical bytes borrowed from yielded nodes.
    pub canonical_bytes: usize,
}

impl TreeNodeClosureWork {
    fn visit_node<R: Relation>(
        &mut self,
        node: &TreeNode<R>,
    ) -> Result<(), TreeNodeTraversalError> {
        self.visited_nodes = self
            .visited_nodes
            .checked_add(1)
            .ok_or(TreeNodeTraversalError::Overflow)?;
        self.canonical_bytes = self
            .canonical_bytes
            .checked_add(node.canonical.as_bytes().len())
            .ok_or(TreeNodeTraversalError::Overflow)?;
        Ok(())
    }

    fn visit_edges(&mut self, count: usize) -> Result<(), TreeNodeTraversalError> {
        self.visited_edges = self
            .visited_edges
            .checked_add(count)
            .ok_or(TreeNodeTraversalError::Overflow)?;
        Ok(())
    }

    fn shared_edge(&mut self) -> Result<(), TreeNodeTraversalError> {
        self.shared_edges = self
            .shared_edges
            .checked_add(1)
            .ok_or(TreeNodeTraversalError::Overflow)?;
        Ok(())
    }
}

/// Failure while accounting for a borrowed node traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeNodeTraversalError {
    /// A traversal counter exceeded `usize`.
    Overflow,
}

impl fmt::Display for TreeNodeTraversalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "persistent tree traversal failed: {self:?}")
    }
}

impl std::error::Error for TreeNodeTraversalError {}

/// Canonically ordered, deduplicated borrowed traversal of one tree closure.
///
/// The iterator is a depth-first pre-order walk: the root is yielded first and
/// each branch's children are visited from left to right according to their
/// authenticated first-key anchors.  A retained interner may make the tree a
/// DAG; commitment identities ensure each node is yielded once while every
/// incoming edge remains visible through [`TreeNodeView::children`].
pub struct TreeNodeClosure<'a, R: Relation> {
    stack: Vec<&'a TreeNode<R>>,
    seen: BTreeSet<TreeNodeId<R>>,
    work: TreeNodeClosureWork,
    complete: bool,
    failure: Option<TreeNodeTraversalError>,
}

impl<R: Relation> fmt::Debug for TreeNodeClosure<'_, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeNodeClosure")
            .field("pending", &self.stack.len())
            .field("work", &self.work)
            .field("complete", &self.complete)
            .finish()
    }
}

impl<'a, R: Relation> TreeNodeClosure<'a, R> {
    pub(super) fn from_root(root: &'a TreeNode<R>) -> Self {
        Self {
            stack: vec![root],
            seen: BTreeSet::new(),
            work: TreeNodeClosureWork::default(),
            complete: false,
            failure: None,
        }
    }

    /// Returns exact work through the traversal's current point.
    #[must_use]
    pub const fn work(&self) -> TreeNodeClosureWork {
        self.work
    }

    /// Returns whether the traversal has exhausted its pending closure.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Returns the accounting error, if the infallible iterator encountered
    /// one. A failed traversal is never reported as complete; durable
    /// adapters should prefer [`Self::try_next`] and propagate the error.
    #[must_use]
    pub const fn error(&self) -> Option<TreeNodeTraversalError> {
        self.failure
    }

    /// Advances the traversal while preserving exact overflow reporting.
    ///
    /// The ordinary [`Iterator`] implementation is convenient for adapters
    /// that already have bounded input.  Storage and replication code should
    /// use this method when it needs a checked error at the accounting
    /// boundary.
    ///
    /// # Errors
    ///
    /// Returns [`TreeNodeTraversalError::Overflow`] if a checked traversal
    /// counter cannot represent the next visit.
    pub fn try_next(&mut self) -> Result<Option<TreeNodeView<'a, R>>, TreeNodeTraversalError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        loop {
            let Some(node) = self.stack.pop() else {
                self.complete = true;
                return Ok(None);
            };
            let id = node_id(node);
            if !self.seen.insert(id) {
                self.work.shared_edge()?;
                continue;
            }
            self.work.visit_node(node)?;
            let children = node.children();
            self.work.visit_edges(children.map_or(0, <[_]>::len))?;
            if let Some(children) = children {
                self.stack
                    .extend(children.iter().rev().map(std::sync::Arc::as_ref));
            }
            return Ok(Some(TreeNodeView::from_node(node)));
        }
    }
}

impl<'a, R: Relation> Iterator for TreeNodeClosure<'a, R> {
    type Item = TreeNodeView<'a, R>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.try_next() {
            Ok(item) => item,
            Err(error) => {
                self.failure = Some(error);
                None
            }
        }
    }
}

impl<R: Relation> std::iter::FusedIterator for TreeNodeClosure<'_, R> {}

/// A borrowed zipper rooted at one checked persistent tree.
///
/// A zipper keeps no owned query key and materializes only the `O(log n)` path
/// of borrowed node references.  It is useful when an adapter needs to emit a
/// proof, inspect one neighborhood, and then continue with the transitive
/// closure without materializing all rows.
#[derive(Clone, Copy, Debug)]
pub struct TreeZipper<'a, R: Relation> {
    root: &'a TreeNode<R>,
}

impl<'a, R: Relation> TreeZipper<'a, R> {
    pub(super) const fn from_root(root: &'a TreeNode<R>) -> Self {
        Self { root }
    }

    /// Locates the canonical leaf that would contain `key`.
    ///
    /// The returned path includes the root and every selected branch.  A key
    /// outside the visible range still returns the nearest canonical leaf,
    /// making the path useful for nonmembership proofs.  The query is borrowed
    /// only during this call.
    #[must_use]
    pub fn seek<Q: ?Sized + Ord>(&self, key: &Q) -> TreePath<'a, R>
    where
        R::Key: Borrow<Q>,
    {
        let mut nodes = Vec::new();
        let mut node = self.root;
        loop {
            nodes.push(TreeNodeView::from_node(node));
            if node.entries().is_some() {
                return TreePath {
                    nodes: nodes.into(),
                };
            }
            let Some(children) = node.children() else {
                return TreePath {
                    nodes: nodes.into(),
                };
            };
            let index = children
                .partition_point(|child| {
                    child
                        .first_key()
                        .is_some_and(|first| first.borrow().cmp(key) != std::cmp::Ordering::Greater)
                })
                .saturating_sub(1)
                .min(children.len().saturating_sub(1));
            let Some(child) = children.get(index) else {
                return TreePath {
                    nodes: nodes.into(),
                };
            };
            node = child.as_ref();
        }
    }

    /// Locates a leaf and accounts for every branch node inspected.
    ///
    /// This checked variant is the boundary for callers that require explicit
    /// overflow handling instead of the infallible convenience method.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Overflow`] if the supplied work counter cannot
    /// represent another inspected node.
    pub fn seek_with_work<Q: ?Sized + Ord>(
        &self,
        key: &Q,
        work: &mut TreeWork,
    ) -> Result<TreePath<'a, R>, TreeError>
    where
        R::Key: Borrow<Q>,
    {
        self.seek_inner(key, Some(work))
    }

    fn seek_inner<Q: ?Sized + Ord>(
        &self,
        key: &Q,
        mut work: Option<&mut TreeWork>,
    ) -> Result<TreePath<'a, R>, TreeError>
    where
        R::Key: Borrow<Q>,
    {
        let mut nodes = Vec::new();
        let mut node = self.root;
        loop {
            if let Some(counter) = work.as_deref_mut() {
                counter.visit()?;
            }
            nodes.push(TreeNodeView::from_node(node));
            if node.entries().is_some() {
                return Ok(TreePath {
                    nodes: nodes.into(),
                });
            }
            let Some(children) = node.children() else {
                return Ok(TreePath {
                    nodes: nodes.into(),
                });
            };
            let index = children
                .partition_point(|child| {
                    child
                        .first_key()
                        .is_some_and(|first| first.borrow().cmp(key) != std::cmp::Ordering::Greater)
                })
                .saturating_sub(1)
                .min(children.len().saturating_sub(1));
            let Some(child) = children.get(index) else {
                return Ok(TreePath {
                    nodes: nodes.into(),
                });
            };
            node = child.as_ref();
        }
    }
}

/// A borrowed root-to-leaf path returned by [`TreeZipper::seek`].
#[derive(Clone, Debug)]
pub struct TreePath<'a, R: Relation> {
    nodes: SmallVec<[TreeNodeView<'a, R>; 12]>,
}

impl<'a, R: Relation> TreePath<'a, R> {
    /// Returns the number of nodes in this path.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns whether the path contains no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns the root view in the path.
    #[must_use]
    pub fn root(&self) -> Option<TreeNodeView<'a, R>> {
        self.nodes.first().copied()
    }

    /// Returns the selected leaf or terminal node view.
    #[must_use]
    pub fn leaf(&self) -> Option<TreeNodeView<'a, R>> {
        self.nodes.last().copied()
    }

    /// Iterates this path from root to selected leaf.
    pub fn iter(&self) -> impl Iterator<Item = TreeNodeView<'a, R>> + '_ {
        self.nodes.iter().copied()
    }
}

impl<'a, R: Relation> IntoIterator for &'a TreePath<'a, R> {
    type Item = TreeNodeView<'a, R>;
    type IntoIter = std::iter::Copied<std::slice::Iter<'a, TreeNodeView<'a, R>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.nodes.iter().copied()
    }
}
