use super::*;

#[test]
fn bound_demand_produces_exact_ordered_coverage_and_fetch_routes() -> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let bound = Need::new(root.id, Projection::CompleteGeneration).bind(&view)?;
    let mut closure = closure_scratch(&root)?;
    let mut planning = plan_scratch(&root)?;
    let sparse_state_bytes = {
        let hydrated = plan(bound, &mut closure, &mut planning, |descriptor| {
            descriptor.content == object(1).content
        })?;
        let required: Vec<_> = hydrated
            .required()
            .map(|descriptor| descriptor.content)
            .collect();
        let present: Vec<_> = hydrated
            .present()
            .map(|descriptor| descriptor.content)
            .collect();
        let promised: Vec<_> = hydrated
            .promised()
            .map(|promise| promise.object.content)
            .collect();
        let missing: Vec<_> = hydrated
            .missing()
            .map(|descriptor| descriptor.content)
            .collect();
        let fetches: Vec<_> = hydrated.fetches().collect();
        assert_eq!(
            required,
            Vec::from([object(1).content, object(2).content, object(3).content])
        );
        assert_eq!(present, Vec::from([object(1).content]));
        assert_eq!(promised, Vec::from([object(2).content]));
        assert_eq!(missing, Vec::from([object(3).content]));
        assert_eq!(
            fetches,
            Vec::from([
                Fetch {
                    object: object(2),
                    route: FetchRoute::Promised(ProviderSet::only(ProviderId::try_from(7_u8)?)),
                },
                Fetch {
                    object: object(3),
                    route: FetchRoute::Unrouted,
                },
            ])
        );
        hydrated.sparse_state_bytes()
    };
    assert_eq!(sparse_state_bytes, (2 * size_of::<u32>()).into());
    assert_eq!(planning.high_water_absent, AbsentCount::from(2));
    Ok(())
}

#[test]
fn planning_probe_records_one_aggregate_coverage_event() -> Result<(), ScenarioError> {
    let root = root()?;
    let locality_bytes = locality(&root)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let mut closure = closure_scratch(&root)?;
    let mut planning = plan_scratch(&root)?;
    let mut probe = FlightRecorder::<HydrationProbeEvent, DropNewest, 1>::new();
    let planned = plan_with_probe(
        demand(&view, Projection::CompleteGeneration),
        &mut closure,
        &mut planning,
        |descriptor| descriptor.content == object(1).content,
        &mut probe,
    )?;
    assert_eq!(
        planned.coverage,
        PlanCoverage {
            required: 3.into(),
            present: 1.into(),
            promised: 1.into(),
            missing: 1.into(),
        }
    );
    assert_eq!(
        probe.events().next(),
        Some(&HydrationProbeEvent {
            outcome: HydrationOutcome::Planned(planned.coverage),
        })
    );
    assert_eq!(probe.events().nth(1), None);
    Ok(())
}

#[test]
fn overlay_and_range_coverage_remain_exact_and_small_scratch_rolls_back()
-> Result<(), ScenarioError> {
    let root = root()?;
    let row = root
        .locality_row(key(3))
        .ok_or(ScenarioError::MissingLocalityRow { key: key(3) })?;
    let locality_bytes = locality_bytes(
        &root,
        &[LocalityException::new(
            row,
            NonResident::Overlaid(RemoteBase::Absent {
                generation: root.id,
            }),
        )],
    )?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let range = nudox_root::EntryRange::new(key(3), key(3))?;
    let mut closure = closure_scratch(&root)?;
    let mut planning = plan_scratch(&root)?;
    let ranged = plan(
        demand(&view, Projection::Range(range)),
        &mut closure,
        &mut planning,
        |_| false,
    )?;
    let required: Vec<_> = ranged
        .required()
        .map(|descriptor| descriptor.content)
        .collect();
    let missing: Vec<_> = ranged
        .missing()
        .map(|descriptor| descriptor.content)
        .collect();
    assert_eq!(required, Vec::from([object(1).content, object(3).content]));
    assert_eq!(missing, required);
    assert_eq!(ranged.promised().count(), 0);
    assert_eq!(ranged.fetches().count(), 2);

    assert_small_scratch_rejected(&view, &mut closure)
}

fn assert_small_scratch_rejected(
    view: &GenerationView<'_, '_, ObjectDomain>,
    closure: &mut ClosureScratch,
) -> Result<(), ScenarioError> {
    let mut too_small = PlanScratch::new(1_u32.into()).map_err(ScenarioError::PlanReservation)?;
    match plan(
        demand(view, Projection::CompleteGeneration),
        closure,
        &mut too_small,
        |_| false,
    ) {
        Err(PlanError::ScratchTooSmall {
            required,
            available,
        }) => {
            assert_eq!(u32::from(required), 3);
            assert_eq!(u32::from(available), 1);
        }
        Err(_) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::PlanCapacity,
                expected: ScenarioExpectation::ScratchTooSmall,
                observed: ScenarioObservation::DifferentPlanError,
            });
        }
        Ok(_) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::PlanCapacity,
                expected: ScenarioExpectation::ScratchTooSmall,
                observed: ScenarioObservation::HydrationPlan,
            });
        }
    }
    assert_eq!(too_small.high_water_absent, AbsentCount::from(0));
    Ok(())
}

#[rstest]
#[case(0_u8)]
#[case(1_u8)]
#[case(50_u8)]
#[case(100_u8)]
fn sparse_absence_state_reports_logical_and_retained_bytes(
    #[case] misses: u8,
) -> Result<(), ScenarioError> {
    let mut entries = Vec::new();
    for byte in 1_u8..=100_u8 {
        entries.push(RootEntry {
            key: key(u64::from(byte)),
            parent: None,
            object: object(byte),
        });
    }
    let root = GenerationRoot::new(entries).map_err(ScenarioError::Root)?;
    let locality_bytes = locality_bytes(&root, &[])?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let mut closure = closure_scratch(&root)?;
    let mut planning = plan_scratch(&root)?;
    let expected_misses = u32::from(misses);
    let logical_sparse_bytes: MetadataBytes = (usize::from(misses) * size_of::<u32>()).into();
    let observed_logical_bytes = {
        let planned = plan(
            demand(&view, Projection::CompleteGeneration),
            &mut closure,
            &mut planning,
            |descriptor| descriptor.content.as_ref()[1] <= 100 - misses,
        )?;
        assert_eq!(planned.coverage.required, 100.into());
        assert_eq!(planned.coverage.missing, expected_misses.into());
        assert_eq!(planned.coverage.promised, 0.into());
        assert_eq!(planned.coverage.present, (100 - expected_misses).into());
        assert_eq!(planned.fetches().count(), usize::from(misses));
        planned.sparse_state_bytes()
    };
    assert_eq!(observed_logical_bytes, logical_sparse_bytes);
    assert_eq!(planning.retained_absence_bytes, 400.into());
    Ok(())
}
