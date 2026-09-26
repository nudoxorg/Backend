//! Demand graph identity and scheduler readiness state.

use super::{
    AuthorityIdentity, EquivalenceIdentity, FlowError, Frontier, InputIdentity, ReadIdentity,
    RecipeIdentity,
};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::ops::RangeInclusive;

mod apply;

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
