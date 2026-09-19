//! Ordered map facade over the shared backend-version persistent tree.

pub(crate) use self::node::{Cas, Node};
use self::node::{StoreInterner, map_tree_error};
use super::{
    Arc, Change, CoverageWitness, DEFAULT_CUT_POLICY, DiffStats, Hash, HashMap, KeyProof, MapDelta,
    Mutex, Proof, RangeProof, RawRelation, StateRoot, StoreError, StoredValue, UpdateStats,
    WorkBudget, check_budget, collect_changes, delta_id_for, diff_nodes, fmt, key_proof,
    range_proof, validate_changes, validate_entries,
};
use std::collections::HashSet;

mod diff;
mod node;

type Persistent = backend_version::PersistentTree<RawRelation, StoreInterner>;

/// Persistent ordered map backed by the shared canonical Merkle tree.
#[derive(Clone)]
pub struct OrderedMap {
    tree: Persistent,
    commitment: StateRoot<RawRelation>,
    len: usize,
    cas: Cas,
    coverage: Option<CoverageWitness>,
    pending_tree_source: Option<Hash>,
    pending_tree_frontier: Option<Arc<[Node]>>,
}

impl fmt::Debug for OrderedMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OrderedMap")
            .field("root", &self.state_root())
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl PartialEq for OrderedMap {
    fn eq(&self, other: &Self) -> bool {
        self.state_root() == other.state_root()
    }
}

impl Eq for OrderedMap {}

impl OrderedMap {
    /// Minimum canonical entries before an anchored cut may occur.
    pub const LEAF_MIN: usize = DEFAULT_CUT_POLICY.min_entries as usize;
    /// Target canonical entries used by the shared cut policy.
    pub const LEAF_TARGET: usize = DEFAULT_CUT_POLICY.target_entries as usize;
    /// Maximum canonical entries in any leaf or branch.
    pub const LEAF_MAX: usize = DEFAULT_CUT_POLICY.max_entries as usize;

    /// Creates an empty canonical map.
    ///
    /// # Errors
    ///
    /// Returns an error if the shared canonical kernel rejects the checked
    /// root or its work accounting cannot be represented.
    pub fn try_empty() -> Result<Self, StoreError> {
        Self::try_empty_with_optional_coverage(None)
    }

    /// Creates an empty map carrying an authority-produced coverage witness.
    ///
    /// # Errors
    ///
    /// Returns an error if the shared canonical kernel rejects the checked
    /// root or its work accounting cannot be represented.
    pub fn try_empty_with_coverage(coverage: CoverageWitness) -> Result<Self, StoreError> {
        Self::try_empty_with_optional_coverage(Some(coverage))
    }

    fn try_empty_with_optional_coverage(
        coverage: Option<CoverageWitness>,
    ) -> Result<Self, StoreError> {
        let cas = Arc::new(Mutex::new(HashMap::new()));
        let interner = StoreInterner { cas: cas.clone() };
        let (tree, _) = backend_version::PersistentTree::empty_with_interner(interner)
            .map_err(map_tree_error)?;
        let root = tree.root_handle();
        Ok(Self {
            commitment: tree.root().commitment(),
            len: root.summary().len,
            tree,
            cas,
            coverage,
            pending_tree_source: None,
            pending_tree_frontier: None,
        })
    }

    /// Creates a map from arbitrary input order after rejecting duplicate keys.
    ///
    /// # Errors
    ///
    /// Returns an error when input keys are duplicated, a value is too large,
    /// or canonical node admission fails.
    pub fn try_from_iter<I: IntoIterator<Item = (Vec<u8>, StoredValue)>>(
        items: I,
    ) -> Result<Self, StoreError> {
        Self::try_from_iter_optional_coverage(items, None)
    }

    /// Creates a map from arbitrary input order and retains an authority
    /// coverage witness for later delta preparation.
    ///
    /// # Errors
    ///
    /// Returns an error when input keys are duplicated, a value is too large,
    /// or canonical node admission fails.
    pub fn try_from_iter_with_coverage<I: IntoIterator<Item = (Vec<u8>, StoredValue)>>(
        items: I,
        coverage: CoverageWitness,
    ) -> Result<Self, StoreError> {
        Self::try_from_iter_optional_coverage(items, Some(coverage))
    }

