//! Lifecycle transition kernels for the typed work relation.

use core::marker::PhantomData;
use std::collections::BTreeSet;

use backend_version::{MapChange, apply_delta, prepare_delta_with_state};

use crate::ControlError;
use crate::ids::{AgentWorkKey, EvidenceSchema, Identity, OutputSchema, OwnerSchema};
use crate::record::{
    AttemptData, ControlRelation, CustodyVerdict, DurableReceipt, WorkRecord, WorkStatus,
};
use crate::spec::WorkSpec;

use super::{
    ActiveLeaseMarker, Admission, Candidate, ControlCommit, ControlDelta, DecisionResult,
    Evaluated, EvaluationResult, Frozen, Lease, PlanResult, Renewed, ReviewResult, Reviewed,
    VersionedControlPlane, ensure_fence, fence_identity,
};

impl VersionedControlPlane {
    /// Admits a new immutable specification or observes an existing one.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::WorkKeyCollision`] when an existing key carries
    /// different immutable specification bytes.
    pub fn plan(&self, spec: WorkSpec) -> Result<PlanResult, ControlError> {
        let key = spec.key();
        let record_key = crate::record::RecordKey::from_work(key);
        if let Some(existing) = self.state.get(&record_key) {
            if existing.spec() != &spec {
                return Err(ControlError::WorkKeyCollision);
            }
            if existing.status().is_reusable() {
                if let Some(receipt) = existing.receipt() {
                    return Ok(PlanResult::Reused(receipt.clone()));
                }
                return Err(ControlError::Corrupt);
            }
            return Ok(PlanResult::Existing {
                status: existing.status(),
                waiters: existing.waiter_count(),
            });
        }
        let record = WorkRecord::planned(spec);
        Ok(PlanResult::Admitted(
            self.replace_record(record_key, None, record)?,
        ))
    }

    /// Performs an exact reusable lookup without changing relation state.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::WorkKeyCollision`] when the key is associated
    /// with different specification bytes.
    pub fn reusable(&self, spec: &WorkSpec) -> Result<Option<DurableReceipt>, ControlError> {
        let key = spec.key();
        let Some(record) = self.get(key) else {
            return Ok(None);
        };
        if record.spec() != spec {
            return Err(ControlError::WorkKeyCollision);
        }
        Ok(record
            .receipt()
            .filter(|_| record.status().is_reusable())
            .cloned())
    }

    /// Acquires a bounded fenced attempt for a planned or retryable row.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, dependency, bounds, retry, or waiter error when
    /// admission cannot produce a valid transition.
    pub fn admit(
        &self,
        key: AgentWorkKey,
        owner: Identity<OwnerSchema>,
        now: u64,
    ) -> Result<(Admission, Option<ControlCommit>), ControlError> {
        let record_key = crate::record::RecordKey::from_work(key);
        let Some(existing) = self.state.get(&record_key) else {
            return Err(ControlError::UnknownWork);
        };
        if existing.status().is_reusable() {
            return existing
                .receipt()
                .cloned()
                .map(|receipt| (Admission::Reused(receipt), None))
                .ok_or(ControlError::Corrupt);
        }
        if existing.status().is_active() {
            let mut updated = existing.clone();
            let waiters = updated.increment_waiters_limit(self.limits.max_waiters)?;
            let commit = self.replace_record(record_key, Some(existing), updated)?;
            return Ok((
                Admission::Coalesced {
                    status: existing.status(),
                    waiters,
                },
                Some(commit),
            ));
        }
        if matches!(
            existing.status(),
            WorkStatus::Frozen | WorkStatus::Evaluated | WorkStatus::Reviewed
        ) {
            let mut updated = existing.clone();
            let waiters = updated.increment_waiters_limit(self.limits.max_waiters)?;
            let commit = self.replace_record(record_key, Some(existing), updated)?;
            return Ok((
                Admission::Coalesced {
                    status: existing.status(),
                    waiters,
                },
                Some(commit),
            ));
        }
        if existing.status().is_terminal() {
            return Err(ControlError::InvalidTransition);
        }
        if !self.dependencies_ready(existing.spec()) {
            let mut updated = existing.clone();
            updated.set_status(WorkStatus::Waiting);
            updated.clear_attempt();
            let commit = self.replace_record(record_key, Some(existing), updated)?;
            return Ok((Admission::Waiting, Some(commit)));
        }
        if self.active_count() >= usize::from(self.limits.max_parallel) {
            let mut updated = existing.clone();
            updated.set_status(WorkStatus::Queued);
            updated.clear_attempt();
            let commit = self.replace_record(record_key, Some(existing), updated)?;
            return Ok((Admission::Queued, Some(commit)));
        }
        if existing.attempts() >= self.limits.max_attempts {
            return Err(ControlError::RetryLimit);
        }
        let mut updated = existing.clone();
        let attempt_no = updated.increment_attempts()?;
        let epoch = existing
            .epoch()
            .checked_add(1)
            .ok_or(ControlError::Bounds)?;
        let fence = fence_identity(key, &owner, epoch)?;
        let expires_at = now
            .checked_add(self.limits.lease_ttl_ns)
            .ok_or(ControlError::Bounds)?;
        let attempt = AttemptData::new(owner, epoch, fence, expires_at, attempt_no);
        updated.set_epoch(epoch);
        updated.set_attempt(Some(attempt.clone()));
        updated.set_status(WorkStatus::Running);
        let commit = self.replace_record(record_key, Some(existing), updated)?;
        Ok((
            Admission::Owned(Lease {
                key,
                attempt,
                marker: PhantomData,
            }),
            Some(commit),
        ))
    }

