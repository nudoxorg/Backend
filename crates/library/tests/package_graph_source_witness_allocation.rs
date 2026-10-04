//! Public-boundary allocation evidence for borrowed package source witnesses.

use allocation_counter::{AllocationInfo, measure};
use backend_library::{
    BorrowedPackageGraphSourceState, CheckedPackageGraphFacts,
    CheckedPackageGraphSourceWitness, DependencyFacts, PackageGraphSourceAuthority,
    PackageGraphSourceKey, PackageReference, ProductText, RegistryAuthorityId,
};

#[test]
fn borrowed_source_state_witness_is_allocation_free_and_legacy_owned_path_allocates() {
    let source = PackageGraphSourceKey::new(
        PackageReference::parse("pkg:cargo/allocation-check@1.0.0").expect("valid source"),
        PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
            [0x52; 32],
        )),
    );
    let reason = ProductText::new("registry omitted dependency metadata").expect("reason");
    let states = [
        BorrowedPackageGraphSourceState::KnownEmpty,
        BorrowedPackageGraphSourceState::Unknown(&reason),
        BorrowedPackageGraphSourceState::Unavailable(&reason),
    ];

    for state in states {
        let mut witness = None;
        let allocations = measure(|| {
            witness = Some(std::hint::black_box(
                CheckedPackageGraphSourceWitness::admit(&source, state)
                    .expect("admitted borrowed source witness"),
            ));
        });
        assert_eq!(allocations, AllocationInfo::default());
        let witness = witness.expect("witness result retained across the measurement");
        std::hint::black_box(witness.digest());
    }

    let mut legacy_facts = None;
    let legacy_allocations = measure(|| {
        legacy_facts = Some(std::hint::black_box(
            CheckedPackageGraphFacts::new(vec![(
                source.clone(),
                DependencyFacts::Unknown(reason.clone()),
            )])
            .expect("legacy owned one-source facts"),
        ));
    });
    assert!(
        legacy_allocations.count_total > 0
            && legacy_allocations.count_current > 0
            && legacy_allocations.count_max > 0,
        "positive control should observe allocations in the legacy owned path: {legacy_allocations:?}"
    );
    assert!(legacy_facts.is_some());
}
