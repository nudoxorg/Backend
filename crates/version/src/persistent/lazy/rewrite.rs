//! Path-copy updates over a lazily opened canonical tree.

use super::helpers::{
    OverlayLoader, child_claim, child_node_from_result, committed_child, leaf_probe_cuts,
    make_branches, make_leaves,
};
use super::{
    LazyPreparedUpdate, LazyTree, LazyTreeError, LazyTreeWork, RewriteResult, TreeNodeLoader,
};
use crate::tree::anchored_cut_points_children;
use crate::{
    CanonicalNode, CanonicalRelation, CheckedCanonicalRoot, CommittedChild, DEFAULT_CUT_POLICY,
    IdContext, MapChange, NodeError, PersistentTree, TreeChange, TreeError, UntrustedId,
    canonical_branch_from_commitments, canonical_empty,
};
use std::borrow::Borrow;

impl<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> LazyTree<'a, R, L> {
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

    /// Loads and admits one claimed node, counting it on this tree.
    pub(super) fn load(
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