    fn try_from_iter_optional_coverage<I: IntoIterator<Item = (Vec<u8>, StoredValue)>>(
        items: I,
        coverage: Option<CoverageWitness>,
    ) -> Result<Self, StoreError> {
        let mut ordered = items.into_iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| left.0.cmp(&right.0));
        if ordered.windows(2).any(|window| window[0].0 == window[1].0) {
            return Err(StoreError::MalformedDelta);
        }
        validate_entries(&ordered)?;
        Self::build_ordered(&ordered, coverage)
    }

    fn build_ordered(
        ordered: &[(Vec<u8>, StoredValue)],
        coverage: Option<CoverageWitness>,
    ) -> Result<Self, StoreError> {
        let cas = Arc::new(Mutex::new(HashMap::new()));
        let interner = StoreInterner { cas: cas.clone() };
        let (tree, _) = backend_version::PersistentTree::from_sorted_items_with_interner_with_work(
            ordered, interner,
        )
        .map_err(map_tree_error)?;
        let root = tree.root_handle();
        Ok(Self {
            commitment: tree.root().commitment(),
            len: root.summary().len,
            tree,
            cas,
            coverage,
            pending_tree_source: None,
            pending_tree_frontier: None,
        })
    }

    /// Returns the coverage capability retained by this map, if any.
    #[must_use]
    pub const fn coverage(&self) -> Option<CoverageWitness> {
        self.coverage
    }

    /// Returns the canonical state identity of this map.
    #[must_use]
    pub fn state_root(&self) -> StateRoot<RawRelation> {
        self.commitment
    }

    pub(crate) fn root_node(&self) -> Node {
        self.tree.root_handle()
    }

    pub(crate) fn pending_tree_frontier(&self) -> Option<(Hash, Arc<[Node]>)> {
        self.pending_tree_source
            .zip(self.pending_tree_frontier.clone())
    }

    #[cfg(test)]
    pub(crate) fn cas_counts(&self) -> (usize, usize) {
        let Ok(entries) = self.cas.lock() else {
            return (0, 0);
        };
        let total = entries.len();
        let live = entries
            .values()
            .filter(|entry| entry.upgrade().is_some())
            .count();
        (total, live)
    }

    /// Returns the value for one key, if present.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&StoredValue> {
        self.tree.get(key)
    }

    /// Returns the number of visible entries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Reports whether the map has no visible entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterates entries in canonical key order with a stack proportional to
    /// the tree height.
    #[must_use]
    pub fn iter(&self) -> OrderedMapIter<'_> {
        self.tree.iter()
    }

    /// Returns the key sequence held by every canonical leaf.
    #[must_use]
    pub fn leaf_cuts(&self) -> Vec<Vec<Vec<u8>>> {
        diff::leaves(&self.tree.root_handle())
            .into_iter()
            .map(|leaf| {
                leaf.entries()
                    .map(|entries| entries.iter().map(|(key, _)| key.clone()).collect())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Applies a checked ordered batch and returns the new immutable map.
    ///
    /// # Errors
    ///
    /// Returns an error when a change is unordered, its before-value is stale,
    /// or canonical update admission fails.
    pub fn apply(&self, changes: &[Change]) -> Result<Self, StoreError> {
        self.apply_with_stats(changes).map(|(map, _)| map)
    }

    /// Applies a batch while limiting the number of old nodes visited.
    ///
    /// # Errors
    ///
    /// Returns an error when validation fails, the node budget is insufficient,
    /// or canonical update admission fails.
    pub fn apply_with_budget(
        &self,
        changes: &[Change],
        budget: WorkBudget,
    ) -> Result<(Self, UpdateStats), StoreError> {
        self.apply_internal(changes, Some(budget))
    }

    /// Applies a batch and reports node reuse and canonical bytes emitted.
    ///
    /// # Errors
    ///
    /// Returns an error when a change is unordered, its before-value is stale,
    /// or canonical update admission fails.
    pub fn apply_with_stats(&self, changes: &[Change]) -> Result<(Self, UpdateStats), StoreError> {
        self.apply_internal(changes, None)
    }

    fn apply_internal(
        &self,
        changes: &[Change],
        budget: Option<WorkBudget>,
    ) -> Result<(Self, UpdateStats), StoreError> {
        validate_changes(changes)?;
        let mut stats = UpdateStats::default();
        let root = self.tree.root_handle();
        for change in changes {
            if !matches_before_counted(
                &root,
                &change.key,
                change.before.as_ref(),
                &mut stats,
                budget,
            )? {
                return Err(StoreError::BeforeMismatch(change.key.clone()));
            }
        }
        if changes.iter().all(|change| change.before == change.after) {
            return Ok((self.clone(), stats));
        }

        let tree_changes = changes
            .iter()
            .map(|change| backend_version::TreeChange {
                key: change.key.clone(),
                after: change.after.clone(),
            })
            .collect::<Vec<_>>();
        let prepared = self
            .tree
            .prepare_update_with_interner(&tree_changes)
            .map_err(map_tree_error)?;
        add_work(&mut stats, prepared.work())?;
        check_budget(&stats, budget)?;
        let tree = prepared.commit();
        let new_root = tree.root_handle();
        let len = new_root.summary().len;
        let commitment = tree.root().commitment();
        let mut frontier = Vec::new();
        collect_changed_frontier(&root, &new_root, &mut frontier);
        if frontier.is_empty() {
            frontier.push(new_root.clone());
        }
        Ok((
            Self {
                tree,
                commitment,
                len,
                cas: self.cas.clone(),
                coverage: self.coverage,
                pending_tree_source: Some(*self.state_root().as_bytes()),
                pending_tree_frontier: Some(Arc::from(frontier.into_boxed_slice())),
            },
            stats,
        ))
    }

    /// Computes an ordered before/after delta to another map.
    ///
    /// # Errors
    ///
    /// Returns an error when no complete authority coverage is attached or
    /// the canonical delta identity cannot be prepared.
    #[must_use = "the prepared delta carries the canonical transition identity"]
    pub fn delta_to(&self, other: &Self) -> Result<MapDelta, StoreError> {
        let coverage = self.coverage.ok_or(StoreError::IncompleteCoverage)?;
        self.delta_to_with_coverage(other, coverage)
    }

    /// Computes a delta with an explicitly supplied authority coverage witness.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied coverage is incomplete or the
    /// canonical delta identity cannot be prepared.
    #[must_use = "the prepared delta carries the canonical transition identity"]
    pub fn delta_to_with_coverage(
        &self,
        other: &Self,
        coverage: CoverageWitness,
    ) -> Result<MapDelta, StoreError> {
        let mut changes = Vec::new();
        collect_changes(
            &self.tree.root_handle(),
            &other.tree.root_handle(),
            &mut changes,
        );
        let id = delta_id_for(self.state_root(), other.state_root(), &changes, coverage)?;
        Ok(MapDelta::from_parts(
            self.state_root(),
            other.state_root(),
            changes,
            id,
            coverage,
        ))
    }

    /// Structural comparison that prunes equal content-addressed subtrees.
    #[must_use]
    pub fn diff_stats(&self, other: &Self) -> DiffStats {
        let mut stats = DiffStats::default();
        diff_nodes(
            &self.tree.root_handle(),
            &other.tree.root_handle(),
            &mut stats,
        );
        stats
    }

    /// A checked proof for one key/value observation in this immutable root.
    #[must_use]
    pub fn prove(&self, key: &[u8]) -> Option<Proof> {
        self.get(key).map(|value| {
            let proof = key_proof(&self.tree.root_handle(), key);
            Proof {
                root: self.state_root(),
                key: key.to_vec(),
                version: value.clone(),
                leaf_entries: proof.leaf_entries,
                path: proof.path,
            }
        })
    }

    /// Builds an authenticated membership or nonmembership proof for `key`.
    #[must_use]
    pub fn prove_key(&self, key: &[u8]) -> KeyProof {
        key_proof(&self.tree.root_handle(), key)
    }

    /// Builds a proof only when `key` is absent from this root.
    #[must_use]
    pub fn prove_nonmembership(&self, key: &[u8]) -> Option<KeyProof> {
        self.get(key).is_none().then(|| self.prove_key(key))
    }

    /// Builds an authenticated half-open range proof `[start, end)`.
    ///
    /// # Errors
    ///
    /// Returns an error when both bounds are present and the lower bound is
    /// not smaller than the upper bound.
    pub fn prove_range(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<RangeProof, StoreError> {
        range_proof(&self.tree.root_handle(), start, end)
    }
}

fn collect_changed_frontier(old: &Node, new: &Node, output: &mut Vec<Node>) {
    let mut seen = HashSet::new();
    collect_changed_frontier_inner(Some(old), new, output, &mut seen);
}

fn collect_changed_frontier_inner(
    old: Option<&Node>,
    new: &Node,
    output: &mut Vec<Node>,
    seen: &mut HashSet<Hash>,
) {
    if old.is_some_and(|candidate| candidate.id() == new.id()) {
        return;
    }
    if seen.insert(new.id().to_bytes()) {
        output.push(new.clone());
    }
    let new_children = new.children().collect::<Vec<_>>();
    if new_children.is_empty() {
        return;
    }
    let old_children = old
        .map(|candidate| candidate.children().collect::<Vec<_>>())
        .unwrap_or_default();
    for child in new_children {
        if old_children
            .iter()
            .any(|candidate| candidate.id() == child.id())
        {
            continue;
        }
        let old_match = old_children.iter().find(|candidate| {
            candidate.summary().level == child.summary().level
                && candidate.summary().first_key == child.summary().first_key
        });
        if let Some(old_match) = old_match {
            collect_changed_frontier_inner(Some(old_match), &child, output, seen);
        } else {
            collect_all_nodes(&child, output, seen);
        }
    }
}

fn collect_all_nodes(node: &Node, output: &mut Vec<Node>, seen: &mut HashSet<Hash>) {
    if !seen.insert(node.id().to_bytes()) {
        return;
    }
    output.push(node.clone());
    for child in node.children() {
        collect_all_nodes(&child, output, seen);
    }
}

/// Borrowing iterator over a persistent map.
pub type OrderedMapIter<'a> = backend_version::TreeIter<'a, RawRelation>;

impl<'a> IntoIterator for &'a OrderedMap {
    type Item = (&'a Vec<u8>, &'a StoredValue);
    type IntoIter = OrderedMapIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

fn matches_before_counted(
    node: &Node,
    key: &[u8],
    expected: Option<&StoredValue>,
    stats: &mut UpdateStats,
    budget: Option<WorkBudget>,
) -> Result<bool, StoreError> {
    let mut current = node.clone();
    loop {
        stats.visited_nodes = stats
            .visited_nodes
            .checked_add(1)
            .ok_or(StoreError::Bounds)?;
        check_budget(stats, budget)?;
        if let Some(entries) = current.entries() {
            let current = entries
                .binary_search_by(|(candidate, _)| candidate.as_slice().cmp(key))
                .ok()
                .map(|index| &entries[index].1);
            return Ok(current == expected);
        }
        let mut selected = None;
        for child in current.children() {
            if child
                .canonical()
                .first_key()
                .is_some_and(|first| first.as_slice() <= key)
            {
                selected = Some(child);
            } else {
                break;
            }
        }
        current = selected.ok_or(StoreError::Corrupt)?;
    }
}

fn add_work(stats: &mut UpdateStats, work: backend_version::TreeWork) -> Result<(), StoreError> {
    stats.visited_nodes = stats
        .visited_nodes
        .checked_add(work.visited_nodes)
        .ok_or(StoreError::Bounds)?;
    stats.copied_nodes = stats
        .copied_nodes
        .checked_add(work.copied_nodes)
        .ok_or(StoreError::Bounds)?;
    stats.reused_nodes = stats
        .reused_nodes
        .checked_add(work.reused_nodes)
        .ok_or(StoreError::Bounds)?;
    stats.encoded_bytes = stats
        .encoded_bytes
        .checked_add(work.encoded_bytes)
        .ok_or(StoreError::Bounds)?;
    Ok(())
}
