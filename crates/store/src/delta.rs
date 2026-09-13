use super::{
    BTreeMap, CoverageWitness, DeltaId, Node, OrderedMap, RawRelation, StateRoot, StoreError,
    StoredValue, checked_key_len, validate_value,
};

pub(crate) fn delta_id_for(
    base: StateRoot<RawRelation>,
    target: StateRoot<RawRelation>,
    changes: &[Change],
    coverage: CoverageWitness,
) -> Result<DeltaId<RawRelation>, StoreError> {
    if !coverage.is_authorized_complete() {
        return Err(StoreError::IncompleteCoverage);
    }
    validate_changes(changes)?;
    let rows = changes
        .iter()
        .filter(|change| change.before != change.after)
        .map(|change| backend_version::MapChange {
            key: change.key.clone(),
            before: change.before.clone(),
            after: change.after.clone(),
        })
        .collect::<Vec<_>>();
    Ok(backend_version::canonical_delta_id::<RawRelation>(
        base, target, &rows,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One checked key/value replacement in an ordered map.
pub struct Change {
    /// Ordered map key.
    pub key: Vec<u8>,
    /// Value expected at the key before the change.
    pub before: Option<StoredValue>,
    /// Value to publish at the key after the change.
    pub after: Option<StoredValue>,
}

pub(crate) fn validate_changes(changes: &[Change]) -> Result<(), StoreError> {
    for pair in changes.windows(2) {
        if pair[0].key >= pair[1].key {
            return Err(StoreError::MalformedDelta);
        }
    }
    for change in changes {
        checked_key_len(change.key.len())?;
        if let Some(before) = &change.before {
            validate_value(before)?;
        }
        if let Some(after) = &change.after {
            validate_value(after)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Checked transition between two canonical map roots.
pub struct MapDelta {
    /// Root the transition starts from.
    base: StateRoot<RawRelation>,
    /// Root the transition produces.
    target: StateRoot<RawRelation>,
    /// Strictly key-ordered map changes.
    changes: Vec<Change>,
    /// Content-addressed transition identity.
    id: DeltaId<RawRelation>,
    /// Producer-supplied coverage capability used to prepare this transition.
    coverage: CoverageWitness,
}

impl MapDelta {
    pub(crate) fn from_parts(
        base: StateRoot<RawRelation>,
        target: StateRoot<RawRelation>,
        changes: Vec<Change>,
        id: DeltaId<RawRelation>,
        coverage: CoverageWitness,
    ) -> Self {
        Self {
            base,
            target,
            changes,
            id,
            coverage,
        }
    }

    /// Returns the root the transition starts from.
    #[must_use]
    pub const fn base(&self) -> StateRoot<RawRelation> {
        self.base
    }

    /// Returns the root the transition produces.
    #[must_use]
    pub const fn target(&self) -> StateRoot<RawRelation> {
        self.target
    }

    /// Returns the canonical transition identity.
    #[must_use]
    pub const fn id(&self) -> DeltaId<RawRelation> {
        self.id
    }

    /// Iterates the strictly ordered transition changes.
    pub fn changes(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter()
    }

    #[cfg(test)]
    pub(crate) fn changes_slice(&self) -> &[Change] {
        &self.changes
    }

    /// Applies this transition after checking its base and target roots.
    ///
    /// # Errors
    ///
    /// Returns an error when the base, before-values, transition identity, or
    /// resulting target root does not match the supplied map.
    pub fn apply(&self, map: &OrderedMap) -> Result<OrderedMap, StoreError> {
        validate_changes(&self.changes)?;
        if map.state_root() != self.base {
            return Err(StoreError::WrongBase);
        }
        if delta_id_for(self.base, self.target, &self.changes, self.coverage)? != self.id {
            return Err(StoreError::MalformedDelta);
        }
        map.apply(&self.changes).and_then(|map| {
            if map.state_root() == self.target {
                Ok(map)
            } else {
                Err(StoreError::TargetMismatch)
            }
        })
    }

    /// Reverses the before/after values and root direction.
    ///
    /// # Errors
    ///
    /// Returns an error when the reversed transition cannot be admitted by
    /// the canonical delta identity validator.
    pub fn inverse(&self) -> Result<Self, StoreError> {
        let changes = self
            .changes
            .iter()
            .map(|change| Change {
                key: change.key.clone(),
                before: change.after.clone(),
                after: change.before.clone(),
            })
            .collect::<Vec<_>>();
        let id = delta_id_for(self.target, self.base, &changes, self.coverage)?;
        Ok(Self {
            base: self.target,
            target: self.base,
            changes,
            id,
            coverage: self.coverage,
        })
    }

    /// Composes this transition with an adjacent transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the transitions are not adjacent or overlapping
    /// before-values disagree.
    pub fn compose(&self, next: &Self) -> Result<Self, StoreError> {
        validate_changes(&self.changes)?;
        validate_changes(&next.changes)?;
        if self.target != next.base {
            return Err(StoreError::WrongBase);
        }
        let mut all = BTreeMap::new();
        for change in &self.changes {
            all.insert(change.key.clone(), change.clone());
        }
        for change in &next.changes {
            if let Some(existing) = all.get(&change.key)
                && existing.after != change.before
            {
                return Err(StoreError::MalformedDelta);
            }
            all.entry(change.key.clone())
                .and_modify(|existing| existing.after.clone_from(&change.after))
                .or_insert_with(|| change.clone());
        }
        let changes = all
            .into_values()
            .filter(|change| change.before != change.after)
            .collect::<Vec<_>>();
        let id = delta_id_for(self.base, next.target, &changes, self.coverage)?;
        Ok(Self {
            base: self.base,
            target: next.target,
            changes,
            id,
            coverage: self.coverage,
        })
    }
}

/// Counters collected while comparing two persistent roots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiffStats {
    /// Number of node commitments inspected.
    pub visited_nodes: usize,
    /// Number of unequal leaf ranges found.
    pub changed_leaves: usize,
}

/// Counters collected while preparing an incremental map update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UpdateStats {
    /// Number of old nodes read while preparing the update.
    pub visited_nodes: usize,
    /// Number of canonical nodes encoded for the resulting root.
    pub copied_nodes: usize,
    /// Number of canonical nodes retained from unchanged children.
    pub reused_nodes: usize,
    /// Total bytes in newly encoded canonical nodes.
    pub encoded_bytes: usize,
}

/// Upper bound on node work permitted for an incremental publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkBudget {
    /// Maximum number of old nodes that may be visited.
    pub max_nodes: usize,
}

impl WorkBudget {
    /// Creates a node-visit budget.
    #[must_use]
    pub const fn new(max_nodes: usize) -> Self {
        Self { max_nodes }
    }
}

pub(crate) fn check_budget(
    stats: &UpdateStats,
    budget: Option<WorkBudget>,
) -> Result<(), StoreError> {
    if budget.is_some_and(|limit| stats.visited_nodes > limit.max_nodes) {
        Err(StoreError::NeedsScopedRebuild)
    } else {
        Ok(())
    }
}

pub(crate) fn diff_nodes(left: &Node, right: &Node, stats: &mut DiffStats) {
    stats.visited_nodes += 1;
    if left.id() == right.id() {
        return;
    }
    match (left.entries(), right.entries()) {
        (Some(left_entries), Some(right_entries)) => {
            if left_entries != right_entries {
                stats.changed_leaves += 1;
            }
        }
        (None, None) => {
            let left_children = left.children().collect::<Vec<_>>();
            let right_children = right.children().collect::<Vec<_>>();
            if same_child_layout(&left_children, &right_children) {
                for (left_child, right_child) in left_children.iter().zip(&right_children) {
                    diff_nodes(left_child, right_child, stats);
                }
            } else {
                merge_leaf_stream_nodes(&left_children, &right_children, stats, None, false);
            }
        }
        _ => merge_leaf_stream(left, right, stats, None),
    }
}

pub(crate) fn collect_changes(left: &Node, right: &Node, changes: &mut Vec<Change>) {
    let mut stats = DiffStats::default();
    collect_structural(left, right, &mut stats, changes);
}

fn collect_structural(left: &Node, right: &Node, stats: &mut DiffStats, changes: &mut Vec<Change>) {
    stats.visited_nodes += 1;
    if left.id() == right.id() {
        return;
    }
    match (left.entries(), right.entries()) {
        (Some(left_entries), Some(right_entries)) => {
            if left_entries != right_entries {
                merge_leaf_entries(left_entries, right_entries, Some(changes));
            }
        }
        (None, None) => {
            let left_children = left.children().collect::<Vec<_>>();
            let right_children = right.children().collect::<Vec<_>>();
            if same_child_layout(&left_children, &right_children) {
                for (left_child, right_child) in left_children.iter().zip(&right_children) {
                    collect_structural(left_child, right_child, stats, changes);
                }
            } else {
                merge_leaf_stream_nodes(
                    &left_children,
                    &right_children,
                    stats,
                    Some(changes),
                    false,
                );
            }
        }
        _ => merge_leaf_stream(left, right, stats, Some(changes)),
    }
}

fn same_child_layout(left: &[Node], right: &[Node]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            let left_summary = left.summary();
            let right_summary = right.summary();
            left_summary.level == right_summary.level
                && left_summary.first_key == right_summary.first_key
        })
}

struct LeafCursor {
    stack: Vec<(Node, usize)>,
    roots_already_counted: usize,
}

impl LeafCursor {
    fn from_nodes(nodes: &[Node], roots_already_counted: bool) -> Self {
        Self {
            stack: nodes.iter().rev().cloned().map(|node| (node, 0)).collect(),
            roots_already_counted: if roots_already_counted {
                nodes.len()
            } else {
                0
            },
        }
    }

    fn next_nonempty(&mut self, stats: &mut DiffStats) -> Option<Node> {
        loop {
            let leaf = self.next_leaf(stats)?;
            if leaf.entries().is_some_and(|entries| !entries.is_empty()) {
                return Some(leaf);
            }
        }
    }

    fn next_leaf(&mut self, stats: &mut DiffStats) -> Option<Node> {
        loop {
            let (node, child_index) = self.stack.last_mut()?;
            if *child_index == 0 {
                if self.roots_already_counted > 0 {
                    self.roots_already_counted -= 1;
                } else {
                    stats.visited_nodes += 1;
                }
            }
            if node.entries().is_some() {
                let leaf = node.clone();
                self.stack.pop();
                return Some(leaf);
            }
            let children = node.children().collect::<Vec<_>>();
            if *child_index < children.len() {
                let child = children[*child_index].clone();
                *child_index += 1;
                self.stack.push((child, 0));
            } else {
                self.stack.pop();
            }
        }
    }
}

fn merge_leaf_stream(
    left: &Node,
    right: &Node,
    stats: &mut DiffStats,
    changes: Option<&mut Vec<Change>>,
) {
    merge_leaf_stream_nodes(
        std::slice::from_ref(left),
        std::slice::from_ref(right),
        stats,
        changes,
        true,
    );
}

fn merge_leaf_stream_nodes(
    left: &[Node],
    right: &[Node],
    stats: &mut DiffStats,
    mut changes: Option<&mut Vec<Change>>,
    roots_already_counted: bool,
) {
    let mut left_cursor = LeafCursor::from_nodes(left, roots_already_counted);
    let mut right_cursor = LeafCursor::from_nodes(right, roots_already_counted);
    let mut left_leaf = left_cursor.next_nonempty(stats);
    let mut right_leaf = right_cursor.next_nonempty(stats);
    let mut left_at = 0usize;
    let mut right_at = 0usize;
    let mut left_dirty = false;
    let mut right_dirty = false;
    while left_leaf.is_some() || right_leaf.is_some() {
        let equal_leaves = match (left_leaf.as_ref(), right_leaf.as_ref()) {
            (Some(left_node), Some(right_node)) => {
                left_at == 0 && right_at == 0 && left_node.id() == right_node.id()
            }
            _ => false,
        };
        if equal_leaves {
            advance_diff_leaf(
                &mut left_cursor,
                &mut left_leaf,
                &mut left_at,
                &mut left_dirty,
                stats,
            );
            advance_diff_leaf(
                &mut right_cursor,
                &mut right_leaf,
                &mut right_at,
                &mut right_dirty,
                stats,
            );
            continue;
        }

        let left_item = left_leaf
            .as_ref()
            .and_then(|node| node.entries().and_then(|entries| entries.get(left_at)));
        let right_item = right_leaf
            .as_ref()
            .and_then(|node| node.entries().and_then(|entries| entries.get(right_at)));
        let Some((left_advanced, right_advanced, left_changed, right_changed)) =
            merge_leaf_entries_one(left_item, right_item, &mut changes)
        else {
            break;
        };
        left_dirty |= left_changed;
        right_dirty |= right_changed;
        if left_advanced {
            left_at += 1;
        }
        if right_advanced {
            right_at += 1;
        }
        let left_done = left_leaf.as_ref().is_some_and(|node| {
            node.entries()
                .is_some_and(|entries| left_at >= entries.len())
        });
        if left_done {
            advance_diff_leaf(
                &mut left_cursor,
                &mut left_leaf,
                &mut left_at,
                &mut left_dirty,
                stats,
            );
        }
        let right_done = right_leaf.as_ref().is_some_and(|node| {
            node.entries()
                .is_some_and(|entries| right_at >= entries.len())
        });
        if right_done {
            advance_diff_leaf(
                &mut right_cursor,
                &mut right_leaf,
                &mut right_at,
                &mut right_dirty,
                stats,
            );
        }
    }
}

fn merge_leaf_entries(
    left: &[(Vec<u8>, StoredValue)],
    right: &[(Vec<u8>, StoredValue)],
    mut changes: Option<&mut Vec<Change>>,
) {
    let mut left_at = 0usize;
    let mut right_at = 0usize;
    while left_at < left.len() || right_at < right.len() {
        let left_item = left.get(left_at);
        let right_item = right.get(right_at);
        let Some((left_advanced, right_advanced, _, _)) =
            merge_leaf_entries_one(left_item, right_item, &mut changes)
        else {
            break;
        };
        if left_advanced {
            left_at += 1;
        }
        if right_advanced {
            right_at += 1;
        }
    }
}

fn merge_leaf_entries_one(
    left: Option<&(Vec<u8>, StoredValue)>,
    right: Option<&(Vec<u8>, StoredValue)>,
    changes: &mut Option<&mut Vec<Change>>,
) -> Option<(bool, bool, bool, bool)> {
    match (left, right) {
        (Some((left_key, left_value)), Some((right_key, right_value))) => {
            match left_key.cmp(right_key) {
                std::cmp::Ordering::Equal => {
                    if left_value == right_value {
                        Some((true, true, false, false))
                    } else {
                        record_change(
                            changes,
                            left_key.clone(),
                            Some(left_value.clone()),
                            Some(right_value.clone()),
                        );
                        Some((true, true, true, true))
                    }
                }
                std::cmp::Ordering::Less => {
                    record_change(changes, left_key.clone(), Some(left_value.clone()), None);
                    Some((true, false, true, false))
                }
                std::cmp::Ordering::Greater => {
                    record_change(changes, right_key.clone(), None, Some(right_value.clone()));
                    Some((false, true, false, true))
                }
            }
        }
        (Some((key, value)), None) => {
            record_change(changes, key.clone(), Some(value.clone()), None);
            Some((true, false, true, false))
        }
        (None, Some((key, value))) => {
            record_change(changes, key.clone(), None, Some(value.clone()));
            Some((false, true, false, true))
        }
        (None, None) => None,
    }
}

fn record_change(
    changes: &mut Option<&mut Vec<Change>>,
    key: Vec<u8>,
    before: Option<StoredValue>,
    after: Option<StoredValue>,
) {
    if let Some(output) = changes.as_deref_mut() {
        output.push(Change { key, before, after });
    }
}

fn advance_diff_leaf(
    cursor: &mut LeafCursor,
    leaf: &mut Option<Node>,
    at: &mut usize,
    dirty: &mut bool,
    stats: &mut DiffStats,
) {
    if *dirty {
        stats.changed_leaves += 1;
    }
    *leaf = cursor.next_nonempty(stats);
    *at = 0;
    *dirty = false;
}
