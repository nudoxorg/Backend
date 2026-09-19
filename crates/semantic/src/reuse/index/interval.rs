use super::{RegistrationId, SemanticReaderKey};
use crate::SemanticError;
use crate::canonical::canonical_scoped_read;
use crate::{ReadSelector, ScopedRead};
use std::collections::BTreeSet;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::reuse) struct IntervalKey<K: SemanticReaderKey = super::SemanticWorkKey> {
    start: Vec<u8>,
    end: Option<Vec<u8>>,
    selector: Arc<[u8]>,
    reader: K,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IntervalNode<K: SemanticReaderKey = super::SemanticWorkKey> {
    key: IntervalKey<K>,
    priority: u64,
    max_end: Option<Vec<u8>>,
    max_unbounded: bool,
    left: Option<Box<Self>>,
    right: Option<Box<Self>>,
}

impl<K: SemanticReaderKey> IntervalNode<K> {
    #[allow(
        clippy::unnecessary_box_returns,
        reason = "the recursive treap stores every node behind an owning box"
    )]
    fn new(key: IntervalKey<K>) -> Box<Self> {
        let priority = interval_priority(&key);
        let max_end = key.end.clone();
        let max_unbounded = key.end.is_none();
        Box::new(Self {
            key,
            priority,
            max_end,
            max_unbounded,
            left: None,
            right: None,
        })
    }

    fn refresh(&mut self) {
        self.max_unbounded = self.key.end.is_none()
            || self.left.as_ref().is_some_and(|node| node.max_unbounded)
            || self.right.as_ref().is_some_and(|node| node.max_unbounded);
        self.max_end = if self.max_unbounded {
            None
        } else {
            [
                self.key.end.clone(),
                self.left.as_ref().and_then(|node| node.max_end.clone()),
                self.right.as_ref().and_then(|node| node.max_end.clone()),
            ]
            .into_iter()
            .flatten()
            .max()
        };
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::reuse) struct IntervalTree<K: SemanticReaderKey = super::SemanticWorkKey> {
    root: Option<Box<IntervalNode<K>>>,
    pub(in crate::reuse) len: usize,
}

impl<K: SemanticReaderKey> Default for IntervalTree<K> {
    fn default() -> Self {
        Self { root: None, len: 0 }
    }
}

impl<K: SemanticReaderKey> IntervalTree<K> {
    pub(in crate::reuse) fn insert(&mut self, key: IntervalKey<K>) {
        let root = self.root.take();
        self.root = Some(insert_interval(root, IntervalNode::new(key)));
        self.len = self.len.saturating_add(1);
    }

    pub(in crate::reuse) fn remove(&mut self, key: &IntervalKey<K>) -> bool {
        let (root, removed) = remove_interval(self.root.take(), key);
        self.root = root;
        if removed {
            self.len = self.len.saturating_sub(1);
        }
        removed
    }

    pub(in crate::reuse) fn query(
        &self,
        start: &[u8],
        end: Option<&[u8]>,
        candidate_limit: usize,
        probe_limit: u64,
    ) -> Result<(BTreeSet<RegistrationId<K>>, u64), SemanticError> {
        let mut registrations = BTreeSet::new();
        let mut probes = 0;
        query_intervals(
            self.root.as_deref(),
            start,
            end,
            &mut registrations,
            &mut probes,
            candidate_limit,
            probe_limit,
        )?;
        Ok((registrations, probes))
    }
}

fn interval_priority<K: SemanticReaderKey>(key: &IntervalKey<K>) -> u64 {
    // A small deterministic hash keeps treap shape independent of insertion
    // order without making the shape part of any semantic identity.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let reader_bytes = key.reader.canonical_bytes();
    let reader_hash = blake3::hash(&reader_bytes);
    for byte in key
        .start
        .iter()
        .chain(key.end.as_deref().unwrap_or_default())
        .chain(key.selector.iter())
        .chain(reader_hash.as_bytes().iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

#[allow(
    clippy::unnecessary_box_returns,
    reason = "the recursive treap returns an owning subtree root"
)]
fn insert_interval<K: SemanticReaderKey>(
    root: Option<Box<IntervalNode<K>>>,
    node: Box<IntervalNode<K>>,
) -> Box<IntervalNode<K>> {
    let Some(mut root) = root else {
        return node;
    };
    if node.priority > root.priority {
        let (left, right) = split_interval(Some(root), &node.key);
        let mut node = node;
        node.left = left;
        node.right = right;
        node.refresh();
        return node;
    }
    if node.key < root.key {
        root.left = Some(insert_interval(root.left.take(), node));
    } else {
        root.right = Some(insert_interval(root.right.take(), node));
    }
    root.refresh();
    root
}

type IntervalSplit<K> = (Option<Box<IntervalNode<K>>>, Option<Box<IntervalNode<K>>>);

