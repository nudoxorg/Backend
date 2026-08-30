use super::*;

#[test]
fn overlay_propagation_marks_root_path_and_preserves_absence() -> Result<(), ScenarioError> {
    let base = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
        resident(3, Some(2), object(3)),
    ]))?;
    let local = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
        RootEntry {
            key: key(3),
            parent: Some(key(2)),
            object: object(9),
        },
    ]))?;
    let locality_bytes = locality_bytes(
        &local,
        &[locality_exception(
            &local,
            key(3),
            NonResident::Overlaid(RemoteBase::Present {
                generation: base.id,
                object: object(3),
            }),
        )?],
    )?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&local, &locality)?;
    let mut marks = vec![false; local.len()];
    let mut output = [0_u8; 512];
    let (propagated, work) = propagate_overlays(&view, &base, &mut marks, &mut output)
        .map_err(|error| overlay_error(ScenarioStep::PropagatedLocality, &error))?;
    assert_eq!(local.id, propagated.generation);
    assert_eq!(work.mark_bytes, local.len().into());
    assert_eq!(work.final_metadata_bytes, propagated.metadata_bytes());
    assert_eq!(
        Some(work.peak_live_bytes),
        work.mark_bytes.checked_combined(work.final_metadata_bytes)
    );
    let view = GenerationView::new(&local, &propagated)?;
    for selected_key in [key(1), key(2), key(3)] {
        assert_propagated_overlay(&view, &base, selected_key)?;
    }
    Ok(())
}

fn assert_propagated_overlay(
    view: &GenerationView<'_, '_, ObjectDomain>,
    base: &GenerationRoot<ObjectDomain>,
    selected_key: EntryKey,
) -> Result<(), ScenarioError> {
    let entry = view.get(selected_key).ok_or(ScenarioError::MissingEntry {
        step: ScenarioStep::PropagatedLocality,
        key: selected_key,
    })?;
    let expected = base
        .get(selected_key)
        .ok_or(ScenarioError::MissingEntry {
            step: ScenarioStep::PropagatedLocality,
            key: selected_key,
        })?
        .object;
    match entry.locality {
        Locality::Overlaid(remote) => {
            assert_eq!(
                remote,
                RemoteBase::Present {
                    generation: base.id,
                    object: expected,
                }
            );
            Ok(())
        }
        Locality::Resident | Locality::Promised(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::PropagatedLocality,
            expected: ScenarioExpectation::Overlaid,
            observed: ScenarioObservation::NonOverlay,
        }),
    }
}

fn overlay_error(step: ScenarioStep, error: &OverlayError<ObjectDomain>) -> ScenarioError {
    let rejection = match error {
        OverlayError::Locality(_) => OverlayRejection::Locality,
        OverlayError::BaseGenerationMismatch { .. } => OverlayRejection::BaseGenerationMismatch,
        OverlayError::BaseObjectMismatch { .. } => OverlayRejection::BaseObjectMismatch,
        OverlayError::ExpectedAbsentBase { .. } => OverlayRejection::ExpectedAbsentBase,
        OverlayError::MarkScratchTooSmall { .. } => OverlayRejection::MarkScratchTooSmall,
        OverlayError::Output(_) => OverlayRejection::Output,
        OverlayError::MetadataByteOverflow => OverlayRejection::MetadataByteOverflow,
    };
    ScenarioError::Overlay { step, rejection }
}

#[test]
fn locality_scan_uses_one_ordered_sparse_route_cursor() -> Result<(), ScenarioError> {
    let root = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
        resident(3, Some(1), object(3)),
    ]))?;
    let provider = nudox_object::ProviderId::try_from(3_u8)?;
    let locality_bytes = routed_locality(&root, provider)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;
    let mut scan = view.measured_closure();
    let composed: Vec<_> = scan.by_ref().collect();
    assert_eq!(composed, routed_entries(&root, provider));
    assert_eq!(scan.rows, 3);
    assert_eq!(scan.sparse_comparisons, 3);
    assert_eq!(locality.generation, root.id);
    Ok(())
}

