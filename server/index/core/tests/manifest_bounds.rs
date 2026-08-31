//! Exercises the `server-index-core` tests manifest-bounds contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use heart_identity::GenerationId;
use server_index_core::{
    ExactManifest, ExactManifestError, ExactSegment, ExactSegmentError, IndexSnapshot,
    MAX_SELECTED_SEGMENTS,
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
