use super::super::index::SemanticReaderKey;
use super::model::{
    CurrentGeneration, GenerationInvalidation, RetentionState, SemanticGeneration,
    SemanticGenerationRoot, VersionedRetention,
};
use crate::SemanticError;
use std::fmt;
use std::sync::{Arc, Mutex};

impl<K: SemanticReaderKey> GenerationInvalidation<K> {
    /// Returns affected readers in the reverse index's canonical order.
    #[must_use]
    pub fn readers(&self) -> &[K] {
        &self.readers
    }

    /// Returns the exact generations retired by this transition.
    #[must_use]
    pub fn roots(&self) -> &[SemanticGenerationRoot<K>] {
        &self.roots
    }

    /// Returns whether one reader was invalidated.
    #[must_use]
    pub fn contains(&self, reader: K) -> bool {
        self.readers.binary_search(&reader).is_ok()
    }
}

/// A pending semantic registration lease.
#[must_use = "drop to roll back an uncommitted semantic reservation"]
pub struct SemanticReservation<K: SemanticReaderKey> {
    pub(super) owner: Arc<Mutex<RetentionState<K>>>,
    pub(super) root: SemanticGenerationRoot<K>,
    pub(super) armed: bool,
}

impl<K: SemanticReaderKey> fmt::Debug for SemanticReservation<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticReservation")
            .field("root", &self.root)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

impl<K: SemanticReaderKey> SemanticReservation<K> {
    /// Returns the private generation root held by this lease.
    #[must_use]
    pub const fn root(&self) -> SemanticGenerationRoot<K> {
        self.root
    }

    /// Returns the lifecycle generation held by this lease.
    #[must_use]
    pub const fn generation(&self) -> SemanticGeneration {
        self.root.generation
    }

    /// Commits the reservation into a pinned current result.
    ///
    /// A reservation that was invalidated before commit is consumed and
    /// returns [`SemanticError::StaleGeneration`].  It cannot be converted
    /// into a proof for a retired result.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::StaleGeneration`] when invalidation or an
    /// earlier release retired this reservation, or
    /// [`SemanticError::RetentionCapacity`] when the pin envelope is full.
    pub fn commit(mut self) -> Result<SemanticPin<K>, SemanticError> {
        let mut state = self
            .owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let max_pins = state.budget.max_pins_per_generation;
        let Some(existing) = state.current.get(&self.root.reader).copied() else {
            release_lease_from_state(&mut state, self.root);
            reclaim_root(&mut state, self.root);
            self.armed = false;
            state.counters.stale_completions = state.counters.stale_completions.saturating_add(1);
            return Err(SemanticError::StaleGeneration);
        };
        if existing.root != self.root {
            release_lease_from_state(&mut state, self.root);
            reclaim_root(&mut state, self.root);
            self.armed = false;
            state.counters.stale_completions = state.counters.stale_completions.saturating_add(1);
            return Err(SemanticError::StaleGeneration);
        }
        if existing.leases == 0 {
            self.armed = false;
            state.counters.stale_completions = state.counters.stale_completions.saturating_add(1);
            return Err(SemanticError::StaleGeneration);
        }
        if existing.pins >= max_pins {
            if let Some(entry) = state.current.get_mut(&self.root.reader) {
                entry.leases -= 1;
            }
            self.armed = false;
            reclaim_root(&mut state, self.root);
            state.counters.stale_completions = state.counters.stale_completions.saturating_add(1);
            return Err(SemanticError::RetentionCapacity);
        }
        if let Some(entry) = state.current.get_mut(&self.root.reader) {
            entry.leases -= 1;
            entry.pins += 1;
        }
        self.armed = false;
        state.counters.commits = state.counters.commits.saturating_add(1);
        Ok(SemanticPin {
            owner: Arc::clone(&self.owner),
            root: self.root,
            armed: true,
        })
    }
}

impl<K: SemanticReaderKey> Drop for SemanticReservation<K> {
    fn drop(&mut self) {
        if self.armed {
            release_lease(&self.owner, self.root);
            self.armed = false;
        }
    }
}

/// A result pin that keeps a generation available for reads and publication
/// validation.
#[must_use = "retain the pin while a result may still be observed"]
pub struct SemanticPin<K: SemanticReaderKey> {
    owner: Arc<Mutex<RetentionState<K>>>,
    root: SemanticGenerationRoot<K>,
    armed: bool,
}

impl<K: SemanticReaderKey> fmt::Debug for SemanticPin<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticPin")
            .field("root", &self.root)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

impl<K: SemanticReaderKey> SemanticPin<K> {
    /// Returns the immutable lifecycle root protected by this pin.
    #[must_use]
    pub const fn root(&self) -> SemanticGenerationRoot<K> {
        self.root
    }

    /// Returns a current-generation proof after checking the owner state.
    ///
    /// The returned proof is a snapshot used with
    /// [`VersionedRetention::validate_current`] at the final publication
    /// boundary.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::StaleGeneration`] when this pin no longer
    /// names the reader's selected current generation.
    pub fn current(&self) -> Result<CurrentGeneration<K>, SemanticError> {
        let state = self
            .owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .current
            .get(&self.root.reader)
            .is_some_and(|entry| entry.root == self.root)
        {
            Ok(CurrentGeneration { root: self.root })
        } else {
            Err(SemanticError::StaleGeneration)
        }
    }

