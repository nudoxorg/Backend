//! Execution-owned semantic dependency registration and invalidation.
//!
//! The semantic crate owns dependency facts and reverse arrangements.  This
//! module owns only the lifecycle join to an execution `WorkKey`: a complete
//! capability reserves one generation, publication commits that generation,
//! and cache eviction or semantic invalidation releases it.

use backend_execution::{VersionedWorkIdentity, WorkKey};
use backend_semantic::{
    DependencyChange, DependencyManifest, InvalidationBudget, InvalidationCounters,
    RetainedReaders, SemanticError, SemanticReaderKeyAdapter,
};
use backend_version::Relation;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

const MAX_INVALIDATION_TOMBSTONES: usize = 4096;

/// Failure while joining an admitted dependency manifest to execution reuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticDependencyError {
    /// The semantic reverse index rejected the manifest or invalidation.
    Semantic(SemanticError),
    /// A work key already has a different retained manifest generation.
    ConflictingManifest,
    /// The bounded registration generation counter exhausted.
    Overflow,
    /// The owner cannot retain more invalidation tombstones safely.
    Capacity,
    /// A capability for a manifest generation that was invalidated remains
    /// in circulation.
    Invalidated,
    /// A recovered catalog entry names a different semantic registration
    /// generation than the live coordinator can safely restore.
    ConflictingGeneration,
}

impl std::fmt::Display for SemanticDependencyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "semantic dependency error: {self:?}")
    }
}

impl std::error::Error for SemanticDependencyError {}

/// Result of one bounded semantic invalidation query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticInvalidationReport {
    work_keys: Vec<WorkKey>,
    /// Reverse-index work counters retained for operational budgets.
    pub counters: InvalidationCounters,
}

impl SemanticInvalidationReport {
    /// Returns the exact execution keys affected by the change.
    #[must_use]
    pub fn work_keys(&self) -> &[WorkKey] {
        &self.work_keys
    }

    /// Returns whether one exact execution key was affected.
    #[must_use]
    pub fn contains(&self, key: WorkKey) -> bool {
        self.work_keys.binary_search(&key).is_ok()
    }
}

#[derive(Clone, Copy)]
struct ExecutionIdentity<R: Relation> {
    identity: VersionedWorkIdentity<R>,
}

impl<R: Relation> SemanticReaderKeyAdapter for ExecutionIdentity<R> {
    type ReaderKey = WorkKey;

    fn reader_key(&self) -> Self::ReaderKey {
        self.identity.work_key()
    }
}

struct Registration {
    manifest: backend_semantic::DependencyManifestVersion,
    generation: u64,
}

#[derive(Default)]
struct State {
    readers: RetainedReaders<WorkKey>,
    registrations: BTreeMap<WorkKey, Registration>,
    invalidated: BTreeMap<WorkKey, backend_semantic::DependencyManifestVersion>,
    /// Retention order for invalidation fences.  A fence is needed only while
    /// an old capability can race replacement admission; keeping every
    /// historical work key forever would make semantic invalidation itself a
    /// process lifetime memory leak.
    invalidated_order: VecDeque<WorkKey>,
    next_generation: u64,
}

/// Bounded execution/semantic lifecycle coordinator.
pub struct SemanticDependencyCoordinator {
    state: Mutex<State>,
}

impl std::fmt::Debug for SemanticDependencyCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SemanticDependencyCoordinator")
            .field("registrations", &self.registration_count())
            .finish()
    }
}

