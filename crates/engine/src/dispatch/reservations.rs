use super::{
    Arc, AuthorityVersion, BTreeMap, CompleteSemanticCoverage, Dispatcher, Mutex, dependencies,
};
use backend_execution::WorkKey;

/// Preflighted completion metadata reservation.  Insertion happens before
/// scheduler publication; dropping after a scheduler error rolls it back,
/// while committing is a local infallible state flip.
pub(crate) struct CompletionReservation {
    pub(super) completed: Arc<Mutex<BTreeMap<[u8; 32], CompletionIdentity>>>,
    pub(super) id: CompletionIdentity,
    /// The prior terminal capability for this work key, if any. A new
    /// attempt may reserve the same key only to replace that row; if a later
    /// scheduler/authority check fails, restoring the prior row keeps the
    /// reservation operation transactional instead of erasing a still-live
    /// publication.
    pub(super) previous: Option<CompletionIdentity>,
    pub(super) committed: bool,
}

/// Rolls back an authority-freshness observation when a scheduled completion
/// fails before the accepted publication row is installed. Freshness is
/// publication metadata, so retaining a row for a cancelled/failed route
/// would consume bounded authority capacity without a corresponding output
/// lease.
pub(crate) struct FreshnessRollback<'a, V, A> {
    dispatcher: &'a Dispatcher<V, A>,
    key: WorkKey,
    previous: Option<AuthorityFreshness>,
    committed: bool,
}

impl<'a, V, A> FreshnessRollback<'a, V, A> {
    pub(super) fn new(
        dispatcher: &'a Dispatcher<V, A>,
        key: WorkKey,
        previous: Option<AuthorityFreshness>,
    ) -> Self {
        Self {
            dispatcher,
            key,
            previous,
            committed: false,
        }
    }

    pub(super) fn commit(&mut self) {
        self.committed = true;
    }
}

impl<V, A> Drop for FreshnessRollback<'_, V, A> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // Completion holds the accepted map while applying the freshness
        // transition (accepted -> freshness).  Observe that map first here
        // as well; taking freshness before accepted would let a failed
        // completion deadlock an invalidation racing the same authority
        // transaction.
        let accepted_is_live = self
            .dispatcher
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&self.key);
        let mut freshness = self
            .dispatcher
            .authority_freshness
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if accepted_is_live {
            if let Some(previous) = self.previous {
                freshness.insert(self.key, previous);
            } else {
                freshness.remove(&self.key);
            }
        } else {
            // A failed completion cannot leave freshness metadata behind when
            // no accepted publication owns it. In particular, a newer
            // authority observation may already have retired the old row;
            // restoring that stale tuple would block future progress.
            freshness.remove(&self.key);
        }
    }
}

impl CompletionReservation {
    pub(super) fn commit(mut self) {
        let mut completed = self
            .completed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Retain one live completion capability per work key. A replacement
        // publication overwrites the prior attempt/fence notice at this
        // commit point, so the bounded map cannot grow on repeated retries.
        completed.insert(self.id.work_key, self.id);
        self.committed = true;
    }
}

