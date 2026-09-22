//! Demand graph identity and scheduler readiness state.

use super::{
    AuthorityIdentity, EquivalenceIdentity, FlowError, Frontier, InputIdentity, ReadIdentity,
    RecipeIdentity,
};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::ops::RangeInclusive;

/// Nonzero generation fencing one live demand lease.
///
/// A consumer identity may be reused after its prior lease is released. The
/// generation makes a delayed cancellation harmless, and its checked
/// allocator refuses exhaustion instead of allowing an old fence to become
/// current again.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DemandToken(NonZeroU64);

impl DemandToken {
    const FIRST: Self = Self(NonZeroU64::MIN);

    /// Returns the compact wire/debug representation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    fn successor(self) -> Option<Self> {
        self.get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
    }
}

/// Demand/control graph key.
///
/// Every identity that can affect reuse is part of this key. In particular,
/// two recipe executions with the same output shape but different source
/// manifests, validated read sets, authority receipts, or equivalence
/// contracts cannot be accidentally coalesced.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkKey {
    /// Exact immutable recipe version.
    pub recipe: RecipeIdentity,
    /// Exact source/input manifest version.
    pub input: InputIdentity,
    /// Exact validated positive/negative/range read-set version.
    pub read: ReadIdentity,
    /// Exact authority policy and receipt version.
    pub authority: AuthorityIdentity,
    /// Exact output equivalence contract version.
    pub equivalence: EquivalenceIdentity,
}

impl WorkKey {
    /// Creates a fully bound demand key.
    #[must_use]
    pub const fn new(
        recipe: RecipeIdentity,
        input: InputIdentity,
        read: ReadIdentity,
        authority: AuthorityIdentity,
        equivalence: EquivalenceIdentity,
    ) -> Self {
        Self {
            recipe,
            input,
            read,
            authority,
            equivalence,
        }
    }
}

/// Explicit demand row for a derived view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Demand {
    /// Consumer/session identity.
    pub consumer: u64,
    /// Demanded work key.
    pub work: WorkKey,
    /// Optional inclusive row range.
    pub range: Option<RangeInclusive<u64>>,
    /// Requested freshness frontier.
    pub freshness: Frontier,
    /// Scheduler priority.
    pub priority: u8,
}

/// Incrementally maintained demand graph.
#[derive(Clone, Debug)]
pub struct DemandGraph {
    edges: BTreeMap<WorkKey, BTreeSet<WorkKey>>,
    demands: BTreeMap<u64, Demand>,
    demand_tokens: BTreeMap<u64, DemandToken>,
    next_demand_token: Option<DemandToken>,
    dirty: BTreeSet<WorkKey>,
    suppressed: BTreeSet<WorkKey>,
}

impl Default for DemandGraph {
    fn default() -> Self {
        Self {
            edges: BTreeMap::new(),
            demands: BTreeMap::new(),
            demand_tokens: BTreeMap::new(),
            next_demand_token: Some(DemandToken::FIRST),
            dirty: BTreeSet::new(),
            suppressed: BTreeSet::new(),
        }
    }
}

impl DemandGraph {
    /// Adds a dependency edge.
    pub fn depends_on(&mut self, output: WorkKey, input: WorkKey) {
        let _ = self.try_depends_on(output, input);
    }

    /// Registers one ephemeral demand row.
    pub fn add_demand(&mut self, demand: Demand) -> Result<DemandToken, FlowError> {
        self.add_demand_with_token(demand)
    }

    /// Registers one demand and returns its lease-generation token.
    pub(super) fn add_demand_with_token(
        &mut self,
        demand: Demand,
    ) -> Result<DemandToken, FlowError> {
        // Preflight the complete identity transition before changing either
        // the demand row or its fence. The final representable token is valid;
        // only the following admission fails.
        let token = self.next_demand_token.ok_or(FlowError::Overflow)?;
        let successor = token.successor();
        let consumer = demand.consumer;
        let replaced = self.demands.insert(consumer, demand).is_some();
        self.demand_tokens.insert(consumer, token);
        self.next_demand_token = successor;
        if replaced {
            // A consumer ID identifies one live lease.  Replacing that row
            // releases the old root's reachability before the new row is
            // observed by later scheduler calls.
            self.prune_unreachable();
        }
        Ok(token)
    }

