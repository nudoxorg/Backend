//! Defines unit borrowed behavior for `heart-hydration`, whose purpose is to plan and verify borrowed object hydration without weakening generation authority.
//! This module owns the unit borrowed invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use alloc::{vec, vec::Vec};

use heart_identity::{GenerationId, ObjectDomain};

use super::*;

#[test]
fn borrowed_complete_plan_preserves_order_routes_coverage_and_dependency_identity()
-> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let mut canonical_root = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut canonical_root)?;
    let validated_root = ValidatedRoot::<ObjectDomain>::try_from(canonical_root.as_slice())?;
    let borrowed_view = BorrowedGenerationView::new(&validated_root, &locality)?;
    let owned_view = GenerationView::new(&root, &locality)?;

    let mut borrowed_scratch = plan_scratch(&root)?;
    let (borrowed_dep_set, borrowed_coverage) =
        assert_complete_borrowed_plan(&root, &borrowed_view, &mut borrowed_scratch)?;

    let mut owned_closure = closure_scratch(&root)?;
    let mut owned_scratch = plan_scratch(&root)?;
    let owned = plan(
        Need::new(owned_view.id, Projection::CompleteGeneration).bind(&owned_view)?,
        &mut owned_closure,
        &mut owned_scratch,
        |descriptor| descriptor.content == object(1).content,
    )?;
    assert_eq!(borrowed_dep_set, owned.dep_set);
    assert_eq!(borrowed_coverage, owned.coverage);
    assert_eq!(borrowed_scratch.high_water_absent, 2.into());
    assert_eq!(borrowed_scratch.retained_absence_bytes, 12_usize.into());
    Ok(())
}

#[test]
fn noncontiguous_absence_advances_sparse_and_selected_cursors_exactly() -> Result<(), ScenarioError>
{
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let mut canonical_root = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut canonical_root)?;
    let validated_root = ValidatedRoot::<ObjectDomain>::try_from(canonical_root.as_slice())?;
    let view = BorrowedGenerationView::new(&validated_root, &locality)?;
    let mut closure = closure_scratch(&root)?;
    let mut scratch = plan_scratch(&root)?;
    let plan = plan_borrowed(
        Need::new(view.id, Projection::CompleteGeneration).bind_borrowed(&view)?,
        &mut closure,
        &mut scratch,
        |descriptor| descriptor == object(2),
    )?;

    assert_eq!(
        plan.required().collect::<Vec<_>>(),
        Vec::from([object(1), object(2), object(3)])
    );
    assert_eq!(plan.present().collect::<Vec<_>>(), Vec::from([object(2)]));
    assert_eq!(plan.promised().count(), 0);
    assert_eq!(
        plan.missing().collect::<Vec<_>>(),
        Vec::from([object(1), object(3)])
    );
    assert_eq!(
        plan.fetches().collect::<Vec<_>>(),
        Vec::from([
            Fetch {
                object: object(1),
                route: FetchRoute::Unrouted,
            },
            Fetch {
                object: object(3),
                route: FetchRoute::Unrouted,
            },
        ])
    );
    Ok(())
}

#[test]
fn borrowed_range_and_verification_keep_partial_and_missing_causes_exact()
-> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let mut canonical_root = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut canonical_root)?;
    let validated_root = ValidatedRoot::<ObjectDomain>::try_from(canonical_root.as_slice())?;
    let view = BorrowedGenerationView::new(&validated_root, &locality)?;
    let range = heart_root::EntryRange::new(key(3), key(3))?;
    let mut closure = closure_scratch(&root)?;
    let mut scratch = plan_scratch(&root)?;
    let ranged = plan_borrowed(
        Need::new(view.id, Projection::Range(range)).bind_borrowed(&view)?,
        &mut closure,
        &mut scratch,
        |_| false,
    )?;
    assert_eq!(
        ranged
            .required()
            .map(|item| item.content)
            .collect::<Vec<_>>(),
        Vec::from([object(1).content, object(3).content])
    );
    assert_eq!(
        ranged.coverage,
        PlanCoverage {
            required: 2.into(),
            present: 0.into(),
            promised: 0.into(),
            missing: 2.into(),
        }
    );
    let full_store = memory_store(&[1, 2, 3])?;
    let partial_verification = ranged.stage().verify_store(&full_store);
    require_partial_verification(&partial_verification, view.id)?;

    let complete = plan_borrowed(
        Need::new(view.id, Projection::CompleteGeneration).bind_borrowed(&view)?,
        &mut closure,
        &mut scratch,
        |_| false,
    )?;
    let partial_store = memory_store(&[1])?;
    let missing_verification = complete.stage().verify_store(&partial_store);
    require_missing_verification(&missing_verification, view.id)?;
    Ok(())
}

