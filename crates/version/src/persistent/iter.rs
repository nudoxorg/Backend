use super::{Item, Node, PersistentTree, TreeWork};
use super::{interner::TreeInterner, node::TreeNode};
use crate::Relation;
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::ops::{Bound, RangeBounds};

/// Iterator over persistent tree rows in canonical order.
pub struct TreeIter<'a, R: Relation> {
    stack: Vec<(&'a TreeNode<R>, usize)>,
    leaf: Option<(&'a [Item<R>], usize)>,
}
impl<'a, R: Relation> TreeIter<'a, R> {
    pub(super) fn new(root: &'a Node<R>) -> Self {
        Self {
            stack: vec![(root.as_ref(), 0)],
            leaf: None,
        }
    }

    fn from_start<Q: ?Sized + Ord>(
        root: &'a Node<R>,
        start: Bound<&Q>,
        mut work: Option<&mut TreeWork>,
    ) -> Self
    where
        R::Key: Borrow<Q>,
    {
        let mut stack = Vec::new();
        let mut node = root.as_ref();
        loop {
            if let Some(counter) = work.as_deref_mut() {
                let _ = counter.visit();
            }
            stack.push((node, 0));
            if let Some(entries) = node.entries() {
                let index = match start {
                    Bound::Unbounded => 0,
                    Bound::Included(key) => entries.partition_point(|(candidate, _)| {
                        candidate.borrow().cmp(key) == Ordering::Less
                    }),
                    Bound::Excluded(key) => entries.partition_point(|(candidate, _)| {
                        candidate.borrow().cmp(key) != Ordering::Greater
                    }),
                };
                return Self {
                    stack,
                    leaf: Some((entries, index)),
                };
            }
            let Some(children) = node.children() else {
                return Self { stack, leaf: None };
            };
            let index = match start {
                Bound::Unbounded => 0,
                Bound::Included(key) | Bound::Excluded(key) => children
                    .partition_point(|child| {
                        child
                            .first_key()
                            .is_some_and(|first| first.borrow().cmp(key) != Ordering::Greater)
                    })
                    .saturating_sub(1)
                    .min(children.len().saturating_sub(1)),
            };
            let Some(last) = stack.last_mut() else {
                return Self { stack, leaf: None };
            };
            // The selected child is descended immediately; leave the parent
            // cursor after it so exhaustion resumes with the next sibling.
            last.1 = index + 1;
            let Some(child) = children.get(index) else {
                return Self { stack, leaf: None };
            };
            node = child.as_ref();
        }
    }
}
impl<'a, R: Relation> Iterator for TreeIter<'a, R> {
    type Item = (&'a R::Key, &'a R::Value);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((items, at)) = &mut self.leaf {
                if *at < items.len() {
                    let (key, value) = &items[*at];
                    *at += 1;
                    return Some((key, value));
                }
                self.leaf = None;
                self.stack.pop();
                continue;
            }
            let (node, child_at) = self.stack.last_mut()?;
            if let Some(items) = node.entries() {
                self.leaf = Some((items, 0));
                continue;
            }
            let children = node.children()?;
            if *child_at < children.len() {
                let child = children[*child_at].as_ref();
                *child_at += 1;
                self.stack.push((child, 0));
            } else {
                self.stack.pop();
            }
        }
    }
}

/// Borrowing ordered range iterator over a persistent canonical tree.
///
/// The range bounds are reduced to a row count while the iterator is created.
/// This is the key property that makes [`PersistentTree::range`] work with a
/// borrowed query type such as `str` or `[u8]`: the iterator retains no owned
/// copy of either bound and does not keep a short-lived bound reference alive.
pub struct TreeRangeIter<'a, R: Relation> {
    inner: TreeIter<'a, R>,
    remaining: usize,
}

impl<'a, R: Relation> Iterator for TreeRangeIter<'a, R> {
    type Item = (&'a R::Key, &'a R::Value);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let Some(item) = self.inner.next() else {
            self.remaining = 0;
            return None;
        };
        self.remaining -= 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<R: Relation> ExactSizeIterator for TreeRangeIter<'_, R> {
    fn len(&self) -> usize {
        self.remaining
    }
}

