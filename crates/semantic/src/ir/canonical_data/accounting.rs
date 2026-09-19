//! Measured operation counters and budget checks.

use core::cell::Cell;

use super::{
    error::CanonicalDataError,
    types::{DataFacts, DataResource, DataResourceBudget},
};

pub(super) struct Counters {
    pub(super) sort_comparisons: Cell<u64>,
    pub(super) hash_evaluations: Cell<u64>,
    pub(super) intern_probes: Cell<u64>,
    pub(super) child_visits: Cell<u64>,
}

impl Counters {
    pub(super) const fn new() -> Self {
        Self {
            sort_comparisons: Cell::new(0),
            hash_evaluations: Cell::new(0),
            intern_probes: Cell::new(0),
            child_visits: Cell::new(0),
        }
    }

    pub(super) fn work(
        &self,
        atom_count: usize,
        product_count: usize,
        constructor_count: usize,
        list_count: usize,
        child_count: usize,
    ) -> Result<u64, CanonicalDataError> {
        let mut work = self.hash_evaluations.get();
        work = work.checked_add(self.sort_comparisons.get()).ok_or(
            CanonicalDataError::ResourceCounterOverflow {
                resource: DataResource::Work,
            },
        )?;
        work = work.checked_add(self.intern_probes.get()).ok_or(
            CanonicalDataError::ResourceCounterOverflow {
                resource: DataResource::Work,
            },
        )?;
        work = work.checked_add(self.child_visits.get()).ok_or(
            CanonicalDataError::ResourceCounterOverflow {
                resource: DataResource::Work,
            },
        )?;
        for count in [
            atom_count,
            product_count,
            constructor_count,
            list_count,
            child_count,
        ] {
            let count = u64::try_from(count).map_err(|source| CanonicalDataError::NativeWork {
                actual: count,
                source,
            })?;
            work = work
                .checked_add(count)
                .ok_or(CanonicalDataError::ResourceCounterOverflow {
                    resource: DataResource::Work,
                })?;
        }
        Ok(work)
    }
}

pub(super) fn increment(counter: &Cell<u64>) {
    let next = counter.get().checked_add(1).map_or(u64::MAX, |next| next);
    counter.set(next);
}

pub(super) fn enforce_budget(
    resource: DataResource,
    observed: u64,
    limit: u64,
) -> Result<(), CanonicalDataError> {
    if observed > limit {
        return Err(CanonicalDataError::BudgetExceeded {
            resource,
            observed,
            limit,
        });
    }
    Ok(())
}

