use allocation_counter::{AllocationInfo, measure};
use nudox_id::{Domain, IndexExactSegmentDomain, IndexLexicalSegmentDomain, IndexSnapshotDomain};
use nudox_index_vocab::{
    Exact, IndexSegmentId, IndexSnapshotId, Lexical, SegmentFamily, UnknownSegmentFamily,
};

#[test]
fn local_and_remote_canonical_bytes_produce_the_same_typed_ids() {
    let local = [7_u8; 24];
    let remote = [7_u8; 24];
    let local_snapshot = IndexSnapshotId::from_canonical_bytes(&local);
    let remote_snapshot = IndexSnapshotId::from_canonical_bytes(&remote);
    let local_exact = IndexSegmentId::<Exact>::from_canonical_bytes(&local);
    let remote_exact = IndexSegmentId::<Exact>::from_canonical_bytes(&remote);
    let local_lexical = IndexSegmentId::<Lexical>::from_canonical_bytes(&local);
    let remote_lexical = IndexSegmentId::<Lexical>::from_canonical_bytes(&remote);
    assert_eq!(local_snapshot, remote_snapshot);
    assert_eq!(local_exact, remote_exact);
    assert_eq!(local_lexical, remote_lexical);
    assert_ne!(local_exact.as_ref(), local_lexical.as_ref());
}

#[test]
fn unknown_segment_family_retains_the_raw_value() {
    for code in 1..=5 {
        assert_eq!(u8::from(SegmentFamily::try_from(code).unwrap()), code);
    }
    for code in [0, 6, 255] {
        assert_eq!(
            SegmentFamily::try_from(code),
            Err(UnknownSegmentFamily(code))
        );
    }
}

#[test]
fn ids_and_markers_have_the_declared_layout() {
    assert_eq!(core::mem::size_of::<IndexSnapshotId>(), 32);
    assert_eq!(core::mem::align_of::<IndexSnapshotId>(), 1);
    assert_eq!(core::mem::size_of::<IndexSegmentId<Exact>>(), 32);
    assert_eq!(core::mem::align_of::<IndexSegmentId<Exact>>(), 1);
    assert_eq!(core::mem::size_of::<Exact>(), 0);
    assert_eq!(core::mem::align_of::<Exact>(), 1);
    assert_eq!(core::mem::size_of::<Lexical>(), 0);
    assert_eq!(core::mem::align_of::<Lexical>(), 1);
}

#[test]
fn canonical_construction_allocates_nothing_after_warmup() {
    let bytes = [3_u8; 24];
    let _ = IndexSnapshotId::from_canonical_bytes(&bytes);
    let _ = IndexSegmentId::<Exact>::from_canonical_bytes(&bytes);
    let _ = IndexSegmentId::<Lexical>::from_canonical_bytes(&bytes);
    let mut result = None;
    let allocations = measure(|| {
        result = Some((
            IndexSnapshotId::from_canonical_bytes(&bytes),
            IndexSegmentId::<Exact>::from_canonical_bytes(&bytes),
            IndexSegmentId::<Lexical>::from_canonical_bytes(&bytes),
        ));
    });
    assert!(result.is_some());
    assert_eq!(
        allocations,
        AllocationInfo {
            count_total: 0,
            count_current: 0,
            count_max: 0,
            bytes_total: 0,
            bytes_current: 0,
            bytes_max: 0
        }
    );
}

#[test]
fn registry_labels_are_distinct() {
    let labels = [
        IndexSnapshotDomain::TAG,
        IndexExactSegmentDomain::TAG,
        IndexLexicalSegmentDomain::TAG,
    ];
    for (index, label) in labels.iter().enumerate() {
        for other in labels.iter().skip(index + 1) {
            assert_ne!(label, other);
        }
    }
}
