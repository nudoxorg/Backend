//! Factorized materializations, higher-order deltas, and planner costs.

use super::{
    CoverageWitness, Delta, FlowError, Frontier, InputIdentity, RecipeIdentity, TraceSpine,
};
use crate::batch::{append_len_prefixed, consolidate_rows};
use backend_version::{ObjectVersion, Schema};
use std::sync::Arc;

fn canonical_len(value: usize) -> u64 {
    u64::try_from(value).map_or(u64::MAX, |value| value)
}

/// Schema for a retained factor materialization identity.
pub struct FactorSchema;
impl Schema for FactorSchema {
    const DOMAIN: u8 = 0x66;
    const TYPE: u16 = 4;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Schema-marked factor root. A factor root is a materialization identity,
/// distinct from the state root of its expanded relation.
pub type FactorRoot = ObjectVersion<FactorSchema>;

/// An immutable factorized prefix node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FactorNode<V> {
    /// A flat leaf of weighted rows.
    Leaf {
        /// Consolidated immutable rows.
        rows: Arc<[Delta<V>]>,
    },
    /// A shared prefix with child factors.
    Prefix {
        /// Ordered prefix coordinates.
        prefix: Arc<[u64]>,
        /// Child factors sharing that prefix.
        children: Arc<[FactorNode<V>]>,
    },
}

impl<V: Clone + Ord> FactorNode<V> {
    /// Freezes and consolidates a factor leaf.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] for a zero row weight or
    /// [`FlowError::Overflow`] when consolidation exceeds the signed weight
    /// range.
    pub fn leaf(rows: impl IntoIterator<Item = Delta<V>>) -> Result<Self, FlowError> {
        let rows = consolidate_rows(rows.into_iter().collect())?;
        Ok(Self::Leaf {
            rows: Arc::from(rows.into_boxed_slice()),
        })
    }

    /// Freezes a shared-prefix factor node and its child nodes.
    #[must_use]
    pub fn prefix(
        prefix: impl IntoIterator<Item = u64>,
        children: impl IntoIterator<Item = FactorNode<V>>,
    ) -> Self {
        Self::Prefix {
            prefix: Arc::from(prefix.into_iter().collect::<Vec<_>>().into_boxed_slice()),
            children: Arc::from(children.into_iter().collect::<Vec<_>>().into_boxed_slice()),
        }
    }

    /// Returns the shared prefix coordinates, if this is a prefix node.
    #[must_use]
    pub fn shared_prefix(&self) -> Option<&[u64]> {
        match self {
            Self::Leaf { .. } => None,
            Self::Prefix { prefix, .. } => Some(prefix),
        }
    }

    /// Expands this factor to its declared flat rows.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] or [`FlowError::Overflow`] if the
    /// expanded rows cannot be consolidated as signed weights.
    pub fn expand(&self) -> Result<Vec<Delta<V>>, FlowError> {
        self.expand_checked()
    }

    /// Expands and consolidates this factor under weighted bag semantics.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] or [`FlowError::Overflow`] if the
    /// expanded rows cannot be consolidated as signed weights.
    pub fn expand_checked(&self) -> Result<Vec<Delta<V>>, FlowError> {
        let rows = match self {
            Self::Leaf { rows } => rows.iter().cloned().collect(),
            Self::Prefix { children, .. } => {
                let mut rows = Vec::new();
                for child in children.iter() {
                    rows.extend(child.expand_checked()?);
                }
                rows
            }
        };
        consolidate_rows(rows)
    }

    /// Returns the number of immediate children/rows retained by this node.
    #[must_use]
    pub fn child_count(&self) -> usize {
        match self {
            Self::Leaf { rows } => rows.len(),
            Self::Prefix { children, .. } => children.len(),
        }
    }

    /// Returns the number of flat rows after expansion.
    #[must_use]
    pub fn expanded_len(&self) -> usize {
        match self {
            Self::Leaf { rows } => rows.len(),
            Self::Prefix { children, .. } => children.iter().map(Self::expanded_len).sum(),
        }
    }
}

