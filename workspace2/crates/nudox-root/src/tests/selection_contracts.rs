use super::*;

#[test]
fn narrow_projection_selects_only_range_and_checked_ancestors() -> Result<(), ScenarioError> {
    let root = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
        resident(3, Some(2), object(3)),
        resident(4, None, object(4)),
    ]))?;
    let mut scratch = ClosureScratch::new(root.len()).map_err(ScenarioError::ClosureReservation)?;
    let range = crate::EntryRange::new(key(3), key(3))?;
    let selected = root.select_closure(Some(range), &mut scratch)?;
    let keys: Vec<_> = selected.iter().map(|entry| entry.key).collect();
    assert_eq!(keys, Vec::from([key(1), key(2), key(3)]));
    assert_eq!(selected.len(), 3);
    assert_eq!(
        scratch.work,
        crate::SelectionWork {
            projected_rows: 1,
            ancestor_edges: 2,
            parent_search_comparisons: 0,
        }
    );
    Ok(())
}

#[test]
fn closure_probe_records_one_aggregate_selection_event() -> Result<(), ScenarioError> {
    let root = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
        resident(3, Some(1), object(3)),
    ]))?;
    let range = crate::EntryRange::new(key(3), key(3))?;
    let mut scratch = ClosureScratch::new(root.len()).map_err(ScenarioError::ClosureReservation)?;
    let mut probe = FlightRecorder::<RootProbeEvent, DropNewest, 1>::new();
    let selected = root.select_closure_with_probe(Some(range), &mut scratch, &mut probe)?;
    assert_eq!(selected.len(), 2);
    assert_eq!(
        probe.events().next(),
        Some(&RootProbeEvent {
            selected_rows: 2,
            work: crate::SelectionWork {
                projected_rows: 1,
                ancestor_edges: 1,
                parent_search_comparisons: 0,
            },
        })
    );
    assert_eq!(probe.events().nth(1), None);
    Ok(())
}

#[test]
fn undersized_selected_ordinal_buffer_does_not_classify_or_mutate() -> Result<(), ScenarioError> {
    let root = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
    ]))?;
    let locality_bytes = locality_bytes(&root, &[])?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let mut closure = ClosureScratch::new(root.len()).map_err(ScenarioError::ClosureReservation)?;
    let selected = view.select_closure(None, &mut closure)?;
    let mut buffer =
        SelectedOrdinalBuffer::new(1.into()).map_err(ScenarioError::ClosureReservation)?;
    let retained_bytes = buffer.retained_bytes();
    let mut predicate_calls = 0_u8;
    let result = selected.retain_ordinals_where(&mut buffer, |_| {
        predicate_calls += 1;
        true
    });
    let Err(error) = result else {
        return Err(ScenarioError::Transition {
            step: ScenarioStep::RootRejection,
            expected: ScenarioExpectation::DuplicateKey,
            observed: ScenarioObservation::RootConstructed,
        });
    };
    assert_eq!(
        error,
        SelectedOrdinalBufferError::TooSmall {
            required: 2.into(),
            available: 1.into(),
        }
    );
    assert_eq!(predicate_calls, 0);
    assert!(buffer.is_empty());
    assert_eq!(buffer.retained_bytes(), retained_bytes);
    Ok(())
}

#[test]
fn sparse_ordinals_remain_bound_to_the_selection_that_emitted_them() -> Result<(), ScenarioError> {
    let root = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, None, object(2)),
    ]))?;
    let locality_bytes = locality_bytes(&root, &[])?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let mut first_scratch =
        ClosureScratch::new(root.len()).map_err(ScenarioError::ClosureReservation)?;
    let first = view.select_closure(
        Some(crate::EntryRange::new(key(1), key(1))?),
        &mut first_scratch,
    )?;
    let mut buffer =
        SelectedOrdinalBuffer::new(first.count()).map_err(ScenarioError::ClosureReservation)?;
    let first_positions = first.retain_ordinals_where(&mut buffer, |_| true)?;
    let mut second_scratch =
        ClosureScratch::new(root.len()).map_err(ScenarioError::ClosureReservation)?;
    let second = view.select_closure(
        Some(crate::EntryRange::new(key(2), key(2))?),
        &mut second_scratch,
    )?;
    let first_keys: Vec<_> = first_positions
        .absent_entries()
        .map(|entry| entry.key)
        .collect();
    let second_keys: Vec<_> = second.iter().map(|entry| entry.key).collect();
    assert_eq!(first_keys, Vec::from([key(1)]));
    assert_eq!(second_keys, Vec::from([key(2)]));
    Ok(())
}
