use backend_version::{Relation, StateRoot};

/// Smallest recomputation scope accepted by the placement planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RebuildScope {
    /// Recompute only the changed item or key range.
    Item,
    /// Recompute one retained partition or dependency-closed shard.
    Partition,
    /// Recompute the complete recipe product.
    Product,
}

/// Version-bound choice between reuse, delta maintenance, and rebuilding.
pub enum DeltaPlan<R: Relation> {
    /// The input root did not change, so the prior output remains valid.
    NoOp {
        /// Root that already represents the requested result.
        result: StateRoot<R>,
    },
    /// The source changed outside the selected dependency scope.
    Reuse {
        /// Previously materialized root retained for reuse.
        prior: StateRoot<R>,
    },
    /// Apply the changed source scope incrementally.
    Update {
        /// Root at which the retained arrangement was valid.
        base: StateRoot<R>,
        /// Root to which the arrangement advances.
        target: StateRoot<R>,
    },
    /// Rebuild one bounded dependency scope.
    ScopedRebuild {
        /// Root at which the rebuild is based.
        base: StateRoot<R>,
        /// Target source root.
        target: StateRoot<R>,
        /// Scope selected by the change cardinality.
        scope: RebuildScope,
    },
    /// Recompute the complete product.
    FullRebuild {
        /// Root at which the old product was based.
        base: StateRoot<R>,
        /// Target source root.
        target: StateRoot<R>,
    },
}

impl<R: Relation> Copy for DeltaPlan<R> {}

impl<R: Relation> Clone for DeltaPlan<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: Relation> PartialEq for DeltaPlan<R> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::NoOp { result: a }, Self::NoOp { result: b })
            | (Self::Reuse { prior: a }, Self::Reuse { prior: b }) => a == b,
            (
                Self::Update {
                    base: a_base,
                    target: a_target,
                },
                Self::Update {
                    base: b_base,
                    target: b_target,
                },
            )
            | (
                Self::FullRebuild {
                    base: a_base,
                    target: a_target,
                },
                Self::FullRebuild {
                    base: b_base,
                    target: b_target,
                },
            ) => a_base == b_base && a_target == b_target,
            (
                Self::ScopedRebuild {
                    base: a_base,
                    target: a_target,
                    scope: a_scope,
                },
                Self::ScopedRebuild {
                    base: b_base,
                    target: b_target,
                    scope: b_scope,
                },
            ) => a_base == b_base && a_target == b_target && a_scope == b_scope,
            _ => false,
        }
    }
}

impl<R: Relation> Eq for DeltaPlan<R> {}

impl<R: Relation> std::fmt::Debug for DeltaPlan<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOp { result } => f.debug_struct("NoOp").field("result", result).finish(),
            Self::Reuse { prior } => f.debug_struct("Reuse").field("prior", prior).finish(),
            Self::Update { base, target } => f
                .debug_struct("Update")
                .field("base", base)
                .field("target", target)
                .finish(),
            Self::ScopedRebuild {
                base,
                target,
                scope,
            } => f
                .debug_struct("ScopedRebuild")
                .field("base", base)
                .field("target", target)
                .field("scope", scope)
                .finish(),
            Self::FullRebuild { base, target } => f
                .debug_struct("FullRebuild")
                .field("base", base)
                .field("target", target)
                .finish(),
        }
    }
}

impl<R: Relation> DeltaPlan<R> {
    /// Returns the selected scope, if this plan performs work.
    #[must_use]
    pub const fn scope(self) -> Option<RebuildScope> {
        match self {
            Self::NoOp { .. } | Self::Reuse { .. } => None,
            Self::Update { .. } => Some(RebuildScope::Item),
            Self::ScopedRebuild { scope, .. } => Some(scope),
            Self::FullRebuild { .. } => Some(RebuildScope::Product),
        }
    }

    /// Returns whether the plan can reuse a prior output without execution.
    #[must_use]
    pub const fn is_reuse(self) -> bool {
        matches!(self, Self::NoOp { .. } | Self::Reuse { .. })
    }
}

