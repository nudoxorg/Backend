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
    let segment = ExactSegment::new(&rows);
    assert!(segment.is_ok());
    let Some(segment) = segment.ok() else {
        return;
    };

    let found = segment.lookup(ExactOperation::new(BETA));
    assert_eq!(found.map(|row| row.key), Some(BETA));
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
    assert!(core::ptr::eq(segment.rows.as_ptr(), rows.as_ptr()));
}

#[test]
fn exact_segment_rejects_duplicate_and_out_of_order_keys_before_publication() {
    let duplicate_rows = [
        ExactRow::present(b"same", b"first"),
        ExactRow::present(b"same", b"second"),
    ];
    assert_eq!(
        ExactSegment::new(&duplicate_rows),
        Err(ExactSegmentError::DuplicateKey {
            index: 1,
            key: b"same",
        })
    );

    let out_of_order_rows = [ExactRow::present(b"zulu", b"z"), ExactRow::tombstone(ALPHA)];
    assert_eq!(
        ExactSegment::new(&out_of_order_rows),
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
        ExactSegment::new(&rows),
        Err(ExactSegmentError::TooManyRows {
            max: 256,
            observed: 257,
        })
    );
}

#[test]
fn segment_identity_is_derived_from_every_queried_row() {
    let first_rows = [ExactRow::present(ALPHA, b"v1")];
    let equal_rows = [ExactRow::present(ALPHA, b"v1")];
    let distinct_rows = [ExactRow::present(ALPHA, b"v2")];
    let first = ExactSegment::new(&first_rows);
    let equal = ExactSegment::new(&equal_rows);
    let distinct = ExactSegment::new(&distinct_rows);
    assert!(first.is_ok() && equal.is_ok() && distinct.is_ok());
    let (Some(first), Some(equal), Some(distinct)) = (first.ok(), equal.ok(), distinct.ok()) else {
        return;
    };
    assert_eq!(first.id, equal.id);
    assert_ne!(first.id, distinct.id);
}