    /// Removes a demand row and releases its scheduler interest.
    #[must_use]
    pub fn remove_demand(&mut self, consumer: u64) -> Option<Demand> {
        let removed = self.demands.remove(&consumer);
        if removed.is_some() {
            self.demand_tokens.remove(&consumer);
        }
        removed
    }

    /// Removes a demand and prunes dependency state that no live demand can
    /// reach.  This is the cancellation path used by leased clients; the
    /// lower-level [`Self::remove_demand`] method remains available when a
    /// planner intentionally retains a reusable graph skeleton.
    pub fn release_demand(&mut self, consumer: u64) -> Option<Demand> {
        let removed = self.remove_demand(consumer);
        if removed.is_some() {
            self.prune_unreachable();
        }
        removed
    }

    /// Releases a demand only when the lease token still owns that row.
    ///
    /// Reusing a consumer ID replaces its demand.  The token prevents an
    /// older lease's delayed cancellation from removing the replacement.
    #[must_use]
    pub fn release_demand_token(&mut self, consumer: u64, token: DemandToken) -> bool {
        if self.demand_tokens.get(&consumer) != Some(&token) {
            return false;
        }
        self.release_demand(consumer).is_some()
    }

    pub(super) fn owns_demand_token(&self, consumer: u64, token: DemandToken) -> bool {
        self.demand_tokens.get(&consumer) == Some(&token)
    }

    /// Returns whether a work key has at least one active demand.
    #[must_use]
    pub fn is_demanded(&self, work: WorkKey) -> bool {
        self.demands.values().any(|demand| demand.work == work)
    }

    /// Marks a work key dirty and eligible for propagation.
    pub fn mark_dirty(&mut self, work: WorkKey) {
        self.dirty.insert(work);
        self.suppressed.remove(&work);
    }

    /// Returns whether a work key is currently dirty.
    #[must_use]
    pub fn is_dirty(&self, work: WorkKey) -> bool {
        self.dirty.contains(&work)
    }

    /// Marks a work key as a content-identical no-op.
    pub fn suppress_noop(&mut self, work: WorkKey) {
        self.dirty.remove(&work);
        self.suppressed.insert(work);
    }

    /// Returns whether a work key was suppressed as unchanged.
    #[must_use]
    pub fn is_noop(&self, work: WorkKey) -> bool {
        self.suppressed.contains(&work)
    }

    /// Returns the number of retained dependency edges for a work key.
    #[must_use]
    pub fn dependency_count(&self, key: WorkKey) -> usize {
        self.edges.get(&key).map_or(0, BTreeSet::len)
    }

    /// Returns the number of work keys retained by active demand or its
    /// dependency closure.
    #[must_use]
    pub fn retained_work_count(&self) -> usize {
        let mut reachable = BTreeSet::new();
        let mut frontier = self
            .demands
            .values()
            .map(|demand| demand.work)
            .collect::<Vec<_>>();
        while let Some(key) = frontier.pop() {
            if !reachable.insert(key) {
                continue;
            }
            if let Some(dependencies) = self.edges.get(&key) {
                frontier.extend(dependencies.iter().copied());
            }
        }
        reachable.len()
    }

    /// Returns the live demand roots that keep dependency state reachable.
    ///
    /// Storage and scheduler owners can use this set as their explicit GC
    /// root hand-off instead of treating every historical graph key as live.
    #[must_use]
    pub fn gc_roots(&self) -> BTreeSet<WorkKey> {
        self.demands.values().map(|demand| demand.work).collect()
    }

    /// Drops dependency, dirty, and suppression state unreachable from live
    /// demand roots and returns the number of work keys removed.
    pub fn collect_unreachable(&mut self) -> usize {
        let before = self
            .edges
            .keys()
            .copied()
            .chain(self.edges.values().flat_map(BTreeSet::iter).copied())
            .chain(self.dirty.iter().copied())
            .chain(self.suppressed.iter().copied())
            .collect::<BTreeSet<_>>();
        self.prune_unreachable();
        let after = self
            .edges
            .keys()
            .copied()
            .chain(self.edges.values().flat_map(BTreeSet::iter).copied())
            .chain(self.dirty.iter().copied())
            .chain(self.suppressed.iter().copied())
            .collect::<BTreeSet<_>>();
        before.difference(&after).count()
    }

