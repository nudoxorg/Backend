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

#[test]
fn input_permutation_is_rejected_and_ties_rank_by_document() {
    let permuted = [row(b"alpha", 9, 4), row(b"alpha", 3, 4)];
    assert_eq!(
        LexicalSegment::new(&permuted),
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
    let segment = LexicalSegment::new(&rows);
    let top_k = LexicalTopK::new(3);
    assert!(segment.is_ok());
    assert!(top_k.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    let Some(top_k) = top_k.ok() else {
        return;
    };

    let mut output = [hit(b"placeholder", 99, 0); 3];
    assert_eq!(
        segment.rank(LexicalOperation::new(b"alpha"), top_k, &mut output),
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
        LexicalSegment::new(&duplicate_rows),
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
    let segment = LexicalSegment::new(&rows);
    let top_k = LexicalTopK::new(2);
    assert!(segment.is_ok());
    assert!(top_k.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    let Some(top_k) = top_k.ok() else {
        return;
    };

    let found = segment.lookup(LexicalOperation::new(alpha));
    assert!(matches!(
        found,
        Some(found)
            if found.len() == 2
                && core::ptr::eq(found.as_ptr(), rows.as_ptr())
                && core::ptr::eq(found[0].term().as_ptr(), alpha.as_ptr())
    ));

    let mut output = [hit(b"placeholder", 0, 0); 2];
    assert_eq!(
        segment.rank(LexicalOperation::new(alpha), top_k, &mut output),
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
    let segment = LexicalSegment::new(&rows);
    let top_k = LexicalTopK::new(3);
    assert!(segment.is_ok());
    assert!(top_k.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    let Some(top_k) = top_k.ok() else {
        return;
    };

    let placeholder = hit(b"placeholder", 77, 11);
    let mut output = [placeholder; 2];
    let before = output;
    assert_eq!(
        segment.rank(LexicalOperation::new(b"alpha"), top_k, &mut output,),
        Err(LexicalOutputError {
            required: 3,
            available: 2,
        })
    );
    assert_eq!(output, before);
}