    /// Renews a currently held lease against the exact current fence.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, expiry, lifecycle, bounds, or transition error.
    pub fn renew<S: ActiveLeaseMarker>(
        &self,
        lease: &Lease<S>,
        now: u64,
    ) -> Result<(Lease<Renewed>, ControlCommit), ControlError> {
        let (record_key, existing) = self.current_attempt(lease.key)?;
        if !existing.status().is_active() {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
        ensure_fence(&attempt, lease)?;
        if now >= attempt.expires_at() {
            return Err(ControlError::LeaseExpired);
        }
        let expires_at = now
            .checked_add(self.limits.lease_ttl_ns)
            .ok_or(ControlError::Bounds)?;
        let renewed = AttemptData::new(
            attempt.owner_identity().clone(),
            attempt.epoch(),
            attempt.fence_identity().clone(),
            expires_at,
            attempt.attempt(),
        );
        let mut updated = existing.clone();
        updated.set_attempt(Some(renewed.clone()));
        let commit = self.replace_record(record_key, Some(existing), updated)?;
        Ok((
            Lease {
                key: lease.key,
                attempt: renewed,
                marker: PhantomData,
            },
            commit,
        ))
    }

    /// Freezes a candidate and mints a receipt bound to its exact fence.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, expiry, lifecycle, bounds, or identity error.
    pub fn freeze<S: ActiveLeaseMarker>(
        &self,
        lease: &Lease<S>,
        output: Identity<OutputSchema>,
        evidence: Identity<EvidenceSchema>,
        now: u64,
    ) -> Result<(Lease<Frozen>, ControlCommit), ControlError> {
        let (record_key, existing) = self.current_attempt(lease.key)?;
        let prepared = crate::custody::freeze(
            existing,
            lease,
            output,
            evidence,
            now,
            self.limits.lease_ttl_ns,
        )?;
        let commit = self.replace_record(record_key, Some(existing), prepared.record)?;
        Ok((prepared.lease, commit))
    }

    /// Applies an independent evaluator verdict to a frozen candidate.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence, lifecycle, or independence error.
    pub fn evaluate(
        &self,
        lease: &Lease<Frozen>,
        evaluator: Identity<OwnerSchema>,
        verdict: CustodyVerdict,
        evidence: Identity<EvidenceSchema>,
        now: u64,
    ) -> Result<EvaluationResult, ControlError> {
        let (record_key, existing) = self.current_attempt(lease.key)?;
        crate::custody::evaluate(existing, lease, evaluator, verdict, evidence, now)?
            .publish_with(|record| self.replace_record(record_key, Some(existing), record))
    }

    /// Applies an independent reviewer verdict to an accepted evaluation.
    ///
    /// # Errors
    ///
    /// Returns a stale-custody, lifecycle, or independence error.
    pub fn review(
        &self,
        candidate: &Candidate<Evaluated>,
        reviewer: Identity<OwnerSchema>,
        verdict: CustodyVerdict,
        evidence: Identity<EvidenceSchema>,
    ) -> Result<ReviewResult, ControlError> {
        let record_key = crate::record::RecordKey::from_work(candidate.key());
        let existing = self
            .state
            .get(&record_key)
            .ok_or(ControlError::UnknownWork)?;
        crate::custody::review(existing, candidate, reviewer, verdict, evidence)?
            .publish_with(|record| self.replace_record(record_key, Some(existing), record))
    }

    /// Applies Sol's final decision to an independently reviewed chain.
    ///
    /// # Errors
    ///
    /// Returns a stale-custody, lifecycle, or independence error.
    pub fn decide(
        &self,
        candidate: &Candidate<Reviewed>,
        sol: Identity<OwnerSchema>,
        verdict: CustodyVerdict,
        evidence: Identity<EvidenceSchema>,
    ) -> Result<DecisionResult, ControlError> {
        let record_key = crate::record::RecordKey::from_work(candidate.key());
        let existing = self
            .state
            .get(&record_key)
            .ok_or(ControlError::UnknownWork)?;
        crate::custody::decide(existing, candidate, sol, verdict, evidence)?
            .publish_with(|record| self.replace_record(record_key, Some(existing), record))
    }

    /// Records a bounded owner failure while releasing the active fence.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence or lifecycle error.
    pub fn fail<S: ActiveLeaseMarker>(
        &self,
        lease: &Lease<S>,
    ) -> Result<ControlCommit, ControlError> {
        let (record_key, existing) = self.current_attempt(lease.key)?;
        if !existing.status().is_active() {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
        ensure_fence(&attempt, lease)?;
        let mut updated = existing.clone();
        updated.set_status(WorkStatus::Failed);
        self.replace_record(record_key, Some(existing), updated)
    }

    /// Cancels an active attempt and releases its scheduler slot.
    ///
    /// # Errors
    ///
    /// Returns a stale-fence or lifecycle error.
    pub fn cancel<S: ActiveLeaseMarker>(
        &self,
        lease: &Lease<S>,
    ) -> Result<ControlCommit, ControlError> {
        let (record_key, existing) = self.current_attempt(lease.key)?;
        if !existing.status().is_active() {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
        ensure_fence(&attempt, lease)?;
        let mut updated = existing.clone();
        updated.set_status(WorkStatus::Cancelled);
        updated.clear_attempt();
        self.replace_record(record_key, Some(existing), updated)
    }

    /// Expires running implementation leases and quarantines candidates whose
    /// independent review lease elapsed. The returned keys let a scheduler
    /// wake dependents without rescanning the relation.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Corrupt`] if an expiry index disagrees with
    /// the canonical relation.
    pub fn recover(
        &self,
        now: u64,
    ) -> Result<(Vec<AgentWorkKey>, Option<ControlCommit>), ControlError> {
        let mut changes = Vec::new();
        let mut keys = Vec::new();
        let expired_keys = self.projection.expired_keys(now).collect::<Vec<_>>();
        for record_key in expired_keys {
            let Some(existing) = self.state.get(&record_key) else {
                return Err(ControlError::Corrupt);
            };
            if !existing.status().has_expiring_lease() {
                continue;
            }
            let Some(attempt) = existing.attempt_data() else {
                return Err(ControlError::Corrupt);
            };
            if now < attempt.expires_at() {
                continue;
            }
            let mut updated = existing.clone();
            updated.set_status(match existing.status() {
                WorkStatus::Running => WorkStatus::Expired,
                WorkStatus::Frozen => WorkStatus::Quarantined,
                _ => return Err(ControlError::Corrupt),
            });
            if existing.status() == WorkStatus::Frozen {
                updated.clear_attempt();
            }
            keys.push(existing.spec().key());
            changes.push(MapChange {
                key: record_key,
                before: Some(existing.clone()),
                after: Some(updated),
            });
        }
        if changes.is_empty() {
            return Ok((keys, None));
        }
        Ok((keys, Some(self.apply_changes(changes)?)))
    }

    /// Invalidates rows that directly depend on a changed prerequisite.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Corrupt`] if a reverse-index entry disagrees
    /// with the canonical relation.
    pub fn invalidate_dependency(
        &self,
        dependency: AgentWorkKey,
    ) -> Result<Option<ControlCommit>, ControlError> {
        let mut changes = Vec::new();
        // Walk the reverse dependency relation to a fixed point.  A single
        // changed prerequisite invalidates the complete transitive slice;
        // stopping after one edge would leave a grandchild reusing a receipt
        // whose own prerequisite has already been revoked.
        let mut pending = vec![dependency];
        let mut visited = BTreeSet::new();
        let mut changed_rows = BTreeSet::new();
        while let Some(changed) = pending.pop() {
            if !visited.insert(changed) {
                continue;
            }
            let impacted = self.projection.dependent_keys(changed).collect::<Vec<_>>();
            for record_key in impacted {
                let Some(existing) = self.state.get(&record_key) else {
                    return Err(ControlError::Corrupt);
                };
                if !existing.spec().dependencies().any(|key| key == changed) {
                    return Err(ControlError::Corrupt);
                }
                // Even a row already marked invalid/rejected can have
                // dependents, so it must still be part of the graph walk.
                pending.push(existing.key());
                if !changed_rows.insert(record_key) {
                    continue;
                }
                if matches!(
                    existing.status(),
                    WorkStatus::Cancelled | WorkStatus::Invalidated | WorkStatus::Rejected
                ) {
                    continue;
                }
                let mut updated = existing.clone();
                updated.set_status(WorkStatus::Invalidated);
                updated.clear_attempt();
                updated.clear_receipt();
                updated.clear_evaluation();
                updated.clear_review();
                updated.clear_decision();
                changes.push(MapChange {
                    key: record_key,
                    before: Some(existing.clone()),
                    after: Some(updated),
                });
            }
        }
        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.apply_changes(changes)?))
    }

    fn dependencies_ready(&self, spec: &WorkSpec) -> bool {
        spec.dependencies().all(|dependency| {
            self.get(dependency)
                .is_some_and(|record| record.status().is_reusable())
        })
    }

    fn active_count(&self) -> usize {
        self.projection.active_count()
    }

    pub(super) fn current_attempt(
        &self,
        key: AgentWorkKey,
    ) -> Result<(crate::record::RecordKey, &WorkRecord), ControlError> {
        let record_key = crate::record::RecordKey::from_work(key);
        let existing = self
            .state
            .get(&record_key)
            .ok_or(ControlError::UnknownWork)?;
        Ok((record_key, existing))
    }

    /// Applies an exact prepared transition to this mutable view.
    ///
    /// The transition carries its complete before root and every before value;
    /// a stale caller therefore receives [`ControlError::StaleRoot`] without
    /// partially changing the relation.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::StaleRoot`] for an old base and
    /// [`ControlError::Corrupt`] when the delta's target or canonical state is
    /// inconsistent.
    pub fn apply(&mut self, commit: ControlCommit) -> Result<(), ControlError> {
        if self.root() != commit.base || commit.delta.base() != commit.base {
            return Err(ControlError::StaleRoot);
        }
        let state = apply_delta(&self.state, &commit.delta.inner)?;
        if state.root() != commit.target || commit.delta.target() != commit.target {
            return Err(ControlError::Corrupt);
        }
        self.state = state;
        self.projection = commit.projection;
        Ok(())
    }

    fn replace_record(
        &self,
        key: crate::record::RecordKey,
        before: Option<&WorkRecord>,
        after: WorkRecord,
    ) -> Result<ControlCommit, ControlError> {
        let changes = vec![MapChange {
            key,
            before: before.cloned(),
            after: Some(after),
        }];
        self.apply_changes(changes)
    }

    fn apply_changes(
        &self,
        mut changes: Vec<MapChange<ControlRelation>>,
    ) -> Result<ControlCommit, ControlError> {
        changes.sort_unstable_by_key(|change| change.key);
        if changes
            .windows(2)
            .any(|window| window[0].key == window[1].key)
        {
            return Err(ControlError::Corrupt);
        }
        for change in &changes {
            if let Some(after) = &change.after {
                // `ControlRelation::encode_value` is deliberately infallible
                // because the version kernel accepts only an encoder.  Keep
                // that promise honest by validating every replacement before
                // it reaches the unchecked relation encoder.
                after.canonical_bytes()?;
            }
        }
        let mut projection = self.projection.clone();
        for change in &changes {
            projection.apply_change(change.key, change.before.as_ref(), change.after.as_ref());
        }
        let (prepared, _) = prepare_delta_with_state(&self.state, changes)?;
        let delta = prepared.delta().clone();
        let base = delta.base();
        let target = delta.target();
        Ok(ControlCommit {
            base,
            target,
            delta: ControlDelta { inner: delta },
            projection,
        })
    }
}
