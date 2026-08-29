use super::*;

#[test]
fn retained_root_owner_and_construction_peak_have_exact_layout_evidence()
-> Result<(), ScenarioError> {
    let entry_count = 2_usize;
    let root = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, None, object(2)),
    ]))?;
    #[cfg(target_pointer_width = "64")]
    assert_eq!(size_of::<GenerationRoot<ObjectDomain>>(), 64);
    assert_eq!(root.metadata_bytes(), (entry_count * 64).into());
    assert_eq!(
        root.construction_peak_bytes,
        (entry_count
            * (size_of::<RootEntry<ObjectDomain>>()
                + size_of::<crate::packed::RootRow<ObjectDomain>>()))
        .into()
    );
    Ok(())
}

#[test]
fn canonical_order_and_every_hierarchy_rejection_are_exact() -> Result<(), ScenarioError> {
    let first = root(Vec::from([
        resident(2, Some(1), object(2)),
        resident(1, None, object(1)),
    ]))?;
    let second = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
    ]))?;
    assert_eq!(first.id, second.id);

    require_root_error(
        GenerationRoot::new(Vec::from([
            resident(1, None, object(1)),
            resident(1, None, object(2)),
        ])),
        ScenarioExpectation::DuplicateKey,
        |error| matches!(error, RootBuildError::DuplicateKey { key: actual } if *actual == key(1)),
    )?;
    require_root_error(
        GenerationRoot::new(Vec::from([resident(1, Some(9), object(1))])),
        ScenarioExpectation::MissingParent,
        |error| matches!(error, RootBuildError::MissingParent { child, parent } if *child == key(1) && *parent == key(9)),
    )?;
    require_root_error(
        GenerationRoot::new(Vec::from([resident(1, Some(1), object(1))])),
        ScenarioExpectation::HierarchyCycle,
        |error| matches!(error, RootBuildError::HierarchyCycle { key: actual } if *actual == key(1)),
    )?;
    require_root_error(
        GenerationRoot::new(Vec::from([
            resident(1, Some(2), object(1)),
            resident(2, Some(3), object(2)),
            resident(3, Some(1), object(3)),
        ])),
        ScenarioExpectation::HierarchyCycle,
        |error| matches!(error, RootBuildError::HierarchyCycle { key: actual } if *actual == key(1)),
    )
}

#[test]
fn streaming_diff_has_exact_canonical_classifications() -> Result<(), ScenarioError> {
    let older = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, Some(1), object(2)),
        resident(3, None, object(3)),
        resident(5, None, object(5)),
    ]))?;
    let newer = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, None, object(2)),
        resident(3, None, object(9)),
        resident(4, None, object(4)),
    ]))?;
    let mut diff = older.diff(&newer);
    let changes: Vec<_> = diff.by_ref().collect();
    assert_eq!(diff.comparisons(), 4);
    assert_eq!(
        changes,
        Vec::from([
            RootChange::Unchanged {
                entry: resident(1, None, object(1)),
            },
            RootChange::Reparented {
                old: resident(2, Some(1), object(2)),
                new: resident(2, None, object(2)),
            },
            RootChange::ContentChanged {
                old: resident(3, None, object(3)),
                new: resident(3, None, object(9)),
            },
            RootChange::Added {
                new: resident(4, None, object(4)),
            },
            RootChange::Removed {
                old: resident(5, None, object(5)),
            },
        ])
    );
    Ok(())
}

