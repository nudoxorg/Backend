//! Compact-root durable control operations.
//!
//! A selected control workspace keeps one authenticated relation root and a
//! small versioned scheduler summary.  Reopening therefore reads a bounded
//! closure-index page and one relation node; lifecycle mutations use the
//! backend-version lazy path copier and publish only the changed frontier.

use std::collections::BTreeSet;
use std::convert::TryInto;
use std::sync::{Arc, Mutex};

use backend_store::{FileStore, SelectedHead, StorePublicationAuthority, TypedObject};
use backend_version::{
    BasisBinding, IdContext, LazyTree, LazyTreeError, ObjectKey, ObjectVersion, PersistedTreeRoot,
    RelationBinding, Schema, UntrustedId, WorkspaceManifest,
};

use crate::ControlError;
use crate::ids::{
    AgentWorkKey, ControlSummary, ControlSummarySchema, FenceSchema, Identity, OwnerSchema,
    WorkKeySchema,
};
use crate::ids::{EvidenceSchema, OutputSchema};
use crate::ledger::{
    ActiveLeaseMarker, Admission, Candidate, ControlCommit, DecisionResult, Evaluated,
    EvaluationResult, Frozen, Held, Lease, PlanResult, Renewed, ReviewResult, Reviewed,
    SchedulerLimits, VersionedControlPlane, control_coverage, ensure_fence, fence_identity,
};
use crate::record::{
    AttemptData, ControlRelation, ControlRoot, CustodyVerdict, RecordKey, WorkRecord, WorkStatus,
};
use crate::spec::WorkSpec;

use super::AUTHORITY_VALUE;
use super::closure::make_root_workspace_closure_from_root;

/// A bounded status page from the selected control relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LazyControlPage {
    /// Rows in canonical work-key order.
    records: Vec<WorkRecord>,
    /// Cursor for the next page, when rows remain.
    next: Option<AgentWorkKey>,
}

impl LazyControlPage {
    pub(crate) fn into_parts(self) -> (Vec<WorkRecord>, Option<AgentWorkKey>) {
        (self.records, self.next)
    }
}

/// Durable control state backed by a compact relation root.
#[derive(Clone, Debug)]
pub(crate) struct LazyControlPlane {
    store: FileStore,
    authority: StorePublicationAuthority,
    root: PersistedTreeRoot<ControlRelation>,
    coverage: backend_version::CoverageWitness,
    summary: ControlSummary,
    limits: SchedulerLimits,
    layout: backend_store::LayoutId,
    pack: backend_store::PackId,
    // Cloned handles share this gate so two local callers cannot prepare
    // against the same relation root and then publish one over the other.
    publication_gate: Arc<Mutex<()>>,
}

impl LazyControlPlane {
    /// Creates the uncommitted genesis view and writes its empty root node.
    pub(crate) fn new(
        store: FileStore,
        authority: StorePublicationAuthority,
        layout: backend_store::LayoutId,
        pack: backend_store::PackId,
        limits: SchedulerLimits,
    ) -> Result<Self, ControlError> {
        let eager = VersionedControlPlane::new(limits)?;
        let root = persisted_root_from_state(eager.state())?;
        store.write_relation_state(eager.state())?;
        Ok(Self {
            store,
            authority,
            root,
            coverage: eager.state().coverage(),
            summary: eager.summary()?,
            limits: eager.limits(),
            layout,
            pack,
            publication_gate: Arc::new(Mutex::new(())),
        })
    }

