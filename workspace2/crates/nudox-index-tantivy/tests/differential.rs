use nudox_index_core::{
    GenerationId, IndexSnapshot, IndexSnapshotId, LexicalDocumentId, LexicalHit, LexicalManifest,
    LexicalManifestError, LexicalOperation, LexicalRow, LexicalScore, LexicalSegment, LexicalTopK,
};
use nudox_index_tantivy::{
    MAX_TANTIVY_DOCUMENTS, MAX_TANTIVY_QUERY_BYTES, TantivyAdapterError, TantivyHit, TantivyLexical,
};

fn stale_snapshot(byte: u8) -> IndexSnapshotId {
    IndexSnapshotId::from_canonical_bytes(&[byte; 32])
}

fn generation() -> GenerationId {
    GenerationId::from_canonical_bytes(b"published-ir-generation")
}

#[test]
fn real_tantivy_document_set_matches_the_borrowed_lexical_core() {
    let rows = [
        LexicalRow::new(b"rust", 2, LexicalScore::from(1)),
        LexicalRow::new(b"rust", 7, LexicalScore::from(1)),
        LexicalRow::new(b"systems", 2, LexicalScore::from(1)),
    ];
    let segment = LexicalSegment::new(&rows).expect("ordered bounded lexical segment");
    let selected = [segment.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected).expect("bounded snapshot");
    let snapshot_id = snapshot.id;
    let segments = [segment];
    let manifest = LexicalManifest::new(snapshot, &segments, &[]).expect("pinned manifest");
    let adapter = TantivyLexical::build(manifest).expect("real Tantivy projection");

    let placeholder = LexicalHit::new(b"", LexicalDocumentId::from(0), LexicalScore::from(0));
    let mut core_output = [placeholder; 2];
    let top_k = LexicalTopK::new(2).expect("bounded top-k");
    assert_eq!(
        segment.rank(LexicalOperation::new(b"rust"), top_k, &mut core_output),
        Ok(2)
    );
    let mut tantivy_output = [None; 2];
    let terminal = adapter
        .search(snapshot_id, "rust", 2, &mut tantivy_output)
        .expect("Tantivy membership query");
    assert_eq!(terminal.snapshot, snapshot_id);
    assert_eq!(terminal.written, 2);
    assert_eq!(
        tantivy_output,
        [
            Some(TantivyHit { document: 2 }),
            Some(TantivyHit { document: 7 }),
        ]
    );
    assert_eq!(
        core_output.map(|hit| u32::from(hit.document)),
        tantivy_output.map(|hit| hit.map_or(u32::MAX, |hit| hit.document))
    );
}

#[test]
fn tantivy_projects_only_newest_live_term_memberships() {
    let old_rows = [LexicalRow::new(b"alpha", 1, LexicalScore::from(9))];
    let update_rows = [
        LexicalRow::tombstone(b"alpha", 1),
        LexicalRow::new(b"beta", 1, LexicalScore::from(7)),
    ];
    let old = LexicalSegment::new(&old_rows).expect("old segment");
    let update = LexicalSegment::new(&update_rows).expect("term-change segment");
    let selected = [update.id, old.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected).expect("updated snapshot");
    let snapshot_id = snapshot.id;
    let segments = [update, old];
    let manifest = LexicalManifest::new(snapshot, &segments, &[]).expect("updated manifest");
    let adapter = TantivyLexical::build(manifest).expect("updated Tantivy projection");

    let mut old_term_output = [None];
    let old_term = adapter
        .search(snapshot_id, "alpha", 1, &mut old_term_output)
        .expect("deleted term query");
    assert_eq!(old_term.written, 0);

    let mut new_term_output = [None];
    let new_term = adapter
        .search(snapshot_id, "beta", 1, &mut new_term_output)
        .expect("new term query");
    assert_eq!(new_term.written, 1);
    assert_eq!(new_term_output, [Some(TantivyHit { document: 1 })]);
}

#[test]
fn a_different_corpus_cannot_claim_the_same_snapshot() {
    let first_rows = [LexicalRow::new(b"rust", 1, LexicalScore::from(1))];
    let second_rows = [LexicalRow::new(b"systems", 2, LexicalScore::from(1))];
    let first = LexicalSegment::new(&first_rows).expect("first segment");
    let second = LexicalSegment::new(&second_rows).expect("second segment");
    let selected = [first.id];
    let snapshot =
        IndexSnapshot::new(generation(), &[], &selected).expect("content-derived snapshot");
    let wrong_segments = [second];
    assert!(matches!(
        LexicalManifest::new(snapshot, &wrong_segments, &[]),
        Err(LexicalManifestError::PresentNotSelected {
            present_position: 0,
            id,
        }) if id == second.id
    ));

    let first_segments = [first];
    let manifest = LexicalManifest::new(snapshot, &first_segments, &[]).expect("matching manifest");
    let adapter = TantivyLexical::build(manifest).expect("bound projection");
    let mut output = [None];
    let terminal = adapter
        .search(snapshot.id, "rust", 1, &mut output)
        .expect("matching query");
    assert_eq!(terminal.written, 1);
    assert_eq!(output, [Some(TantivyHit { document: 1 })]);
}

