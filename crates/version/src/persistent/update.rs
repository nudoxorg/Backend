use super::{
    Item, Node, TreeWork, build,
    interner::{TreeInterner, checked_intern},
    node::TreeNodeHandle,
    work::TreeError,
};
use crate::tree::{anchored_cut_points_children, anchored_cut_points_sized};
use crate::{DEFAULT_CUT_POLICY, Relation, TreeChange};
use std::borrow::Borrow;
use std::cmp::Ordering;

pub(super) fn get_node<'a, R: Relation, Q: ?Sized + Ord>(
    node: &'a Node<R>,
    key: &Q,
) -> Option<&'a R::Value>
where
    R::Key: Borrow<Q>,
{
    if let Some(entries) = node.entries() {
        return entries
            .binary_search_by(|(candidate, _)| candidate.borrow().cmp(key))
            .ok()
            .map(|index| &entries[index].1);
    }
    let children = node.children()?;
    let index = children
        .partition_point(|child| {
            child
                .first_key()
                .is_some_and(|first| first.borrow().cmp(key) != Ordering::Greater)
        })
        .saturating_sub(1)
        .min(children.len().saturating_sub(1));
    children.get(index).and_then(|child| get_node(child, key))
}
pub(super) fn target_shape<R: Relation>(
    root: &Node<R>,
    changes: &[TreeChange<R>],
) -> Result<(usize, bool, bool), TreeError> {
    let mut len = root.entry_count();
    let mut single_present = false;
    let mut single_noop = false;
    for c in changes {
        let existing = get_node(root, &c.key);
        if changes.len() == 1 {
            single_present = existing.is_some();
            single_noop = existing == c.after.as_ref();
        }
        match (existing.is_some(), c.after.is_some()) {
            (false, true) => len = len.checked_add(1).ok_or(TreeError::Overflow)?,
            (true, false) => len = len.checked_sub(1).ok_or(TreeError::InvalidRoot)?,
            _ => {}
        }
    }
    Ok((len, single_present, single_noop))
}
pub(super) fn collect_target_range<R: Relation>(changes: &[TreeChange<R>]) -> Vec<Item<R>> {
    changes
        .iter()
        .filter_map(|change| {
            change
                .after
                .as_ref()
                .map(|value| (change.key.clone(), value.clone()))
        })
        .collect()
}
fn node_at_level<R: Relation>(
    node: &Node<R>,
    level: u16,
    mut index: usize,
    work: &mut TreeWork,
) -> Result<Node<R>, TreeError> {
    work.visit()?;
    if node.level() == level {
        return (index == 0)
            .then(|| node.clone())
            .ok_or(TreeError::InvalidRoot);
    }
    let children = node.children().ok_or(TreeError::InvalidRoot)?;
    for child in children {
        let count = child.node_count_at_level(level);
        if index < count {
            return node_at_level(child, level, index, work);
        }
        index = index.checked_sub(count).ok_or(TreeError::InvalidRoot)?;
    }
    Err(TreeError::InvalidRoot)
}

fn find_node_index_at_level<R: Relation>(
    node: &Node<R>,
    level: u16,
    key: &R::Key,
    work: &mut TreeWork,
) -> Result<usize, TreeError> {
    work.visit()?;
    if node.level() == level {
        return Ok(0);
    }
    let children = node.children().ok_or(TreeError::InvalidRoot)?;
    let child_index = children
        .partition_point(|child| child.first_key().is_some_and(|first| first <= key))
        .saturating_sub(1)
        .min(children.len().saturating_sub(1));
    let mut offset = 0usize;
    for child in &children[..child_index] {
        offset = offset
            .checked_add(child.node_count_at_level(level))
            .ok_or(TreeError::Overflow)?;
    }
    offset
        .checked_add(find_node_index_at_level(
            &children[child_index],
            level,
            key,
            work,
        )?)
        .ok_or(TreeError::Overflow)
}

fn boundary_is_cut<'a, R, I>(
    keys: I,
    suffix: Option<&'a R::Key>,
    key_count: usize,
    level: u16,
) -> Result<bool, TreeError>
where
    R: Relation + 'a,
    I: IntoIterator<Item = &'a R::Key>,
{
    if key_count == 0 {
        return Ok(suffix.is_none());
    }
    Ok(anchored_cut_points_children::<R, _>(
        keys.into_iter().chain(suffix),
        DEFAULT_CUT_POLICY,
        level,
    )
    .map_err(|_| TreeError::InvalidRoot)?
    .contains(&key_count))
}