impl Default for SemanticDependencyCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticDependencyCoordinator {
    /// Creates an empty lifecycle coordinator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
        }
    }

    /// Returns the number of retained execution manifests.
    #[must_use]
    pub fn registration_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .registrations
            .len()
    }

    /// Reserves one exact manifest generation for a scheduled identity.
    ///
    /// A same-version registration is shared.  A different manifest for the
    /// same key is rejected until the old generation is explicitly invalidated
    /// or evicted.  New registrations are transactional inside the semantic
    /// reverse index; dropping the returned reservation rolls them back.
    pub(super) fn reserve<R: Relation>(
        self: &Arc<Self>,
        identity: &VersionedWorkIdentity<R>,
        manifest: &DependencyManifest,
    ) -> Result<SemanticDependencyReservation, SemanticDependencyError> {
        self.reserve_inner(identity, manifest, None)
    }

    /// Restores one persisted registration with its exact generation.  A
    /// restartable output catalog stores this generation alongside the
    /// dependency manifest; assigning a fresh process-local number here would
    /// let a stale capability masquerade as the recovered publication.
    pub(super) fn reserve_at_generation<R: Relation>(
        self: &Arc<Self>,
        identity: &VersionedWorkIdentity<R>,
        manifest: &DependencyManifest,
        generation: u64,
    ) -> Result<SemanticDependencyReservation, SemanticDependencyError> {
        if generation == 0 {
            return Err(SemanticDependencyError::ConflictingGeneration);
        }
        self.reserve_inner(identity, manifest, Some(generation))
    }

    fn reserve_inner<R: Relation>(
        self: &Arc<Self>,
        identity: &VersionedWorkIdentity<R>,
        manifest: &DependencyManifest,
        requested_generation: Option<u64>,
    ) -> Result<SemanticDependencyReservation, SemanticDependencyError> {
        let key = identity.work_key();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let version = manifest.version();
        if state
            .invalidated
            .get(&key)
            .is_some_and(|invalidated| *invalidated == version)
        {
            return Err(SemanticDependencyError::Invalidated);
        }
        if state
            .invalidated
            .get(&key)
            .is_some_and(|invalidated| *invalidated != version)
        {
            state.invalidated.remove(&key);
            state
                .invalidated_order
                .retain(|candidate| *candidate != key);
        }
        if let Some(previous) = state.registrations.get(&key) {
            if previous.manifest != version {
                return Err(SemanticDependencyError::ConflictingManifest);
            }
            if requested_generation.is_some_and(|generation| generation != previous.generation) {
                return Err(SemanticDependencyError::ConflictingGeneration);
            }
            return Ok(SemanticDependencyReservation {
                coordinator: Arc::clone(self),
                key,
                generation: previous.generation,
                inserted: false,
                committed: false,
            });
        }
        let generation = match requested_generation {
            // A persisted generation may be lower than this process's
            // allocation counter after the hot cache evicted an entry. The
            // key-specific registration check above still prevents a live
            // replacement from being rebound to a different generation; the
            // global counter is advanced below so subsequent fresh
            // registrations never move backwards.
            Some(generation) => generation,
            None => state
                .next_generation
                .checked_add(1)
                .ok_or(SemanticDependencyError::Overflow)?,
        };
        let adapter = ExecutionIdentity {
            identity: *identity,
        };
        state
            .readers
            .register_manifest(adapter.reader_key(), manifest)
            .map_err(SemanticDependencyError::Semantic)?;
        if generation > state.next_generation {
            state.next_generation = generation;
        }
        state.registrations.insert(
            key,
            Registration {
                manifest: version,
                generation,
            },
        );
        Ok(SemanticDependencyReservation {
            coordinator: Arc::clone(self),
            key,
            generation,
            inserted: true,
            committed: false,
        })
    }

    /// Invalidates exact readers for changed selectors, recipes, or authority.
    pub(super) fn invalidate_changes(
        &self,
        changes: &[DependencyChange],
    ) -> Result<SemanticInvalidationReport, SemanticDependencyError> {
        self.invalidate_changes_budgeted(changes, InvalidationBudget::default())
    }

    /// Budgeted invalidation entry point used by daemon owners with a tighter
    /// fan-out envelope.
    pub(super) fn invalidate_changes_budgeted(
        &self,
        changes: &[DependencyChange],
        budget: InvalidationBudget,
    ) -> Result<SemanticInvalidationReport, SemanticDependencyError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let report = state
            .readers
            .invalidate_changes_budgeted(changes, budget)
            .map_err(SemanticDependencyError::Semantic)?;
        let work_keys = report.readers;
        for key in &work_keys {
            let manifest = state
                .registrations
                .get(key)
                .map(|registration| registration.manifest);
            if let Some(manifest) = manifest {
                // Retain only a bounded suffix of invalidation fences.  The
                // registration generation check below prevents a delayed
                // eviction from releasing a replacement; old fences that no
                // longer have a live publication are safe to retire here.
                if !state.invalidated.contains_key(key) {
                    while state.invalidated.len() >= MAX_INVALIDATION_TOMBSTONES {
                        let Some(oldest) = state.invalidated_order.pop_front() else {
                            break;
                        };
                        state.invalidated.remove(&oldest);
                    }
                }
                state.invalidated.insert(*key, manifest);
                state
                    .invalidated_order
                    .retain(|candidate| *candidate != *key);
                state.invalidated_order.push_back(*key);
            }
            state.readers.unregister_reader(*key);
            state.registrations.remove(key);
        }
        Ok(SemanticInvalidationReport {
            work_keys,
            counters: report.counters,
        })
    }

    /// Releases one publication generation after cache eviction.
    pub(super) fn release(&self, key: WorkKey, generation: u64) {
        self.release_if_generation(key, generation);
    }

    fn release_if_generation(&self, key: WorkKey, generation: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .registrations
            .get(&key)
            .is_some_and(|registration| registration.generation == generation)
        {
            state.registrations.remove(&key);
            state.readers.unregister_reader(key);
        }
    }
}

