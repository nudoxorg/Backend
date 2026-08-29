use allocation_counter::{AllocationInfo, measure};
use core::{
    mem::{align_of, size_of},
    ops::Deref,
};
use nudox_index_vocab::{
    Exact, IndexSegmentId, IndexSnapshotId, Lexical, SegmentFamily, UnknownSegmentFamily,
};
use std::hint::black_box;

const CANONICAL_BYTES: usize = 24;
const FIRST_UNASSIGNED_CODE: u8 = SegmentFamily::Vector as u8 + 1;
const LOCAL_BYTE: u8 = 7;
const MUTATED_BYTE: u8 = 8;
const MUTATED_POSITION: usize = CANONICAL_BYTES - 1;
const WARMUP_BYTE: u8 = 3;

#[test]
fn local_and_remote_canonical_bytes_produce_the_same_typed_ids() {
    let local = [LOCAL_BYTE; CANONICAL_BYTES];
    let remote = [LOCAL_BYTE; CANONICAL_BYTES];
    assert_ne!(local.as_ptr(), remote.as_ptr());
    let mut different = local;
    different[MUTATED_POSITION] = MUTATED_BYTE;
    let local_snapshot = IndexSnapshotId::from_canonical_bytes(&local);
    let remote_snapshot = IndexSnapshotId::from_canonical_bytes(&remote);
    let local_exact = IndexSegmentId::<Exact>::from_canonical_bytes(&local);
    let remote_exact = IndexSegmentId::<Exact>::from_canonical_bytes(&remote);
    let local_lexical = IndexSegmentId::<Lexical>::from_canonical_bytes(&local);
    let remote_lexical = IndexSegmentId::<Lexical>::from_canonical_bytes(&remote);
    let different_snapshot = IndexSnapshotId::from_canonical_bytes(&different);
    let different_exact = IndexSegmentId::<Exact>::from_canonical_bytes(&different);
    let different_lexical = IndexSegmentId::<Lexical>::from_canonical_bytes(&different);
    assert_eq!(local_snapshot, remote_snapshot);
    assert_eq!(local_exact, remote_exact);
    assert_eq!(local_lexical, remote_lexical);
    assert_ne!(local_exact.as_ref(), local_lexical.as_ref());
    assert_ne!(local_snapshot, different_snapshot);
    assert_ne!(local_exact, different_exact);
    assert_ne!(local_lexical, different_lexical);
}

#[test]
fn unknown_segment_family_retains_the_raw_value() {
    for (code, family) in [
        (1, SegmentFamily::Exact),
        (2, SegmentFamily::Lexical),
        (3, SegmentFamily::Relation),
        (4, SegmentFamily::Usage),
        (5, SegmentFamily::Vector),
    ] {
        assert_eq!(SegmentFamily::try_from(code), Ok(family));
        assert_eq!(u8::from(family), code);
    }
    for code in [u8::MIN, FIRST_UNASSIGNED_CODE, u8::MAX] {
        assert_eq!(
            SegmentFamily::try_from(code),
            Err(UnknownSegmentFamily { code })
        );
    }
}

#[test]
fn ids_and_markers_have_the_declared_layout() {
    type SnapshotDigest = <IndexSnapshotId as Deref>::Target;
    type ExactDigest = <IndexSegmentId<Exact> as Deref>::Target;
    assert_eq!(size_of::<IndexSnapshotId>(), size_of::<SnapshotDigest>());
    assert_eq!(align_of::<IndexSnapshotId>(), align_of::<SnapshotDigest>());
    assert_eq!(size_of::<IndexSegmentId<Exact>>(), size_of::<ExactDigest>());
    assert_eq!(
        align_of::<IndexSegmentId<Exact>>(),
        align_of::<ExactDigest>()
    );
    assert_eq!(size_of::<Exact>(), size_of::<()>());
    assert_eq!(align_of::<Exact>(), align_of::<()>());
    assert_eq!(size_of::<Lexical>(), size_of::<()>());
    assert_eq!(align_of::<Lexical>(), align_of::<()>());
}

#[test]
fn canonical_construction_allocates_nothing_after_warmup() {
    let bytes = [WARMUP_BYTE; CANONICAL_BYTES];
    black_box(IndexSnapshotId::from_canonical_bytes(&bytes));
    black_box(IndexSegmentId::<Exact>::from_canonical_bytes(&bytes));
    black_box(IndexSegmentId::<Lexical>::from_canonical_bytes(&bytes));
    let allocations = measure(|| {
        black_box(IndexSnapshotId::from_canonical_bytes(&bytes));
        black_box(IndexSegmentId::<Exact>::from_canonical_bytes(&bytes));
        black_box(IndexSegmentId::<Lexical>::from_canonical_bytes(&bytes));
    });
    assert_eq!(allocations, AllocationInfo::default());
}
