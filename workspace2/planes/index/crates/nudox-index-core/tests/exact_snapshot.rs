use nudox_index_core::{
    ExactDegradation, ExactManifest, ExactOperation, ExactResolution, ExactRow, ExactSegment,
    ExactTerminal, IndexSnapshot,
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
    let original = ExactSegment::new(&original_rows);
    let update = ExactSegment::new(&update_rows);
    let compacted = ExactSegment::new(&compacted_rows);
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
    let incremental_ids = [update.id(), original.id()];
    let compacted_ids = [compacted.id()];
    let incremental_snapshot = IndexSnapshot::new(&incremental_ids, &[]);
    let compacted_snapshot = IndexSnapshot::new(&compacted_ids, &[]);
    assert!(incremental_snapshot.is_ok() && compacted_snapshot.is_ok());
    let (Some(incremental_snapshot), Some(compacted_snapshot)) =
        (incremental_snapshot.ok(), compacted_snapshot.ok())
    else {
        return;
    };
    let incremental = ExactManifest::new(incremental_snapshot, &incremental_segments, &[]);
    let compacted_manifest = ExactManifest::new(compacted_snapshot, &compacted_segments, &[]);
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
    let present = ExactSegment::new(&rows);
    assert!(present.is_ok());
    let Some(present) = present.ok() else {
        return;
    };
    let missing = nudox_index_core::ExactSegmentId::from_canonical_bytes(b"missing");
    let segments = [present];
    let missing_segments = [missing];
    let selected = [present.id(), missing];
    let snapshot = IndexSnapshot::new(&selected, &[]);
    assert!(snapshot.is_ok());
    let Some(snapshot) = snapshot.ok() else {
        return;
    };
    let snapshot_id = snapshot.id();
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
        } if observed_snapshot == snapshot_id && observed_missing == missing_segments && value == b"value"
    ));
    assert!(matches!(
        degraded.execute(ExactOperation::new(ALPHA)),
        ExactTerminal::Degraded {
            snapshot: observed_snapshot,
            resolution: ExactResolution::Present { value, .. },
            missing: observed_missing,
            reason: ExactDegradation::StaleRoute,
        } if observed_snapshot == snapshot_id && observed_missing == missing_segments && value == b"value"
    ));
}

#[test]
fn divergent_rows_cannot_enter_the_same_snapshot_authority() {
    let first_rows = [ExactRow::present(ALPHA, b"v1")];
    let second_rows = [ExactRow::present(ALPHA, b"v2")];
    let first = ExactSegment::new(&first_rows);
    let second = ExactSegment::new(&second_rows);
    assert!(first.is_ok() && second.is_ok());
    let (Some(first), Some(second)) = (first.ok(), second.ok()) else {
        return;
    };
    let first_ids = [first.id()];
    let second_ids = [second.id()];
    let first_snapshot = IndexSnapshot::new(&first_ids, &[]);
    let second_snapshot = IndexSnapshot::new(&second_ids, &[]);
    assert!(first_snapshot.is_ok() && second_snapshot.is_ok());
    let (Some(first_snapshot), Some(second_snapshot)) = (first_snapshot.ok(), second_snapshot.ok())
    else {
        return;
    };
    assert_ne!(first_snapshot.id(), second_snapshot.id());

    let wrong_segments = [second];
    assert!(matches!(
        ExactManifest::new(first_snapshot, &wrong_segments, &[]),
        Err(nudox_index_core::ExactManifestError::PresentNotSelected { id, .. })
            if id == second.id()
    ));
}
