//! Exercises the `backend-semantic::index_core` tests manifest-bounds contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, ExactManifest, ExactManifestError, ExactOperation,
    ExactResolution, ExactRow, ExactSegment, ExactSegmentError, ExactSegmentId, ExactTerminal,
    IndexSnapshot, LexicalManifest, LexicalManifestError, LexicalRow, LexicalScore, LexicalSegment,
    MAX_SELECTED_SEGMENTS,
};
use backend_semantic::ir::EntityId;
use backend_version::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};

fn generation() -> GenerationId {
    GenerationId::from_canonical_bytes(b"published-ir-generation")
}

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"lexical-snapshot-test-fragment",
            ),
        ),
        entity: EntityId::new(entity),
    }
}

fn repeated_segment() -> Result<ExactSegment<'static>, ExactSegmentError<'static>> {
    ExactSegment::new(&[])
}

#[test]
fn hostile_selection_limit_fails_before_duplicate_validation()
-> Result<(), ExactSegmentError<'static>> {
    let segments = [repeated_segment()?; MAX_SELECTED_SEGMENTS + 1];
    let snapshot = IndexSnapshot::new(generation(), &[], &[]);
    assert!(snapshot.is_ok());
    let Some(snapshot) = snapshot.ok() else {
        return Ok(());
    };
    let result = ExactManifest::new(snapshot, &segments, &[]);

    assert_eq!(
        result,
        Err(ExactManifestError::SelectedSegmentLimit {
            limit: MAX_SELECTED_SEGMENTS,
            observed: MAX_SELECTED_SEGMENTS + 1,
        })
    );
    Ok(())
}

#[test]
fn two_hundred_selected_segments_seal_and_resolve() {
    let keys: Vec<Vec<u8>> = (0..200)
        .map(|ordinal| format!("key-{ordinal:03}").into_bytes())
        .collect();
    let values: Vec<Vec<u8>> = (0..200)
        .map(|ordinal| format!("value-{ordinal:03}").into_bytes())
        .collect();
    let rows: Vec<[ExactRow<'_>; 1]> = keys
        .iter()
        .zip(values.iter())
        .map(|(key, value)| [ExactRow::present(key, value)])
        .collect();
    let segments: Vec<ExactSegment<'_>> = rows
        .iter()
        .map(|row| ExactSegment::new(row.as_slice()).expect("one-row exact segment"))
        .collect();
    let ids: Vec<ExactSegmentId> = segments.iter().map(|segment| segment.id).collect();
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"multi-segment-corpus"),
        &ids,
        &[],
    )
    .expect("bounded multi-segment selection");
    let manifest = ExactManifest::new(snapshot, &segments, &[]).expect("ordered selection");
    assert!(matches!(
        manifest.execute(ExactOperation::new(&keys[199])),
        ExactTerminal::Complete {
            resolution: ExactResolution::Present { value, .. },
            ..
        } if value == values[199].as_slice()
    ));
}

