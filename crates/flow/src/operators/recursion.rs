//! Bounded recursive rederivation and SCC-safe fallbacks.

use crate::{FlowError, WorkScope};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    mem::size_of,
};

/// Marks the scope that must be rederived after recursive graph changes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecursionBarrier<K> {
    affected: BTreeSet<K>,
    complete: bool,
    max_work: usize,
}

impl<K: Clone + Ord> RecursionBarrier<K> {
    /// Creates a barrier with a bounded rederivation budget.
    #[must_use]
    pub fn new(max_work: usize) -> Self {
        Self {
            affected: BTreeSet::new(),
            complete: false,
            max_work,
        }
    }

    /// Adds changed nodes to the affected region.
    pub fn mark<I: IntoIterator<Item = K>>(&mut self, changed: I) {
        self.affected.extend(changed);
        self.complete = false;
    }

    /// Returns the currently affected nodes.
    #[must_use]
    pub fn affected(&self) -> &BTreeSet<K> {
        &self.affected
    }

    /// Returns whether the affected scope has reached a closed fixed point.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Closes the barrier when work stayed within its declared bound.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::RecursionWorkLimit`] when `work` exceeds the
    /// barrier's declared budget.
    pub fn close(&mut self, work: usize) -> Result<(), FlowError> {
        if work > self.max_work {
            return Err(FlowError::RecursionWorkLimit);
        }
        self.complete = true;
        Ok(())
    }

    /// Reopens the barrier after another recursive input delta.
    pub fn reopen(&mut self) {
        self.complete = false;
    }
}

/// Computes the weakly connected region touched by changed graph nodes.
///
/// # Errors
///
/// Returns [`FlowError::RecursionWorkLimit`] when the affected region exceeds
/// `max_work`.
pub fn affected_region<K: Clone + Ord>(
    edges: &BTreeMap<K, BTreeSet<K>>,
    changed: &BTreeSet<K>,
    max_work: usize,
) -> Result<BTreeSet<K>, FlowError> {
    let mut reverse = BTreeMap::<K, BTreeSet<K>>::new();
    for (source, targets) in edges {
        for target in targets {
            reverse
                .entry(target.clone())
                .or_default()
                .insert(source.clone());
        }
    }
    let mut queue = changed.iter().cloned().collect::<VecDeque<_>>();
    let mut region = changed.clone();
    while let Some(node) = queue.pop_front() {
        if region.len() > max_work {
            return Err(FlowError::RecursionWorkLimit);
        }
        let neighbors = edges
            .get(&node)
            .into_iter()
            .flat_map(BTreeSet::iter)
            .chain(reverse.get(&node).into_iter().flat_map(BTreeSet::iter));
        for neighbor in neighbors {
            if region.insert(neighbor.clone()) {
                queue.push_back(neighbor.clone());
            }
        }
    }
    Ok(region)
}

/// Computes an affected weak region while charging graph scans and queue work
/// to `scope`.
///
/// The ordinary [`affected_region`] helper remains a compatibility oracle for
/// small callers. Planners handling shared recursive graphs should use this
/// entry point so reverse-edge construction cannot retain unbounded work
/// before the caller's quota is consulted.
///
/// # Errors
///
/// Returns [`FlowError::RecursionWorkLimit`] when graph or queue work exceeds
/// `scope`, or [`FlowError::Overflow`] when byte accounting wraps.
pub fn affected_region_budgeted<K: Clone + Ord>(
    edges: &BTreeMap<K, BTreeSet<K>>,
    changed: &BTreeSet<K>,
    scope: &mut WorkScope,
) -> Result<BTreeSet<K>, FlowError> {
    let mut reverse = BTreeMap::<K, BTreeSet<K>>::new();
    for (source, targets) in edges {
        for target in targets {
            scope.charge_rows(1)?;
            scope.charge_bytes(size_of::<K>().checked_mul(2).ok_or(FlowError::Overflow)?)?;
            reverse
                .entry(target.clone())
                .or_default()
                .insert(source.clone());
        }
    }
    scope.charge_rows(changed.len())?;
    scope.charge_bytes(
        size_of::<K>()
            .checked_mul(changed.len())
            .ok_or(FlowError::Overflow)?,
    )?;
    let mut queue = changed.iter().cloned().collect::<VecDeque<_>>();
    let mut region = changed.clone();
    while let Some(node) = queue.pop_front() {
        let neighbors = edges
            .get(&node)
            .into_iter()
            .flat_map(BTreeSet::iter)
            .chain(reverse.get(&node).into_iter().flat_map(BTreeSet::iter));
        for neighbor in neighbors {
            if !region.contains(neighbor) {
                scope.charge_rows(1)?;
                scope.charge_bytes(size_of::<K>())?;
            }
            if region.insert(neighbor.clone()) {
                queue.push_back(neighbor.clone());
            }
        }
    }
    Ok(region)
}

