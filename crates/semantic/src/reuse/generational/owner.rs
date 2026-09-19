use super::super::index::{RetainedReaders, SemanticReaderKey};
use super::super::{DependencyChange, InvalidationBudget};
use super::model::{
    CurrentGeneration, GenerationEntry, GenerationInvalidation, RetentionBudget, RetentionCounters,
    RetentionState, RetentionStats, SemanticGeneration, SemanticGenerationRoot, VersionedRetention,
};
use super::roots::{SemanticReservation, reclaim_unrooted, retire_current};
use crate::{DependencyManifest, SemanticError};
use std::fmt;
use std::sync::{Arc, Mutex};

impl<K: SemanticReaderKey> fmt::Debug for VersionedRetention<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VersionedRetention")
            .field("stats", &self.stats())
            .field("budget", &self.budget())
            .finish()
    }
}

impl<K: SemanticReaderKey> Default for VersionedRetention<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: SemanticReaderKey> VersionedRetention<K> {
    /// Creates an empty owner with the default bounded retention budget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RetentionState::default())),
        }
    }

    /// Creates an empty owner with an explicit memory and root budget.
    #[must_use]
    pub fn with_budget(budget: RetentionBudget) -> Self {
        let owner = Self::new();
        owner.with_state_mut(|state| state.budget = budget);
        owner
    }

    /// Returns the immutable retention budget.
    #[must_use]
    pub fn budget(&self) -> RetentionBudget {
        self.with_state(|state| state.budget)
    }

    /// Returns current lifecycle and reverse-index counts.
    #[must_use]
    pub fn stats(&self) -> RetentionStats {
        self.with_state(|state| {
            let (lease_roots, pin_roots) = state
                .current
                .values()
                .chain(state.retired.values())
                .fold((0_u64, 0_u64), |(leases, pins), entry| {
                    (
                        leases.saturating_add(u64::from(entry.leases)),
                        pins.saturating_add(u64::from(entry.pins)),
                    )
                });
            RetentionStats {
                live_generations: state.current.len(),
                retired_generations: state.retired.len(),
                lease_roots,
                pin_roots,
                registrations: state.readers.registration_count(),
            }
        })
    }

    /// Returns lifecycle counters accumulated by the owner.
    #[must_use]
    pub fn counters(&self) -> RetentionCounters {
        self.with_state(|state| state.counters)
    }

    /// Runs an immutable query against the retained reverse arrangements.
    pub fn with_readers<R>(&self, operation: impl FnOnce(&RetainedReaders<K>) -> R) -> R {
        self.with_state(|state| operation(&state.readers))
    }

    /// Reserves a manifest generation for one reader.
    ///
    /// A same-manifest reservation coalesces onto the existing current
    /// generation.  A different manifest requires explicit retirement or
    /// invalidation first; silently replacing it would allow an older result
    /// to appear current.  Dropping the returned reservation releases its
    /// lease and rolls back an uncommitted registration.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ConflictingGeneration`] for a different live
    /// manifest, [`SemanticError::RetentionCapacity`] when the live envelope
    /// is full, or the underlying semantic admission error.
    pub fn reserve(
        &self,
        reader: K,
        manifest: &DependencyManifest,
    ) -> Result<SemanticReservation<K>, SemanticError> {
        let mut state = self.lock_state();
        let manifest_version = manifest.version();
        let max_leases = state.budget.max_leases_per_generation;
        if state.current.contains_key(&reader) {
            let root = {
                let entry = state
                    .current
                    .get_mut(&reader)
                    .ok_or(SemanticError::StaleGeneration)?;
                if entry.root.manifest != manifest_version {
                    return Err(SemanticError::ConflictingGeneration);
                }
                super::roots::increment_counter(&mut entry.leases, max_leases)?;
                entry.root
            };
            state.counters.reservations = state.counters.reservations.saturating_add(1);
            return Ok(SemanticReservation {
                owner: Arc::clone(&self.state),
                root,
                armed: true,
            });
        }
        if state.current.len() >= state.budget.max_live_generations {
            return Err(SemanticError::RetentionCapacity);
        }
        // A newly admitted candidate always owns its first lease.  Check the
        // per-generation envelope before registering anything so a zero lease
        // budget cannot leave an unreleaseable reader in the reverse index.
        if state.budget.max_leases_per_generation == 0 {
            return Err(SemanticError::RetentionCapacity);
        }
        let generation = state
            .last_generation
            .map_or(Some(SemanticGeneration::first()), SemanticGeneration::next)
            .ok_or(SemanticError::Overflow)?;
        state.readers.register_manifest(reader, manifest)?;
        let root = SemanticGenerationRoot {
            reader,
            generation,
            manifest: manifest_version,
            relation_root: state.readers.relation_state_root(),
        };
        state.current.insert(
            reader,
            GenerationEntry {
                root,
                leases: 1,
                pins: 0,
            },
        );
        state.last_generation = Some(generation);
        state.counters.reservations = state.counters.reservations.saturating_add(1);
        Ok(SemanticReservation {
            owner: Arc::clone(&self.state),
            root,
            armed: true,
        })
    }

    /// Explicitly retires a current generation.
    ///
    /// A rooted generation moves to the bounded retired set; an unrooted
    /// generation is reclaimed immediately.  The reverse reader index is
    /// removed in the same transition.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::RetentionCapacity`] without mutating state if
    /// a rooted retirement would exceed the configured retired envelope.
    pub fn retire(&self, root: SemanticGenerationRoot<K>) -> Result<bool, SemanticError> {
        let mut state = self.lock_state();
        retire_current(&mut state, root)
    }

    /// Invalidates all current readers selected by a versioned dependency
    /// change and retires the affected generations atomically.
    ///
    /// The reverse index computes its candidate set before this owner mutates
    /// any generation.  Capacity is checked for the complete set first, so a
    /// bounded failure cannot leave half an invalidation visible.
    ///
    /// # Errors
    ///
    /// Returns an admission, work-limit, or retention-capacity error while
    /// preserving the prior lifecycle state.
    pub fn invalidate_changes(
        &self,
        changes: &[DependencyChange],
    ) -> Result<GenerationInvalidation<K>, SemanticError> {
        self.invalidate_changes_budgeted(changes, InvalidationBudget::default())
    }

    /// Budgeted form of [`Self::invalidate_changes`].
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ReuseWorkLimit`] when reverse-index work
    /// exceeds `budget`, [`SemanticError::RetentionCapacity`] when rooted
    /// retirements exceed the configured envelope, or an admission error
    /// from the underlying semantic index.
    pub fn invalidate_changes_budgeted(
        &self,
        changes: &[DependencyChange],
        budget: InvalidationBudget,
    ) -> Result<GenerationInvalidation<K>, SemanticError> {
        let mut state = self.lock_state();
        let report = state.readers.invalidate_changes_budgeted(changes, budget)?;
        let roots = report
            .readers
            .iter()
            .filter_map(|reader| state.current.get(reader).map(|entry| entry.root))
            .collect::<Vec<_>>();
        let rooted_retirements = roots
            .iter()
            .filter(|root| {
                state
                    .current
                    .get(&root.reader)
                    .is_some_and(|entry| entry.has_roots())
            })
            .count();
        if state
            .retired
            .len()
            .checked_add(rooted_retirements)
            .is_none_or(|count| count > state.budget.max_retired_generations)
        {
            return Err(SemanticError::RetentionCapacity);
        }
        let mut retired = Vec::with_capacity(roots.len());
        for root in roots {
            if retire_current(&mut state, root)? {
                retired.push(root);
            }
        }
        state.counters.invalidation_batches = state.counters.invalidation_batches.saturating_add(1);
        state.counters.invalidated_generations = state
            .counters
            .invalidated_generations
            .saturating_add(u64::try_from(retired.len()).unwrap_or(u64::MAX));
        Ok(GenerationInvalidation {
            readers: report.readers,
            roots: retired.into_boxed_slice(),
            counters: report.counters,
        })
    }

    /// Verifies that a current-generation proof still names the selected
    /// generation.  Call this at the publication CAS boundary; checking a pin
    /// earlier and then publishing without rechecking would reintroduce a
    /// stale-completion race.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::StaleGeneration`] when the proof no longer
    /// names the reader's selected current generation.
    pub fn validate_current(&self, proof: &CurrentGeneration<K>) -> Result<(), SemanticError> {
        let state = self.lock_state();
        if state
            .current
            .get(&proof.root.reader)
            .is_some_and(|entry| entry.root == proof.root)
        {
            Ok(())
        } else {
            Err(SemanticError::StaleGeneration)
        }
    }

    /// Reclaims any unrooted generation left by a caller that released its
    /// last root through a non-standard lifecycle path.
    #[must_use]
    pub fn sweep(&self) -> usize {
        let mut state = self.lock_state();
        reclaim_unrooted(&mut state)
    }

    fn with_state<R>(&self, operation: impl FnOnce(&RetentionState<K>) -> R) -> R {
        let state = self.lock_state();
        operation(&state)
    }

    fn with_state_mut<R>(&self, operation: impl FnOnce(&mut RetentionState<K>) -> R) -> R {
        let mut state = self.lock_state();
        operation(&mut state)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, RetentionState<K>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