fn leaf_boundary_is_cut<R: Relation>(
    items: &[Item<R>],
    suffix: Option<&R::Key>,
    key_count: usize,
) -> Result<bool, TreeError> {
    if key_count == 0 {
        return Ok(suffix.is_none());
    }
    let mut sized = items
        .iter()
        .map(|(key, value)| {
            let mut encoded = Vec::new();
            R::encode_value(value, &mut encoded);
            (key, 8usize.saturating_add(encoded.len()))
        })
        .collect::<Vec<_>>();
    if let Some(suffix) = suffix {
        sized.push((suffix, 0));
    }
    Ok(
        anchored_cut_points_sized::<R, _>(sized, DEFAULT_CUT_POLICY, 0)
            .map_err(|_| TreeError::InvalidRoot)?
            .contains(&key_count),
    )
}

fn merge_segment<R: Relation>(existing: &[Item<R>], changes: &[TreeChange<R>]) -> Vec<Item<R>> {
    let mut output = Vec::with_capacity(existing.len().saturating_add(changes.len()));
    let mut change_at = 0usize;
    for (key, value) in existing {
        while change_at < changes.len() && changes[change_at].key < *key {
            if let Some(after) = changes[change_at].after.as_ref() {
                output.push((changes[change_at].key.clone(), after.clone()));
            }
            change_at += 1;
        }
        if change_at < changes.len() && changes[change_at].key == *key {
            if let Some(after) = changes[change_at].after.as_ref() {
                output.push((key.clone(), after.clone()));
            }
            change_at += 1;
        } else {
            output.push((key.clone(), value.clone()));
        }
    }
    while change_at < changes.len() {
        if let Some(after) = changes[change_at].after.as_ref() {
            output.push((changes[change_at].key.clone(), after.clone()));
        }
        change_at += 1;
    }
    output
}

#[derive(Clone)]
struct NodeSplice<R: Relation> {
    old_start: usize,
    old_end: usize,
    replacement: Vec<Node<R>>,
}

fn leaf_splice<R: Relation, I: TreeInterner<R>>(
    root: &Node<R>,
    changes: &[TreeChange<R>],
    work: &mut TreeWork,
    interner: &I,
) -> Result<NodeSplice<R>, TreeError> {
    let leaf_total = root.leaf_count();
    if leaf_total == 0 {
        return Err(TreeError::InvalidRoot);
    }
    let mut first_leaf = leaf_total - 1;
    let mut last_leaf = 0usize;
    for change in changes {
        let index = find_node_index_at_level(root, 0, &change.key, work)?;
        first_leaf = first_leaf.min(index);
        last_leaf = last_leaf.max(index);
    }
    let mut old_items = Vec::new();
    let mut target_segment = Vec::new();
    let mut end_leaf = leaf_total;
    for candidate_end in (first_leaf + 1)..=leaf_total {
        let leaf = node_at_level(root, 0, candidate_end - 1, work)?;
        old_items.extend(
            leaf.entries()
                .ok_or(TreeError::InvalidRoot)?
                .iter()
                .cloned(),
        );
        if candidate_end <= last_leaf {
            continue;
        }
        let candidate = merge_segment(&old_items, changes);
        let suffix = if candidate_end < leaf_total {
            Some(
                node_at_level(root, 0, candidate_end, work)?
                    .first_key()
                    .cloned()
                    .ok_or(TreeError::InvalidRoot)?,
            )
        } else {
            None
        };
        if candidate_end == leaf_total
            || leaf_boundary_is_cut::<R>(&candidate, suffix.as_ref(), candidate.len())?
        {
            target_segment = candidate;
            end_leaf = candidate_end;
            break;
        }
    }
    if end_leaf == leaf_total && target_segment.is_empty() {
        target_segment = merge_segment(&old_items, changes);
    }
    let replacement = if target_segment.is_empty() {
        if first_leaf == 0 && end_leaf == leaf_total {
            let root = build::empty_node();
            work.rebuild_node(&root)?;
            vec![checked_intern(interner, TreeNodeHandle { node: root })?]
        } else {
            Vec::new()
        }
    } else {
        build::build_leaves(&target_segment, work, interner)?
    };
    work.rows(old_items.len())?;
    Ok(NodeSplice {
        old_start: first_leaf,
        old_end: end_leaf,
        replacement,
    })
}