/// Builds a factor root from all logical identity inputs and visibility data.
#[must_use]
pub fn factor_root_from_parts(
    key_order: u8,
    support: i64,
    recipe: RecipeIdentity,
    input: InputIdentity,
    frontier: Frontier,
    coverage: CoverageWitness,
    children: &[FactorRoot],
) -> FactorRoot {
    factor_root_from_trace(
        key_order,
        support,
        recipe,
        input,
        &TraceSpine::from_frontier(frontier),
        coverage,
        children,
    )
}

/// Builds a factor root while binding both retained trace frontiers.
#[must_use]
pub fn factor_root_from_trace(
    key_order: u8,
    support: i64,
    recipe: RecipeIdentity,
    input: InputIdentity,
    trace: &TraceSpine,
    coverage: CoverageWitness,
    children: &[FactorRoot],
) -> FactorRoot {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"flow.factor.v2\0");
    bytes.push(key_order);
    bytes.extend_from_slice(&support.to_be_bytes());
    append_len_prefixed(&mut bytes, recipe.as_bytes());
    append_len_prefixed(&mut bytes, input.as_bytes());
    let upper = trace.upper();
    bytes.extend_from_slice(&canonical_len(upper.elements().len()).to_be_bytes());
    for time in upper.elements() {
        bytes.extend_from_slice(&time.epoch.0.to_be_bytes());
        bytes.extend_from_slice(&time.iteration.to_be_bytes());
    }
    let since = trace.since_frontier();
    bytes.extend_from_slice(&canonical_len(since.elements().len()).to_be_bytes());
    for time in since.elements() {
        bytes.extend_from_slice(&time.epoch.0.to_be_bytes());
        bytes.extend_from_slice(&time.iteration.to_be_bytes());
    }
    bytes.push(match coverage.state() {
        backend_version::Coverage::Complete => 0,
        backend_version::Coverage::Partial => 1,
        backend_version::Coverage::Unavailable => 2,
        backend_version::Coverage::Unsupported => 3,
        backend_version::Coverage::Closed => 4,
    });
    append_len_prefixed(&mut bytes, coverage.scope_root().as_bytes());
    bytes.extend_from_slice(&canonical_len(children.len()).to_be_bytes());
    for child in children {
        append_len_prefixed(&mut bytes, child.as_bytes());
    }
    ObjectVersion::from_value(&bytes)
}

/// Exact delta for a factor representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactorDelta<V> {
    /// Exact base factor materialization.
    pub base: FactorRoot,
    /// Exact target factor materialization.
    pub target: FactorRoot,
    /// Exact immutable input transition identity that this factor advances.
    pub input: InputIdentity,
    /// Frontier closed by this transition.
    pub frontier: Frontier,
    /// Authority coverage admitted for this transition.
    pub coverage: CoverageWitness,
    /// Consolidated factor changes.
    pub changes: Vec<Delta<V>>,
}

impl<V: Clone + Ord> FactorDelta<V> {
    /// Admits a complete factor delta and removes zero changes.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::IncompleteFrontier`] when coverage is incomplete
    /// or a change lies outside the admitted frontier. Returns
    /// [`FlowError::ZeroDiff`] or [`FlowError::Overflow`] when changes cannot
    /// be consolidated as signed weights.
    pub fn admit(
        base: FactorRoot,
        target: FactorRoot,
        input: InputIdentity,
        frontier: Frontier,
        changes: Vec<Delta<V>>,
        coverage: CoverageWitness,
    ) -> Result<Self, FlowError> {
        if !coverage.state().is_complete() && !coverage.state().is_closed() {
            return Err(FlowError::IncompleteFrontier);
        }
        if changes.iter().any(|change| !frontier.covers(change.time)) {
            return Err(FlowError::IncompleteFrontier);
        }
        Ok(Self {
            base,
            target,
            input,
            frontier,
            coverage,
            changes: consolidate_rows(changes)?,
        })
    }

    /// Returns the exact inverse factor transition.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when a signed weight cannot be
    /// negated or the inverse changes cannot be consolidated.
    pub fn inverse(&self) -> Result<Self, FlowError> {
        let changes = self
            .changes
            .iter()
            .map(|row| {
                Ok(Delta {
                    key: row.key,
                    value: row.value.clone(),
                    time: row.time,
                    diff: row.diff.checked_neg()?,
                })
            })
            .collect::<Result<Vec<_>, FlowError>>()?;
        Ok(Self {
            base: self.target,
            target: self.base,
            input: self.input,
            frontier: self.frontier.clone(),
            coverage: self.coverage,
            changes: consolidate_rows(changes)?,
        })
    }
}