/// RAII reservation for semantic registration.  The reservation is committed
/// only after scheduler publication; a failed attempt therefore cannot leave a
/// reader in the reverse index.
#[derive(Debug)]
pub(crate) struct SemanticDependencyReservation {
    coordinator: Arc<SemanticDependencyCoordinator>,
    key: WorkKey,
    generation: u64,
    inserted: bool,
    committed: bool,
}

impl SemanticDependencyReservation {
    pub(super) fn commit(&mut self) {
        self.committed = true;
    }

    /// Returns the generation this reservation owns, allowing eviction to
    /// release only the publication that actually occupied the cache slot.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for SemanticDependencyReservation {
    fn drop(&mut self) {
        if self.inserted && !self.committed {
            self.coordinator
                .release_if_generation(self.key, self.generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::{
        DependencyFact, FacetKind, FacetValue, FacetValueSchema, ReadDependencyFact, ReadManifest,
        Recipe, ScopedRead,
    };
    use backend_version::{
        AuthorityScopeClaim, AuthorizedCompleteCoverage, CoverageWitness, ObjectVersion,
        ProducerObservationVerifier, RelationState, ScopeRoot, UntrustedProducerObservation,
        admit_complete_scope, admit_producer_observation, partial_coverage,
    };

    #[derive(Debug)]
    struct TestRelation;

    impl Relation for TestRelation {
        const DOMAIN: u8 = 0x7e;
        const TYPE: u16 = 1;
        type Key = u64;
        type Value = u64;

        fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
            output.extend_from_slice(&value.to_be_bytes());
        }

        fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(&value.to_be_bytes());
        }
    }

    fn identity(seed: u64) -> VersionedWorkIdentity<TestRelation> {
        let input = RelationState::<TestRelation>::from_entries(
            [(seed, seed)],
            CoverageWitness::Partial(partial_coverage(1)),
        )
        .expect("test relation state")
        .root();
        VersionedWorkIdentity::new(
            backend_execution::RecipeId::from_value(b"dispatch-dependency-recipe"),
            input,
            backend_execution::ReadManifestId::from_value(b"dispatch-dependency-reads"),
            backend_execution::AuthorityVersion::from_value(b"dispatch-dependency-authority"),
            backend_execution::OutputEquivalence::from_value(b"dispatch-dependency-equivalence"),
        )
    }

    fn complete_scope(
        seed: u8,
    ) -> (
        ScopeRoot,
        AuthorizedCompleteCoverage,
        ObjectVersion<FacetValueSchema>,
    ) {
        let value = FacetValue::new(FacetKind::Name, vec![seed]);
        let version = ObjectVersion::<FacetValueSchema>::from_value(&value);
        let declaration = AuthorityScopeClaim::from_object_version(version);
        let observation = UntrustedProducerObservation::new(
            [0x61; 32],
            declaration.scope_root(),
            [0x62; 32],
            vec![seed],
        );
        let verifier = ExactProducerObservation(observation.clone());
        let observed =
            admit_producer_observation(observation, &verifier).expect("producer observation");
        let complete = admit_complete_scope(declaration, observed).expect("complete scope");
        (ScopeRoot::from_bytes(version.to_bytes()), complete, version)
    }

    #[derive(Clone, Debug)]
    struct ExactProducerObservation(UntrustedProducerObservation);

    impl ProducerObservationVerifier for ExactProducerObservation {
        type Error = &'static str;

        fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
            (observation == &self.0)
                .then_some(())
                .ok_or("test producer observation mismatch")
        }
    }

    fn manifest(selector: &[u8], scope_seed: u8) -> (DependencyManifest, ScopedRead) {
        let (scope, coverage, observed) = complete_scope(scope_seed);
        let read = ScopedRead::exact(FacetKind::Name, scope, selector.to_vec());
        let recipe = Recipe::Names
            .typed(
                1,
                vec![scope_seed],
                ReadManifest::new(Vec::new()).expect("empty recipe reads"),
            )
            .value_version();
        let fact = ReadDependencyFact::positive(recipe, read.clone(), observed, coverage)
            .expect("complete positive read");
        (
            DependencyManifest::new(vec![DependencyFact::read(fact)])
                .expect("valid dependency manifest"),
            read,
        )
    }

