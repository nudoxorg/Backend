//! Exercises the `server-index-core` tests lexical-snapshot contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use compiler_ir_vocabulary::EntityId;
use heart_identity::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};
use server_index_core::{
    EntityDocumentId, IndexSnapshot, LexicalManifest, LexicalOperation, LexicalOutputError,
    LexicalQueryError, LexicalRow, LexicalScore, LexicalSegment, LexicalSnapshotHit,
    LexicalTerminal, LexicalTopK,
};

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            b"lexical-snapshot-test-fragment",
        ),
        entity: EntityId::new(entity),
    }
}

fn generation() -> GenerationId {
    GenerationId::from_canonical_bytes(b"published-ir-generation")
}

fn placeholder(segment: server_index_core::LexicalSegmentId) -> LexicalSnapshotHit<'static> {
    LexicalSnapshotHit::new(segment, b"placeholder", document(0), LexicalScore::from(0))
}

#[test]
fn lexical_terminal_retains_derived_snapshot_and_segment_provenance() {
    let rows = [
        LexicalRow::new(b"needle", document(3), LexicalScore::from(7)),
        LexicalRow::new(b"needle", document(8), LexicalScore::from(7)),
        LexicalRow::new(b"needle", document(9), LexicalScore::from(11)),
    ];
    let segment = LexicalSegment::new(&rows);
    assert!(segment.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    let segment_id = segment.id;
    let segments = [segment];
    let lexical_ids = [segment_id];
    let snapshot = IndexSnapshot::new(generation(), &[], &lexical_ids);
    assert!(snapshot.is_ok());
    let Some(snapshot) = snapshot.ok() else {
        return;
    };
    let snapshot_id = snapshot.id;
    let manifest = LexicalManifest::new(snapshot, &segments, &[]);
    let top_k = LexicalTopK::new(2);
    assert!(manifest.is_ok() && top_k.is_ok());
    let (Some(manifest), Some(top_k)) = (manifest.ok(), top_k.ok()) else {
        return;
    };
    let mut scratch = [None; 3];
    let mut output = [placeholder(segment_id); 2];
    let terminal = manifest.execute(
        LexicalOperation::new(b"needle"),
        top_k,
        &mut scratch,
        &mut output,
    );
    assert!(terminal.is_ok());
    let Some(terminal) = terminal.ok() else {
        return;
    };

    assert!(matches!(
        terminal,
        LexicalTerminal::Complete {
            snapshot: observed_snapshot,
            hits,
        } if observed_snapshot == snapshot_id
            && hits == [
                LexicalSnapshotHit::new(
                    segment_id,
                    b"needle",
                    document(9),
                    LexicalScore::from(11),
                ),
                LexicalSnapshotHit::new(
                    segment_id,
                    b"needle",
                    document(3),
                    LexicalScore::from(7),
                ),
            ]
    ));
}

#[test]
fn manifest_wide_updates_and_compaction_keep_the_same_global_ranking() {
    let old_rows = [
        LexicalRow::new(b"needle", document(1), LexicalScore::from(10)),
        LexicalRow::new(b"needle", document(2), LexicalScore::from(5)),
    ];
    let update_rows = [
        LexicalRow::new(b"needle", document(1), LexicalScore::from(1)),
        LexicalRow::new(b"needle", document(3), LexicalScore::from(7)),
    ];
    let compacted_rows = [
        LexicalRow::new(b"needle", document(1), LexicalScore::from(1)),
        LexicalRow::new(b"needle", document(2), LexicalScore::from(5)),
        LexicalRow::new(b"needle", document(3), LexicalScore::from(7)),
    ];
    let old = LexicalSegment::new(&old_rows);
    let update = LexicalSegment::new(&update_rows);
    let compacted = LexicalSegment::new(&compacted_rows);
    assert!(old.is_ok() && update.is_ok() && compacted.is_ok());
    let (Some(old), Some(update), Some(compacted)) = (old.ok(), update.ok(), compacted.ok()) else {
        return;
    };

    let incremental_segments = [update, old];
    let incremental_ids = [update.id, old.id];
    let compacted_segments = [compacted];
    let compacted_ids = [compacted.id];
    let incremental_snapshot = IndexSnapshot::new(generation(), &[], &incremental_ids);
    let compacted_snapshot = IndexSnapshot::new(generation(), &[], &compacted_ids);
    assert!(incremental_snapshot.is_ok() && compacted_snapshot.is_ok());
    let (Some(incremental_snapshot), Some(compacted_snapshot)) =
        (incremental_snapshot.ok(), compacted_snapshot.ok())
    else {
        return;
    };
    let incremental = LexicalManifest::new(incremental_snapshot, &incremental_segments, &[]);
    let compacted_manifest = LexicalManifest::new(compacted_snapshot, &compacted_segments, &[]);
    let top_k = LexicalTopK::new(3);
    assert!(incremental.is_ok() && compacted_manifest.is_ok() && top_k.is_ok());
    let (Some(incremental), Some(compacted_manifest), Some(top_k)) =
        (incremental.ok(), compacted_manifest.ok(), top_k.ok())
    else {
        return;
    };
    let mut incremental_scratch = [None; 4];
    let mut compacted_scratch = [None; 3];
    let mut incremental_output = [placeholder(update.id); 3];
    let mut compacted_output = [placeholder(compacted.id); 3];
    assert!(
        incremental
            .execute(
                LexicalOperation::new(b"needle"),
                top_k,
                &mut incremental_scratch,
                &mut incremental_output,
            )
            .is_ok()
    );
    assert!(
        compacted_manifest
            .execute(
                LexicalOperation::new(b"needle"),
                top_k,
                &mut compacted_scratch,
                &mut compacted_output,
            )
            .is_ok()
    );
    assert_eq!(
        incremental_output.map(|hit| (hit.document, hit.score)),
        compacted_output.map(|hit| (hit.document, hit.score))
    );
    assert_eq!(
        incremental_output.map(|hit| hit.document.entity.raw),
        [3, 2, 1]
    );
}

#[test]
fn term_change_tombstone_hides_old_membership_and_publishes_new_membership() {
    let old_rows = [LexicalRow::new(
        b"alpha",
        document(1),
        LexicalScore::from(9),
    )];
    let update_rows = [
        LexicalRow::tombstone(b"alpha", document(1)),
        LexicalRow::new(b"beta", document(1), LexicalScore::from(7)),
    ];
    let old = LexicalSegment::new(&old_rows);
    let update = LexicalSegment::new(&update_rows);
    assert!(old.is_ok() && update.is_ok());
    let (Some(old), Some(update)) = (old.ok(), update.ok()) else {
        return;
    };
    let segments = [update, old];
    let selected = [update.id, old.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected);
    assert!(snapshot.is_ok());
    let Some(snapshot) = snapshot.ok() else {
        return;
    };
    let manifest = LexicalManifest::new(snapshot, &segments, &[]);
    let top_k = LexicalTopK::new(1);
    assert!(manifest.is_ok() && top_k.is_ok());
    let (Some(manifest), Some(top_k)) = (manifest.ok(), top_k.ok()) else {
        return;
    };

    let mut old_term_scratch = [None; 2];
    let mut old_term_output = [placeholder(update.id)];
    let old_term = manifest.execute(
        LexicalOperation::new(b"alpha"),
        top_k,
        &mut old_term_scratch,
        &mut old_term_output,
    );
    assert!(matches!(
        old_term,
        Ok(LexicalTerminal::Complete { hits: [], .. })
    ));

    let mut new_term_scratch = [None; 1];
    let mut new_term_output = [placeholder(update.id)];
    let new_term = manifest.execute(
        LexicalOperation::new(b"beta"),
        top_k,
        &mut new_term_scratch,
        &mut new_term_output,
    );
    assert!(matches!(
        new_term,
        Ok(LexicalTerminal::Complete { hits: [hit], .. })
            if hit.document == document(1)
                && hit.score == LexicalScore::from(7)
    ));
}

#[test]
fn insufficient_dedup_scratch_precedes_output_mutation() {
    let rows = [LexicalRow::new(
        b"needle",
        document(1),
        LexicalScore::from(1),
    )];
    let segment = LexicalSegment::new(&rows);
    assert!(segment.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    let segments = [segment];
    let lexical_ids = [segment.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &lexical_ids);
    assert!(snapshot.is_ok());
    let Some(snapshot) = snapshot.ok() else {
        return;
    };
    let manifest = LexicalManifest::new(snapshot, &segments, &[]);
    let top_k = LexicalTopK::new(1);
    assert!(manifest.is_ok() && top_k.is_ok());
    let (Some(manifest), Some(top_k)) = (manifest.ok(), top_k.ok()) else {
        return;
    };
    let mut output = [placeholder(segment.id)];
    let before = output;
    let error = manifest.execute(
        LexicalOperation::new(b"needle"),
        top_k,
        &mut [],
        &mut output,
    );

    assert_eq!(
        error,
        Err(LexicalQueryError::ScratchCapacity {
            required: 1,
            available: 0,
        })
    );
    assert_eq!(output, before);
}

#[test]
fn insufficient_output_precedes_scratch_and_output_mutation() {
    let rows = [
        LexicalRow::new(b"needle", document(1), LexicalScore::from(2)),
        LexicalRow::new(b"needle", document(2), LexicalScore::from(1)),
    ];
    let segment = LexicalSegment::new(&rows);
    assert!(segment.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    let segments = [segment];
    let lexical_ids = [segment.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &lexical_ids);
    assert!(snapshot.is_ok());
    let Some(snapshot) = snapshot.ok() else {
        return;
    };
    let manifest = LexicalManifest::new(snapshot, &segments, &[]);
    let top_k = LexicalTopK::new(2);
    assert!(manifest.is_ok() && top_k.is_ok());
    let (Some(manifest), Some(top_k)) = (manifest.ok(), top_k.ok()) else {
        return;
    };
    let mut scratch = [Some(document(91)), Some(document(92))];
    let before_scratch = scratch;
    let mut output = [placeholder(segment.id)];
    let before_output = output;
    let error = manifest.execute(
        LexicalOperation::new(b"needle"),
        top_k,
        &mut scratch,
        &mut output,
    );

    assert_eq!(
        error,
        Err(LexicalQueryError::OutputCapacity(LexicalOutputError {
            required: 2,
            available: 1,
        }))
    );
    assert_eq!(scratch, before_scratch);
    assert_eq!(output, before_output);
}

#[test]
fn lexical_snapshot_rejects_reversed_update_precedence() {
    let old_rows = [LexicalRow::new(
        b"needle",
        document(1),
        LexicalScore::from(9),
    )];
    let update_rows = [LexicalRow::new(
        b"needle",
        document(1),
        LexicalScore::from(2),
    )];
    let old = LexicalSegment::new(&old_rows);
    let update = LexicalSegment::new(&update_rows);
    assert!(old.is_ok() && update.is_ok());
    let (Some(old), Some(update)) = (old.ok(), update.ok()) else {
        return;
    };
    let selected = [update.id, old.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected);
    assert!(snapshot.is_ok());
    let Some(snapshot) = snapshot.ok() else {
        return;
    };
    let reversed = [old, update];
    assert!(matches!(
        LexicalManifest::new(snapshot, &reversed, &[]),
        Err(server_index_core::LexicalManifestError::PresentOrderMismatch {
            present_position: 1,
            preceding_selected_position: 1,
            selected_position: 0,
            id,
        }) if id == update.id
    ));
}
