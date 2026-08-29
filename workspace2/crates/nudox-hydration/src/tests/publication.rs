use super::*;

#[test]
fn verified_generation_has_only_its_two_runtime_facts() {
    let witness_bytes = size_of::<GenerationId>() + size_of::<nudox_object::DepSetId>();
    assert_eq!(size_of::<crate::VerifiedGeneration>(), witness_bytes);
}

#[test]
fn binding_and_verification_reject_adjacent_invalid_states() -> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    require_demand_mismatch(
        &Need::new(GenerationId::from([9; 32]), Projection::CompleteGeneration).bind(&view),
        root.id,
    )?;

    let range = nudox_root::EntryRange::new(key(2), key(2))?;
    let bound = demand(&view, Projection::Range(range));
    let mut closure = closure_scratch(&root)?;
    let mut planning = plan_scratch(&root)?;
    let partial = plan(bound, &mut closure, &mut planning, |_| true)?;
    match partial.stage().verify(|_| true) {
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
    match partial.stage().verify(|descriptor| descriptor == object(1)) {
        Err(VerificationError::MissingObject {
            pinned_root,
            object: missing,
        }) => {
            assert_eq!(pinned_root, root.id);
            assert_eq!(missing, object(2));
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
    assert_eq!(replay.fetches().collect::<Result<Vec<_>, _>>()?.len(), 0);
    let verified = replay
        .stage()
        .verify(|_| true)
        .map_err(ScenarioError::Verification)?;
    assert_eq!(verified.pinned_root, root.id);
    assert_eq!(verified.dep_set, replay.dep_set);
    Ok(())
}