    #[test]
    fn disjoint_exact_read_changes_invalidate_only_intersecting_work() {
        let coordinator = Arc::new(SemanticDependencyCoordinator::new());
        let (first_manifest, first_read) = manifest(b"a", 1);
        let (second_manifest, _) = manifest(b"b", 1);
        let first_identity = identity(1);
        let second_identity = identity(2);
        let mut first = coordinator
            .reserve(&first_identity, &first_manifest)
            .expect("first registration");
        let mut second = coordinator
            .reserve(&second_identity, &second_manifest)
            .expect("second registration");
        let first_key = first_identity.work_key();
        let second_key = second_identity.work_key();
        first.commit();
        second.commit();

        let report = coordinator
            .invalidate_changes(&[DependencyChange::Read(first_read)])
            .expect("bounded invalidation");
        assert_eq!(report.work_keys(), &[first_key]);
        assert!(!report.contains(second_key));
        assert_eq!(coordinator.registration_count(), 1);
        let second_generation = second.generation();
        coordinator.release(second_key, second_generation);
        assert_eq!(coordinator.registration_count(), 0);
    }

    #[test]
    fn stale_eviction_generation_cannot_release_a_replacement_manifest() {
        let coordinator = Arc::new(SemanticDependencyCoordinator::new());
        let (old_manifest, old_read) = manifest(b"old", 2);
        let (new_manifest, _) = manifest(b"new", 2);
        let work = identity(3);
        let key = work.work_key();
        let mut old = coordinator
            .reserve(&work, &old_manifest)
            .expect("old registration");
        let old_generation = old.generation();
        old.commit();
        let report = coordinator
            .invalidate_changes(&[DependencyChange::Read(old_read)])
            .expect("invalidate old manifest");
        assert!(report.contains(key));
        let mut replacement = coordinator
            .reserve(&work, &new_manifest)
            .expect("replacement registration");
        let replacement_generation = replacement.generation();
        replacement.commit();
        assert_ne!(old_generation, replacement_generation);

        coordinator.release(key, old_generation);
        assert_eq!(coordinator.registration_count(), 1);
        coordinator.release(key, replacement_generation);
        assert_eq!(coordinator.registration_count(), 0);
    }

    #[test]
    fn failed_reservation_rolls_back_reverse_index() {
        let coordinator = Arc::new(SemanticDependencyCoordinator::new());
        let (manifest, _) = manifest(b"rollback", 4);
        let work = identity(4);
        {
            let _reservation = coordinator
                .reserve(&work, &manifest)
                .expect("temporary registration");
            assert_eq!(coordinator.registration_count(), 1);
        }
        assert_eq!(coordinator.registration_count(), 0);
    }

    #[test]
    fn persisted_generation_can_be_restored_after_cache_eviction() {
        let coordinator = Arc::new(SemanticDependencyCoordinator::new());
        let (manifest, _) = manifest(b"restore", 9);
        let work = identity(9);
        let key = work.work_key();
        let mut first = coordinator
            .reserve(&work, &manifest)
            .expect("initial registration");
        let generation = first.generation();
        first.commit();
        coordinator.release(key, generation);

        let mut restored = coordinator
            .reserve_at_generation(&work, &manifest, generation)
            .expect("persisted registration restores exactly");
        assert_eq!(restored.generation(), generation);
        restored.commit();
    }

    #[test]
    fn invalidation_fences_are_bounded_during_distinct_key_churn() {
        let coordinator = Arc::new(SemanticDependencyCoordinator::new());
        let total = MAX_INVALIDATION_TOMBSTONES
            .saturating_mul(2)
            .saturating_add(1);
        for seed in 0..total {
            let selector = seed.to_be_bytes();
            let bucket = u8::try_from(seed % 251).unwrap_or_default();
            let work = identity(u64::try_from(seed).unwrap_or_default() + 10_000);
            let (manifest, read) = manifest(&selector, bucket);
            let mut reservation = coordinator
                .reserve(&work, &manifest)
                .expect("churn registration remains admitted");
            reservation.commit();
            let report = coordinator
                .invalidate_changes(&[DependencyChange::Read(read)])
                .expect("bounded invalidation remains available");
            assert!(report.contains(work.work_key()));
        }
        let state = coordinator
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.invalidated.len() <= MAX_INVALIDATION_TOMBSTONES);
        assert!(state.invalidated_order.len() <= MAX_INVALIDATION_TOMBSTONES);
        assert_eq!(state.registrations.len(), 0);
    }
}
