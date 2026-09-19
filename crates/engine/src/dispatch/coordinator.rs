use super::{
    Arc, AttestationVerifier, BTreeMap, Budget, CompleteSemanticCoverage, DispatchError,
    Dispatcher, Mutex, OutputAdmissionValidator, Relation, RemoteAuthorityPolicy, ResultReceipt,
    Scheduler, SemanticCoverageValidator, SemanticDependencyCoordinator, authority, cost, fmt,
};

impl<V, A> fmt::Debug for Dispatcher<V, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dispatcher")
            .field("scheduler", &self.scheduler)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl<V, A> Dispatcher<V, A>
where
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    /// Creates a dispatcher. Validator and authority verifier capabilities are
    /// mandatory; there is no hash-only production default.
    #[must_use]
    pub fn new(budget: Budget, validator: V, authority: A, policy: RemoteAuthorityPolicy) -> Self {
        Self {
            scheduler: Scheduler::new(budget),
            validator,
            authority,
            policy,
            authority_lock: Mutex::new(()),
            accepted: Mutex::new(BTreeMap::new()),
            authority_quarantine: Mutex::new(BTreeMap::new()),
            authority_index: Mutex::new(BTreeMap::new()),
            authority_transitions: Mutex::new(BTreeMap::new()),
            authority_freshness: Mutex::new(BTreeMap::new()),
            completed: Arc::new(Mutex::new(BTreeMap::new())),
            pending_authority: Arc::new(authority::PendingAuthorityState::default()),
            replayed: Arc::new(authority::ReplayState::default()),
            semantic: Arc::new(SemanticDependencyCoordinator::new()),
            costs: cost::RouteCostModel::new(),
            derived_proofs: Mutex::new(BTreeMap::new()),
            active_semantic: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Creates a dispatcher from an already composed scheduler.
    #[must_use]
    pub fn with_scheduler(
        scheduler: Scheduler,
        validator: V,
        authority: A,
        policy: RemoteAuthorityPolicy,
    ) -> Self {
        Self {
            scheduler,
            validator,
            authority,
            policy,
            authority_lock: Mutex::new(()),
            accepted: Mutex::new(BTreeMap::new()),
            authority_quarantine: Mutex::new(BTreeMap::new()),
            authority_index: Mutex::new(BTreeMap::new()),
            authority_transitions: Mutex::new(BTreeMap::new()),
            authority_freshness: Mutex::new(BTreeMap::new()),
            completed: Arc::new(Mutex::new(BTreeMap::new())),
            pending_authority: Arc::new(authority::PendingAuthorityState::default()),
            replayed: Arc::new(authority::ReplayState::default()),
            semantic: Arc::new(SemanticDependencyCoordinator::new()),
            costs: cost::RouteCostModel::new(),
            derived_proofs: Mutex::new(BTreeMap::new()),
            active_semantic: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Returns the scheduler.
    #[must_use]
    pub const fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Returns whether a durable catalog entry admitted under this authority
    /// class may be hydrated under the current remote policy.  The policy is
    /// checked again on restart so an output accepted under a prior policy
    /// cannot silently become a local result after a downgrade or revocation.
    pub(crate) fn cached_authority_allowed(
        &self,
        class: backend_replication::AttestationClass,
    ) -> bool {
        matches!(
            class,
            backend_replication::AttestationClass::LocallyVerifiable
        ) || self.policy.allows(class)
    }

    /// Gives the owner one immutable proof snapshot so it can stage its CAS
    /// objects before the dispatch journal records `Accepted`.  The proof is
    /// intentionally retained until [`Self::publish_derived_output_retryable`]
    /// succeeds; a crash or a transient head race must leave a durable retry
    /// capability rather than silently dropping the catalog publication.
    pub(crate) fn stage_derived_output<F, T>(
        &self,
        key: backend_execution::WorkKey,
        stage: F,
    ) -> Result<Option<T>, DispatchError>
    where
        F: FnOnce(&crate::workspace::catalog::DerivedOutputProof) -> Result<T, DispatchError>,
    {
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let proof = self
            .derived_proofs
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .get(&key)
            .cloned();
        let Some(proof) = proof else {
            return Ok(None);
        };
        match stage(&proof) {
            Ok(staged) => Ok(Some(staged)),
            Err(error) => {
                self.revoke_published_output_locked_inner(key, false);
                Err(error)
            }
        }
    }

    /// Publishes a previously staged proof while retaining it on a transient
    /// owner error.  This is the journaled completion primitive: the caller
    /// appends `Accepted` before invoking it, so a failed publication can be
    /// retried after reopen against the exact same immutable proof.
    pub(crate) fn publish_derived_output_retryable<F>(
        &self,
        key: backend_execution::WorkKey,
        publish: F,
    ) -> Result<bool, DispatchError>
    where
        F: FnOnce(&crate::workspace::catalog::DerivedOutputProof) -> Result<(), DispatchError>,
    {
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let proof = self
            .derived_proofs
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .get(&key)
            .cloned();
        let Some(proof) = proof else {
            return Ok(false);
        };
        publish(&proof)?;
        self.derived_proofs
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .remove(&key);
        Ok(true)
    }

    pub(super) fn revoke_derived_output_proof(&self, key: backend_execution::WorkKey) {
        self.derived_proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
    }

    pub(super) fn prepare_derived_output_proof<R: Relation>(
        receipt: &ResultReceipt<R>,
        semantic: &CompleteSemanticCoverage,
        bytes: Arc<Vec<u8>>,
        dependency_generation: Option<u64>,
        authority_class: backend_replication::AttestationClass,
    ) -> Result<Option<crate::workspace::catalog::DerivedOutputProof>, DispatchError> {
        let Some(generation) = dependency_generation else {
            return Ok(None);
        };
        if semantic.dependency_manifest().is_none() {
            return Ok(None);
        }
        crate::workspace::catalog::DerivedOutputProof::from_receipt(
            receipt,
            semantic,
            bytes,
            generation,
            authority_class,
        )
        .map(Some)
        .map_err(|error| DispatchError::Workspace(error.to_string()))
    }

    pub(super) fn retain_derived_output_proof(
        &self,
        key: backend_execution::WorkKey,
        proof: Option<crate::workspace::catalog::DerivedOutputProof>,
    ) {
        let Some(proof) = proof else {
            return;
        };
        self.derived_proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, proof);
    }

    pub(super) fn reserve_derived_output_proof_slot(
        &self,
        key: backend_execution::WorkKey,
    ) -> Result<(), DispatchError> {
        let proofs = self
            .derived_proofs
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        if proofs.len() >= authority::MAX_ACCEPTED_AUTHORITY_ENTRIES && !proofs.contains_key(&key) {
            return Err(DispatchError::AuthorityRejected);
        }
        Ok(())
    }

    /// Revokes one accepted output and every scheduler/authority bridge
    /// associated with it after durable catalog publication fails.
    #[cfg(test)]
    pub(crate) fn revoke_published_output(&self, key: backend_execution::WorkKey) {
        let _authority_transaction = self
            .authority_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.revoke_published_output_locked(key);
    }

    pub(super) fn revoke_published_output_locked(&self, key: backend_execution::WorkKey) {
        self.revoke_published_output_locked_inner(key, true);
    }

    pub(super) fn revoke_published_output_locked_inner(
        &self,
        key: backend_execution::WorkKey,
        revoke_proof: bool,
    ) {
        let removed = self
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        let semantic_generation = removed
            .as_ref()
            .and_then(|accepted| accepted.semantic_generation);
        if let Some(accepted) = removed {
            self.remove_authority_index(accepted.authority, key);
        }
        let _ = self.scheduler.output_lookup().invalidate(key);
        if revoke_proof {
            self.revoke_derived_output_proof(key);
        }
        self.completed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key.to_bytes());
        self.replayed.release_committed_for_key(key);
        if let Some(generation) = semantic_generation {
            self.semantic.release(key, generation);
        }
        self.retire_active_semantic(key);
        self.authority_freshness
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        self.retire_or_quarantine_attempt(key);
    }

    /// Returns the execution-owned semantic dependency lifecycle coordinator.
    #[must_use]
    pub fn semantic_dependencies(&self) -> &SemanticDependencyCoordinator {
        &self.semantic
    }
}
