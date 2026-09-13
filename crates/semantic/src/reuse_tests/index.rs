//! Reverse-index and cleanup scaling tests.

use super::*;

#[test]
fn reader_identity_uses_explicit_canonical_bytes_after_hash_collision() {
    let first = HashCollisionReaderKey(1);
    let second = HashCollisionReaderKey(2);
    let mut first_bytes = Vec::new();
    let mut second_bytes = Vec::new();
    first.encode_canonical(&mut first_bytes);
    second.encode_canonical(&mut second_bytes);
    assert_ne!(first_bytes, second_bytes);

    let scope_root = scope(201);
    let coverage = complete(201);
    let observation = ScopedReadObservation::positive(
        ScopedRead::exact(FacetKind::Name, scope_root, b"same".to_vec()),
        ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![201])),
        coverage,
    )
    .expect("observation");
    let mut index = RetainedReaders::<HashCollisionReaderKey>::default();
    index
        .register(first, observation.clone())
        .expect("first reader");
    index.register(second, observation).expect("second reader");
    assert_eq!(index.registration_count(), 2);
    assert!(index.relation_index_parity());
}

#[test]
fn reverse_reader_index_matches_full_scan_for_exact_ranges_and_prefixes() {
    let scope_root = scope(3);
    let coverage = complete(3);
    let facet_version = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1]));
    let mut index = RetainedReaders::default();
    for seed in 0..256_u16 {
        let reader = work(u8::try_from(seed).expect("bounded seed"));
        let key = format!("key-{seed:03}").into_bytes();
        let read = if seed % 3 == 0 {
            ScopedRead::exact(FacetKind::Name, scope_root, key)
        } else if seed % 3 == 1 {
            ScopedRead::range(
                FacetKind::Name,
                scope_root,
                b"key-000".to_vec(),
                b"key-128".to_vec(),
            )
            .expect("range")
        } else {
            ScopedRead::prefix(FacetKind::Name, scope_root, b"key-2".to_vec())
        };
        let observation =
            ScopedReadObservation::positive(read, facet_version, coverage).expect("observation");
        index.register(reader, observation).expect("register");
    }
    let changed = ScopedRead::exact(FacetKind::Name, scope_root, b"key-127".to_vec());
    let indexed = index
        .invalidate(std::slice::from_ref(&changed))
        .expect("index query");
    let oracle = index.full_scan_invalidation(std::slice::from_ref(&changed));
    assert_eq!(indexed.readers, oracle.readers);
    assert!(indexed.counters.interval_probes < oracle.counters.full_scan_reads);
    assert!(indexed.counters.invalidated_readers > 0);

    let changed_prefix = ScopedRead::prefix(FacetKind::Name, scope_root, b"key-2".to_vec());
    assert!(
        index
            .equivalent_to_full_scan(&[changed_prefix])
            .expect("equivalence")
    );
}

#[test]
fn retained_reader_manifest_registration_rolls_back_all_state_on_error() {
    let scope_root = scope(40);
    let coverage = complete(40);
    let observation = ScopedReadObservation::positive(
        ScopedRead::exact(FacetKind::Name, scope_root, b"same".to_vec()),
        ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![7])),
        coverage,
    )
    .expect("observation");
    let fact = DependencyFact::read(
        ReadDependencyFact::witnessed(recipe(40), observation.clone()).expect("fact"),
    );
    let manifest = DependencyManifest::new(vec![fact.clone()]).expect("manifest");
    let reader = work(40);
    let mut index = RetainedReaders::default();
    let version = index
        .register_manifest(reader, &manifest)
        .expect("first registration");
    assert_eq!(index.registration_count(), 1);
    assert_eq!(index.dependency_graph().manifest_count(), 1);
    assert!(index.relation_index_parity());

    let duplicate = DependencyManifest::new(vec![fact]).expect("manifest");
    assert_eq!(
        index.register_manifest(reader, &duplicate),
        Err(SemanticError::DuplicateRead)
    );
    assert_eq!(index.registration_count(), 1);
    assert_eq!(
        index.dependency_graph().manifest_reference_count(version),
        1
    );
    assert!(index.unregister_manifest(reader, version));
    assert_eq!(index.registration_count(), 1);
    index.unregister_reader(reader);
    assert_eq!(index.registration_count(), 0);
    assert_eq!(index.dependency_graph().manifest_count(), 0);
    assert!(index.relation_index_parity());
}

#[test]
fn relation_root_advances_with_canonical_delta_and_rebuilds_indexes() {
    let scope_root = scope(44);
    let coverage = complete(44);
    let observed = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![44]));
    let reader = work(44);
    let read = ScopedRead::exact(FacetKind::Name, scope_root, b"root".to_vec());
    let observation =
        ScopedReadObservation::positive(read.clone(), observed, coverage).expect("observation");
    let mut index = RetainedReaders::default();
    let empty_root = index.relation_root();
    assert_eq!(index.relation_record_count(), 0);
    index.register(reader, observation).expect("register");
    assert_ne!(index.relation_root(), empty_root);
    assert_eq!(index.relation_record_count(), 1);
    assert!(index.relation_index_parity());
    assert!(index.unregister(reader, &read));
    assert_eq!(index.relation_record_count(), 0);
    assert!(index.relation_index_parity());
}

