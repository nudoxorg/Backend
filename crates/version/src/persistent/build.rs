use super::{
    Item, Node, TreeWork,
    interner::{TreeInterner, checked_intern},
    node::{TreeNode, TreeNodeHandle},
    work::TreeError,
};
use crate::tree::{anchored_cut_points_children, anchored_cut_points_items};
use crate::{
    ChildCommitment, CommittedChild, DEFAULT_CUT_POLICY, Relation,
    canonical_branch_from_commitments, canonical_empty, canonical_leaf,
};
use std::sync::Arc;

pub(super) fn empty_node<R: Relation>() -> Node<R> {
    Arc::new(TreeNode {
        canonical: Arc::new(canonical_empty::<R>()),
        leaf_items: Some(Arc::from(Vec::<Item<R>>::new().into_boxed_slice())),
        children: None,
        child_entry_offsets: None,
        len: 0,
        level_counts: Arc::from([1usize]),
    })
}
pub(super) fn make_leaf<R: Relation, I: TreeInterner<R>>(
    items: Vec<Item<R>>,
    interner: &I,
    work: &mut TreeWork,
) -> Result<Node<R>, TreeError> {
    let len = items.len();
    let canonical = canonical_leaf::<R>(&items).map_err(TreeError::Canonical)?;
    let node = Arc::new(TreeNode {
        canonical: Arc::new(canonical),
        leaf_items: Some(Arc::from(items.into_boxed_slice())),
        children: None,
        child_entry_offsets: None,
        len,
        level_counts: Arc::from([1usize]),
    });
    work.rebuild_node(&node)?;
    checked_intern(interner, TreeNodeHandle { node })
}
pub(super) fn make_branch<R: Relation, I: TreeInterner<R>>(
    level: u16,
    children: Vec<Node<R>>,
    interner: &I,
    work: &mut TreeWork,
) -> Result<Node<R>, TreeError> {
    if children.is_empty() {
        return Err(TreeError::InvalidRoot);
    }
    let summaries = children
        .iter()
        .map(|child| {
            Ok(CommittedChild {
                first_key: child.first_key().cloned().ok_or(TreeError::InvalidRoot)?,
                commitment: ChildCommitment::from(child.canonical.commitment()),
                level: child.level(),
                row_count: u64::try_from(child.entry_count()).map_err(|_| TreeError::Overflow)?,
            })
        })
        .collect::<Result<Vec<_>, TreeError>>()?;
    let canonical =
        canonical_branch_from_commitments::<R>(level, &summaries).map_err(TreeError::Canonical)?;
    let mut len = 0usize;
    let mut child_entry_offsets =
        Vec::with_capacity(children.len().checked_add(1).ok_or(TreeError::Overflow)?);
    child_entry_offsets.push(0);
    let mut counts = vec![0usize; usize::from(level) + 1];
    counts[usize::from(level)] = 1;
    for child in &children {
        len = len
            .checked_add(child.entry_count())
            .ok_or(TreeError::Overflow)?;
        child_entry_offsets.push(len);
        for (at, count) in child.level_counts.iter().copied().enumerate() {
            if at >= counts.len() {
                return Err(TreeError::InvalidRoot);
            }
            counts[at] = counts[at].checked_add(count).ok_or(TreeError::Overflow)?;
        }
    }
    let node = Arc::new(TreeNode {
        canonical: Arc::new(canonical),
        leaf_items: None,
        children: Some(Arc::from(children.into_boxed_slice())),
        child_entry_offsets: Some(Arc::from(child_entry_offsets.into_boxed_slice())),
        len,
        level_counts: Arc::from(counts.into_boxed_slice()),
    });
    work.rebuild_node(&node)?;
    checked_intern(interner, TreeNodeHandle { node })
}
pub(super) fn build_leaves<R: Relation, I: TreeInterner<R>>(
    items: &[Item<R>],
    work: &mut TreeWork,
    interner: &I,
) -> Result<Vec<Node<R>>, TreeError> {
    let boundaries = anchored_cut_points_items::<R, _>(items.iter(), DEFAULT_CUT_POLICY, 0)
        .map_err(|_| TreeError::InvalidRoot)?;
    let mut leaves = Vec::with_capacity(boundaries.len());
    let mut start = 0;
    for end in boundaries {
        let leaf = make_leaf(items[start..end].to_vec(), interner, work)?;
        leaves.push(leaf);
        start = end;
    }
    Ok(leaves)
}
pub(super) fn build_branches<R: Relation, I: TreeInterner<R>>(
    children: &[Node<R>],
    level: u16,
    work: &mut TreeWork,
    interner: &I,
) -> Result<Vec<Node<R>>, TreeError> {
    if children.iter().any(|child| child.first_key().is_none()) {
        return Err(TreeError::InvalidRoot);
    }
    let boundaries = anchored_cut_points_children::<R, _>(
        children.iter().filter_map(|child| child.first_key()),
        DEFAULT_CUT_POLICY,
        level,
    )
    .map_err(|_| TreeError::InvalidRoot)?;
    let mut branches = Vec::with_capacity(boundaries.len());
    let mut start = 0;
    for end in boundaries {
        let branch = make_branch(level, children[start..end].to_vec(), interner, work)?;
        branches.push(branch);
        start = end;
    }
    Ok(branches)
}
pub(super) fn build_tree<R: Relation, I: TreeInterner<R>>(
    items: &[Item<R>],
    work: &mut TreeWork,
    interner: &I,
) -> Result<Node<R>, TreeError> {
    if items.is_empty() {
        let root = empty_node();
        work.rebuild_node(&root)?;
        return checked_intern(interner, TreeNodeHandle { node: root });
    }
    let mut nodes = build_leaves(items, work, interner)?;
    let mut level = 1u16;
    while nodes.len() > 1 {
        let next = build_branches(&nodes, level, work, interner)?;
        if next.len() >= nodes.len() {
            return Err(TreeError::InvalidRoot);
        }
        nodes = next;
        level = level.checked_add(1).ok_or(TreeError::Overflow)?;
    }
    nodes.pop().ok_or(TreeError::InvalidRoot)
}
