use nudox_index_core::{
    IndexSnapshotId, LexicalDocumentId, LexicalHit, LexicalOperation, LexicalRow, LexicalScore,
    LexicalSegment, LexicalTopK,
};
use nudox_index_tantivy::{TantivyAdapterError, TantivyDocumentInput, TantivyHit, TantivyLexical};

fn snapshot(byte: u8) -> IndexSnapshotId {
    IndexSnapshotId::from_canonical_bytes(&[byte; 32])
}

#[test]
fn real_tantivy_document_set_matches_the_borrowed_lexical_core() {
    let snapshot = snapshot(1);
    let rows = [
        LexicalRow::new(b"rust", 2, LexicalScore::new(1)),
        LexicalRow::new(b"rust", 7, LexicalScore::new(1)),
        LexicalRow::new(b"systems", 2, LexicalScore::new(1)),
    ];
    let segment = LexicalSegment::new(&rows);
    assert!(segment.is_ok());
    let Ok(segment) = segment else {
        return;
    };
    let documents = [
        TantivyDocumentInput {
            document: 7,
            text: "rust indexing",
        },
        TantivyDocumentInput {
            document: 2,
            text: "rust systems",
        },
    ];
    let adapter = TantivyLexical::build(snapshot, &documents);
    assert!(adapter.is_ok());
    let Ok(adapter) = adapter else {
        return;
    };
    let placeholder = LexicalHit::new(b"", LexicalDocumentId::new(0), LexicalScore::new(0));
    let mut core_output = [placeholder; 2];
    let top_k = LexicalTopK::new(2);
    assert!(top_k.is_ok());
    let Ok(top_k) = top_k else {
        return;
    };
    assert_eq!(
        segment.rank(LexicalOperation::new(b"rust"), top_k, &mut core_output),
        Ok(2)
    );
    let mut tantivy_output = [None; 2];
    let terminal = adapter.search(snapshot, "rust", 2, &mut tantivy_output);
    assert!(terminal.is_ok());
    let Ok(terminal) = terminal else {
        return;
    };
    assert_eq!(terminal.snapshot(), snapshot);
    assert_eq!(terminal.written(), 2);
    assert_eq!(
        tantivy_output,
        [
            Some(TantivyHit { document: 2 }),
            Some(TantivyHit { document: 7 }),
        ]
    );
    assert_eq!(
        core_output.map(|hit| hit.document().ordinal()),
        tantivy_output.map(|hit| hit.map_or(u32::MAX, |hit| hit.document))
    );
}

#[test]
fn wrong_snapshot_and_short_output_fail_before_mutation() {
    let pinned = snapshot(2);
    let stale = snapshot(3);
    let adapter = TantivyLexical::build(
        pinned,
        &[TantivyDocumentInput {
            document: 1,
            text: "rust",
        }],
    );
    assert!(adapter.is_ok());
    let Ok(adapter) = adapter else {
        return;
    };
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
