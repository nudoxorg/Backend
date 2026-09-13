//! Bounded page traversal helpers for Merkle reconciliation.

use super::{
    MerkleChild, MerkleLeafEntry, MerklePage, MerklePageBody, MerklePageRequest, MerklePageSource,
    MerkleRoot, NodeDigest, NodeKind, PageCursor, ReconcileBudget, SubtreePair,
};
use crate::ReplicationError;
use std::cmp::Ordering;

pub(super) struct PageBudget {
    pub(super) used: usize,
    pub(super) total: u64,
}

impl PageBudget {
    pub(super) const fn new(total: u64) -> Self {
        Self { used: 0, total }
    }

    fn record(&mut self) -> Result<(), ReplicationError> {
        self.used = self.used.checked_add(1).ok_or(ReplicationError::Overflow)?;
        self.total = self
            .total
            .checked_add(1)
            .ok_or(ReplicationError::Overflow)?;
        Ok(())
    }
}

pub(super) fn fetch_page<S: MerklePageSource>(
    source: &mut S,
    root: MerkleRoot,
    node: NodeDigest,
    cursor: PageCursor,
    pair: &mut SubtreePair,
    page_budget: &mut PageBudget,
    budget: ReconcileBudget,
) -> Result<MerklePage, ReplicationError> {
    let page = source.page(MerklePageRequest {
        root,
        node,
        cursor,
        max_items: budget.max_page_items,
    })?;
    page.validate(usize::from(budget.max_page_items), budget.max_key_bytes)?;
    if page.root != root || page.node != node || page.cursor != cursor {
        return Err(ReplicationError::IdentityMismatch);
    }
    let kind = NodeKind::of(&page.body);
    if let Some(expected) = pair.kind {
        if expected != kind {
            return Err(ReplicationError::InvalidWire);
        }
    } else {
        pair.kind = Some(kind);
    }
    if let Some(expected) = pair.level {
        if expected != page.level {
            return Err(ReplicationError::InvalidWire);
        }
    } else {
        pair.level = Some(page.level);
    }
    page_budget.record()?;
    Ok(page)
}

pub(super) fn page_children(page: Option<&MerklePage>) -> Result<&[MerkleChild], ReplicationError> {
    match page.map(|page| &page.body) {
        None => Ok(&[]),
        Some(MerklePageBody::Branch(children)) => Ok(children),
        Some(MerklePageBody::Leaf(_)) => Err(ReplicationError::InvalidWire),
    }
}

pub(super) fn page_entries(
    page: Option<&MerklePage>,
) -> Result<&[MerkleLeafEntry], ReplicationError> {
    match page.map(|page| &page.body) {
        None => Ok(&[]),
        Some(MerklePageBody::Leaf(entries)) => Ok(entries),
        Some(MerklePageBody::Branch(_)) => Err(ReplicationError::InvalidWire),
    }
}

/// Advances a fully consumed page without retaining its body in the cursor.
/// Returning `true` tells the caller to refetch the next page (or treat this
/// side as terminal) before comparing another item.
pub(super) fn advance_empty_page(
    cursor: &mut PageCursor,
    index: &mut usize,
    exhausted: &mut bool,
    page: Option<&MerklePage>,
    len: usize,
) -> bool {
    if *exhausted || *index < len {
        return false;
    }
    let Some(page) = page else {
        *exhausted = true;
        return true;
    };
    *index = 0;
    if let Some(next) = page.next {
        *cursor = next;
    } else {
        *exhausted = true;
    }
    true
}

pub(super) fn child_end(children: &[MerkleChild], index: usize) -> Option<&[u8]> {
    children[index].end_key.as_deref().or_else(|| {
        children
            .get(index + 1)
            .map(|child| child.first_key.as_slice())
    })
}

pub(super) fn normalize_leaf_index(
    entries: &[MerkleLeafEntry],
    index: &mut usize,
    range_start: Option<&[u8]>,
    range_end: Option<&[u8]>,
) -> bool {
    while *index < entries.len()
        && range_start.is_some_and(|start| entries[*index].key.as_slice() < start)
    {
        *index += 1;
    }
    if *index < entries.len() && range_end.is_some_and(|end| entries[*index].key.as_slice() >= end)
    {
        *index = entries.len();
        true
    } else {
        false
    }
}

pub(super) fn advance_shorter_branch_child(
    pair: &mut SubtreePair,
    local: &[MerkleChild],
    remote: &[MerkleChild],
) {
    let left = &local[pair.local_index];
    let right = &remote[pair.remote_index];
    let left_end = child_end(local, pair.local_index);
    let right_end = child_end(remote, pair.remote_index);

    // First account for disjoint intervals.  This keeps the interval join
    // linear even when the two immutable trees have different fan-out.
    if left_end.is_some_and(|end| end <= right.first_key.as_slice()) {
        pair.local_index += 1;
        return;
    }
    if right_end.is_some_and(|end| end <= left.first_key.as_slice()) {
        pair.remote_index += 1;
        return;
    }

    match (left_end, right_end) {
        (Some(left), Some(right)) => match left.cmp(right) {
            Ordering::Less => pair.local_index += 1,
            Ordering::Greater => pair.remote_index += 1,
            Ordering::Equal => {
                pair.local_index += 1;
                pair.remote_index += 1;
            }
        },
        (Some(_), None) => pair.local_index += 1,
        (None, Some(_)) => pair.remote_index += 1,
        (None, None) => {
            pair.local_index += 1;
            pair.remote_index += 1;
        }
    }
}

pub(super) fn ranges_overlap(
    left_start: &[u8],
    left_end: Option<&[u8]>,
    right_start: &[u8],
    right_end: Option<&[u8]>,
) -> bool {
    let start = left_start.max(right_start);
    let end = match (left_end, right_end) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    };
    end.is_none_or(|end| start < end)
}

pub(super) fn child_pair_bounds(
    parent_start: Option<&[u8]>,
    parent_end: Option<&[u8]>,
    left_start: &[u8],
    left_end: Option<&[u8]>,
    right_start: &[u8],
    right_end: Option<&[u8]>,
) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    if !ranges_overlap(left_start, left_end, right_start, right_end) {
        return None;
    }
    let overlap_start = left_start.max(right_start);
    let overlap_end = match (left_end, right_end) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    };
    let start = match parent_start {
        Some(parent) => parent.max(overlap_start),
        None => overlap_start,
    };
    let end = match (parent_end, overlap_end) {
        (Some(parent), Some(overlap)) => Some(parent.min(overlap)),
        (Some(parent), None) => Some(parent),
        (None, Some(overlap)) => Some(overlap),
        (None, None) => None,
    };
    if end.is_some_and(|end| start >= end) {
        None
    } else {
        Some((start.to_vec(), end.map(ToOwned::to_owned)))
    }
}
