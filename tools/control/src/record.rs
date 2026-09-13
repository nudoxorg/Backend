//! Versioned work rows, leases, receipts, and lifecycle evidence.

use backend_version::{Relation, RelationDecodeError};

use crate::ControlError;
use crate::ids::{AgentWorkKey, FenceId, FenceSchema, Identity, OwnerId, OwnerSchema};
use crate::spec::WorkSpec;

/// Maximum bytes in a candidate/evidence receipt's immutable payload.
pub const MAX_RECEIPT_BYTES: usize = 64 * 1024;
/// Maximum number of waiters coalesced onto one running work item.
pub const MAX_WAITERS: u16 = 64;

/// One hidden relation key. Raw bytes are admitted only by pairing the key
/// with the checked work specification in [`crate::ledger::VersionedControlPlane`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RecordKey([u8; backend_version::ID_BYTES]);

impl RecordKey {
    pub(crate) const fn from_work(key: AgentWorkKey) -> Self {
        Self(key.to_bytes())
    }

    pub(crate) const fn from_bytes(bytes: [u8; backend_version::ID_BYTES]) -> Self {
        Self(bytes)
    }
}

/// The one versioned relation owned by the control plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlRelation;

impl Relation for ControlRelation {
    const DOMAIN: u8 = 0x41;
    const TYPE: u16 = 0x0001;
    type Key = RecordKey;
    type Value = WorkRecord;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&key.0);
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        value.append_canonical_unchecked(output);
    }
}

impl backend_version::CanonicalRelation for ControlRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        let bytes: [u8; backend_version::ID_BYTES] = bytes
            .try_into()
            .map_err(|_| RelationDecodeError::Malformed)?;
        Ok(RecordKey::from_bytes(bytes))
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        WorkRecord::decode(bytes).map_err(|_| RelationDecodeError::Malformed)
    }
}

/// Typed root of the control relation.
pub type ControlRoot = backend_version::StateRoot<ControlRelation>;

/// Lifecycle state for one immutable work specification.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WorkStatus {
    /// The coordinator has admitted the specification but has not scheduled it.
    Planned,
    /// All prerequisite work is complete and the item may run.
    Ready,
    /// The item is retained while waiting for prerequisites or capacity.
    Waiting,
    /// The item is admitted behind the active-attempt budget.
    Queued,
    /// One fenced owner currently holds execution custody.
    Running,
    /// A candidate output is frozen and awaits independent evaluation.
    Frozen,
    /// An independent evaluator accepted the frozen candidate.
    Evaluated,
    /// An independent reviewer accepted the evaluator receipt.
    Reviewed,
    /// Sol promoted the exact reviewed chain into the reusable set.
    Completed,
    /// The attempt failed and can be retried if budget remains.
    Failed,
    /// The owner or controller cancelled the attempt.
    Cancelled,
    /// The lease expired before a terminal receipt was published.
    Expired,
    /// The output was retained for diagnosis but removed from the reusable set.
    Quarantined,
    /// A prerequisite changed, invalidating this candidate.
    Invalidated,
    /// An independent reviewer rejected the candidate.
    Rejected,
}

impl WorkStatus {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Planned => 0,
            Self::Ready => 1,
            Self::Waiting => 2,
            Self::Queued => 3,
            Self::Running => 4,
            Self::Frozen => 5,
            Self::Completed => 6,
            Self::Failed => 7,
            Self::Cancelled => 8,
            Self::Expired => 9,
            Self::Quarantined => 10,
            Self::Invalidated => 11,
            Self::Rejected => 12,
            Self::Evaluated => 13,
            Self::Reviewed => 14,
        }
    }

    pub(crate) const fn from_tag(tag: u8) -> Result<Self, ControlError> {
        match tag {
            0 => Ok(Self::Planned),
            1 => Ok(Self::Ready),
            2 => Ok(Self::Waiting),
            3 => Ok(Self::Queued),
            4 => Ok(Self::Running),
            5 => Ok(Self::Frozen),
            6 => Ok(Self::Completed),
            7 => Ok(Self::Failed),
            8 => Ok(Self::Cancelled),
            9 => Ok(Self::Expired),
            10 => Ok(Self::Quarantined),
            11 => Ok(Self::Invalidated),
            12 => Ok(Self::Rejected),
            13 => Ok(Self::Evaluated),
            14 => Ok(Self::Reviewed),
            _ => Err(ControlError::Corrupt),
        }
    }

    /// Returns whether this state is a successful reusable terminal state.
    #[must_use]
    pub const fn is_reusable(self) -> bool {
        matches!(self, Self::Completed)
    }

    /// Returns whether no new attempt may be attached to this row.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Evaluated
                | Self::Reviewed
                | Self::Cancelled
                | Self::Quarantined
                | Self::Invalidated
                | Self::Rejected
        )
    }

    /// Returns whether this state consumes one scheduler execution slot.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Running)
    }

    /// Returns whether this state retains an expiring fenced lease.
    #[must_use]
    pub const fn has_expiring_lease(self) -> bool {
        matches!(self, Self::Running | Self::Frozen)
    }
}

#[path = "record_codec.rs"]
mod codec;
#[path = "record_receipts.rs"]
mod receipts;

pub use receipts::{
    AttemptOutcome, Authority, CustodyReceipt, CustodyStage, CustodyVerdict, DecisionReceipt,
    DurableReceipt, EvaluationReceipt, ReviewReceipt,
};