impl Drop for CompletionReservation {
    fn drop(&mut self) {
        if !self.committed {
            let mut completed = self
                .completed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if completed
                .get(&self.id.work_key)
                .is_some_and(|identity| *identity == self.id)
            {
                match self.previous {
                    Some(previous) => {
                        completed.insert(self.id.work_key, previous);
                    }
                    None => {
                        completed.remove(&self.id.work_key);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CompletionIdentity {
    pub(super) work_key: [u8; 32],
    pub(super) output: [u8; 32],
    pub(super) ordinal: u32,
    pub(super) fence: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AuthorityFreshness {
    pub(super) authority_epoch: u64,
    pub(super) revocation_version: u64,
}

/// Active manifest admission retained for the lifetime of a scheduled route.
/// The reverse-index reservation is intentionally affine: if the route never
/// publishes, dropping this value unregisters the reader; after publication,
/// the completion path commits the reservation and replaces it with the
/// accepted publication's generation.
pub(crate) struct ActiveSemantic {
    pub(super) reservation: dependencies::SemanticDependencyReservation,
    pub(super) authority: AuthorityVersion,
    manifest: [u8; 32],
    identity: [u8; 32],
    scope: u64,
    read_manifest: [u8; 32],
    authority_epoch: u64,
    revocation_version: u64,
}

impl ActiveSemantic {
    pub(super) fn new(
        reservation: dependencies::SemanticDependencyReservation,
        semantic: &CompleteSemanticCoverage,
    ) -> Self {
        Self {
            reservation,
            authority: semantic.authority(),
            manifest: semantic
                .dependency_manifest()
                .map_or([0; 32], |manifest| manifest.version().to_bytes()),
            identity: semantic.identity().to_bytes(),
            scope: semantic.scope(),
            read_manifest: semantic.read_manifest().to_bytes(),
            authority_epoch: semantic.authority_epoch().0,
            revocation_version: semantic.revocation_version().0,
        }
    }

    pub(super) fn matches(&self, semantic: &CompleteSemanticCoverage) -> bool {
        self.manifest
            == semantic
                .dependency_manifest()
                .map_or([0; 32], |manifest| manifest.version().to_bytes())
            && self.identity == semantic.identity().to_bytes()
            && self.scope == semantic.scope()
            && self.read_manifest == semantic.read_manifest().to_bytes()
            && self.authority_epoch == semantic.authority_epoch().0
            && self.revocation_version == semantic.revocation_version().0
    }

    pub(super) fn generation(&self) -> u64 {
        self.reservation.generation()
    }
}

impl<V, A> Dispatcher<V, A> {
    pub(super) fn retire_active_semantic(&self, key: WorkKey) {
        let removed = self
            .active_semantic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        // Drop outside the map lock: releasing a semantic reservation takes
        // the coordinator lock, and no lock-order inversion should be
        // possible if invalidation is running concurrently.
        drop(removed);
    }

    pub(super) fn commit_active_semantic(&self, key: WorkKey, generation: Option<u64>) {
        let removed = self.take_active_semantic_if_generation(key, generation);
        if let Some(mut active) = removed {
            active.reservation.commit();
        }
    }

    pub(super) fn retire_active_semantic_if_generation(
        &self,
        key: WorkKey,
        generation: Option<u64>,
    ) {
        let removed = self.take_active_semantic_if_generation(key, generation);
        drop(removed);
    }

    pub(super) fn take_active_semantic_if_generation(
        &self,
        key: WorkKey,
        generation: Option<u64>,
    ) -> Option<ActiveSemantic> {
        let generation = generation?;
        let mut active = self
            .active_semantic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active
            .get(&key)
            .is_some_and(|candidate| candidate.generation() == generation)
        {
            active.remove(&key)
        } else {
            None
        }
    }
}

/// Removes a live semantic registration if an affine completion path fails
/// after consuming its scheduled plan. The guard is committed only after the
/// accepted publication row and scheduler lookup are visible.
pub(crate) struct ActiveSemanticRollback<'a, V, A> {
    dispatcher: &'a Dispatcher<V, A>,
    key: WorkKey,
    generation: Option<u64>,
    committed: bool,
}

impl<'a, V, A> ActiveSemanticRollback<'a, V, A> {
    pub(super) fn new(dispatcher: &'a Dispatcher<V, A>, key: WorkKey) -> Self {
        let generation = dispatcher
            .active_semantic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(ActiveSemantic::generation);
        Self {
            dispatcher,
            key,
            generation,
            committed: false,
        }
    }

    pub(super) fn commit(&mut self) {
        self.dispatcher
            .commit_active_semantic(self.key, self.generation);
        self.committed = true;
    }

    pub(super) fn keep(mut self) {
        // Ownership moves into the ticket/plan that remains live after the
        // admission function returns. Its completion or cancellation path
        // will perform the eventual commit/release.
        self.committed = true;
    }
}

impl<V, A> Drop for ActiveSemanticRollback<'_, V, A> {
    fn drop(&mut self) {
        if !self.committed {
            self.dispatcher
                .retire_active_semantic_if_generation(self.key, self.generation);
        }
    }
}