#[test]
fn locality_lookup_and_selected_cursor_have_exact_sparse_bounds() -> Result<(), ScenarioError> {
    let root = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
        resident(3, Some(1), object(3)),
    ]))?;
    let provider = nudox_object::ProviderId::try_from(3_u8)?;
    let locality_bytes = routed_locality(&root, provider)?;
    let locality = ValidatedLocality::try_from(locality_bytes.as_slice())?;
    let view = GenerationView::new(&root, &locality)?;

    let (found, lookup) = view.measured_get(key(2));
    let found = found.ok_or(ScenarioError::MissingEntry {
        step: ScenarioStep::PropagatedLocality,
        key: key(2),
    })?;
    assert_eq!(
        found.locality,
        Locality::Promised(nudox_object::ProviderSet::only(provider))
    );
    assert_eq!(lookup.route_binary_searches, 1);
    assert_eq!(lookup.route_comparisons, 2);

    let range = crate::EntryRange::new(key(3), key(3))?;
    let mut scratch = ClosureScratch::new(root.len()).map_err(ScenarioError::ClosureReservation)?;
    let selected = view.select_closure(Some(range), &mut scratch)?;
    let mut work = crate::LocalityScanWork::default();
    let keys: Vec<_> = selected
        .measured_iter(&mut work)
        .map(|entry| entry.key)
        .collect();
    assert_eq!(keys, Vec::from([key(1), key(3)]));
    assert_eq!(work.rows, 2);
    assert_eq!(work.sparse_comparisons, 3);
    assert!(work.sparse_comparisons <= work.rows + 2);
    Ok(())
}

fn routed_locality(
    root: &GenerationRoot<ObjectDomain>,
    provider: nudox_object::ProviderId,
) -> Result<Vec<u8>, ScenarioError> {
    locality_bytes(
        root,
        &[
            locality_exception(
                root,
                key(2),
                NonResident::Promised(nudox_object::ProviderSet::only(provider)),
            )?,
            locality_exception(
                root,
                key(3),
                NonResident::Overlaid(RemoteBase::Absent {
                    generation: root.id,
                }),
            )?,
        ],
    )
}

fn routed_entries(
    root: &GenerationRoot<ObjectDomain>,
    provider: nudox_object::ProviderId,
) -> Vec<crate::GenerationEntry<ObjectDomain>> {
    Vec::from([
        crate::GenerationEntry {
            key: key(1),
            parent: None,
            object: object(1),
            locality: Locality::Resident,
        },
        crate::GenerationEntry {
            key: key(2),
            parent: Some(key(1)),
            object: object(2),
            locality: Locality::Promised(nudox_object::ProviderSet::only(provider)),
        },
        crate::GenerationEntry {
            key: key(3),
            parent: Some(key(1)),
            object: object(3),
            locality: Locality::Overlaid(RemoteBase::Absent {
                generation: root.id,
            }),
        },
    ])
}

#[test]
fn locality_changes_preserve_identity_and_narrow_large_ranges_touch_only_demand()
-> Result<(), ScenarioError> {
    let mut entries = Vec::new();
    for raw in FIRST_ROOT_KEY..LARGE_ROOT_ROWS {
        entries.push(resident(raw, None, object(1)));
    }
    let large_root = root(entries)?;
    let provider = nudox_object::ProviderId::try_from(2_u8)?;
    let promised_bytes = locality_bytes(
        &large_root,
        &[locality_exception(
            &large_root,
            key(50_000),
            NonResident::Promised(nudox_object::ProviderSet::only(provider)),
        )?],
    )?;
    let promised = ValidatedLocality::<ObjectDomain>::try_from(promised_bytes.as_slice())?;
    let overlaid_bytes = locality_bytes(
        &large_root,
        &[locality_exception(
            &large_root,
            key(50_000),
            NonResident::Overlaid(RemoteBase::Absent {
                generation: large_root.id,
            }),
        )?],
    )?;
    let overlaid = ValidatedLocality::<ObjectDomain>::try_from(overlaid_bytes.as_slice())?;
    assert_eq!(promised.generation, large_root.id);
    assert_eq!(overlaid.generation, large_root.id);
    assert_ne!(promised.metadata_bytes(), overlaid.metadata_bytes());

    let changed = root(Vec::from([resident(0, None, object(2))]))?;
    let original = root(Vec::from([resident(0, None, object(1))]))?;
    assert_ne!(changed.id, original.id);

    let range = crate::EntryRange::new(key(50_000), key(50_000))?;
    let mut scratch =
        ClosureScratch::new(large_root.len()).map_err(ScenarioError::ClosureReservation)?;
    let selected = large_root.select_closure(Some(range), &mut scratch)?;
    assert_eq!(selected.len(), 1);
    assert_eq!(scratch.work.projected_rows, 1);
    assert_eq!(scratch.work.ancestor_edges, 0);
    Ok(())
}

#[test]
fn remote_tombstone_is_a_distinct_preserved_fact() -> Result<(), ScenarioError> {
    let remote: RemoteBase<ObjectDomain> = RemoteBase::Absent {
        generation: GenerationId::from_digest([8; 32]),
    };
    match remote {
        RemoteBase::Absent { generation } => {
            assert_eq!(generation, GenerationId::from_digest([8; 32]));
        }
        RemoteBase::Present { .. } => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::RemoteTombstone,
                expected: ScenarioExpectation::RemoteAbsent,
                observed: ScenarioObservation::RemotePresent,
            });
        }
    }
    Ok(())
}
