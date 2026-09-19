//! Generational retention laws and stale-publication regressions.

use super::*;

fn manifest(selector: &[u8], scope_seed: u8) -> (DependencyManifest, ScopedRead) {
    let authority = authority_scope(scope_seed);
    let scope = authority.scope_root();
    let observed = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![scope_seed]));
    let read = ScopedRead::exact(FacetKind::Name, scope, selector.to_vec());
    let recipe = Recipe::Names
        .typed(
            1,
            vec![scope_seed],
            ReadManifest::new(Vec::new()).expect("empty recipe reads"),
        )
        .value_version();
    let fact = ReadDependencyFact::positive(
        recipe,
        read.clone(),
        observed,
        complete_for_scope(&authority),
    )
    .expect("complete positive read");
    (
        DependencyManifest::new(vec![DependencyFact::read(fact)]).expect("manifest"),
        read,
    )
}

#[test]
fn same_manifest_reservations_coalesce_without_duplicate_index_work() {
    let owner = VersionedRetention::new();
    let (manifest, _) = manifest(b"same", 60);
    let first = owner.reserve(work(60), &manifest).expect("first lease");
    let generation = first.generation();
    let second = owner.reserve(work(60), &manifest).expect("coalesced lease");
    assert_eq!(second.generation(), generation);
    assert_eq!(owner.stats().live_generations, 1);
    assert_eq!(owner.stats().lease_roots, 2);

    let first_pin = first.commit().expect("first commit");
    let second_pin = second.commit().expect("second commit");
    assert_eq!(owner.stats().pin_roots, 2);
    assert!(first_pin.validate().is_ok());
    drop(first_pin);
    drop(second_pin);
    assert_eq!(owner.stats(), RetentionStats::default());
    assert_eq!(owner.counters().commits, 2);
}

#[test]
fn stale_completion_cannot_commit_or_release_a_replacement() {
    let owner = VersionedRetention::with_budget(RetentionBudget {
        max_live_generations: 1,
        max_retired_generations: 1,
        max_leases_per_generation: 8,
        max_pins_per_generation: 8,
    });
    let reader = work(61);
    let (old_manifest, old_read) = manifest(b"old", 61);
    let (new_manifest, _) = manifest(b"new", 61);
    let old_reservation = owner.reserve(reader, &old_manifest).expect("old lease");
    let old_pin = old_reservation.commit().expect("old commit");
    let stale_reservation = owner
        .reserve(reader, &old_manifest)
        .expect("old in-flight lease");
    let report = owner
        .invalidate_changes(&[DependencyChange::Read(old_read)])
        .expect("invalidate old generation");
    assert_eq!(report.readers(), &[reader]);
    assert_eq!(report.roots().len(), 1);
    assert_eq!(owner.stats().retired_generations, 1);
    // Retiring the generation drops its reverse-index registration even while
    // the pin keeps the lifecycle root alive. The pin retains only the root
    // value, so immutable Arc payloads cannot form a state-retention cycle.
    assert_eq!(owner.stats().registrations, 0);
    assert_eq!(old_pin.validate(), Err(SemanticError::StaleGeneration));

    // The old pin is still a live root, but a replacement can occupy the
    // current slot.  Its generation is intentionally different.
    let replacement = owner
        .reserve(reader, &new_manifest)
        .expect("replacement lease");
    assert_ne!(replacement.generation(), report.roots()[0].generation());
    let replacement_pin = replacement.commit().expect("replacement commit");
    assert!(replacement_pin.validate().is_ok());
    assert_eq!(owner.stats().registrations, 1);

    // A completion that races from the old lifecycle cannot manufacture a
    // current proof, and dropping it later cannot remove the replacement.
    assert!(matches!(
        stale_reservation.commit(),
        Err(SemanticError::StaleGeneration)
    ));
    drop(old_pin);
    assert_eq!(owner.stats().live_generations, 1);
    assert_eq!(owner.stats().retired_generations, 0);
    assert!(replacement_pin.validate().is_ok());
    drop(replacement_pin);
    assert_eq!(owner.stats(), RetentionStats::default());
    assert_eq!(owner.counters().stale_completions, 1);
}

#[test]
fn reservation_drop_rolls_back_index_and_generation_roots() {
    let owner = VersionedRetention::new();
    let (manifest, _) = manifest(b"rollback", 62);
    {
        let reservation = owner.reserve(work(62), &manifest).expect("reservation");
        assert_eq!(owner.stats().registrations, 1);
        assert_eq!(reservation.root().manifest(), manifest.version());
    }
    assert_eq!(owner.stats(), RetentionStats::default());
    assert_eq!(owner.with_readers(RetainedReaders::reader_count), 0);
}

#[test]
fn retired_root_capacity_rejects_invalidation_without_partial_mutation() {
    let owner = VersionedRetention::with_budget(RetentionBudget {
        max_live_generations: 1,
        max_retired_generations: 0,
        max_leases_per_generation: 4,
        max_pins_per_generation: 4,
    });
    let reader = work(63);
    let (manifest, read) = manifest(b"capacity", 63);
    let pin = owner
        .reserve(reader, &manifest)
        .expect("reservation")
        .commit()
        .expect("pin");
    assert_eq!(
        owner.invalidate_changes(&[DependencyChange::Read(read)]),
        Err(SemanticError::RetentionCapacity)
    );
    assert!(pin.validate().is_ok());
    assert_eq!(owner.stats().live_generations, 1);
    assert_eq!(owner.stats().retired_generations, 0);
    drop(pin);
    assert_eq!(owner.stats(), RetentionStats::default());
}

#[test]
fn zero_lease_budget_rejects_new_generation_before_index_mutation() {
    let owner = VersionedRetention::with_budget(RetentionBudget {
        max_live_generations: 1,
        max_retired_generations: 1,
        max_leases_per_generation: 0,
        max_pins_per_generation: 1,
    });
    let (manifest, _) = manifest(b"zero-lease", 65);
    assert!(matches!(
        owner.reserve(work(65), &manifest),
        Err(SemanticError::RetentionCapacity)
    ));
    assert_eq!(owner.stats(), RetentionStats::default());
    assert_eq!(owner.counters(), RetentionCounters::default());
}

#[test]
fn repeated_retire_and_reopen_churn_keeps_root_memory_bounded() {
    let owner = VersionedRetention::with_budget(RetentionBudget {
        max_live_generations: 1,
        max_retired_generations: 1,
        max_leases_per_generation: 2,
        max_pins_per_generation: 2,
    });
    let reader = work(64);
    let rounds = 2 * 1_usize.saturating_add(1_024);
    for seed in 0..rounds {
        let selector = seed.to_be_bytes();
        let scope_seed = u8::try_from(seed % 251).expect("modulo keeps scope seed in u8");
        let (manifest, read) = manifest(&selector, scope_seed);
        let pin = owner
            .reserve(reader, &manifest)
            .expect("churn reservation")
            .commit()
            .expect("churn commit");
        let report = owner
            .invalidate_changes(&[DependencyChange::Read(read)])
            .expect("churn invalidation");
        assert!(report.contains(reader));
        assert!(owner.stats().retired_generations <= 1);
        drop(pin);
        assert_eq!(owner.stats().retired_generations, 0);
        assert_eq!(owner.stats().live_generations, 0);
    }
    let counters = owner.counters();
    assert_eq!(counters.invalidated_generations, rounds as u64);
    assert_eq!(counters.retirements, rounds as u64);
    assert_eq!(owner.stats(), RetentionStats::default());
}