#[test]
fn duplicate_present_and_reversed_order_rejected_for_exact_and_lexical_manifests() {
    let old_exact_rows = [ExactRow::present(b"key", b"old")];
    let update_exact_rows = [ExactRow::present(b"key", b"new")];
    let old_exact = ExactSegment::new(&old_exact_rows).expect("old exact segment");
    let update_exact = ExactSegment::new(&update_exact_rows).expect("update exact segment");
    let exact_selected = [update_exact.id, old_exact.id];
    let exact_snapshot =
        IndexSnapshot::new(generation(), &exact_selected, &[]).expect("exact snapshot");
    let duplicate_exact = [update_exact, update_exact];
    assert_eq!(
        ExactManifest::new(exact_snapshot, &duplicate_exact, &[]),
        Err(ExactManifestError::DuplicatePresentSegment {
            left_position: 0,
            right_position: 1,
            id: update_exact.id,
        })
    );
    let reversed_exact = [old_exact, update_exact];
    assert_eq!(
        ExactManifest::new(exact_snapshot, &reversed_exact, &[]),
        Err(ExactManifestError::PresentOrderMismatch {
            present_position: 1,
            preceding_selected_position: 1,
            selected_position: 0,
            id: update_exact.id,
        })
    );
    let extra_exact_rows = [ExactRow::present(b"extra", b"value")];
    let extra_exact = ExactSegment::new(&extra_exact_rows).expect("extra exact segment");
    assert_eq!(
        ExactManifest::new(exact_snapshot, &[update_exact], &[]),
        Err(ExactManifestError::SnapshotSelectionWidth {
            selected: 2,
            observed: 1,
        })
    );
    assert_eq!(
        ExactManifest::new(exact_snapshot, &[update_exact], &[extra_exact.id]),
        Err(ExactManifestError::MissingNotSelected {
            missing_position: 0,
            id: extra_exact.id,
        })
    );

    let old_lexical_rows = [LexicalRow::new(
        b"needle",
        document(1),
        LexicalScore::from(9),
    )];
    let update_lexical_rows = [LexicalRow::new(
        b"needle",
        document(1),
        LexicalScore::from(2),
    )];
    let old_lexical = LexicalSegment::new(&old_lexical_rows).expect("old lexical segment");
    let update_lexical = LexicalSegment::new(&update_lexical_rows).expect("update lexical segment");
    let lexical_selected = [update_lexical.id, old_lexical.id];
    let lexical_snapshot =
        IndexSnapshot::new(generation(), &[], &lexical_selected).expect("lexical snapshot");
    let duplicate_lexical = [update_lexical, update_lexical];
    assert_eq!(
        LexicalManifest::new(lexical_snapshot, &duplicate_lexical, &[]),
        Err(LexicalManifestError::DuplicatePresentSegment {
            left_position: 0,
            right_position: 1,
            id: update_lexical.id,
        })
    );
    let reversed_lexical = [old_lexical, update_lexical];
    assert_eq!(
        LexicalManifest::new(lexical_snapshot, &reversed_lexical, &[]),
        Err(LexicalManifestError::PresentOrderMismatch {
            present_position: 1,
            preceding_selected_position: 1,
            selected_position: 0,
            id: update_lexical.id,
        })
    );
    let extra_lexical_rows = [LexicalRow::new(
        b"extra",
        document(2),
        LexicalScore::from(1),
    )];
    let extra_lexical = LexicalSegment::new(&extra_lexical_rows).expect("extra lexical segment");
    assert_eq!(
        LexicalManifest::new(lexical_snapshot, &[update_lexical], &[]),
        Err(LexicalManifestError::SnapshotSelectionWidth {
            selected: 2,
            observed: 1,
        })
    );
    assert_eq!(
        LexicalManifest::new(lexical_snapshot, &[update_lexical], &[extra_lexical.id]),
        Err(LexicalManifestError::MissingNotSelected {
            missing_position: 0,
            id: extra_lexical.id,
        })
    );
}

#[test]
fn three_present_segments_report_leftmost_duplicate_not_smaller_id() {
    let a_exact_rows = [ExactRow::present(b"key", b"old")];
    let b_exact_rows = [ExactRow::present(b"key", b"new")];
    let c_exact_rows = [ExactRow::present(b"extra", b"value")];
    let a_exact = ExactSegment::new(&a_exact_rows).expect("a exact segment");
    let b_exact = ExactSegment::new(&b_exact_rows).expect("b exact segment");
    let c_exact = ExactSegment::new(&c_exact_rows).expect("c exact segment");
    let exact_selected = [b_exact.id, a_exact.id, c_exact.id];
    let exact_snapshot =
        IndexSnapshot::new(generation(), &exact_selected, &[]).expect("exact snapshot");
    let duplicate_exact = [b_exact, a_exact, b_exact];
    assert_eq!(
        ExactManifest::new(exact_snapshot, &duplicate_exact, &[]),
        Err(ExactManifestError::DuplicatePresentSegment {
            left_position: 0,
            right_position: 2,
            id: b_exact.id,
        })
    );

    let a_lexical_rows = [LexicalRow::new(
        b"needle",
        document(1),
        LexicalScore::from(9),
    )];
    let b_lexical_rows = [LexicalRow::new(
        b"needle",
        document(1),
        LexicalScore::from(2),
    )];
    let c_lexical_rows = [LexicalRow::new(
        b"extra",
        document(2),
        LexicalScore::from(1),
    )];
    let a_lexical = LexicalSegment::new(&a_lexical_rows).expect("a lexical segment");
    let b_lexical = LexicalSegment::new(&b_lexical_rows).expect("b lexical segment");
    let c_lexical = LexicalSegment::new(&c_lexical_rows).expect("c lexical segment");
    let lexical_selected = [b_lexical.id, a_lexical.id, c_lexical.id];
    let lexical_snapshot =
        IndexSnapshot::new(generation(), &[], &lexical_selected).expect("lexical snapshot");
    let duplicate_lexical = [b_lexical, a_lexical, b_lexical];
    assert_eq!(
        LexicalManifest::new(lexical_snapshot, &duplicate_lexical, &[]),
        Err(LexicalManifestError::DuplicatePresentSegment {
            left_position: 0,
            right_position: 2,
            id: b_lexical.id,
        })
    );
}