impl<R: Relation> std::iter::FusedIterator for TreeRangeIter<'_, R> {}

impl<'a, R: Relation> TreeRangeIter<'a, R> {
    pub(crate) fn new<Q: ?Sized + Ord, B: RangeBounds<Q>>(
        root: &'a Node<R>,
        bounds: B,
        work: Option<&mut TreeWork>,
    ) -> TreeRangeIter<'a, R>
    where
        R::Key: Borrow<Q>,
    {
        let start = bounds.start_bound();
        let end = bounds.end_bound();
        let (start_rank, end_rank) = if let Some(work) = work {
            (
                rank(root, start, start_is_inclusive(start), Some(work)),
                rank(root, end, end_is_inclusive(end), Some(work)),
            )
        } else {
            (
                rank(root, start, start_is_inclusive(start), None),
                rank(root, end, end_is_inclusive(end), None),
            )
        };
        TreeRangeIter {
            inner: TreeIter::from_start(root, start, None),
            remaining: end_rank.saturating_sub(start_rank),
        }
    }
}

const fn start_is_inclusive<Q: ?Sized>(bound: Bound<&Q>) -> bool {
    matches!(bound, Bound::Excluded(_))
}

const fn end_is_inclusive<Q: ?Sized>(bound: Bound<&Q>) -> bool {
    matches!(bound, Bound::Included(_) | Bound::Unbounded)
}

/// Counts rows before the supplied bound. The `inclusive` flag selects `<=`
/// for an upper bound (or `<=` for an excluded lower bound); callers translate
/// the public lower/upper bound semantics before entering this kernel.
///
/// A canonical tree has checked child counts, so summing the counts cannot
/// overflow. Saturating arithmetic keeps this total function even if a
/// caller is holding a malformed node produced by a future implementation.
fn rank<R: Relation, Q: ?Sized + Ord>(
    node: &Node<R>,
    bound: Bound<&Q>,
    upper_inclusive: bool,
    mut work: Option<&mut TreeWork>,
) -> usize
where
    R::Key: Borrow<Q>,
{
    if let Some(counter) = work.as_deref_mut() {
        // A range with an unbounded side does not seek that side.
        if !matches!(bound, Bound::Unbounded) {
            let _ = counter.visit();
        }
    }
    let Some(key) = (match bound {
        Bound::Included(key) | Bound::Excluded(key) => Some(key),
        Bound::Unbounded => None,
    }) else {
        return if upper_inclusive {
            node.entry_count()
        } else {
            0
        };
    };

    let Some(entries) = node.entries() else {
        let Some(children) = node.children() else {
            return 0;
        };
        let child_index = children
            .partition_point(|child| {
                child
                    .first_key()
                    .is_some_and(|first| first.borrow().cmp(key) != Ordering::Greater)
            })
            .saturating_sub(1)
            .min(children.len().saturating_sub(1));
        let preceding = node
            .child_entry_offsets()
            .and_then(|offsets| offsets.get(child_index).copied())
            .unwrap_or_else(|| {
                children
                    .get(..child_index)
                    .unwrap_or(&[])
                    .iter()
                    .map(|child| child.entry_count())
                    .fold(0usize, usize::saturating_add)
            });
        let Some(child) = children.get(child_index) else {
            return preceding;
        };
        return preceding.saturating_add(rank(child, Bound::Included(key), upper_inclusive, work));
    };

    entries.partition_point(|(candidate, _)| {
        let ordering = candidate.borrow().cmp(key);
        if upper_inclusive {
            ordering != Ordering::Greater
        } else {
            ordering == Ordering::Less
        }
    })
}
impl<'a, R: Relation, I: TreeInterner<R> + Clone> IntoIterator for &'a PersistentTree<R, I> {
    type Item = (&'a R::Key, &'a R::Value);
    type IntoIter = TreeIter<'a, R>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