    /// Checks this pin at the publication boundary.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::StaleGeneration`] when the pin or its
    /// publication snapshot is no longer current.
    pub fn validate(&self) -> Result<(), SemanticError> {
        let owner = VersionedRetention {
            state: Arc::clone(&self.owner),
        };
        let proof = self.current()?;
        owner.validate_current(&proof)
    }
}

impl<K: SemanticReaderKey> Drop for SemanticPin<K> {
    fn drop(&mut self) {
        if self.armed {
            release_pin(&self.owner, self.root);
            self.armed = false;
        }
    }
}

impl<K: SemanticReaderKey> CurrentGeneration<K> {
    /// Returns the generation root carried by this proof.
    #[must_use]
    pub const fn root(self) -> SemanticGenerationRoot<K> {
        self.root
    }
}

pub(super) fn increment_counter(counter: &mut u32, maximum: u32) -> Result<(), SemanticError> {
    let next = counter.checked_add(1).ok_or(SemanticError::Overflow)?;
    if next > maximum {
        return Err(SemanticError::RetentionCapacity);
    }
    *counter = next;
    Ok(())
}

pub(super) fn reclaim_unrooted<K: SemanticReaderKey>(state: &mut RetentionState<K>) -> usize {
    let current = state
        .current
        .iter()
        .filter_map(|(reader, entry)| (!entry.has_roots()).then_some(*reader))
        .collect::<Vec<_>>();
    let mut reclaimed = 0;
    for reader in current {
        if state.current.remove(&reader).is_some() {
            state.readers.unregister_reader(reader);
            reclaimed += 1;
        }
    }
    let retired = state
        .retired
        .iter()
        .filter_map(|(root, entry)| (!entry.has_roots()).then_some(*root))
        .collect::<Vec<_>>();
    for root in retired {
        if state.retired.remove(&root).is_some() {
            reclaimed += 1;
        }
    }
    state.counters.reclaimed_generations = state
        .counters
        .reclaimed_generations
        .saturating_add(u64::try_from(reclaimed).unwrap_or(u64::MAX));
    reclaimed
}

pub(super) fn retire_current<K: SemanticReaderKey>(
    state: &mut RetentionState<K>,
    root: SemanticGenerationRoot<K>,
) -> Result<bool, SemanticError> {
    let Some(entry) = state.current.get(&root.reader).copied() else {
        return Ok(false);
    };
    if entry.root != root {
        return Ok(false);
    }
    if entry.has_roots() && state.retired.len() >= state.budget.max_retired_generations {
        return Err(SemanticError::RetentionCapacity);
    }
    state.current.remove(&root.reader);
    state.readers.unregister_reader(root.reader);
    if entry.has_roots() {
        state.retired.insert(root, entry);
    }
    state.counters.retirements = state.counters.retirements.saturating_add(1);
    Ok(true)
}

fn release_lease<K: SemanticReaderKey>(
    owner: &Arc<Mutex<RetentionState<K>>>,
    root: SemanticGenerationRoot<K>,
) {
    let mut state = owner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    release_lease_from_state(&mut state, root);
    reclaim_root(&mut state, root);
}

fn release_lease_from_state<K: SemanticReaderKey>(
    state: &mut RetentionState<K>,
    root: SemanticGenerationRoot<K>,
) {
    if let Some(entry) = state.current.get_mut(&root.reader)
        && entry.root == root
        && entry.leases != 0
    {
        entry.leases -= 1;
        return;
    }
    if let Some(entry) = state.retired.get_mut(&root)
        && entry.leases != 0
    {
        entry.leases -= 1;
    }
}

fn release_pin<K: SemanticReaderKey>(
    owner: &Arc<Mutex<RetentionState<K>>>,
    root: SemanticGenerationRoot<K>,
) {
    let mut state = owner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let released_current = if let Some(entry) = state.current.get_mut(&root.reader) {
        if entry.root == root && entry.pins != 0 {
            entry.pins -= 1;
            true
        } else {
            false
        }
    } else {
        false
    };
    if !released_current
        && let Some(entry) = state.retired.get_mut(&root)
        && entry.pins != 0
    {
        entry.pins -= 1;
    }
    reclaim_root(&mut state, root);
}

/// Reclaims one generation after its last affine lease or pin is released.
///
/// The normal lifecycle always knows the released root, so it can update the
/// two ordered ownership maps directly.  Keeping the full-map sweep in
/// [`reclaim_unrooted`] for explicit diagnostics means dropping a lease does
/// not accidentally turn into O(number of readers) work.
fn reclaim_root<K: SemanticReaderKey>(
    state: &mut RetentionState<K>,
    root: SemanticGenerationRoot<K>,
) {
    if state
        .current
        .get(&root.reader)
        .is_some_and(|entry| entry.root == root && !entry.has_roots())
    {
        state.current.remove(&root.reader);
        state.readers.unregister_reader(root.reader);
        state.counters.reclaimed_generations =
            state.counters.reclaimed_generations.saturating_add(1);
        return;
    }
    if state
        .retired
        .get(&root)
        .is_some_and(|entry| !entry.has_roots())
    {
        state.retired.remove(&root);
        state.counters.reclaimed_generations =
            state.counters.reclaimed_generations.saturating_add(1);
    }
}
