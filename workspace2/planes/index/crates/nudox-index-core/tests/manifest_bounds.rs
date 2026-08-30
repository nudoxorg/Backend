use nudox_index_core::{
    ExactManifest, ExactManifestError, ExactSegment, ExactSegmentError, IndexSnapshotId,
    MAX_SELECTED_SEGMENTS,
};

const SEGMENT_BYTES: &[u8] = b"bounded-manifest-segment";

fn repeated_segment() -> Result<ExactSegment<'static>, ExactSegmentError<'static>> {
    ExactSegment::new(SEGMENT_BYTES, &[])
}

#[test]
fn hostile_selection_limit_fails_before_duplicate_validation()
-> Result<(), ExactSegmentError<'static>> {
    let segments = [repeated_segment()?; MAX_SELECTED_SEGMENTS + 1];
    let result = ExactManifest::new(
        IndexSnapshotId::from_canonical_bytes(b"snapshot"),
        &segments,
        &[],
    );

    assert_eq!(
        result,
        Err(ExactManifestError::SelectedSegmentLimit {
            limit: MAX_SELECTED_SEGMENTS,
            observed: MAX_SELECTED_SEGMENTS + 1,
        })
    );
    Ok(())
}
