use crate::ids::{append_field, digest, state_root_from_digest};
use crate::{CANONICAL_CUT_POLICY_VERSION, CANONICAL_TREE_ABI, Relation};

use super::{
    NODE_MAGIC, anchored_cut_points_children, anchored_cut_points_items,
    cut::DEFAULT_CUT_POLICY,
    node::{CanonicalNode, Child, ChildCommitment, CommittedChild, NodeError},
};

/// Builds one canonical leaf from strictly key ordered entries.
///
/// # Errors
///
/// Returns [`NodeError::UnsortedOrDuplicate`] for malformed ordering and
/// [`NodeError::OversizedNode`] when the leaf exceeds canonical limits.
pub fn canonical_leaf<R: Relation>(
    entries: &[(R::Key, R::Value)],
) -> Result<CanonicalNode<R>, NodeError> {
    if entries.windows(2).any(|window| window[0].0 >= window[1].0) {
        return Err(NodeError::UnsortedOrDuplicate);
    }
    if entries.len() > usize::from(DEFAULT_CUT_POLICY.max_entries) {
        return Err(NodeError::OversizedNode);
    }
    let mut bytes = node_prefix::<R>(0x01, 0);
    append_field(&mut bytes, |out| {
        for (key, value) in entries {
            append_field(out, |encoded| R::encode_key(key, encoded));
            append_field(out, |encoded| R::encode_value(value, encoded));
        }
    });
    if bytes.len() > DEFAULT_CUT_POLICY.max_encoded_bytes() {
        return Err(NodeError::OversizedNode);
    }
    Ok(node_from_bytes::<R>(
        bytes,
        entries.first().map(|entry| entry.0.clone()),
        0,
        u64::try_from(entries.len()).map_err(|_| NodeError::OversizedNode)?,
    ))
}

/// Builds one canonical branch with strictly increasing child anchors.
///
/// # Errors
///
/// Returns [`NodeError::InvalidBranch`] for an empty, oversized, or
/// nonmonotonic branch,
/// [`NodeError::AnchorMismatch`] when a child claims a different first key,
/// or [`NodeError::LevelMismatch`] for an invalid child level.
pub fn canonical_branch<R: Relation>(
    level: u16,
    children: &[Child<R>],
) -> Result<CanonicalNode<R>, NodeError> {
    let mut summaries = Vec::with_capacity(children.len());
    for child in children {
        if child.node.first_key() != Some(&child.first_key) {
            return Err(NodeError::AnchorMismatch);
        }
        summaries.push(CommittedChild {
            first_key: child.first_key.clone(),
            commitment: ChildCommitment::from(child.node.commitment()),
            level: child.node.level(),
            row_count: child.node.row_count(),
        });
    }
    canonical_branch_from_commitments(level, &summaries)
}

/// Reconstructs a canonical branch from authenticated child summaries.
///
/// # Errors
///
/// Returns [`NodeError::InvalidBranch`] for an empty, leaf-level, oversized,
/// or nonmonotonic branch, and [`NodeError::LevelMismatch`] when any child is
/// not exactly one level below the requested parent.
pub fn canonical_branch_from_commitments<R: Relation>(
    level: u16,
    children: &[CommittedChild<R>],
) -> Result<CanonicalNode<R>, NodeError> {
    if level == 0 || children.is_empty() {
        return Err(NodeError::InvalidBranch);
    }
    if children.len() > usize::from(DEFAULT_CUT_POLICY.max_entries) {
        return Err(NodeError::OversizedNode);
    }
    if children
        .windows(2)
        .any(|window| window[0].first_key >= window[1].first_key)
    {
        return Err(NodeError::InvalidBranch);
    }
    if children.iter().any(|child| {
        child
            .level
            .checked_add(1)
            .is_none_or(|child_level| child_level != level)
    }) {
        return Err(NodeError::LevelMismatch);
    }
    if children.iter().any(|child| child.row_count == 0) {
        return Err(NodeError::InvalidBranch);
    }
    let mut bytes = node_prefix::<R>(0x02, level);
    append_field(&mut bytes, |out| {
        for child in children {
            append_field(out, |encoded| R::encode_key(&child.first_key, encoded));
            extend_length_delimited(out, child.commitment.as_bytes());
            out.extend_from_slice(&child.row_count.to_be_bytes());
        }
    });
    if bytes.len() > DEFAULT_CUT_POLICY.max_encoded_bytes() {
        return Err(NodeError::OversizedNode);
    }
    Ok(node_from_bytes::<R>(
        bytes,
        children.first().map(|child| child.first_key.clone()),
        level,
        children
            .iter()
            .try_fold(0u64, |total, child| total.checked_add(child.row_count))
            .ok_or(NodeError::OversizedNode)?,
    ))
}