/// Reserve the largest counter footprint that the bounded canonicalizer can
/// touch before it starts writing caller scratch.  The implementation is
/// deliberately conservative: a caller that cannot admit this reservation
/// receives a typed budget error while every scratch/output lane is still
/// untouched.  Once admitted, the exact counters remain in the normal
/// execution path and are included in the returned metrics.
pub(super) fn reserve_budget(
    facts: DataFacts<'_, '_>,
    budget: DataResourceBudget,
) -> Result<(), CanonicalDataError> {
    let atoms = lane_count(facts.atoms.len(), DataResource::Work)?;
    let products = lane_count(facts.products.len(), DataResource::Work)?;
    let constructors = lane_count(facts.constructors.len(), DataResource::Work)?;
    let lists = lane_count(facts.lists.len(), DataResource::Work)?;
    let children = lane_count(facts.children.len(), DataResource::Work)?;

    // The fixed-point loop has at most one strict partition refinement per
    // source product.  A stable pass still evaluates each product twice (the
    // current and next hash), hence the factor of two below.
    let refinement_rounds = products;
    if u64::from(budget.max_refinement_rounds) < refinement_rounds {
        return Err(CanonicalDataError::BudgetAdmission {
            resource: DataResource::RefinementRounds,
            required: refinement_rounds,
            limit: u64::from(budget.max_refinement_rounds),
        });
    }

    let hash_upper = checked_mul(
        checked_mul(products, refinement_rounds, DataResource::HashEvaluations)?,
        2,
        DataResource::HashEvaluations,
    )?;
    if budget.max_hash_evaluations < hash_upper {
        return Err(CanonicalDataError::BudgetAdmission {
            resource: DataResource::HashEvaluations,
            required: hash_upper,
            limit: budget.max_hash_evaluations,
        });
    }

    let atom_sort_upper = checked_mul(atoms, atoms, DataResource::SortComparisons)?;
    let product_sort_round = checked_add(
        checked_mul(products, products, DataResource::SortComparisons)?,
        checked_mul(products, 2, DataResource::SortComparisons)?,
        DataResource::SortComparisons,
    )?;
    let sort_upper = checked_add(
        atom_sort_upper,
        checked_mul(
            product_sort_round,
            refinement_rounds,
            DataResource::SortComparisons,
        )?,
        DataResource::SortComparisons,
    )?;
    if budget.max_sort_comparisons < sort_upper {
        return Err(CanonicalDataError::BudgetAdmission {
            resource: DataResource::SortComparisons,
            required: sort_upper,
            limit: budget.max_sort_comparisons,
        });
    }

    let intern_upper = checked_mul(products, products, DataResource::InternProbes)?;
    if budget.max_intern_probes < intern_upper {
        return Err(CanonicalDataError::BudgetAdmission {
            resource: DataResource::InternProbes,
            required: intern_upper,
            limit: budget.max_intern_probes,
        });
    }

    // Each refinement hash pass visits the pooled child lane once.  Key
    // comparisons can inspect the complete lane, so multiply the conservative
    // comparison/probe bound by the lane length for a physical work reserve.
    //
    // The Work product of the two largest coupled lanes (children × the
    // cubic product-comparison bound) exceeds `u64` once the shared emission
    // geometry reaches 32,768 facts at 64 children each. The reservation is
    // a conservative upper bound, so the multiply saturates instead of
    // failing: a saturated reserve admits only callers whose own budget is
    // at least as conservative, and the real counters still answer to
    // `enforce_budget` at runtime.
    let refinement_child_visits = children.saturating_mul(refinement_rounds).saturating_mul(2);
    let keyed_comparisons = checked_add(
        checked_mul(product_sort_round, refinement_rounds, DataResource::Work)?,
        intern_upper,
        DataResource::Work,
    )?;
    let keyed_child_visits = children.saturating_mul(keyed_comparisons);
    let child_visits = refinement_child_visits.saturating_add(keyed_child_visits);
    let lane_work = checked_add(
        checked_add(
            checked_add(atoms, products, DataResource::Work)?,
            constructors,
            DataResource::Work,
        )?,
        checked_add(lists, children, DataResource::Work)?,
        DataResource::Work,
    )?;
    let work_upper = hash_upper
        .saturating_add(sort_upper)
        .saturating_add(intern_upper)
        .saturating_add(child_visits)
        .saturating_add(lane_work);
    if budget.max_work < work_upper {
        return Err(CanonicalDataError::BudgetAdmission {
            resource: DataResource::Work,
            required: work_upper,
            limit: budget.max_work,
        });
    }
    Ok(())
}

fn lane_count(actual: usize, resource: DataResource) -> Result<u64, CanonicalDataError> {
    u64::try_from(actual).map_err(|_| CanonicalDataError::ResourceCounterOverflow { resource })
}

fn checked_add(left: u64, right: u64, resource: DataResource) -> Result<u64, CanonicalDataError> {
    left.checked_add(right)
        .ok_or(CanonicalDataError::ResourceCounterOverflow { resource })
}

fn checked_mul(left: u64, right: u64, resource: DataResource) -> Result<u64, CanonicalDataError> {
    left.checked_mul(right)
        .ok_or(CanonicalDataError::ResourceCounterOverflow { resource })
}
