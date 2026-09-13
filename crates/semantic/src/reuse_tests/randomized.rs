//! Randomized and budget-bound invalidation tests.

use super::*;

#[test]
fn randomized_reverse_index_matches_full_scan_oracle() {
    fn next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state
    }

    let scope_root = scope(44);
    let coverage = complete(44);
    let facet_version = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![9]));
    let mut index = RetainedReaders::default();
    for reader_seed in 0..16_u8 {
        let reader = work(reader_seed);
        for offset in 0..24_u8 {
            let selector = match (u64::from(reader_seed) * 31 + u64::from(offset)) % 3 {
                0 => ScopedRead::exact(FacetKind::Name, scope_root, vec![reader_seed, offset, 0]),
                1 => ScopedRead::range(
                    FacetKind::Name,
                    scope_root,
                    vec![reader_seed, offset, 0],
                    vec![reader_seed, offset, 255],
                )
                .expect("range"),
                _ => ScopedRead::prefix(FacetKind::Name, scope_root, vec![reader_seed, offset, 1]),
            };
            index
                .register(
                    reader,
                    ScopedReadObservation::positive(selector, facet_version, coverage)
                        .expect("observation"),
                )
                .expect("register");
        }
    }

    let mut state = 0x4c_u64;
    for _ in 0..128 {
        let reader_seed = u8::try_from(next(&mut state) % 16).expect("bounded reader");
        let offset = u8::try_from(next(&mut state) % 24).expect("bounded offset");
        let changed = match next(&mut state) % 3 {
            0 => ScopedRead::exact(FacetKind::Name, scope_root, vec![reader_seed, offset, 0]),
            1 => ScopedRead::range(
                FacetKind::Name,
                scope_root,
                vec![reader_seed, offset, 0],
                vec![reader_seed, offset.saturating_add(1), 0],
            )
            .expect("changed range"),
            _ => ScopedRead::prefix(FacetKind::Name, scope_root, vec![reader_seed, offset]),
        };
        let indexed = index
            .invalidate(std::slice::from_ref(&changed))
            .expect("indexed");
        let oracle = index.full_scan_invalidation(std::slice::from_ref(&changed));
        assert_eq!(indexed.readers, oracle.readers);
    }
}

#[test]
fn invalidation_budget_bounds_candidate_and_probe_work() {
    let scope_root = scope(45);
    let coverage = complete(45);
    let facet_version = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![5]));
    let mut index = RetainedReaders::default();
    for seed in 0..8_u8 {
        let reader = work(seed);
        let read = ScopedRead::exact(FacetKind::Name, scope_root, vec![seed, 1]);
        index
            .register(
                reader,
                ScopedReadObservation::positive(read, facet_version, coverage)
                    .expect("observation"),
            )
            .expect("register");
    }
    let changed = ScopedRead::prefix(FacetKind::Name, scope_root, vec![]);
    assert_eq!(
        index.invalidate_budgeted(
            std::slice::from_ref(&changed),
            InvalidationBudget {
                max_changed_reads: 1,
                max_probes: 64,
                max_candidates: 2,
            },
        ),
        Err(SemanticError::ReuseWorkLimit)
    );
}

#[test]
fn invalidation_budget_counts_unique_registration_hits_across_indexes() {
    let scope_root = scope(46);
    let coverage = complete(46);
    let facet_version = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![6]));
    let reader = work(46);
    let changed = ScopedRead::exact(FacetKind::Name, scope_root, b"same".to_vec());
    let mut index = RetainedReaders::default();
    index
        .register(
            reader,
            ScopedReadObservation::positive(changed.clone(), facet_version, coverage)
                .expect("observation"),
        )
        .expect("register");

    let report = index
        .invalidate_budgeted(
            std::slice::from_ref(&changed),
            InvalidationBudget {
                max_changed_reads: 1,
                max_probes: 64,
                max_candidates: 1,
            },
        )
        .expect("one registration is within the budget");
    assert_eq!(report.readers, vec![reader]);
    assert_eq!(report.counters.candidate_registrations, 1);
    assert_eq!(report.counters.verified_reads, 1);
}

#[test]
fn invalidation_budget_counts_absent_exact_lookups() {
    let scope_root = scope(47);
    let changed = ScopedRead::exact(FacetKind::Name, scope_root, b"missing".to_vec());
    let index = RetainedReaders::<backend_flow::WorkKey>::default();
    assert_eq!(
        index.invalidate_budgeted(
            std::slice::from_ref(&changed),
            InvalidationBudget {
                max_changed_reads: 1,
                max_probes: 0,
                max_candidates: 0,
            },
        ),
        Err(SemanticError::ReuseWorkLimit)
    );
    let report = index
        .invalidate_budgeted(
            std::slice::from_ref(&changed),
            InvalidationBudget {
                max_changed_reads: 1,
                max_probes: 1,
                max_candidates: 0,
            },
        )
        .expect("one absent exact lookup");
    assert!(report.readers.is_empty());
    assert_eq!(report.counters.exact_probes, 1);
}
