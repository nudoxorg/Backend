//! Exercises the `backend-library` tests source contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Source-admission bounds, ownership, and allocation evidence.

use std::mem::size_of;

use allocation_counter::{AllocationInfo, measure};
use backend_library::interface::{
    PORTABLE_LOCAL_SOURCE_LIMIT, RejectedSourceText, SourceText, SourceTextLimit,
};

#[test]
fn source_admission_accepts_exact_utf8_capacity_and_returns_the_plus_one_owner() {
    let exact = "é".repeat(PORTABLE_LOCAL_SOURCE_LIMIT.bytes / "é".len());
    let accepted = SourceText::try_from(exact);
    assert!(matches!(accepted, Ok(source) if source.len() == PORTABLE_LOCAL_SOURCE_LIMIT.bytes));

    let mut plus_one = "é".repeat(PORTABLE_LOCAL_SOURCE_LIMIT.bytes / "é".len());
    plus_one.push('x');
    let rejected = SourceText::try_from(plus_one);
    assert!(matches!(
        rejected,
        Err(RejectedSourceText {
            source,
            error: SourceTextLimit {
                observed,
                limit,
            },
        }) if source.len() == PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1
            && observed == PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1
            && limit == PORTABLE_LOCAL_SOURCE_LIMIT
    ));
}

#[test]
fn source_owner_is_a_fat_pointer_and_admission_never_adds_more_than_one_owner_allocation() {
    assert_eq!(size_of::<SourceText>(), size_of::<Box<str>>());

    let mut source = String::with_capacity(128);
    source.push_str("fn one() -> u8 { 1 }");
    let mut admitted = None;
    let allocations = measure(|| {
        admitted = match SourceText::try_from(source) {
            Ok(value) => Some(value),
            Err(rejected) => {
                drop(rejected);
                None
            }
        };
    });
    assert!(
        allocations.count_total <= 1
            && allocations.count_current <= 1
            && allocations.count_max <= 1,
        "source admission measured {allocations:?}"
    );
    assert!(matches!(admitted.as_deref(), Some("fn one() -> u8 { 1 }")));
    drop(admitted);
}

#[test]
fn oversize_source_returns_without_another_allocation() {
    let source = "x".repeat(PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1);
    let mut rejected = None;
    let allocations = measure(|| {
        rejected = SourceText::try_from(source).err();
    });
    assert_eq!(allocations, AllocationInfo::default());
    assert!(matches!(
        rejected,
        Some(RejectedSourceText {
            source,
            error: SourceTextLimit { observed, .. },
        }) if source.len() == PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1
            && observed == PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1
    ));
}
