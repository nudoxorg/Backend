use super::{
    AttestationVerifier, AuthorityEpoch, AuthorityFreshness, AuthorityTransition, AuthorityVersion,
    BTreeMap, CompleteSemanticCoverage, DispatchError, Dispatcher, OutputAdmissionValidator,
    Relation, RevocationVersion, SemanticCoverageValidator, SemanticInvalidationReport, authority,
};

impl<V, A> Dispatcher<V, A>
where
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    /// Records authority freshness only after a semantic claim has passed the
    /// mandatory validator. A newer admitted epoch or revocation observation
    /// immediately retires the prior reusable publication and its metadata.
    pub(crate) fn observe_semantic_freshness<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<(), DispatchError> {
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let mut accepted = self
            .accepted
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        self.apply_authority_freshness(
            identity.authority,
            identity.work_key(),
            semantic,
            &mut accepted,
        )
    }

    /// Admits an authority epoch/revocation transition and fences every
    /// retained publication and active semantic reservation indexed under the
    /// exact authority identity. Equal events are idempotent; rollback is
    /// rejected so an old worker cannot reopen a revoked publication.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn admit_authority_transition(
        &self,
        authority: AuthorityVersion,
        epoch: AuthorityEpoch,
        revocation_version: RevocationVersion,
    ) -> Result<AuthorityTransition, DispatchError> {
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let freshness = AuthorityFreshness {
            authority_epoch: epoch.0,
            revocation_version: revocation_version.0,
        };
        let previous = self
            .authority_transitions
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .get(&authority)
            .copied();
        if let Some(previous) = previous {
            if freshness.authority_epoch < previous.authority_epoch
                || freshness.revocation_version < previous.revocation_version
            {
                return Err(DispatchError::AuthorityRejected);
            }
            if freshness == previous {
                return Ok(AuthorityTransition {
                    authority,
                    epoch,
                    revocation_version,
                    affected: Box::new([]),
                });
            }
        }
        // Use the same bounded transition owner as semantic observations.
        // This permits inactive authority identities to be evicted after a
        // long stream of external revocation events while retaining every
        // transition that still indexes a live output or semantic lease.
        self.observe_authority_transition(authority, freshness)?;

        let mut affected = self
            .authority_index
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?
            .get(&authority)
            .cloned()
            .unwrap_or_default();
        affected.extend(
            self.active_semantic
                .lock()
                .map_err(|_| DispatchError::AuthorityRejected)?
                .iter()
                .filter_map(|(key, active)| (active.authority == authority).then_some(*key)),
        );
        for key in affected.iter().copied() {
            self.revoke_published_output_locked(key);
        }
        Ok(AuthorityTransition {
            authority,
            epoch,
            revocation_version,
            affected: affected.into_iter().collect(),
        })
    }

    /// Rechecks an already minted semantic capability against the current
    /// recipe/scope authority before a durable catalog lookup. This keeps the
    /// restart path inside the same trust boundary as ordinary completion;
    /// callers cannot use a deserialized claim or stale freshness tuple to
    /// authorize reuse.
    pub(crate) fn validate_semantic_for_reuse<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<(), DispatchError> {
        self.validate_semantic_capability(identity, semantic)?;
        self.observe_semantic_freshness(identity, semantic)
    }

    pub(super) fn apply_authority_freshness(
        &self,
        authority: AuthorityVersion,
        key: backend_execution::WorkKey,
        semantic: &CompleteSemanticCoverage,
        accepted: &mut BTreeMap<backend_execution::WorkKey, authority::AcceptedAuthority>,
    ) -> Result<(), DispatchError> {
        let freshness = AuthorityFreshness {
            authority_epoch: semantic.authority_epoch().0,
            revocation_version: semantic.revocation_version().0,
        };
        self.observe_authority_transition(authority, freshness)?;
        let mut current = self
            .authority_freshness
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        // `admit_semantic` may be called before a plan exists.  Do not turn
        // that one-shot validation observation into a permanent per-work
        // metadata row: freshness is retained only while a publication or a
        // live attempt can use it.  The completion path calls this function
        // again after scheduling, at which point the active attempt keeps the
        // row through publication.
        if !current.contains_key(&key)
            && !accepted.contains_key(&key)
            && !self
                .scheduler
                .attempts()
                .current(key)
                .is_some_and(|lease| lease.state() == backend_execution::AttemptState::Active)
        {
            return Ok(());
        }
        if let Some(previous) = current.get(&key).copied() {
            if freshness.authority_epoch < previous.authority_epoch
                || freshness.revocation_version < previous.revocation_version
            {
                return Err(DispatchError::AuthorityRejected);
            }
            if freshness != previous {
                let removed = accepted.remove(&key);
                let semantic_generation = removed
                    .as_ref()
                    .and_then(|authority| authority.semantic_generation);
                if let Some(authority) = removed {
                    self.remove_authority_index(authority.authority, key);
                }
                let _ = self.scheduler.output_lookup().invalidate(key);
                // `completed` is keyed directly by the fixed-width work-key
                // bytes.  Remove the one row by key; retaining over the whole
                // bounded map turns an authority transition into O(N) work
                // and obscures the one-row lifecycle invariant.
                self.completed
                    .lock()
                    .map_err(|_| DispatchError::AuthorityRejected)?
                    .remove(&key.to_bytes());
                self.replayed.release_committed_for_key(key);
                if let Some(generation) = semantic_generation {
                    self.semantic.release(key, generation);
                }
                self.retire_active_semantic(key);
                self.retire_or_quarantine_attempt(key);
            }
        } else if current.len() >= authority::MAX_ACCEPTED_AUTHORITY_ENTRIES {
            return Err(DispatchError::AuthorityRejected);
        }
        current.insert(key, freshness);
        Ok(())
    }

    /// Records freshness observed through an engine-admitted semantic
    /// capability. A caller can present only a value accepted by the recipe
    /// authority, and an older value can never roll back a transition event.
    /// Unreferenced authority rows may be retired at the bound; referenced
    /// rows stay until their output/attempt lifecycle releases them.
    pub(super) fn observe_authority_transition(
        &self,
        authority: AuthorityVersion,
        freshness: AuthorityFreshness,
    ) -> Result<(), DispatchError> {
        let mut transitions = self
            .authority_transitions
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        if let Some(previous) = transitions.get(&authority).copied() {
            if freshness.authority_epoch < previous.authority_epoch
                || freshness.revocation_version < previous.revocation_version
            {
                return Err(DispatchError::AuthorityRejected);
            }
            if freshness == previous {
                return Ok(());
            }
        } else if transitions.len() >= authority::MAX_AUTHORITY_TRANSITIONS {
            let evict = transitions.keys().copied().find(|candidate| {
                let indexed = self
                    .authority_index
                    .lock()
                    .map_or(true, |index| index.contains_key(candidate));
                let active = self.active_semantic.lock().map_or(true, |active| {
                    active.values().any(|entry| entry.authority == *candidate)
                });
                !indexed && !active
            });
            let Some(evict) = evict else {
                return Err(DispatchError::AuthorityRejected);
            };
            transitions.remove(&evict);
        }
        transitions.insert(authority, freshness);
        Ok(())
    }

    pub(super) fn is_authority_quarantined(&self, key: backend_execution::WorkKey) -> bool {
        self.authority_quarantine
            .lock()
            .map_or(true, |quarantine| quarantine.contains_key(&key))
    }

    /// Records both sides of a deterministic disagreement and fences every
    /// publication/reuse owner for the work key. This method is called only
    /// while the dispatcher authority transaction is held.
    pub(crate) fn quarantine_authority_conflict(
        &self,
        key: backend_execution::WorkKey,
        first_statement: [u8; 32],
        first_output: backend_execution::OutputVersion,
        second_statement: [u8; 32],
        second_output: backend_execution::OutputVersion,
    ) {
        let evidence = authority::AuthorityConflictEvidence {
            first_statement,
            first_output: first_output.to_bytes(),
            second_statement,
            second_output: second_output.to_bytes(),
        };
        let mut quarantine = self
            .authority_quarantine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if quarantine.len() >= authority::MAX_QUARANTINED_AUTHORITY_ENTRIES
            && !quarantine.contains_key(&key)
            && let Some(oldest) = quarantine.keys().next().copied()
        {
            quarantine.remove(&oldest);
        }
        quarantine.insert(key, evidence);
        drop(quarantine);

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
        self.revoke_derived_output_proof(key);
        self.completed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key.to_bytes());
        self.replayed.release_committed_for_key(key);
        self.pending_authority.quarantine(key);
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

    pub(super) fn clear_authority_quarantine(&self, key: backend_execution::WorkKey) {
        self.authority_quarantine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
    }

    pub(super) fn remove_authority_index(
        &self,
        authority: AuthorityVersion,
        key: backend_execution::WorkKey,
    ) {
        let mut index = self
            .authority_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(keys) = index.get_mut(&authority) {
            keys.remove(&key);
            if keys.is_empty() {
                index.remove(&authority);
            }
        }
    }

    pub(super) fn record_authority_index(
        &self,
        authority: AuthorityVersion,
        key: backend_execution::WorkKey,
    ) {
        self.authority_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(authority)
            .or_default()
            .insert(key);
    }

    /// Retires authority, replay, completion, and semantic metadata for cache
    /// entries evicted by one scheduler publication. Cleanup is deliberately
    /// infallible after the reusable entry is visible; poisoned bookkeeping
    /// locks recover their state rather than returning an ordinary error.
    pub(super) fn retire_evicted(
        &self,
        current: backend_execution::WorkKey,
        evicted: &[backend_execution::WorkKey],
        accepted: &mut BTreeMap<backend_execution::WorkKey, authority::AcceptedAuthority>,
    ) {
        let mut completed = self
            .completed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in evicted.iter().copied().filter(|key| *key != current) {
            let removed = accepted.remove(&key);
            let semantic_generation = removed
                .as_ref()
                .and_then(|authority| authority.semantic_generation);
            if let Some(authority) = removed {
                self.remove_authority_index(authority.authority, key);
            }
            let bytes = key.to_bytes();
            completed.remove(&bytes);
            self.replayed.release_committed_for_key(key);
            if let Some(generation) = semantic_generation {
                // A delayed eviction event must not unregister a replacement
                // manifest that has already claimed the same work key.
                self.semantic.release(key, generation);
            }
            self.retire_active_semantic(key);
            if let Ok(mut freshness) = self.authority_freshness.lock() {
                freshness.remove(&key);
            }
            // The reusable entry is the lifecycle owner for the terminal
            // attempt. Once it leaves the bounded lookup, no stale ordinal
            // needs to be retained by the execution attempt registry.
            let _ = self.scheduler.attempts().retire_and_forget(key);
            self.revoke_derived_output_proof(key);
        }
    }

    /// A freshness or semantic revocation can race either a terminal
    /// publication or a still-running route.  Terminal attempt rows are
    /// forgotten with their accepted metadata; active rows are quarantined so
    /// their old guard cannot publish after a replacement is admitted.
    pub(super) fn retire_or_quarantine_attempt(&self, key: backend_execution::WorkKey) {
        let Some(lease) = self.scheduler.attempts().current(key) else {
            return;
        };
        if lease.state().is_terminal() {
            let _ = self.scheduler.attempts().retire_and_forget(key);
        } else {
            let _ = self.scheduler.attempts().transition(
                key,
                lease.fence(),
                backend_execution::AttemptState::Quarantined,
            );
        }
    }

    /// Invalidates semantic readers and atomically revokes their reusable
    /// outputs and publication metadata.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn invalidate_semantic_changes(
        &self,
        changes: &[backend_semantic::DependencyChange],
    ) -> Result<SemanticInvalidationReport, DispatchError> {
        let _authority_transaction = self
            .authority_lock
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let mut accepted = self
            .accepted
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let mut completed = self
            .completed
            .lock()
            .map_err(|_| DispatchError::AuthorityRejected)?;
        let report = self
            .semantic
            .invalidate_changes(changes)
            .map_err(DispatchError::SemanticDependency)?;
        for key in report.work_keys().iter().copied() {
            let _ = self.scheduler.output_lookup().invalidate(key);
            if let Some(removed) = accepted.remove(&key) {
                self.remove_authority_index(removed.authority, key);
            }
            if let Ok(mut freshness) = self.authority_freshness.lock() {
                freshness.remove(&key);
            }
            let bytes = key.to_bytes();
            completed.remove(&bytes);
            self.replayed.release_committed_for_key(key);
            self.revoke_derived_output_proof(key);
            self.retire_active_semantic(key);
            self.retire_or_quarantine_attempt(key);
        }
        Ok(report)
    }
}
