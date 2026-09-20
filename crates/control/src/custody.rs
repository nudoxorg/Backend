//! Pure candidate-custody transition preparation shared by memory and disk.
//!
//! Storage adapters contribute only lookup and exact-base publication. Every
//! authority, fence, stage-order, and identity rule lives here once, so the
//! eager reference plane and lazy durable plane cannot drift semantically.

use crate::ControlError;
use crate::ids::{EvidenceSchema, Identity, OutputSchema, OwnerSchema};
use crate::ledger::{
    ActiveLeaseMarker, Candidate, CandidateStage, ControlCommit, DecisionResult, Evaluated, Frozen,
    Lease, Reviewed, StageResult, ensure_fence,
};
use crate::record::{
    AttemptData, AttemptOutcome, Authority, CustodyVerdict, DecisionReceipt, DurableReceipt,
    EvaluationReceipt, ReviewReceipt, WorkRecord, WorkStatus,
};

pub(crate) struct PreparedFreeze {
    pub(crate) record: WorkRecord,
    pub(crate) lease: Lease<Frozen>,
}

pub(crate) struct PreparedStage<S: CandidateStage> {
    pub(crate) record: WorkRecord,
    status: WorkStatus,
    candidate: Option<Candidate<S>>,
}

impl<S: CandidateStage> PreparedStage<S> {
    pub(crate) fn publish_with(
        self,
        publish: impl FnOnce(WorkRecord) -> Result<ControlCommit, ControlError>,
    ) -> Result<StageResult<S>, ControlError> {
        let Self {
            record,
            status,
            candidate,
        } = self;
        let commit = publish(record)?;
        Ok(match candidate {
            Some(candidate) => StageResult::Advanced { candidate, commit },
            None => StageResult::Terminal { status, commit },
        })
    }
}

pub(crate) struct PreparedDecision {
    pub(crate) record: WorkRecord,
    status: WorkStatus,
    receipt: DecisionReceipt,
}

impl PreparedDecision {
    pub(crate) fn publish_with(
        self,
        publish: impl FnOnce(WorkRecord) -> Result<ControlCommit, ControlError>,
    ) -> Result<DecisionResult, ControlError> {
        let Self {
            record,
            status,
            receipt,
        } = self;
        let commit = publish(record)?;
        Ok(DecisionResult {
            status,
            receipt,
            commit,
        })
    }
}

pub(crate) fn freeze<S: ActiveLeaseMarker>(
    existing: &WorkRecord,
    lease: &Lease<S>,
    output: Identity<OutputSchema>,
    evidence: Identity<EvidenceSchema>,
    now: u64,
    lease_ttl_ns: u64,
) -> Result<PreparedFreeze, ControlError> {
    if existing.status() != WorkStatus::Running {
        return Err(ControlError::InvalidTransition);
    }
    let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
    ensure_fence(&attempt, lease)?;
    if now >= attempt.expires_at() {
        return Err(ControlError::LeaseExpired);
    }
    let receipt = DurableReceipt::new(
        lease.key(),
        attempt.owner_identity().clone(),
        attempt.fence_identity().clone(),
        Authority::Controller,
        output,
        evidence,
        AttemptOutcome::Succeeded,
    )?;
    let review_expires_at = now.checked_add(lease_ttl_ns).ok_or(ControlError::Bounds)?;
    let frozen_attempt = AttemptData::new(
        attempt.owner_identity().clone(),
        attempt.epoch(),
        attempt.fence_identity().clone(),
        review_expires_at,
        attempt.attempt(),
    );
    let mut record = existing.clone();
    record.set_receipt(receipt);
    record.set_attempt(Some(frozen_attempt.clone()));
    record.set_status(WorkStatus::Frozen);
    Ok(PreparedFreeze {
        record,
        lease: Lease::from_parts(lease.key(), frozen_attempt),
    })
}