fn split_interval<K: SemanticReaderKey>(
    root: Option<Box<IntervalNode<K>>>,
    key: &IntervalKey<K>,
) -> IntervalSplit<K> {
    let Some(mut root) = root else {
        return (None, None);
    };
    if root.key < *key {
        let (left, right) = split_interval(root.right.take(), key);
        root.right = left;
        root.refresh();
        (Some(root), right)
    } else {
        let (left, right) = split_interval(root.left.take(), key);
        root.left = right;
        root.refresh();
        (left, Some(root))
    }
}

fn merge_intervals<K: SemanticReaderKey>(
    left: Option<Box<IntervalNode<K>>>,
    right: Option<Box<IntervalNode<K>>>,
) -> Option<Box<IntervalNode<K>>> {
    match (left, right) {
        (None, other) | (other, None) => other,
        (Some(mut left), Some(mut right)) => {
            if left.priority > right.priority {
                left.right = merge_intervals(left.right.take(), Some(right));
                left.refresh();
                Some(left)
            } else {
                right.left = merge_intervals(Some(left), right.left.take());
                right.refresh();
                Some(right)
            }
        }
    }
}

fn remove_interval<K: SemanticReaderKey>(
    root: Option<Box<IntervalNode<K>>>,
    key: &IntervalKey<K>,
) -> (Option<Box<IntervalNode<K>>>, bool) {
    let Some(mut root) = root else {
        return (None, false);
    };
    if *key == root.key {
        return (merge_intervals(root.left.take(), root.right.take()), true);
    }
    let removed;
    if *key < root.key {
        let (left, did_remove) = remove_interval(root.left.take(), key);
        root.left = left;
        removed = did_remove;
    } else {
        let (right, did_remove) = remove_interval(root.right.take(), key);
        root.right = right;
        removed = did_remove;
    }
    root.refresh();
    (Some(root), removed)
}

fn end_after(end: Option<&[u8]>, start: &[u8]) -> bool {
    end.is_none_or(|end| start < end)
}

fn start_before(start: &[u8], end: Option<&[u8]>) -> bool {
    end.is_none_or(|end| start < end)
}

fn max_end_after(max_end: Option<&[u8]>, start: &[u8]) -> bool {
    max_end.is_none_or(|end| start < end)
}

fn query_intervals<K: SemanticReaderKey>(
    node: Option<&IntervalNode<K>>,
    start: &[u8],
    end: Option<&[u8]>,
    registrations: &mut BTreeSet<RegistrationId<K>>,
    probes: &mut u64,
    candidate_limit: usize,
    probe_limit: u64,
) -> Result<(), SemanticError> {
    let Some(node) = node else {
        return Ok(());
    };
    if *probes >= probe_limit {
        return Err(SemanticError::ReuseWorkLimit);
    }
    *probes = probes.saturating_add(1);
    if !node.max_unbounded && !max_end_after(node.max_end.as_deref(), start) {
        return Ok(());
    }
    if start_before(node.key.start.as_slice(), end) {
        query_intervals(
            node.left.as_deref(),
            start,
            end,
            registrations,
            probes,
            candidate_limit,
            probe_limit,
        )?;
        if end_after(node.key.end.as_deref(), start) {
            let registration = RegistrationId {
                reader: node.key.reader,
                selector: node.key.selector.clone(),
            };
            if !registrations.contains(&registration) && registrations.len() >= candidate_limit {
                return Err(SemanticError::ReuseWorkLimit);
            }
            registrations.insert(registration);
        }
        query_intervals(
            node.right.as_deref(),
            start,
            end,
            registrations,
            probes,
            candidate_limit,
            probe_limit,
        )?;
    } else {
        query_intervals(
            node.left.as_deref(),
            start,
            end,
            registrations,
            probes,
            candidate_limit,
            probe_limit,
        )?;
    }
    Ok(())
}

pub(in crate::reuse) fn selector_interval(selector: &ReadSelector) -> (Vec<u8>, Option<Vec<u8>>) {
    match selector {
        ReadSelector::Exact(key) => {
            let mut end = key.clone();
            end.push(0);
            (key.clone(), Some(end))
        }
        ReadSelector::Range(range) => (range.start().to_vec(), Some(range.end().to_vec())),
        ReadSelector::Prefix(prefix) => (prefix.clone(), prefix_successor(prefix)),
    }
}

fn prefix_successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut successor = prefix.to_vec();
    while let Some(last) = successor.last_mut() {
        if *last == u8::MAX {
            successor.pop();
        } else {
            *last = last.saturating_add(1);
            return Some(successor);
        }
    }
    None
}

pub(in crate::reuse) fn interval_key<K: SemanticReaderKey>(
    read: &ScopedRead,
    reader: K,
) -> IntervalKey<K> {
    interval_key_with_selector(read, reader, Arc::from(canonical_scoped_read(read)))
}

pub(in crate::reuse) fn interval_key_with_selector<K: SemanticReaderKey>(
    read: &ScopedRead,
    reader: K,
    selector: Arc<[u8]>,
) -> IntervalKey<K> {
    let (start, end) = selector_interval(read.selector());
    IntervalKey {
        start,
        end,
        selector,
        reader,
    }
}
