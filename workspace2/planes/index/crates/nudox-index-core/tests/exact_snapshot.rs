use nudox_index_core::{
    ExactDegradation, ExactManifest, ExactOperation, ExactResolution, ExactRow, ExactSegment,
    ExactTerminal, IndexSnapshotId,
};

const ALPHA: &[u8] = b"alpha";
const BETA: &[u8] = b"beta";

#[test]
fn newest_delta_and_compacted_replacement_have_equivalent_exact_meaning() {
    let original_rows = [
        ExactRow::present(ALPHA, b"old"),
        ExactRow::present(BETA, b"old-beta"),
    ];
    let update_rows = [ExactRow::present(ALPHA, b"new"), ExactRow::tombstone(BETA)];
    let compacted_rows = [ExactRow::present(ALPHA, b"new"), ExactRow::tombstone(BETA)];
    let original = ExactSegment::new(b"original", &original_rows);
    let update = ExactSegment::new(b"update", &update_rows);
    let compacted = ExactSegment::new(b"compacted", &compacted_rows);
    assert!(original.is_ok());
    assert!(update.is_ok());
    assert!(compacted.is_ok());
    let Some(original) = original.ok() else {
        return;
    };
    let Some(update) = update.ok() else {
        return;
    };
    let Some(compacted) = compacted.ok() else {
        return;
    };
    let incremental_segments = [update, original];
    let compacted_segments = [compacted];
    let incremental = ExactManifest::new(
        IndexSnapshotId::from_canonical_bytes(b"incremental-snapshot"),
        &incremental_segments,
        &[],
    );
    let compacted_manifest = ExactManifest::new(
        IndexSnapshotId::from_canonical_bytes(b"compacted-snapshot"),
        &compacted_segments,
        &[],
    );
    assert!(incremental.is_ok());
    assert!(compacted_manifest.is_ok());
    let Some(incremental) = incremental.ok() else {
        return;
    };
    let Some(compacted_manifest) = compacted_manifest.ok() else {
        return;
    };

    let incremental_alpha = incremental.execute(ExactOperation::new(ALPHA));
    let compacted_alpha = compacted_manifest.execute(ExactOperation::new(ALPHA));
    assert!(matches!(
        incremental_alpha,
        ExactTerminal::Complete {
            resolution: ExactResolution::Present { value, .. },
            ..
        } if value == b"new"
    ));
    assert!(matches!(
        compacted_alpha,
        ExactTerminal::Complete {
            resolution: ExactResolution::Present { value, .. },
            ..
        } if value == b"new"
    ));
    assert!(matches!(
        incremental.execute(ExactOperation::new(BETA)),
        ExactTerminal::Complete {
            resolution: ExactResolution::Deleted { .. },
            ..
        }
    ));
    assert!(matches!(
        compacted_manifest.execute(ExactOperation::new(BETA)),
        ExactTerminal::Complete {
            resolution: ExactResolution::Deleted { .. },
            ..
        }
    ));
}

#[test]
fn partial_and_degraded_terminals_keep_the_requested_snapshot_and_missing_identity() {
    let rows = [ExactRow::present(ALPHA, b"value")];
    let present = ExactSegment::new(b"present", &rows);
    assert!(present.is_ok());
    let Some(present) = present.ok() else {
        return;
    };
    let missing = nudox_index_core::ExactSegmentId::from_canonical_bytes(b"missing");
    let segments = [present];
    let missing_segments = [missing];
    let snapshot = IndexSnapshotId::from_canonical_bytes(b"pinned-snapshot");
    let partial = ExactManifest::new(snapshot, &segments, &missing_segments);
    let degraded = ExactManifest::new_degraded(
        snapshot,
        &segments,
        &missing_segments,
        ExactDegradation::StaleRoute,
    );
    assert!(partial.is_ok());
    assert!(degraded.is_ok());
    let Some(partial) = partial.ok() else {
        return;
    };
    let Some(degraded) = degraded.ok() else {
        return;
    };

    assert!(matches!(
        partial.execute(ExactOperation::new(ALPHA)),
        ExactTerminal::Partial {
            snapshot: observed_snapshot,
            resolution: ExactResolution::Present { value, .. },
            missing: observed_missing,
        } if observed_snapshot == snapshot && observed_missing == missing_segments && value == b"value"
    ));
    assert!(matches!(
        degraded.execute(ExactOperation::new(ALPHA)),
        ExactTerminal::Degraded {
            snapshot: observed_snapshot,
            resolution: ExactResolution::Present { value, .. },
            missing: observed_missing,
            reason: ExactDegradation::StaleRoute,
        } if observed_snapshot == snapshot && observed_missing == missing_segments && value == b"value"
    ));
}