#[test]
fn partial_manifest_cannot_publish_a_complete_tantivy_claim() {
    let present_rows = [LexicalRow::new(b"rust", 1, LexicalScore::from(1))];
    let missing_rows = [LexicalRow::new(b"systems", 2, LexicalScore::from(1))];
    let present = LexicalSegment::new(&present_rows).expect("present segment");
    let unavailable = LexicalSegment::new(&missing_rows).expect("unavailable segment identity");
    let selected = [present.id, unavailable.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected).expect("partial snapshot");
    let segments = [present];
    let missing = [unavailable.id];
    let manifest =
        LexicalManifest::new(snapshot, &segments, &missing).expect("validated partial manifest");
    assert!(matches!(
        TantivyLexical::build(manifest),
        Err(TantivyAdapterError::IncompleteManifest {
            missing: 1,
            degraded: false,
        })
    ));
}

#[test]
fn wrong_snapshot_and_short_output_fail_before_mutation() {
    let rows = [LexicalRow::new(b"rust", 1, LexicalScore::from(1))];
    let segment = LexicalSegment::new(&rows).expect("segment");
    let selected = [segment.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected).expect("snapshot");
    let pinned = snapshot.id;
    let segments = [segment];
    let manifest = LexicalManifest::new(snapshot, &segments, &[]).expect("manifest");
    let adapter = TantivyLexical::build(manifest).expect("projection");
    let stale = stale_snapshot(3);
    let sentinel = Some(TantivyHit { document: 99 });
    let mut output = [sentinel];
    assert!(matches!(
        adapter.search(stale, "rust", 1, &mut output),
        Err(TantivyAdapterError::WrongSnapshot { expected, observed })
            if expected == pinned && observed == stale
    ));
    assert_eq!(output, [sentinel]);
    assert!(matches!(
        adapter.search(pinned, "rust", 2, &mut output),
        Err(TantivyAdapterError::InsufficientOutput {
            required: 2,
            available: 1,
        })
    ));
    assert_eq!(output, [sentinel]);
}

#[test]
fn hostile_corpus_and_query_bounds_precede_backend_work() {
    let first_rows = (0..MAX_TANTIVY_DOCUMENTS)
        .map(|document| LexicalRow::new(b"rust", document as u32, LexicalScore::from(1)))
        .collect::<Vec<_>>();
    let second_rows = [LexicalRow::new(
        b"rust",
        MAX_TANTIVY_DOCUMENTS as u32,
        LexicalScore::from(1),
    )];
    let first = LexicalSegment::new(&first_rows).expect("maximum-sized core segment");
    let second = LexicalSegment::new(&second_rows).expect("one additional row");
    let selected = [first.id, second.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected).expect("snapshot");
    let segments = [first, second];
    let manifest = LexicalManifest::new(snapshot, &segments, &[]).expect("manifest");
    assert!(matches!(
        TantivyLexical::build(manifest),
        Err(TantivyAdapterError::DocumentLimit { limit, observed })
            if limit == MAX_TANTIVY_DOCUMENTS && observed == MAX_TANTIVY_DOCUMENTS + 1
    ));

    let rows = [LexicalRow::new(b"rust", 7, LexicalScore::from(1))];
    let segment = LexicalSegment::new(&rows).expect("small segment");
    let selected = [segment.id];
    let snapshot = IndexSnapshot::new(generation(), &[], &selected).expect("small snapshot");
    let snapshot_id = snapshot.id;
    let segments = [segment];
    let manifest = LexicalManifest::new(snapshot, &segments, &[]).expect("small manifest");
    let adapter = TantivyLexical::build(manifest).expect("small projection");
    let query = "x".repeat(MAX_TANTIVY_QUERY_BYTES + 1);
    let mut output = [Some(TantivyHit { document: 99 })];
    let before = output;
    assert!(matches!(
        adapter.search(snapshot_id, &query, 1, &mut output),
        Err(TantivyAdapterError::QueryBytesLimit { limit, observed })
            if limit == MAX_TANTIVY_QUERY_BYTES && observed == MAX_TANTIVY_QUERY_BYTES + 1
    ));
    assert_eq!(output, before);
}
