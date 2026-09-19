//! Reader lease cancellation tests.

use super::*;

#[test]
fn reader_leases_release_retained_state_on_drop_and_cancel() {
    let scope_root = scope(4);
    let coverage = complete(4);
    let read = ScopedRead::negative(FacetKind::Type, scope_root, b"missing".to_vec());
    let observation = ScopedReadObservation::negative(read.clone(), coverage).expect("negative");
    let store = RetainedReaderStore::new();
    let lease = store.lease(work(9), observation).expect("lease");
    assert_eq!(store.with(RetainedReaders::registration_count), 1);
    assert!(lease.release());
    assert_eq!(store.with(RetainedReaders::registration_count), 0);

    let lease = store
        .lease(
            work(10),
            ScopedReadObservation::negative(read, coverage).expect("negative"),
        )
        .expect("lease");
    drop(lease);
    assert_eq!(store.with(RetainedReaders::registration_count), 0);
}
