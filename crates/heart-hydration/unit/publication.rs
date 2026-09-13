//! Defines unit publication behavior for `heart-hydration`, whose purpose is to plan and verify borrowed object hydration without weakening generation authority.
//! This module owns the unit publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use super::*;

#[test]
fn verified_generation_retains_facts_and_one_exact_evidence_reference() {
    let witness_bytes =
        size_of::<GenerationId>() + size_of::<backend_version::object::DepSetId>() + size_of::<&()>();
    assert_eq!(
        size_of::<crate::VerifiedGeneration<'static, ObjectDomain, Box<[u8]>>>(),
        witness_bytes
    );
}

#[test]
fn binding_and_verification_reject_adjacent_invalid_states() -> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    require_demand_mismatch(
        &Need::new(
            GenerationId::from_digest([9; 32]),
            Projection::CompleteGeneration,
        )
        .bind(&view),
        root.id,
    )?;

    let range = heart_root::EntryRange::new(key(2), key(2))?;
    let bound = demand(&view, Projection::Range(range));
    let mut closure = closure_scratch(&root)?;
    let mut planning = plan_scratch(&root)?;
    let partial = plan(bound, &mut closure, &mut planning, |_| true)?;
    let store = memory_store(&[1, 2, 3])?;
    match partial.stage().verify_store(&store) {
        Err(VerificationError::PartialProjection { pinned_root }) => {
            assert_eq!(pinned_root, root.id);
        }
        Err(_) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::PartialVerification,
                expected: ScenarioExpectation::PartialProjection,
                observed: ScenarioObservation::DifferentVerificationError,
            });
        }
        Ok(_) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::PartialVerification,
                expected: ScenarioExpectation::PartialProjection,
                observed: ScenarioObservation::VerifiedGeneration,
            });
        }
    }
    Ok(())
}

#[test]
fn missing_closure_cannot_issue_a_verified_capability_and_replay_fetches_nothing()
-> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let mut closure = closure_scratch(&root)?;
    let mut planning = plan_scratch(&root)?;
    let partial = plan(
        demand(&view, Projection::CompleteGeneration),
        &mut closure,
        &mut planning,
        |_| false,
    )?;
    let partial_store = memory_store(&[1])?;
    match partial.stage().verify_store(&partial_store) {
        Err(VerificationError::MissingObject { report }) => {
            assert_eq!(report.pinned_root, root.id);
            assert_eq!(report.object, object(2));
        }
        Err(_) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::MissingVerification,
                expected: ScenarioExpectation::MissingObject,
                observed: ScenarioObservation::DifferentVerificationError,
            });
        }
        Ok(_) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::MissingVerification,
                expected: ScenarioExpectation::MissingObject,
                observed: ScenarioObservation::VerifiedGeneration,
            });
        }
    }

    let replay = plan(
        demand(&view, Projection::CompleteGeneration),
        &mut closure,
        &mut planning,
        |_| true,
    )?;
    assert!(replay.is_complete());
    assert_eq!(replay.fetches().count(), 0);
    let store = memory_store(&[1, 2, 3])?;
    let verified = replay
        .stage()
        .verify_store(&store)
        .map_err(ScenarioError::Verification)?;
    assert_eq!(verified.pinned_root, root.id);
    assert_eq!(verified.dep_set, replay.dep_set);
    assert!(core::ptr::eq(
        core::ptr::from_ref(verified.as_ref()),
        core::ptr::from_ref(&store)
    ));
    Ok(())
}

#[test]
fn exact_content_under_different_metadata_cannot_verify() -> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let mut closure = closure_scratch(&root)?;
    let mut planning = plan_scratch(&root)?;
    let complete = plan(
        demand(&view, Projection::CompleteGeneration),
        &mut closure,
        &mut planning,
        |_| true,
    )?;
    let mut store = MemoryStore::new(StoreCapacity {
        bytes: 12_u64.into(),
        slots: 3_u32.into(),
    })?;
    insert_fixture(&mut store, object(2), 2)?;
    insert_fixture(&mut store, object(3), 3)?;
    let expected = object(1);
    let substituted = ObjectRef {
        schema: SchemaId::Frame,
        ..expected
    };
    insert_fixture(&mut store, substituted, 1)?;

    match complete.stage().verify_store(&store) {
        Err(VerificationError::StoredDescriptorMismatch { report }) => {
            assert_eq!(report.pinned_root, root.id);
            assert_eq!(report.expected, expected);
            assert_eq!(report.actual, substituted);
            Ok(())
        }
        Err(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::DescriptorVerification,
            expected: ScenarioExpectation::StoredDescriptorMismatch,
            observed: ScenarioObservation::DifferentVerificationError,
        }),
        Ok(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::DescriptorVerification,
            expected: ScenarioExpectation::StoredDescriptorMismatch,
            observed: ScenarioObservation::VerifiedGeneration,
        }),
    }
}