    /// Reopens a compact root-only workspace in O(1) rows.
    pub(crate) fn open(
        store: FileStore,
        authority: StorePublicationAuthority,
        head: &SelectedHead,
        layout: backend_store::LayoutId,
        pack: backend_store::PackId,
        limits: SchedulerLimits,
    ) -> Result<Self, ControlError> {
        let descriptor = head.descriptor();
        let binding = descriptor.workspace().ok_or(ControlError::Corrupt)?;
        if binding.root() != &descriptor.target() || binding.closure() != descriptor.closure() {
            return Err(ControlError::Corrupt);
        }
        let manifest = store.open_closure(descriptor.closure())?;
        let page = manifest.page(None, 8)?;
        if page.next().is_some() || page.objects().len() != 3 {
            return Err(ControlError::Corrupt);
        }
        let relation_schema = backend_version::SchemaIdentity::of_relation::<ControlRelation>();
        let summary_schema = backend_version::SchemaIdentity::new(
            ControlSummarySchema::DOMAIN,
            ControlSummarySchema::TYPE,
            ControlSummarySchema::VERSION,
        );
        let authority_schema = backend_version::SchemaIdentity::new(
            crate::ids::ControlAuthoritySchema::DOMAIN,
            crate::ids::ControlAuthoritySchema::TYPE,
            crate::ids::ControlAuthoritySchema::VERSION,
        );
        let relation = page
            .objects()
            .iter()
            .find(|object| object.schema() == relation_schema)
            .ok_or(ControlError::Corrupt)?;
        let summary_object = page
            .objects()
            .iter()
            .find(|object| object.schema() == summary_schema)
            .ok_or(ControlError::Corrupt)?;
        let authority_object = page
            .objects()
            .iter()
            .find(|object| object.schema() == authority_schema)
            .ok_or(ControlError::Corrupt)?;
        let expected_authority_key =
            ObjectKey::<crate::ids::ControlAuthoritySchema>::from_value(&AUTHORITY_VALUE);
        let expected_authority = TypedObject::from_value(&expected_authority_key, &AUTHORITY_VALUE);
        if authority_object != &expected_authority {
            return Err(ControlError::Corrupt);
        }
        let claim = UntrustedId::<ControlRelation>::from_wire(
            relation.version(),
            IdContext::relation::<ControlRelation>(),
        )
        .map_err(|_| ControlError::Corrupt)?;
        let root =
            PersistedTreeRoot::admit(claim, relation.bytes()).map_err(|_| ControlError::Corrupt)?;
        let summary = ControlSummary::decode(summary_object.bytes())?;
        let summary_claim = UntrustedId::<ControlSummarySchema>::from_wire(
            summary_object.version(),
            IdContext::schema::<ControlSummarySchema>(),
        )
        .map_err(|_| ControlError::Corrupt)?;
        if ObjectVersion::admit_value(summary_claim, &summary).is_err() {
            return Err(ControlError::Corrupt);
        }
        let coverage = control_coverage()?;
        let semantic_authority =
            ObjectVersion::<crate::ids::ControlAuthoritySchema>::from_value(&AUTHORITY_VALUE);
        let reconstructed = WorkspaceManifest::from_versions(
            1,
            vec![RelationBinding::from_persisted_root(&root, coverage)],
            vec![BasisBinding::from_version(ObjectVersion::<
                ControlSummarySchema,
            >::from_value(
                &summary
            ))],
            semantic_authority,
            coverage,
        )?;
        if reconstructed.root().as_bytes() != &descriptor.target()
            || reconstructed.root().as_bytes() != binding.root()
        {
            return Err(ControlError::Corrupt);
        }
        Ok(Self {
            store,
            authority,
            root,
            coverage,
            summary,
            limits: limits_checked(limits)?,
            layout,
            pack,
            publication_gate: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) const fn root(&self) -> ControlRoot {
        self.root.root()
    }

    pub(crate) fn page(
        &self,
        after: Option<AgentWorkKey>,
        limit: usize,
    ) -> Result<LazyControlPage, ControlError> {
        let tree = self.tree();
        let after_key = after.map(RecordKey::from_work);
        let page = tree
            .page(after_key.as_ref(), limit)
            .map_err(|error| lazy_error(&error))?;
        let records = page
            .entries()
            .iter()
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        let next = page.next().and(records.last().map(WorkRecord::key));
        Ok(LazyControlPage { records, next })
    }

    pub(crate) fn get(&self, key: AgentWorkKey) -> Result<Option<WorkRecord>, ControlError> {
        self.tree()
            .lookup(&RecordKey::from_work(key))
            .map_err(|error| lazy_error(&error))
    }

    pub(super) fn lookup_work_key(&self, bytes: &[u8]) -> Result<AgentWorkKey, ControlError> {
        let bytes: [u8; backend_version::ID_BYTES] = bytes
            .try_into()
            .map_err(|_| ControlError::InvalidDigestLength)?;
        let claim =
            UntrustedId::<WorkKeySchema>::from_wire(&bytes, IdContext::schema::<WorkKeySchema>())
                .map_err(|_| ControlError::InvalidDigestLength)?;
        let record = self
            .tree()
            .lookup(&RecordKey::from_bytes(bytes))
            .map_err(|error| lazy_error(&error))?
            .ok_or(ControlError::UnknownWork)?;
        if record.spec().key().as_bytes() != claim.as_bytes() {
            return Err(ControlError::Corrupt);
        }
        Ok(record.spec().key())
    }

    pub(super) fn lease_for(
        &self,
        key: AgentWorkKey,
        fence: &[u8],
    ) -> Result<Lease<Held>, ControlError> {
        let record = self.get(key)?.ok_or(ControlError::UnknownWork)?;
        if record.status() != WorkStatus::Running {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = record.attempt_data().ok_or(ControlError::StaleFence)?;
        verify_fence(&attempt, fence)?;
        Ok(Lease::from_parts(key, attempt))
    }

    pub(super) fn frozen_for(
        &self,
        key: AgentWorkKey,
        fence: &[u8],
    ) -> Result<Lease<Frozen>, ControlError> {
        let record = self.get(key)?.ok_or(ControlError::UnknownWork)?;
        if record.status() != WorkStatus::Frozen {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = record.attempt_data().ok_or(ControlError::StaleFence)?;
        verify_fence(&attempt, fence)?;
        Ok(Lease::from_parts(key, attempt))
    }

    pub(super) fn evaluated_for(
        &self,
        key: AgentWorkKey,
        receipt: &[u8],
    ) -> Result<Candidate<Evaluated>, ControlError> {
        let record = self.get(key)?.ok_or(ControlError::UnknownWork)?;
        if record.status() != WorkStatus::Evaluated {
            return Err(ControlError::InvalidTransition);
        }
        let evaluation = record.evaluation().ok_or(ControlError::Corrupt)?;
        if evaluation.id().as_bytes() != receipt {
            return Err(ControlError::StaleRoot);
        }
        Ok(Candidate::from_parts(key, evaluation.id()))
    }

    pub(super) fn reviewed_for(
        &self,
        key: AgentWorkKey,
        receipt: &[u8],
    ) -> Result<Candidate<Reviewed>, ControlError> {
        let record = self.get(key)?.ok_or(ControlError::UnknownWork)?;
        if record.status() != WorkStatus::Reviewed {
            return Err(ControlError::InvalidTransition);
        }
        let review = record.review().ok_or(ControlError::Corrupt)?;
        if review.id().as_bytes() != receipt {
            return Err(ControlError::StaleRoot);
        }
        Ok(Candidate::from_parts(key, review.id()))
    }

    pub(crate) fn plan(&mut self, spec: WorkSpec) -> Result<PlanResult, ControlError> {
        let key = spec.key();
        let existing = self.get(key)?;
        let Some(existing) = existing else {
            let record = WorkRecord::planned(spec);
            return self.replace(key, None, &record).map(PlanResult::Admitted);
        };
        if existing.spec() != &spec {
            return Err(ControlError::WorkKeyCollision);
        }
        if existing.status().is_reusable() {
            return existing
                .receipt()
                .cloned()
                .map(PlanResult::Reused)
                .ok_or(ControlError::Corrupt);
        }
        Ok(PlanResult::Existing {
            status: existing.status(),
            waiters: existing.waiter_count(),
        })
    }

    pub(crate) fn admit(
        &mut self,
        key: AgentWorkKey,
        owner: Identity<OwnerSchema>,
        now: u64,
    ) -> Result<(Admission, Option<ControlCommit>), ControlError> {
        let existing = self.get(key)?.ok_or(ControlError::UnknownWork)?;
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
            let commit = self.replace(key, Some(&existing), &updated)?;
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
            let commit = self.replace(key, Some(&existing), &updated)?;
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
        if !self.dependencies_ready(existing.spec())? {
            let mut updated = existing.clone();
            updated.set_status(WorkStatus::Waiting);
            updated.clear_attempt();
            let commit = self.replace(key, Some(&existing), &updated)?;
            return Ok((Admission::Waiting, Some(commit)));
        }
        if self.active_count()? >= usize::from(self.limits.max_parallel) {
            let mut updated = existing.clone();
            updated.set_status(WorkStatus::Queued);
            updated.clear_attempt();
            let commit = self.replace(key, Some(&existing), &updated)?;
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
        let commit = self.replace(key, Some(&existing), &updated)?;
        Ok((
            Admission::Owned(Lease::from_parts(key, attempt)),
            Some(commit),
        ))
    }

    pub(crate) fn renew<S: ActiveLeaseMarker>(
        &mut self,
        lease: &Lease<S>,
        now: u64,
    ) -> Result<(Lease<Renewed>, ControlCommit), ControlError> {
        let existing = self.get(lease.key())?.ok_or(ControlError::UnknownWork)?;
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
        let renewed_attempt = AttemptData::new(
            attempt.owner_identity().clone(),
            attempt.epoch(),
            attempt.fence_identity().clone(),
            expires_at,
            attempt.attempt(),
        );
        let mut updated = existing.clone();
        updated.set_attempt(Some(renewed_attempt.clone()));
        let commit = self.replace(lease.key(), Some(&existing), &updated)?;
        Ok((Lease::from_parts(lease.key(), renewed_attempt), commit))
    }

    pub(crate) fn freeze<S: ActiveLeaseMarker>(
        &mut self,
        lease: &Lease<S>,
        output: Identity<OutputSchema>,
        evidence: Identity<EvidenceSchema>,
        now: u64,
    ) -> Result<(Lease<Frozen>, ControlCommit), ControlError> {
        let existing = self.get(lease.key())?.ok_or(ControlError::UnknownWork)?;
        let prepared = crate::custody::freeze(
            &existing,
            lease,
            output,
            evidence,
            now,
            self.limits.lease_ttl_ns,
        )?;
        let commit = self.replace(lease.key(), Some(&existing), &prepared.record)?;
        Ok((prepared.lease, commit))
    }

    pub(crate) fn evaluate(
        &mut self,
        lease: &Lease<Frozen>,
        evaluator: Identity<OwnerSchema>,
        verdict: CustodyVerdict,
        evidence: Identity<EvidenceSchema>,
        now: u64,
    ) -> Result<EvaluationResult, ControlError> {
        let existing = self.get(lease.key())?.ok_or(ControlError::UnknownWork)?;
        crate::custody::evaluate(&existing, lease, evaluator, verdict, evidence, now)?
            .publish_with(|record| self.replace(lease.key(), Some(&existing), &record))
    }

    pub(crate) fn review(
        &mut self,
        candidate: &Candidate<Evaluated>,
        reviewer: Identity<OwnerSchema>,
        verdict: CustodyVerdict,
        evidence: Identity<EvidenceSchema>,
    ) -> Result<ReviewResult, ControlError> {
        let existing = self
            .get(candidate.key())?
            .ok_or(ControlError::UnknownWork)?;
        crate::custody::review(&existing, candidate, reviewer, verdict, evidence)?
            .publish_with(|record| self.replace(candidate.key(), Some(&existing), &record))
    }

    pub(crate) fn decide(
        &mut self,
        candidate: &Candidate<Reviewed>,
        sol: Identity<OwnerSchema>,
        verdict: CustodyVerdict,
        evidence: Identity<EvidenceSchema>,
    ) -> Result<DecisionResult, ControlError> {
        let existing = self
            .get(candidate.key())?
            .ok_or(ControlError::UnknownWork)?;
        crate::custody::decide(&existing, candidate, sol, verdict, evidence)?
            .publish_with(|record| self.replace(candidate.key(), Some(&existing), &record))
    }

    pub(crate) fn fail<S: ActiveLeaseMarker>(
        &mut self,
        lease: &Lease<S>,
    ) -> Result<ControlCommit, ControlError> {
        let existing = self.get(lease.key())?.ok_or(ControlError::UnknownWork)?;
        if !existing.status().is_active() {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
        ensure_fence(&attempt, lease)?;
        let mut updated = existing.clone();
        updated.set_status(WorkStatus::Failed);
        self.replace(lease.key(), Some(&existing), &updated)
    }

    pub(crate) fn cancel<S: ActiveLeaseMarker>(
        &mut self,
        lease: &Lease<S>,
    ) -> Result<ControlCommit, ControlError> {
        let existing = self.get(lease.key())?.ok_or(ControlError::UnknownWork)?;
        if !existing.status().is_active() {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
        ensure_fence(&attempt, lease)?;
        let mut updated = existing.clone();
        updated.set_status(WorkStatus::Cancelled);
        updated.clear_attempt();
        self.replace(lease.key(), Some(&existing), &updated)
    }

    pub(crate) fn recover(
        &mut self,
        now: u64,
    ) -> Result<(Vec<AgentWorkKey>, Option<ControlCommit>), ControlError> {
        let mut after = None;
        let mut expired = Vec::new();
        let mut rows = Vec::new();
        loop {
            let page = self.page(after, 256)?;
            if page.records.is_empty() {
                break;
            }
            after = page.next;
            for record in page.records {
                if record.status().has_expiring_lease()
                    && record
                        .attempt_data()
                        .is_some_and(|attempt| now >= attempt.expires_at())
                {
                    expired.push(record.key());
                    let mut updated = record.clone();
                    updated.set_status(match record.status() {
                        WorkStatus::Running => WorkStatus::Expired,
                        WorkStatus::Frozen => WorkStatus::Quarantined,
                        _ => return Err(ControlError::Corrupt),
                    });
                    if record.status() == WorkStatus::Frozen {
                        updated.clear_attempt();
                    }
                    rows.push((record.key(), record, updated));
                }
            }
            if after.is_none() {
                break;
            }
        }
        let mut last = None;
        for (key, before, after) in rows {
            last = Some(self.replace(key, Some(&before), &after)?);
        }
        Ok((expired, last))
    }

    pub(crate) fn invalidate_dependency(
        &mut self,
        dependency: AgentWorkKey,
    ) -> Result<Option<ControlCommit>, ControlError> {
        // Materialize only the bounded row values needed for this graph walk;
        // relation nodes themselves remain lazy.  The reverse index is not
        // persisted in the selected closure, so recover the transitive slice
        // once and then publish each changed row through the normal exact
        // base/CAS path.
        let mut after = None;
        let mut records = Vec::new();
        loop {
            let page = self.page(after, 256)?;
            if page.records.is_empty() {
                break;
            }
            after = page.next;
            records.extend(page.records);
            if after.is_none() {
                break;
            }
        }
        let mut pending = vec![dependency];
        let mut visited = BTreeSet::new();
        let mut changed_rows = BTreeSet::new();
        let mut impacted = Vec::new();
        while let Some(changed) = pending.pop() {
            if !visited.insert(changed) {
                continue;
            }
            for record in &records {
                if !record.spec().dependencies().any(|key| key == changed) {
                    continue;
                }
                pending.push(record.key());
                if !changed_rows.insert(RecordKey::from_work(record.key()))
                    || matches!(
                        record.status(),
                        WorkStatus::Cancelled | WorkStatus::Invalidated | WorkStatus::Rejected
                    )
                {
                    continue;
                }
                let mut updated = record.clone();
                updated.set_status(WorkStatus::Invalidated);
                updated.clear_attempt();
                updated.clear_receipt();
                updated.clear_evaluation();
                updated.clear_review();
                updated.clear_decision();
                impacted.push((record.key(), record.clone(), updated));
            }
        }
        let mut last = None;
        for (key, before, after) in impacted {
            last = Some(self.replace(key, Some(&before), &after)?);
        }
        Ok(last)
    }

    fn tree(&self) -> LazyTree<'_, ControlRelation, FileStore> {
        LazyTree::from_admitted(&self.store, self.root.clone())
    }

    fn dependencies_ready(&self, spec: &WorkSpec) -> Result<bool, ControlError> {
        for dependency in spec.dependencies() {
            if !self
                .get(dependency)?
                .is_some_and(|record| record.status().is_reusable())
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn active_count(&self) -> Result<usize, ControlError> {
        usize::try_from(self.summary.active()).map_err(|_| ControlError::Bounds)
    }

    fn replace(
        &mut self,
        key: AgentWorkKey,
        before: Option<&WorkRecord>,
        after: &WorkRecord,
    ) -> Result<ControlCommit, ControlError> {
        after.canonical_bytes()?;
        let record_key = RecordKey::from_work(key);
        let tree = self.tree();
        let update = match before {
            Some(_) => tree
                .prepare_replace(&record_key, after.clone())
                .map_err(|error| lazy_error(&error))?,
            None => tree
                .prepare_insert(&record_key, after.clone())
                .map_err(|error| lazy_error(&error))?,
        };
        let delta = update.delta();
        let summary = next_summary(self.summary, before, Some(after))?;
        self.publish(&update, summary)?;
        Ok(ControlCommit::from_delta(delta))
    }

    fn publish(
        &mut self,
        update: &backend_version::LazyPreparedUpdate<ControlRelation>,
        summary: ControlSummary,
    ) -> Result<(), ControlError> {
        let _gate = self
            .publication_gate
            .lock()
            .map_err(|_| ControlError::Corrupt)?;
        if update.base() != self.root.root() {
            return Err(ControlError::StaleRoot);
        }
        let head = self.store.head()?;
        let base = if let Some(head) = head {
            // The selected workspace must describe this handle's exact
            // relation root and summary.  Otherwise a cloned or restarted
            // handle would publish a path copy prepared from an obsolete
            // state under a fresh workspace base.
            let (current_manifest, _) =
                make_root_workspace_closure_from_root(&self.root, self.coverage, self.summary)?;
            if head.descriptor().target() != current_manifest.root().to_bytes() {
                return Err(ControlError::StaleRoot);
            }
            Some(head.as_base())
        } else {
            None
        };
        self.store.write_lazy_relation_update(update)?;
        let target = update.target_root();
        let (manifest, closure) =
            make_root_workspace_closure_from_root(&target, self.coverage, summary)?;
        let prepared = self.store.prepare_workspace_publication(
            manifest.root(),
            self.layout,
            self.pack,
            closure,
            base,
        )?;
        let published = prepared.durable()?.publish_with_authority(&self.authority);
        if let Err(error) = published {
            // The store can report a post-journal sync failure after the
            // selected head has already advanced.  Re-read the authoritative
            // head before returning so this handle never continues from an
            // obsolete root and accidentally prepares a second transition.
            let selected = self.store.head().map_err(ControlError::from)?;
            if selected.is_some_and(|head| head.descriptor().target() == manifest.root().to_bytes())
            {
                self.root = target;
                self.summary = summary;
                return Ok(());
            }
            return Err(ControlError::from(error));
        }
        self.root = target;
        self.summary = summary;
        Ok(())
    }
}

fn persisted_root_from_state(
    state: &backend_version::RelationState<ControlRelation>,
) -> Result<PersistedTreeRoot<ControlRelation>, ControlError> {
    let object = state.materialize();
    let claim = UntrustedId::<ControlRelation>::from_wire(
        object.root().as_bytes(),
        IdContext::relation::<ControlRelation>(),
    )
    .map_err(|_| ControlError::Corrupt)?;
    PersistedTreeRoot::admit(claim, object.canonical_bytes()).map_err(|_| ControlError::Corrupt)
}

fn limits_checked(limits: SchedulerLimits) -> Result<SchedulerLimits, ControlError> {
    limits.validate()
}

fn verify_fence(attempt: &AttemptData, fence: &[u8]) -> Result<(), ControlError> {
    let claim = UntrustedId::<FenceSchema>::from_wire(fence, IdContext::schema::<FenceSchema>())
        .map_err(|_| ControlError::InvalidDigestLength)?;
    if claim.as_bytes() != attempt.fence().as_bytes() {
        return Err(ControlError::StaleFence);
    }
    Ok(())
}

fn next_summary(
    current: ControlSummary,
    before: Option<&WorkRecord>,
    after: Option<&WorkRecord>,
) -> Result<ControlSummary, ControlError> {
    let active = adjust(
        current.active(),
        before.is_some_and(|record| record.status().is_active()),
        after.is_some_and(|record| record.status().is_active()),
    )?;
    let ready = adjust(
        current.ready(),
        before.is_some_and(|record| {
            matches!(record.status(), WorkStatus::Ready | WorkStatus::Queued)
        }),
        after.is_some_and(|record| {
            matches!(record.status(), WorkStatus::Ready | WorkStatus::Queued)
        }),
    )?;
    Ok(ControlSummary::new(active, ready))
}

fn adjust(current: u64, was: bool, is: bool) -> Result<u64, ControlError> {
    match (was, is) {
        (false, true) => current.checked_add(1).ok_or(ControlError::Bounds),
        (true, false) => current.checked_sub(1).ok_or(ControlError::Corrupt),
        _ => Ok(current),
    }
}

fn lazy_error(error: &LazyTreeError<backend_store::StoreError>) -> ControlError {
    ControlError::Store(format!("{error:?}"))
}