    /// Returns the next ready key in deterministic order.
    #[must_use]
    pub fn ready(&self, done: &BTreeSet<WorkKey>) -> Option<WorkKey> {
        let mut keys = BTreeSet::new();
        keys.extend(self.edges.keys().copied());
        keys.extend(self.edges.values().flat_map(BTreeSet::iter).copied());
        keys.extend(self.demands.values().map(|demand| demand.work));
        keys.extend(self.dirty.iter().copied());
        keys.into_iter()
            .filter(|key| !done.contains(key))
            .filter(|key| !self.suppressed.contains(key))
            .find(|key| {
                self.edges.get(key).is_none_or(|deps| {
                    deps.iter()
                        .all(|dependency| done.contains(dependency) || self.is_noop(*dependency))
                })
            })
    }

    /// Adds an edge only if it does not introduce a dependency cycle.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::DependencyCycle`] and removes the attempted edge
    /// when the new dependency would create a cycle.
    pub fn try_depends_on(&mut self, output: WorkKey, input: WorkKey) -> Result<(), FlowError> {
        let already_present = self
            .edges
            .get(&output)
            .is_some_and(|dependencies| dependencies.contains(&input));
        self.edges.entry(output).or_default().insert(input);
        if self.has_cycle() {
            if !already_present && let Some(deps) = self.edges.get_mut(&output) {
                deps.remove(&input);
                if deps.is_empty() {
                    self.edges.remove(&output);
                }
            }
            return Err(FlowError::DependencyCycle);
        }
        Ok(())
    }

    fn has_cycle(&self) -> bool {
        fn visit(
            node: WorkKey,
            edges: &BTreeMap<WorkKey, BTreeSet<WorkKey>>,
            active: &mut BTreeSet<WorkKey>,
            done: &mut BTreeSet<WorkKey>,
        ) -> bool {
            if active.contains(&node) {
                return true;
            }
            if done.contains(&node) {
                return false;
            }
            active.insert(node);
            if edges
                .get(&node)
                .is_some_and(|deps| deps.iter().any(|dep| visit(*dep, edges, active, done)))
            {
                return true;
            }
            active.remove(&node);
            done.insert(node);
            false
        }
        let mut active = BTreeSet::new();
        let mut done = BTreeSet::new();
        self.edges
            .keys()
            .any(|key| visit(*key, &self.edges, &mut active, &mut done))
    }

    fn prune_unreachable(&mut self) {
        let mut reachable = BTreeSet::new();
        let mut frontier = self
            .demands
            .values()
            .map(|demand| demand.work)
            .collect::<Vec<_>>();
        while let Some(key) = frontier.pop() {
            if !reachable.insert(key) {
                continue;
            }
            if let Some(dependencies) = self.edges.get(&key) {
                frontier.extend(dependencies.iter().copied());
            }
        }
        self.edges.retain(|output, dependencies| {
            if !reachable.contains(output) {
                return false;
            }
            dependencies.retain(|dependency| reachable.contains(dependency));
            !dependencies.is_empty()
        });
        self.dirty.retain(|key| reachable.contains(key));
        self.suppressed.retain(|key| reachable.contains(key));
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;
    use crate::Time;
    use backend_version::ObjectVersion;

    fn work(seed: u8) -> WorkKey {
        WorkKey::new(
            ObjectVersion::from_value(&[seed; 32]),
            ObjectVersion::from_value(&[seed; 32]),
            ObjectVersion::from_value(&[seed; 32]),
            ObjectVersion::from_value(&[seed; 32]),
            ObjectVersion::from_value(&[seed; 32]),
        )
    }

    fn demand(consumer: u64, seed: u8) -> Demand {
        Demand {
            consumer,
            work: work(seed),
            range: None,
            freshness: Frontier::new(Time::default()),
            priority: 1,
        }
    }

    #[test]
    fn final_token_is_admitted_once_and_exhaustion_is_atomic() {
        let mut graph = DemandGraph::default();
        graph.next_demand_token = NonZeroU64::new(u64::MAX).map(DemandToken);

        let final_token = graph.add_demand(demand(1, 1)).expect("final token");
        assert_eq!(final_token.get(), u64::MAX);
        assert_eq!(graph.demands.len(), 1);

        assert_eq!(graph.add_demand(demand(2, 2)), Err(FlowError::Overflow));
        assert_eq!(graph.demands.len(), 1);
        assert!(graph.is_demanded(work(1)));
        assert!(!graph.is_demanded(work(2)));
    }
}
