use nudox_index_core::{
    IndexSnapshotId, LexicalDocumentId, LexicalHit, LexicalManifest, LexicalOperation,
    LexicalQueryError, LexicalRow, LexicalScore, LexicalSegment, LexicalTerminal, LexicalTopK,
};

#[test]
fn lexical_terminal_retains_snapshot_and_segment_provenance() {
    let rows = [
        LexicalRow::new(b"needle", 3, LexicalScore::new(7)),
        LexicalRow::new(b"needle", 8, LexicalScore::new(7)),
        LexicalRow::new(b"needle", 9, LexicalScore::new(11)),
    ];
    let segment = LexicalSegment::new(b"lexical-segment", &rows);
    assert!(segment.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    let segment_id = segment.id();
    let segments = [segment];
    let snapshot = IndexSnapshotId::from_canonical_bytes(b"lexical-snapshot");
    let manifest = LexicalManifest::new(snapshot, &segments, &[]);
    let top_k = LexicalTopK::new(2);
    assert!(manifest.is_ok());
    assert!(top_k.is_ok());
    let Some(manifest) = manifest.ok() else {
        return;
    };
    let Some(top_k) = top_k.ok() else {
        return;
    };
    let placeholder = LexicalHit::new(
        b"placeholder",
        LexicalDocumentId::new(0),
        LexicalScore::new(0),
    );
    let mut output = [placeholder; 2];
    let terminal = manifest.execute(0, LexicalOperation::new(b"needle"), top_k, &mut output);
    assert!(terminal.is_ok());
    let Some(terminal) = terminal.ok() else {
        return;
    };

    assert!(matches!(
        terminal,
        LexicalTerminal::Complete {
            snapshot: observed_snapshot,
            segment: observed_segment,
            hits,
        } if observed_snapshot == snapshot
            && observed_segment == segment_id
            && hits == [
                LexicalHit::new(b"needle", LexicalDocumentId::new(9), LexicalScore::new(11)),
                LexicalHit::new(b"needle", LexicalDocumentId::new(3), LexicalScore::new(7)),
            ]
    ));
}

#[test]
fn invalid_segment_selection_preserves_caller_output() {
    let rows = [LexicalRow::new(b"needle", 1, LexicalScore::new(1))];
    let segment = LexicalSegment::new(b"selection-segment", &rows);
    assert!(segment.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    let segments = [segment];
    let manifest = LexicalManifest::new(
        IndexSnapshotId::from_canonical_bytes(b"selection-snapshot"),
        &segments,
        &[],
    );
    let top_k = LexicalTopK::new(1);
    assert!(manifest.is_ok());
    assert!(top_k.is_ok());
    let Some(manifest) = manifest.ok() else {
        return;
    };
    let Some(top_k) = top_k.ok() else {
        return;
    };
    let placeholder = LexicalHit::new(
        b"placeholder",
        LexicalDocumentId::new(0),
        LexicalScore::new(0),
    );
    let mut output = [placeholder; 1];
    let before = output;
    let error = manifest.execute(1, LexicalOperation::new(b"needle"), top_k, &mut output);

    assert_eq!(
        error,
        Err(LexicalQueryError::UnselectedSegment {
            selected: 1,
            present: 1,
        })
    );
    assert_eq!(output, before);
}
