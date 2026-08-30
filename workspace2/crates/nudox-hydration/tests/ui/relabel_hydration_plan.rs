use nudox_hydration::{BorrowedHydrationPlanView, HydrationPlanView, Projection};
use nudox_id::ObjectDomain;

fn relabel_owned(plan: &mut HydrationPlanView<'_, '_, ObjectDomain>) {
    plan.projection = Projection::CompleteGeneration;
}

fn relabel_borrowed(
    plan: &mut BorrowedHydrationPlanView<'_, '_, '_, '_, ObjectDomain>,
) {
    plan.projection = Projection::CompleteGeneration;
}

fn main() {}