fn append_children<R: Relation>(
    root: &Node<R>,
    level: u16,
    start: usize,
    end: usize,
    output: &mut Vec<Node<R>>,
    work: &mut TreeWork,
) -> Result<(), TreeError> {
    for index in start..end {
        let node = node_at_level(root, level, index, work)?;
        output.extend(
            node.children()
                .ok_or(TreeError::InvalidRoot)?
                .iter()
                .cloned(),
        );
    }
    Ok(())
}

type ParentSpliceWindow<R> = (usize, usize, usize, Vec<Node<R>>);

fn parent_splice_window<R: Relation>(
    root: &Node<R>,
    lower: NodeSplice<R>,
    level: u16,
    work: &mut TreeWork,
) -> Result<ParentSpliceWindow<R>, TreeError> {
    let lower_total = root.node_count_at_level(level - 1);
    if lower.old_start >= lower_total || lower.old_end > lower_total {
        return Err(TreeError::InvalidRoot);
    }
    let lower_start_node = node_at_level(root, level - 1, lower.old_start, work)?;
    let current_start = find_node_index_at_level(
        root,
        level,
        lower_start_node.first_key().ok_or(TreeError::InvalidRoot)?,
        work,
    )?;
    let lower_end_node = node_at_level(root, level - 1, lower.old_end - 1, work)?;
    let current_end = find_node_index_at_level(
        root,
        level,
        lower_end_node.first_key().ok_or(TreeError::InvalidRoot)?,
        work,
    )?
    .checked_add(1)
    .ok_or(TreeError::Overflow)?;
    let current_total = root.node_count_at_level(level);
    let first_current = node_at_level(root, level, current_start, work)?;
    let base_lower = find_node_index_at_level(
        root,
        level - 1,
        first_current.first_key().ok_or(TreeError::InvalidRoot)?,
        work,
    )?;
    let relative_start = lower
        .old_start
        .checked_sub(base_lower)
        .ok_or(TreeError::InvalidRoot)?;
    let relative_end = lower
        .old_end
        .checked_sub(base_lower)
        .ok_or(TreeError::InvalidRoot)?;
    let mut target_children = Vec::new();
    append_children(
        root,
        level,
        current_start,
        current_end,
        &mut target_children,
        work,
    )?;
    let retained_children = target_children.len();
    if relative_end > target_children.len() || relative_start > relative_end {
        return Err(TreeError::InvalidRoot);
    }
    let replaced_children = relative_end
        .checked_sub(relative_start)
        .ok_or(TreeError::InvalidRoot)?;
    target_children.splice(relative_start..relative_end, lower.replacement);
    work.reuse(
        retained_children
            .checked_sub(replaced_children)
            .ok_or(TreeError::InvalidRoot)?,
    )?;
    Ok((current_start, current_end, current_total, target_children))
}

fn extend_parent_boundary<R: Relation, I: TreeInterner<R>>(
    root: &Node<R>,
    level: u16,
    mut end_current: usize,
    current_total: usize,
    target_children: &mut Vec<Node<R>>,
    work: &mut TreeWork,
    _interner: &I,
) -> Result<usize, TreeError> {
    while end_current < current_total {
        let suffix = node_at_level(root, level, end_current, work)?;
        let children = suffix.children().ok_or(TreeError::InvalidRoot)?;
        let suffix_key = children
            .first()
            .and_then(|child| child.first_key())
            .ok_or(TreeError::InvalidRoot)?;
        if boundary_is_cut::<R, _>(
            target_children.iter().filter_map(|child| child.first_key()),
            Some(suffix_key),
            target_children.len(),
            level,
        )? {
            break;
        }
        target_children.extend(children.iter().cloned());
        work.reuse(children.len())?;
        end_current = end_current.checked_add(1).ok_or(TreeError::Overflow)?;
    }
    Ok(end_current)
}