/// The attempt fence retained by a running relation row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttemptData {
    owner: Identity<OwnerSchema>,
    epoch: u64,
    fence: Identity<FenceSchema>,
    expires_at: u64,
    attempt: u16,
}

impl AttemptData {
    pub(crate) const fn new(
        owner: Identity<OwnerSchema>,
        epoch: u64,
        fence: Identity<FenceSchema>,
        expires_at: u64,
        attempt: u16,
    ) -> Self {
        Self {
            owner,
            epoch,
            fence,
            expires_at,
            attempt,
        }
    }

    pub(crate) fn owner(&self) -> OwnerId {
        self.owner.id()
    }

    pub(crate) const fn owner_identity(&self) -> &Identity<OwnerSchema> {
        &self.owner
    }

    pub(crate) const fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(crate) fn fence(&self) -> FenceId {
        self.fence.id()
    }

    pub(crate) const fn fence_identity(&self) -> &Identity<FenceSchema> {
        &self.fence
    }

    pub(crate) const fn expires_at(&self) -> u64 {
        self.expires_at
    }

    pub(crate) const fn attempt(&self) -> u16 {
        self.attempt
    }
}

/// One mutable lifecycle row stored as an immutable value version in the
/// control relation. Mutations replace the row through an exact version delta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkRecord {
    spec: WorkSpec,
    status: WorkStatus,
    attempts: u16,
    /// Last fencing epoch minted for this key.  It survives release of an
    /// attempt so a late owner can never regain custody with a reused token.
    epoch: u64,
    waiters: u16,
    attempt: Option<AttemptData>,
    receipt: Option<DurableReceipt>,
    evaluation: Option<EvaluationReceipt>,
    review: Option<ReviewReceipt>,
    decision: Option<DecisionReceipt>,
}

impl WorkRecord {
    pub(crate) fn planned(spec: WorkSpec) -> Self {
        Self {
            spec,
            status: WorkStatus::Planned,
            attempts: 0,
            epoch: 0,
            waiters: 0,
            attempt: None,
            receipt: None,
            evaluation: None,
            review: None,
            decision: None,
        }
    }

    /// Returns the immutable specification carried by this row.
    #[must_use]
    pub const fn spec(&self) -> &WorkSpec {
        &self.spec
    }

    /// Returns the immutable key carried by this row.
    #[must_use]
    pub const fn key(&self) -> AgentWorkKey {
        self.spec.key()
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub const fn status(&self) -> WorkStatus {
        self.status
    }

    /// Returns the number of attempts consumed by this work key.
    #[must_use]
    pub const fn attempts(&self) -> u16 {
        self.attempts
    }

    /// Returns the last fencing epoch minted for this work key.
    #[must_use]
    pub(crate) const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the number of coalesced waiters.
    #[must_use]
    pub const fn waiter_count(&self) -> u16 {
        self.waiters
    }

    /// Returns the current attempt fence, when one is active or retained.
    #[must_use]
    pub(crate) fn attempt_data(&self) -> Option<AttemptData> {
        self.attempt.clone()
    }

    /// Returns the reusable receipt, when review accepted this exact key.
    #[must_use]
    pub const fn receipt(&self) -> Option<&DurableReceipt> {
        self.receipt.as_ref()
    }

    /// Returns the independent review record, when present.
    #[must_use]
    pub const fn review(&self) -> Option<&ReviewReceipt> {
        self.review.as_ref()
    }

    /// Returns the independent evaluator receipt, when present.
    #[must_use]
    pub const fn evaluation(&self) -> Option<&EvaluationReceipt> {
        self.evaluation.as_ref()
    }

    /// Returns Sol's final decision receipt, when present.
    #[must_use]
    pub const fn decision(&self) -> Option<&DecisionReceipt> {
        self.decision.as_ref()
    }

    pub(crate) fn set_status(&mut self, status: WorkStatus) {
        self.status = status;
    }

    pub(crate) fn set_attempt(&mut self, attempt: Option<AttemptData>) {
        self.attempt = attempt;
    }

    pub(crate) fn increment_attempts(&mut self) -> Result<u16, ControlError> {
        self.attempts = self.attempts.checked_add(1).ok_or(ControlError::Bounds)?;
        Ok(self.attempts)
    }

    pub(crate) fn set_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
    }

    pub(crate) fn increment_waiters_limit(&mut self, limit: u16) -> Result<u16, ControlError> {
        if self.waiters >= limit {
            return Err(ControlError::WaiterLimit);
        }
        self.waiters += 1;
        Ok(self.waiters)
    }

    pub(crate) fn set_receipt(&mut self, receipt: DurableReceipt) {
        self.receipt = Some(receipt);
    }

    pub(crate) fn set_review(&mut self, review: ReviewReceipt) {
        self.review = Some(review);
    }

    pub(crate) fn set_evaluation(&mut self, evaluation: EvaluationReceipt) {
        self.evaluation = Some(evaluation);
    }

    pub(crate) fn set_decision(&mut self, decision: DecisionReceipt) {
        self.decision = Some(decision);
    }

    pub(crate) fn clear_receipt(&mut self) {
        self.receipt = None;
    }

    pub(crate) fn clear_review(&mut self) {
        self.review = None;
    }

    pub(crate) fn clear_evaluation(&mut self) {
        self.evaluation = None;
    }

    pub(crate) fn clear_decision(&mut self) {
        self.decision = None;
    }

    pub(crate) fn clear_attempt(&mut self) {
        self.attempt = None;
    }
}
