use nudox_index_core::{ExactOperation, ExactRow, ExactSegment, ExactSegmentError};

const ALPHA: &[u8] = b"alpha";
const BETA: &[u8] = b"beta";
const DELETED: &[u8] = b"deleted";

#[test]
fn immutable_exact_segment_borrows_sorted_rows_and_keeps_tombstones() {
    let rows = [
        ExactRow::present(ALPHA, b"v1"),
        ExactRow::present(BETA, b"v2"),
        ExactRow::tombstone(DELETED),
    ];
    let segment = ExactSegment::new(b"segment-v1", &rows);
    assert!(segment.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };

    let found = segment.lookup(ExactOperation::new(BETA));
    assert_eq!(found.map(|row| row.key()), Some(BETA));
    assert_eq!(
        found.and_then(|row| row.value_bytes()),
        Some(b"v2".as_slice())
    );

    let tombstone = segment.lookup(ExactOperation::new(DELETED));
    assert!(tombstone.is_some());
    assert!(tombstone.is_some_and(|row| row.is_tombstone()));
    assert_eq!(tombstone.and_then(|row| row.value_bytes()), None);

    let missing = segment.lookup(ExactOperation::new(b"missing"));
    assert_eq!(missing, None);
    assert!(core::ptr::eq(segment.rows().as_ptr(), rows.as_ptr()));
}

#[test]
fn exact_segment_rejects_duplicate_and_out_of_order_keys_before_publication() {
    let duplicate_rows = [
        ExactRow::present(b"same", b"first"),
        ExactRow::present(b"same", b"second"),
    ];
    assert_eq!(
        ExactSegment::new(b"duplicate-segment", &duplicate_rows),
        Err(ExactSegmentError::DuplicateKey {
            index: 1,
            key: b"same",
        })
    );

    let out_of_order_rows = [ExactRow::present(b"zulu", b"z"), ExactRow::tombstone(ALPHA)];
    assert_eq!(
        ExactSegment::new(b"out-of-order-segment", &out_of_order_rows),
        Err(ExactSegmentError::OutOfOrder {
            index: 1,
            previous: b"zulu",
            observed: ALPHA,
        })
    );
}

#[test]
fn exact_segment_rejects_the_first_row_beyond_the_bounded_capacity() {
    let rows = [ExactRow::tombstone(b"row"); 257];
    assert_eq!(
        ExactSegment::new(b"too-many-segment", &rows),
        Err(ExactSegmentError::TooManyRows {
            max: 256,
            observed: 257,
        })
    );
}

#[test]
fn segment_identity_is_derived_from_the_borrowed_segment_bytes() {
    let rows = [ExactRow::present(ALPHA, b"v1")];
    let segment = ExactSegment::new(b"segment-v1", &rows);
    assert!(segment.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };
    assert_eq!(
        segment.id(),
        nudox_index_vocab::ExactSegmentId::from_canonical_bytes(b"segment-v1")
    );
}
