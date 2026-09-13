//! Exercises the `heart-hydration` tests ui relabel-hydration-plan contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use heart_hydration::{BorrowedHydrationPlanView, HydrationPlanView, Projection};
use backend_version::ObjectDomain;

fn relabel_owned(plan: &mut HydrationPlanView<'_, '_, ObjectDomain>) {
    plan.projection = Projection::CompleteGeneration;
}

fn relabel_borrowed(
    plan: &mut BorrowedHydrationPlanView<'_, '_, '_, '_, ObjectDomain>,
) {
    plan.projection = Projection::CompleteGeneration;
}

fn main() {}