#[test]
fn invalidation_verifies_only_intersecting_registration_ids() {
    let scope_root = scope(41);
    let coverage = complete(41);
    let facet_version = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1]));
    let reader = work(41);
    let mut index = RetainedReaders::default();
    for seed in 0..512_u16 {
        let key = format!("unrelated-{seed:04}").into_bytes();
        let read = ScopedRead::exact(FacetKind::Name, scope_root, key);
        index
            .register(
                reader,
                ScopedReadObservation::positive(read, facet_version, coverage)
                    .expect("observation"),
            )
            .expect("register");
    }
    let target = ScopedRead::exact(
        FacetKind::Name,
        scope_root,
        b"z-target-with-a-long-key".to_vec(),
    );
    index
        .register(
            reader,
            ScopedReadObservation::positive(target.clone(), facet_version, coverage)
                .expect("target observation"),
        )
        .expect("target register");

    let indexed = index
        .invalidate(std::slice::from_ref(&target))
        .expect("indexed");
    let oracle = index.full_scan_invalidation(std::slice::from_ref(&target));
    assert_eq!(indexed.readers, oracle.readers);
    assert_eq!(indexed.counters.candidate_registrations, 1);
    assert_eq!(indexed.counters.verified_reads, 1);
    assert!(oracle.counters.full_scan_reads > indexed.counters.verified_reads * 100);
}

#[test]
fn unregister_reader_releases_many_global_recipe_and_authority_buckets() {
    let reader = work(42);
    let mut index = RetainedReaders::default();
    let mut recipes = Vec::new();
    let mut authorities = Vec::new();
    for seed in 0..128_u8 {
        let recipe_root = recipe(seed);
        let authority = authority_scope(seed);
        let authority_fact = DependencyFact::authority(
            AuthorityDependencyFact::new(recipe_root, &authority, complete_for_scope(&authority))
                .expect("authority fact"),
        );
        index
            .register_fact(reader, &authority_fact)
            .expect("register authority");
        recipes.push(recipe_root);
        authorities.push(authority.authority_version());
    }
    assert_eq!(index.reader_count(), 1);
    assert_eq!(index.recipe_user_count(recipes[127]), 1);
    assert_eq!(index.authority_user_count(authorities[127]), 1);
    assert_eq!(index.unregister_reader(reader), 0);
    assert_eq!(index.reader_count(), 0);
    for recipe_root in recipes {
        assert_eq!(index.recipe_user_count(recipe_root), 0);
    }
    for authority in authorities {
        assert_eq!(index.authority_user_count(authority), 0);
    }
}

#[test]
fn reader_index_requires_an_explicit_typed_execution_key_adapter() {
    let scope_root = scope(43);
    let coverage = complete(43);
    let observation = ScopedReadObservation::negative(
        ScopedRead::negative(FacetKind::Name, scope_root, b"typed".to_vec()),
        coverage,
    )
    .expect("observation");
    let identity = ExecutionIdentity(ExecutionWorkKey([43; 32]));
    let mut index = RetainedReaders::<ExecutionWorkKey>::default();
    index
        .register_identity(&identity, observation)
        .expect("typed registration");
    let changed = ScopedRead::exact(FacetKind::Name, scope_root, b"typed".to_vec());
    let report = index.invalidate(&[changed]).expect("invalidate");
    assert_eq!(report.readers, vec![identity.0]);
}

#[test]
fn overlapping_key_ranges_remain_fenced_by_their_scope_roots() {
    // Two authority scopes can cover the same logical key range while still
    // having different provenance/coverage roots.  A change observed under
    // one scope must not invalidate a reader admitted by the other scope:
    // scope identity is part of the semantic proof, not a routing hint.
    let left = authority_scope(48);
    let right = authority_scope(49);
    let left_read =
        ScopedRead::negative(FacetKind::Name, left.scope_root(), b"shared-key".to_vec());
    let right_read =
        ScopedRead::negative(FacetKind::Name, right.scope_root(), b"shared-key".to_vec());
    let mut index = RetainedReaders::default();
    index
        .register(
            work(48),
            ScopedReadObservation::negative(left_read.clone(), complete_for_scope(&left))
                .expect("left observation"),
        )
        .expect("left reader");
    index
        .register(
            work(49),
            ScopedReadObservation::negative(right_read, complete_for_scope(&right))
                .expect("right observation"),
        )
        .expect("right reader");

    let report = index.invalidate(&[left_read]).expect("scope invalidation");
    assert_eq!(report.readers, vec![work(48)]);
}