/// A selected higher-order delta view. `cross_terms` records derivatives that
/// depend on simultaneous changes of multiple inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HigherOrderDeltaView<V> {
    /// Base factor materialization.
    pub base: FactorRoot,
    /// Target factor materialization.
    pub target: FactorRoot,
    /// Derivative order retained by the plan.
    pub order: u8,
    /// First-order terms.
    pub terms: Vec<FactorDelta<V>>,
    /// Higher-order simultaneous terms.
    pub cross_terms: Vec<FactorDelta<V>>,
}

impl<V: Clone + Ord> HigherOrderDeltaView<V> {
    /// Creates a higher-order view and validates that it has a usable order.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidFactorOrder`] when `order` is zero.
    pub fn new(
        base: FactorRoot,
        target: FactorRoot,
        order: u8,
        terms: Vec<FactorDelta<V>>,
        cross_terms: Vec<FactorDelta<V>>,
    ) -> Result<Self, FlowError> {
        if order == 0 {
            return Err(FlowError::InvalidFactorOrder);
        }
        Ok(Self {
            base,
            target,
            order,
            terms,
            cross_terms,
        })
    }

    /// Expands all derivative terms under weighted bag semantics.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] or [`FlowError::Overflow`] when the
    /// derivative terms cannot be consolidated as signed weights.
    pub fn expand(&self) -> Result<Vec<Delta<V>>, FlowError> {
        let mut rows = Vec::new();
        for term in self.terms.iter().chain(self.cross_terms.iter()) {
            rows.extend(term.changes.iter().cloned());
        }
        consolidate_rows(rows)
    }

    /// Checks the additive expansion law against an independently supplied
    /// flat transition.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] or [`FlowError::Overflow`] when either
    /// side cannot be consolidated as signed weights.
    pub fn satisfies_expansion(&self, flat: &[Delta<V>]) -> Result<bool, FlowError> {
        Ok(self.expand()? == consolidate_rows(flat.to_vec())?)
    }
}

/// Flat or factorized retained-computation plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactorPlan {
    /// Materialize ordinary flat rows.
    Flat,
    /// Retain grouped prefixes/factors.
    Factorized,
}

/// Inputs used to compare flat and factorized work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FactorCost {
    /// Rows in the incoming delta.
    pub update_rows: u64,
    /// Expected matching fan-out per changed row.
    pub fanout: u64,
    /// Bytes retained by the reusable state.
    pub retained_bytes: u64,
    /// Bytes needed by the demanded flat output.
    pub demanded_bytes: u64,
    /// Estimated derivative order.
    pub derivative_order: u8,
}

impl FactorCost {
    /// Estimates flat work using checked saturating arithmetic.
    #[must_use]
    pub fn flat_work(self) -> u64 {
        self.update_rows
            .saturating_mul(self.fanout.max(1))
            .saturating_add(self.demanded_bytes / 64)
    }

    /// Estimates factor maintenance work.
    #[must_use]
    pub fn factor_work(self) -> u64 {
        self.update_rows
            .saturating_mul(u64::from(self.derivative_order.max(1)))
            .saturating_add(self.retained_bytes / 256)
    }
}

/// Chooses a factor plan from actual update/fan-out/retention estimates.
#[must_use]
pub fn choose_factor_plan_cost(cost: FactorCost) -> FactorPlan {
    if cost.demanded_bytes > 0 && cost.demanded_bytes <= cost.update_rows.saturating_mul(64) {
        return FactorPlan::Flat;
    }
    if cost.factor_work() < cost.flat_work() && cost.retained_bytes > 0 {
        FactorPlan::Factorized
    } else {
        FactorPlan::Flat
    }
}

/// Chooses a plan with a conservative fan-out estimate of one.
#[must_use]
pub fn choose_factor_plan(update_rows: u64, retained_bytes: u64) -> FactorPlan {
    choose_factor_plan_cost(FactorCost {
        update_rows,
        retained_bytes,
        ..FactorCost::default()
    })
}