/// Returns the canonical empty leaf for a relation.
#[must_use]
pub fn canonical_empty<R: Relation>() -> CanonicalNode<R> {
    let mut bytes = node_prefix::<R>(0x01, 0);
    append_field(&mut bytes, |_| {});
    node_from_bytes::<R>(bytes, None, 0, 0)
}

fn node_prefix<R: Relation>(kind: u8, level: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(NODE_MAGIC);
    bytes.push(CANONICAL_TREE_ABI);
    bytes.push(CANONICAL_CUT_POLICY_VERSION);
    bytes.push(kind);
    bytes.push(R::VERSION);
    bytes.push(R::DOMAIN);
    bytes.extend_from_slice(&R::TYPE.to_be_bytes());
    bytes.extend_from_slice(&level.to_be_bytes());
    bytes
}

fn extend_length_delimited(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

pub(crate) fn node_from_bytes<R: Relation>(
    bytes: Vec<u8>,
    first_key: Option<R::Key>,
    level: u16,
    row_count: u64,
) -> CanonicalNode<R> {
    let commitment =
        state_root_from_digest::<R>(digest(0x52, R::DOMAIN, R::TYPE, R::VERSION, &[&bytes]));
    CanonicalNode::from_parts(bytes, commitment, first_key, level, row_count)
}

/// Builds the canonical multilevel ordered tree used by bulk and path-copy
/// builders.  Tiny maps occupy one leaf; every parent level must strictly
/// reduce the child count.
///
/// # Errors
///
/// Returns [`NodeError::UnsortedOrDuplicate`] for malformed input ordering,
/// [`NodeError::InvalidBranch`] when a level cannot reduce or overflows, and
/// the node construction errors from the canonical leaf/branch builders.
pub fn canonical_root<R: Relation>(
    items: &[(R::Key, R::Value)],
) -> Result<CanonicalNode<R>, NodeError> {
    if items.windows(2).any(|window| window[0].0 >= window[1].0) {
        return Err(NodeError::UnsortedOrDuplicate);
    }
    if items.is_empty() {
        return Ok(canonical_empty::<R>());
    }

    let cuts = anchored_cut_points_items::<R, _>(items.iter(), DEFAULT_CUT_POLICY, 0)
        .map_err(|_| NodeError::InvalidBranch)?;
    let mut level = Vec::with_capacity(cuts.len());
    let mut start = 0usize;
    for end in cuts {
        let node = canonical_leaf::<R>(&items[start..end])?;
        level.push(Child {
            first_key: items[start].0.clone(),
            node,
        });
        start = end;
    }

    let mut height = 1u16;
    while level.len() > 1 {
        let parent_cuts = anchored_cut_points_children::<R, _>(
            level.iter().map(|child| &child.first_key),
            DEFAULT_CUT_POLICY,
            height,
        )
        .map_err(|_| NodeError::InvalidBranch)?;
        let mut next = Vec::with_capacity(parent_cuts.len());
        let mut parent_start = 0usize;
        for end in parent_cuts {
            let node = canonical_branch::<R>(height, &level[parent_start..end])?;
            next.push(Child {
                first_key: level[parent_start].first_key.clone(),
                node,
            });
            parent_start = end;
        }
        if next.len() >= level.len() {
            return Err(NodeError::InvalidBranch);
        }
        level = next;
        height = height.checked_add(1).ok_or(NodeError::InvalidBranch)?;
    }
    level
        .pop()
        .map(|child| child.node)
        .ok_or(NodeError::InvalidBranch)
}