/// Recomputes a finite transitive closure behind an affected-region barrier.
///
/// The closure is recomputed from the complete edge relation because paths can
/// enter or leave the changed weak component. This is the exact fallback for
/// non-monotone deletion and SCC split cases; the work limit makes incomplete
/// execution explicit to the caller.
///
/// # Errors
///
/// Returns [`FlowError::RecursionWorkLimit`] when graph traversal exceeds
/// `max_work`.
pub fn recursive_recompute<K: Clone + Ord>(
    edges: &BTreeMap<K, BTreeSet<K>>,
    changed: &BTreeSet<K>,
    max_work: usize,
) -> Result<BTreeSet<(K, K)>, FlowError> {
    if changed.is_empty() {
        return Ok(BTreeSet::new());
    }
    let _region = affected_region(edges, changed, max_work)?;
    let mut out = BTreeSet::new();
    for source in edges.keys() {
        let mut queue = VecDeque::from([source.clone()]);
        let mut seen = BTreeSet::new();
        while let Some(node) = queue.pop_front() {
            if !seen.insert(node.clone()) {
                continue;
            }
            if seen
                .len()
                .checked_add(out.len())
                .is_none_or(|work| work > max_work)
            {
                return Err(FlowError::RecursionWorkLimit);
            }
            if node != *source {
                out.insert((source.clone(), node.clone()));
            }
            if let Some(next) = edges.get(&node) {
                queue.extend(next.iter().cloned());
            }
        }
    }
    Ok(out)
}

/// Recomputes transitive closure only for sources in the changed weak
/// component. Consumers that retain a previous closure can replace this
/// region and leave every disjoint component untouched.
///
/// The returned pairs are exact for the affected component, including paths
/// that cross an SCC boundary. This is the mutation primitive used by a
/// recursive operator; [`recursive_recompute`] remains the explicit
/// full-view oracle for callers that do not retain disjoint state.
///
/// # Errors
///
/// Returns [`FlowError::RecursionWorkLimit`] when the affected region or its
/// traversal exceeds `max_work`.
pub fn recursive_recompute_region<K: Clone + Ord>(
    edges: &BTreeMap<K, BTreeSet<K>>,
    changed: &BTreeSet<K>,
    max_work: usize,
) -> Result<BTreeSet<(K, K)>, FlowError> {
    if changed.is_empty() {
        return Ok(BTreeSet::new());
    }
    let region = affected_region(edges, changed, max_work)?;
    let mut out = BTreeSet::new();
    for source in region {
        let mut queue = VecDeque::from([source.clone()]);
        let mut seen = BTreeSet::new();
        while let Some(node) = queue.pop_front() {
            if !seen.insert(node.clone()) {
                continue;
            }
            if seen
                .len()
                .checked_add(out.len())
                .is_some_and(|work| work > max_work)
            {
                return Err(FlowError::RecursionWorkLimit);
            }
            if node != source {
                out.insert((source.clone(), node.clone()));
            }
            if let Some(next) = edges.get(&node) {
                queue.extend(next.iter().cloned());
            }
        }
    }
    Ok(out)
}

/// Recomputes a recursive region with one explicit graph/queue work scope.
///
/// This variant keeps the exact SCC boundary behavior of
/// [`recursive_recompute_region`] while making reverse-index construction and
/// transitive traversal observable to a scheduler.
///
/// # Errors
///
/// Returns [`FlowError::RecursionWorkLimit`] when the affected region or its
/// traversal exceeds `scope`.
pub fn recursive_recompute_region_budgeted<K: Clone + Ord>(
    edges: &BTreeMap<K, BTreeSet<K>>,
    changed: &BTreeSet<K>,
    scope: &mut WorkScope,
) -> Result<BTreeSet<(K, K)>, FlowError> {
    if changed.is_empty() {
        return Ok(BTreeSet::new());
    }
    let region = affected_region_budgeted(edges, changed, scope)?;
    let mut out = BTreeSet::new();
    for source in region {
        let mut queue = VecDeque::from([source.clone()]);
        let mut seen = BTreeSet::new();
        while let Some(node) = queue.pop_front() {
            if !seen.insert(node.clone()) {
                continue;
            }
            scope.charge_rows(1)?;
            scope.charge_bytes(size_of::<K>())?;
            if node != source {
                out.insert((source.clone(), node.clone()));
            }
            if let Some(next) = edges.get(&node) {
                queue.extend(next.iter().cloned());
            }
        }
    }
    Ok(out)
}