#[test]
fn changed_only_diff_omits_unchanged_rows_and_preserves_edge_transitions()
-> Result<(), ScenarioError> {
    let older = root(Vec::from([
        resident(1, None, object(1)),
        resident(3, Some(1), object(3)),
        resident(5, None, object(5)),
    ]))?;
    let newer = root(Vec::from([
        resident(1, None, object(1)),
        resident(2, None, object(2)),
        resident(3, None, object(3)),
        resident(4, None, object(4)),
    ]))?;
    let mut changes = older.changed_diff(&newer);
    assert_eq!(
        changes.next(),
        Some(RootChange::Added {
            new: resident(2, None, object(2)),
        })
    );
    assert_eq!(
        changes.next(),
        Some(RootChange::Reparented {
            old: resident(3, Some(1), object(3)),
            new: resident(3, None, object(3)),
        })
    );
    assert_eq!(
        changes.next(),
        Some(RootChange::Added {
            new: resident(4, None, object(4)),
        })
    );
    assert_eq!(
        changes.next(),
        Some(RootChange::Removed {
            old: resident(5, None, object(5)),
        })
    );
    assert_eq!(changes.next(), None);
    assert_eq!(changes.comparisons(), 4);
    Ok(())
}

#[test]
fn changed_only_diff_streams_a_million_unchanged_rows_without_output() -> Result<(), ScenarioError>
{
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(
            usize::try_from(MILLION_UNCHANGED_ROWS).map_err(ScenarioError::MillionRowCount)?,
        )
        .map_err(ScenarioError::ClosureReservation)?;
    for raw in FIRST_ROOT_KEY..MILLION_UNCHANGED_ROWS {
        entries.push(resident(raw, None, object(1)));
    }
    let root = root(entries)?;
    let mut changes = root.changed_diff(&root);
    assert_eq!(changes.next(), None);
    assert_eq!(
        changes.comparisons(),
        usize::try_from(MILLION_UNCHANGED_ROWS).map_err(ScenarioError::MillionRowCount)?
    );
    Ok(())
}

#[test]
fn hundred_thousand_rows_have_fixed_resident_budget_and_streaming_diff() -> Result<(), ScenarioError>
{
    let mut older_entries = Vec::new();
    let mut newer_entries = Vec::new();
    for raw in FIRST_ROOT_KEY..LARGE_ROOT_ROWS {
        older_entries.push(resident(raw, None, object(1)));
        newer_entries.push(resident(
            raw,
            None,
            object(if raw % DIFF_MUTATION_STRIDE == 0 {
                2
            } else {
                1
            }),
        ));
    }
    let older = root(older_entries)?;
    let newer = root(newer_entries)?;
    assert_eq!(older.metadata_bytes(), (LARGE_ROOT_ROW_COUNT * 64).into());
    let mut diff = older.diff(&newer);
    let changed = diff
        .by_ref()
        .filter(|change| matches!(change, RootChange::ContentChanged { .. }))
        .count();
    assert_eq!(changed, 10);
    assert_eq!(diff.comparisons(), LARGE_ROOT_ROW_COUNT);
    Ok(())
}

#[test]
fn bolero_canonical_identity_is_independent_of_input_permutation() {
    check!().for_each(|input: &[u8]| {
        let entries: Vec<_> = input
            .iter()
            .take(32)
            .zip(0_u64..)
            .map(|(byte, position)| resident(position, None, object(*byte)))
            .collect();
        let mut reversed = entries.clone();
        reversed.reverse();
        let ordered = GenerationRoot::new(entries);
        let permuted = GenerationRoot::new(reversed);
        let outcome = match (ordered, permuted) {
            (Ok(ordered), Ok(permuted)) if ordered.id == permuted.id => {
                PermutationOutcome::Equivalent
            }
            (Ok(_), Ok(_)) => PermutationOutcome::DifferentIdentity,
            (Err(older), Ok(_)) => PermutationOutcome::OlderRejected(root_error_kind(&older)),
            (Ok(_), Err(newer)) => PermutationOutcome::NewerRejected(root_error_kind(&newer)),
            (Err(older), Err(newer)) => PermutationOutcome::BothRejected {
                older: root_error_kind(&older),
                newer: root_error_kind(&newer),
            },
        };
        assert_eq!(outcome, PermutationOutcome::Equivalent);
    });
}