#[test]
fn borrowed_binding_and_scratch_failures_preserve_exact_operands() -> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let mut canonical_root = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut canonical_root)?;
    let validated_root = ValidatedRoot::<ObjectDomain>::try_from(canonical_root.as_slice())?;
    let view = BorrowedGenerationView::new(&validated_root, &locality)?;

    let mismatch = Need::new(
        GenerationId::from_digest([9; 32]),
        Projection::CompleteGeneration,
    )
    .bind_borrowed(&view);
    require_borrowed_demand_mismatch(&mismatch, view.id)?;

    let mut too_small_closure =
        ClosureScratch::new(0).map_err(ScenarioError::ClosureReservation)?;
    let mut full_scratch = plan_scratch(&root)?;
    let closure_failure = plan_borrowed(
        Need::new(view.id, Projection::CompleteGeneration).bind_borrowed(&view)?,
        &mut too_small_closure,
        &mut full_scratch,
        |_| false,
    )
    .map(|_| ());
    require_borrowed_closure_scratch(&closure_failure, root.len())?;

    let mut full_closure = closure_scratch(&root)?;
    let mut too_small_plan = PlanScratch::new(1.into()).map_err(ScenarioError::PlanReservation)?;
    let plan_failure = plan_borrowed(
        Need::new(view.id, Projection::CompleteGeneration).bind_borrowed(&view)?,
        &mut full_closure,
        &mut too_small_plan,
        |_| false,
    )
    .map(|_| ());
    require_borrowed_plan_scratch(&plan_failure)?;
    assert_eq!(too_small_plan.high_water_absent, AbsentCount::from(0));
    Ok(())
}

fn assert_complete_borrowed_plan(
    root: &GenerationRoot<ObjectDomain>,
    view: &BorrowedGenerationView<'_, '_, ObjectDomain>,
    scratch: &mut PlanScratch,
) -> Result<(heart_object::DepSetId, PlanCoverage), ScenarioError> {
    let mut closure = closure_scratch(root)?;
    let borrowed = plan_borrowed(
        Need::new(view.id, Projection::CompleteGeneration).bind_borrowed(view)?,
        &mut closure,
        scratch,
        |descriptor| descriptor.content == object(1).content,
    )?;
    assert_eq!(
        borrowed
            .required()
            .map(|item| item.content)
            .collect::<Vec<_>>(),
        Vec::from([object(1).content, object(2).content, object(3).content])
    );
    assert_eq!(
        borrowed
            .present()
            .map(|item| item.content)
            .collect::<Vec<_>>(),
        Vec::from([object(1).content])
    );
    assert_eq!(
        borrowed
            .promised()
            .map(|promise| promise.object.content)
            .collect::<Vec<_>>(),
        Vec::from([object(2).content])
    );
    assert_eq!(
        borrowed
            .missing()
            .map(|item| item.content)
            .collect::<Vec<_>>(),
        Vec::from([object(3).content])
    );
    assert_eq!(
        borrowed.coverage,
        PlanCoverage {
            required: 3.into(),
            present: 1.into(),
            promised: 1.into(),
            missing: 1.into(),
        }
    );
    assert_eq!(borrowed.sparse_state_bytes(), 8_usize.into());
    Ok((borrowed.dep_set, borrowed.coverage))
}