/// Cost facts used when choosing incremental maintenance versus rebuilding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefreshCost {
    /// Estimated bytes read from the delta/support arrangement.
    pub delta_bytes: u64,
    /// Estimated bytes touched by a scoped rebuild.
    pub rebuild_bytes: u64,
    /// Estimated delta fan-out and support maintenance cost.
    pub delta_work: u64,
    /// Estimated rebuild work.
    pub rebuild_work: u64,
}

impl RefreshCost {
    fn delta_total(self) -> Option<u64> {
        self.delta_bytes.checked_add(self.delta_work)
    }

    fn rebuild_total(self) -> Option<u64> {
        self.rebuild_bytes.checked_add(self.rebuild_work)
    }
}

/// Explicit result of the cost comparison used by a planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshChoice {
    /// Retain and advance the existing arrangement.
    Delta,
    /// Rebuild the selected scope.
    Rebuild(RebuildScope),
}

/// Chooses delta maintenance only when its complete charged cost is lower.
/// Ties prefer rebuild because it avoids retaining unbounded support debt.
#[must_use]
pub fn choose_refresh(cost: RefreshCost, scope: RebuildScope) -> RefreshChoice {
    match (cost.delta_total(), cost.rebuild_total()) {
        (Some(delta), Some(rebuild)) if delta < rebuild => RefreshChoice::Delta,
        _ => RefreshChoice::Rebuild(scope),
    }
}

/// Bounded deterministic refresh planner.
pub struct DeltaPlanner {
    scope_limit: usize,
}

impl std::fmt::Debug for DeltaPlanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeltaPlanner")
            .field("scope_limit", &self.scope_limit)
            .finish()
    }
}

impl DeltaPlanner {
    /// Creates a planner whose first rebuild threshold is `scope_limit` items.
    #[must_use]
    pub const fn new(scope_limit: usize) -> Self {
        Self { scope_limit }
    }

    /// Selects a version-bound refresh strategy.
    #[must_use]
    pub fn plan<R: Relation>(
        &self,
        prior: &StateRoot<R>,
        current: &StateRoot<R>,
        changed_items: usize,
    ) -> DeltaPlan<R> {
        if prior == current {
            return DeltaPlan::NoOp { result: *prior };
        }
        if changed_items == 0 {
            // A caller supplied cardinality is not a semantic disjointness
            // proof.  Distinct roots with an empty or truncated change list
            // must fail closed to a rebuild; cross-root reuse belongs behind a
            // checked dependency proof API.
            return DeltaPlan::FullRebuild {
                base: *prior,
                target: *current,
            };
        }
        if changed_items <= self.scope_limit {
            return DeltaPlan::Update {
                base: *prior,
                target: *current,
            };
        }
        let partition_limit = self.scope_limit.checked_mul(4);
        if partition_limit.is_some_and(|limit| changed_items <= limit) {
            return DeltaPlan::ScopedRebuild {
                base: *prior,
                target: *current,
                scope: RebuildScope::Partition,
            };
        }
        DeltaPlan::FullRebuild {
            base: *prior,
            target: *current,
        }
    }

    /// Plans a flow arrangement's already prepared delta using the same
    /// bounded delta-versus-rebuild policy as any other relation root.
    ///
    /// Flow owns the weighted rows and closed frontier. Execution only
    /// consumes its immutable base/target evidence and the number of emitted
    /// changes; it does not create a second flow identity or mutate the trace.
    #[must_use]
    pub fn plan_flow_output<
        V: Clone + Ord + std::fmt::Debug + Eq + backend_flow::CanonicalValue + 'static,
    >(
        &self,
        prepared: &backend_flow::PreparedOutput<V>,
        changed_items: usize,
    ) -> DeltaPlan<backend_flow::ArrangementRelation<V>> {
        self.plan(
            &prepared.base,
            &prepared.target,
            changed_items.max(prepared.deltas().len()),
        )
    }
}