fn advance_splice<R: Relation, I: TreeInterner<R>>(
    root: &Node<R>,
    lower: NodeSplice<R>,
    level: u16,
    root_level: u16,
    work: &mut TreeWork,
    interner: &I,
) -> Result<NodeSplice<R>, TreeError> {
    if level == 0 {
        return Err(TreeError::InvalidRoot);
    }
    let (current_start, current_end, current_total, mut target_children) =
        parent_splice_window(root, lower, level, work)?;
    let end_current = extend_parent_boundary(
        root,
        level,
        current_end,
        current_total,
        &mut target_children,
        work,
        interner,
    )?;
    let is_root = level == root_level && current_start == 0 && end_current == current_total;
    if target_children.is_empty() {
        return Ok(NodeSplice {
            old_start: current_start,
            old_end: end_current,
            replacement: Vec::new(),
        });
    }
    if is_root && target_children.len() == 1 {
        return Ok(NodeSplice {
            old_start: current_start,
            old_end: end_current,
            replacement: target_children,
        });
    }
    let replacement = build::build_branches(&target_children, level, work, interner)?;
    Ok(NodeSplice {
        old_start: current_start,
        old_end: end_current,
        replacement,
    })
}

fn build_parent_levels<R: Relation, I: TreeInterner<R>>(
    mut nodes: Vec<Node<R>>,
    mut level: u16,
    work: &mut TreeWork,
    interner: &I,
) -> Result<Node<R>, TreeError> {
    if nodes.is_empty() {
        let root = build::empty_node();
        work.rebuild_node(&root)?;
        return checked_intern(interner, TreeNodeHandle { node: root });
    }
    while nodes.len() > 1 {
        let next = build::build_branches(&nodes, level, work, interner)?;
        if next.len() >= nodes.len() {
            return Err(TreeError::InvalidRoot);
        }
        nodes = next;
        level = level.checked_add(1).ok_or(TreeError::Overflow)?;
    }
    nodes.pop().ok_or(TreeError::InvalidRoot)
}

pub(super) fn apply_structural<R: Relation, I: TreeInterner<R>>(
    root: &Node<R>,
    changes: &[TreeChange<R>],
    work: &mut TreeWork,
    interner: &I,
) -> Result<Node<R>, TreeError> {
    let mut splice = leaf_splice(root, changes, work, interner)?;
    let root_level = root.level();
    if root_level == 0 {
        if splice.replacement.len() == 1 {
            return Ok(splice.replacement.remove(0));
        }
        return build_parent_levels(splice.replacement, 1, work, interner);
    }
    for level in 1..=root_level {
        splice = advance_splice(root, splice, level, root_level, work, interner)?;
    }
    if splice.replacement.len() == 1 {
        return Ok(splice.replacement.remove(0));
    }
    build_parent_levels(
        splice.replacement,
        root_level.checked_add(1).ok_or(TreeError::Overflow)?,
        work,
        interner,
    )
}

pub(super) fn replace_existing<R: Relation, I: TreeInterner<R>>(
    node: &Node<R>,
    key: &R::Key,
    value: R::Value,
    work: &mut TreeWork,
    interner: &I,
) -> Result<Node<R>, TreeError> {
    work.visit()?;
    if node.level() == 0 {
        let entries = node.entries().ok_or(TreeError::InvalidRoot)?;
        let mut next = entries.to_vec();
        let Some((_, slot)) = next.iter_mut().find(|(candidate, _)| candidate == key) else {
            return Err(TreeError::InvalidRoot);
        };
        *slot = value;
        work.rows(entries.len())?;
        let leaf = build::make_leaf(next, interner, work)?;
        return Ok(leaf);
    }
    let children = node.children().ok_or(TreeError::InvalidRoot)?;
    let index = children
        .partition_point(|child| child.first_key().is_some_and(|first| first <= key))
        .saturating_sub(1)
        .min(children.len().saturating_sub(1));
    let mut next = children.to_vec();
    next[index] = replace_existing(&children[index], key, value, work, interner)?;
    work.reuse(children.len().saturating_sub(1))?;
    let branch = build::make_branch(node.level(), next, interner, work)?;
    Ok(branch)
}
