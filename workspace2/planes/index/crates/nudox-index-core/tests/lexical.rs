use nudox_index_core::{
    LexicalDocumentId, LexicalHit, LexicalOperation, LexicalOutputError, LexicalRow, LexicalScore,
    LexicalSegment, LexicalSegmentError, LexicalTopK, MAX_LEXICAL_ROWS,
};

fn score(units: u32) -> LexicalScore {
    LexicalScore::new(units)
}

fn row(term: &'static [u8], document: u32, units: u32) -> LexicalRow<'static> {
    LexicalRow::new(term, document, score(units))
}

fn hit(term: &'static [u8], document: u32, units: u32) -> LexicalHit<'static> {
    LexicalHit::new(term, LexicalDocumentId::new(document), score(units))
}

fn top_k(value: usize) -> LexicalTopK {
    match LexicalTopK::new(value) {
        Ok(top_k) => top_k,
        Err(error) => panic!("valid test TopK was rejected: {error:?}"),
    }
}

#[test]
fn input_permutation_is_rejected_and_ties_rank_by_document() {
    let permuted = [row(b"alpha", 9, 4), row(b"alpha", 3, 4)];
    assert_eq!(
        LexicalSegment::new(b"permuted", &permuted),
        Err(LexicalSegmentError::OutOfOrder {
            index: 1,
            previous: permuted[0],
            observed: permuted[1],
        })
    );

    let rows = [
        row(b"alpha", 3, 4),
        row(b"alpha", 9, 4),
        row(b"alpha", 12, 8),
        row(b"beta", 1, 100),
    ];
    let segment = LexicalSegment::new(b"ties", &rows);
    let segment = match segment {
        Ok(segment) => segment,
        Err(error) => panic!("valid test segment was rejected: {error:?}"),
    };

    let mut output = [hit(b"placeholder", 99, 0); 3];
    assert_eq!(
        segment.rank(LexicalOperation::new(b"alpha"), top_k(3), &mut output),
        Ok(3)
    );
    assert_eq!(
        output,
        [
            hit(b"alpha", 12, 8),
            hit(b"alpha", 3, 4),
            hit(b"alpha", 9, 4)
        ]
    );
}

#[test]
fn hostile_row_bound_precedes_duplicate_order_and_identity_work() {
    let duplicate_rows = [row(b"same", 0, 1); MAX_LEXICAL_ROWS + 1];
    assert_eq!(
        LexicalSegment::new(b"hostile", &duplicate_rows),
        Err(LexicalSegmentError::TooManyRows {
            max: MAX_LEXICAL_ROWS,
            observed: MAX_LEXICAL_ROWS + 1,
        })
    );
}

#[test]
fn invalid_top_k_retains_the_bound_and_precedes_ranking() {
    assert_eq!(
        LexicalTopK::new(MAX_LEXICAL_ROWS + 1),
        Err(nudox_index_core::LexicalTopKError::TooLarge {
            max: MAX_LEXICAL_ROWS,
            observed: MAX_LEXICAL_ROWS + 1,
        })
    );
}

#[test]
fn term_lookup_and_ranked_hits_borrow_original_term_bytes() {
    let alpha = b"alpha";
    let rows = [
        LexicalRow::new(alpha, 1, score(2)),
        LexicalRow::new(alpha, 2, score(1)),
        row(b"beta", 3, 9),
    ];
    let segment = LexicalSegment::new(b"borrowed", &rows);
    let segment = match segment {
        Ok(segment) => segment,
        Err(error) => panic!("valid test segment was rejected: {error:?}"),
    };

    let found = match segment.lookup(LexicalOperation::new(alpha)) {
        Some(found) => found,
        None => panic!("valid test term was not found"),
    };
    assert_eq!(found.len(), 2);
    assert!(core::ptr::eq(found.as_ptr(), rows.as_ptr()));
    assert!(core::ptr::eq(found[0].term().as_ptr(), alpha.as_ptr()));

    let mut output = [hit(b"placeholder", 0, 0); 2];
    assert_eq!(
        segment.rank(LexicalOperation::new(alpha), top_k(2), &mut output),
        Ok(2)
    );
    assert!(core::ptr::eq(output[0].term().as_ptr(), alpha.as_ptr()));
    assert!(core::ptr::eq(output[1].term().as_ptr(), alpha.as_ptr()));
}

#[test]
fn insufficient_output_reports_exact_capacity_and_remains_unchanged() {
    let rows = [
        row(b"alpha", 1, 3),
        row(b"alpha", 2, 2),
        row(b"alpha", 3, 1),
    ];
    let segment = LexicalSegment::new(b"short", &rows);
    let segment = match segment {
        Ok(segment) => segment,
        Err(error) => panic!("valid test segment was rejected: {error:?}"),
    };

    let placeholder = hit(b"placeholder", 77, 11);
    let mut output = [placeholder; 2];
    let before = output;
    assert_eq!(
        segment.rank(LexicalOperation::new(b"alpha"), top_k(3), &mut output),
        Err(LexicalOutputError {
            required: 3,
            available: 2,
        })
    );
    assert_eq!(output, before);
}
