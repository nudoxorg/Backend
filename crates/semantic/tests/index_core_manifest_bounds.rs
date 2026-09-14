//! Exercises the `backend-semantic::index_core` tests manifest-bounds contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_version::GenerationId;
use backend_semantic::index_core::{
    ExactManifest, ExactManifestError, ExactOperation, ExactResolution, ExactRow, ExactSegment,
    ExactSegmentError, ExactSegmentId, ExactTerminal, IndexSnapshot, MAX_SELECTED_SEGMENTS,
};

fn repeated_segment() -> Result<ExactSegment<'static>, ExactSegmentError<'static>> {
    ExactSegment::new(&[])
}

#[test]
fn hostile_selection_limit_fails_before_duplicate_validation()
-> Result<(), ExactSegmentError<'static>> {
    let segments = [repeated_segment()?; MAX_SELECTED_SEGMENTS + 1];
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"published-ir-generation"),
        &[],
        &[],
    );
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