fn require_partial_verification<PayloadOwner: AsRef<[u8]>>(
    result: &Result<
        crate::VerifiedGeneration<'_, ObjectDomain, PayloadOwner>,
        VerificationError<ObjectDomain>,
    >,
    expected_root: GenerationId,
) -> Result<(), ScenarioError> {
    match result {
        Err(VerificationError::PartialProjection { pinned_root }) => {
            assert_eq!(*pinned_root, expected_root);
            Ok(())
        }
        Err(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::PartialVerification,
            expected: ScenarioExpectation::PartialProjection,
            observed: ScenarioObservation::DifferentVerificationError,
        }),
        Ok(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::PartialVerification,
            expected: ScenarioExpectation::PartialProjection,
            observed: ScenarioObservation::VerifiedGeneration,
        }),
    }
}

fn require_missing_verification<PayloadOwner: AsRef<[u8]>>(
    result: &Result<
        crate::VerifiedGeneration<'_, ObjectDomain, PayloadOwner>,
        VerificationError<ObjectDomain>,
    >,
    expected_root: GenerationId,
) -> Result<(), ScenarioError> {
    match result {
        Err(VerificationError::MissingObject { report }) => {
            assert_eq!(report.pinned_root, expected_root);
            assert_eq!(report.object, object(2));
            Ok(())
        }
        Err(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::MissingVerification,
            expected: ScenarioExpectation::MissingObject,
            observed: ScenarioObservation::DifferentVerificationError,
        }),
        Ok(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::MissingVerification,
            expected: ScenarioExpectation::MissingObject,
            observed: ScenarioObservation::VerifiedGeneration,
        }),
    }
}

fn require_borrowed_demand_mismatch(
    result: &Result<crate::BoundBorrowedNeed<'_, '_, '_, ObjectDomain>, PlanError>,
    actual: GenerationId,
) -> Result<(), ScenarioError> {
    match result {
        Err(PlanError::Demand(DemandBindError::GenerationMismatch {
            requested,
            actual: seen,
        })) => {
            assert_eq!(*requested, GenerationId::from_digest([9; 32]));
            assert_eq!(*seen, actual);
            Ok(())
        }
        Ok(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::DemandBinding,
            expected: ScenarioExpectation::GenerationMismatch,
            observed: ScenarioObservation::BoundDemand,
        }),
        Err(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::DemandBinding,
            expected: ScenarioExpectation::GenerationMismatch,
            observed: ScenarioObservation::DifferentPlanError,
        }),
    }
}

fn require_borrowed_closure_scratch(
    result: &Result<(), PlanError>,
    expected_required: usize,
) -> Result<(), ScenarioError> {
    match result {
        Err(PlanError::Closure(ClosureError::ScratchTooSmall {
            required,
            available,
        })) => {
            assert_eq!(*required, expected_required);
            assert_eq!(*available, 0);
            Ok(())
        }
        Err(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::PlanCapacity,
            expected: ScenarioExpectation::ScratchTooSmall,
            observed: ScenarioObservation::DifferentPlanError,
        }),
        Ok(()) => Err(ScenarioError::Transition {
            step: ScenarioStep::PlanCapacity,
            expected: ScenarioExpectation::ScratchTooSmall,
            observed: ScenarioObservation::HydrationPlan,
        }),
    }
}

fn require_borrowed_plan_scratch(result: &Result<(), PlanError>) -> Result<(), ScenarioError> {
    match result {
        Err(PlanError::ScratchTooSmall {
            required,
            available,
        }) => {
            assert_eq!(*required, 3.into());
            assert_eq!(*available, 1.into());
            Ok(())
        }
        Err(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::PlanCapacity,
            expected: ScenarioExpectation::ScratchTooSmall,
            observed: ScenarioObservation::DifferentPlanError,
        }),
        Ok(()) => Err(ScenarioError::Transition {
            step: ScenarioStep::PlanCapacity,
            expected: ScenarioExpectation::ScratchTooSmall,
            observed: ScenarioObservation::HydrationPlan,
        }),
    }
}