pub(crate) fn evaluate(
    existing: &WorkRecord,
    lease: &Lease<Frozen>,
    evaluator: Identity<OwnerSchema>,
    verdict: CustodyVerdict,
    evidence: Identity<EvidenceSchema>,
    now: u64,
) -> Result<PreparedStage<Evaluated>, ControlError> {
    if existing.status() != WorkStatus::Frozen {
        return Err(ControlError::InvalidTransition);
    }
    let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
    ensure_fence(&attempt, lease)?;
    if now >= attempt.expires_at() {
        return Err(ControlError::LeaseExpired);
    }
    if evaluator.id() == attempt.owner() {
        return Err(ControlError::AuthorityNotIndependent);
    }
    let receipt = existing.receipt().ok_or(ControlError::Corrupt)?;
    let evaluation =
        EvaluationReceipt::new(lease.key(), receipt.id(), evaluator, verdict, evidence)?;
    let status = status_after(verdict, WorkStatus::Evaluated);
    let candidate = (status == WorkStatus::Evaluated)
        .then(|| Candidate::from_parts(lease.key(), evaluation.id()));
    let mut record = existing.clone();
    record.set_evaluation(evaluation);
    record.set_status(status);
    record.clear_attempt();
    Ok(PreparedStage {
        record,
        status,
        candidate,
    })
}

pub(crate) fn review(
    existing: &WorkRecord,
    candidate: &Candidate<Evaluated>,
    reviewer: Identity<OwnerSchema>,
    verdict: CustodyVerdict,
    evidence: Identity<EvidenceSchema>,
) -> Result<PreparedStage<Reviewed>, ControlError> {
    if existing.status() != WorkStatus::Evaluated {
        return Err(ControlError::InvalidTransition);
    }
    let receipt = existing.receipt().ok_or(ControlError::Corrupt)?;
    let evaluation = existing.evaluation().ok_or(ControlError::Corrupt)?;
    if evaluation.id() != candidate.evaluation_receipt() {
        return Err(ControlError::StaleRoot);
    }
    if reviewer.id() == receipt.owner() || reviewer.id() == evaluation.actor() {
        return Err(ControlError::AuthorityNotIndependent);
    }
    let review = ReviewReceipt::new(
        candidate.key(),
        evaluation.id(),
        reviewer,
        verdict,
        evidence,
    )?;
    let status = status_after(verdict, WorkStatus::Reviewed);
    let next = (status == WorkStatus::Reviewed)
        .then(|| Candidate::from_parts(candidate.key(), review.id()));
    let mut record = existing.clone();
    record.set_review(review);
    record.set_status(status);
    Ok(PreparedStage {
        record,
        status,
        candidate: next,
    })
}

pub(crate) fn decide(
    existing: &WorkRecord,
    candidate: &Candidate<Reviewed>,
    sol: Identity<OwnerSchema>,
    verdict: CustodyVerdict,
    evidence: Identity<EvidenceSchema>,
) -> Result<PreparedDecision, ControlError> {
    if existing.status() != WorkStatus::Reviewed {
        return Err(ControlError::InvalidTransition);
    }
    let candidate_receipt = existing.receipt().ok_or(ControlError::Corrupt)?;
    let evaluation = existing.evaluation().ok_or(ControlError::Corrupt)?;
    let review = existing.review().ok_or(ControlError::Corrupt)?;
    if review.id() != candidate.review_receipt() {
        return Err(ControlError::StaleRoot);
    }
    if [
        candidate_receipt.owner(),
        evaluation.actor(),
        review.actor(),
    ]
    .contains(&sol.id())
    {
        return Err(ControlError::AuthorityNotIndependent);
    }
    let decision = DecisionReceipt::new(candidate.key(), review.id(), sol, verdict, evidence)?;
    let status = status_after(verdict, WorkStatus::Completed);
    let mut record = existing.clone();
    record.set_decision(decision.clone());
    record.set_status(status);
    Ok(PreparedDecision {
        record,
        status,
        receipt: decision,
    })
}

const fn status_after(verdict: CustodyVerdict, accepted: WorkStatus) -> WorkStatus {
    match verdict {
        CustodyVerdict::Accepted => accepted,
        CustodyVerdict::Rejected => WorkStatus::Rejected,
        CustodyVerdict::Quarantined => WorkStatus::Quarantined,
    }
}
