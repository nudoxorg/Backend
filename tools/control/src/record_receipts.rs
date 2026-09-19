//! Receipt and review evidence for the versioned control relation.

use crate::ControlError;
use crate::ids::{
    AgentWorkKey, DecisionReceiptSchema, EvaluationReceiptSchema, EvidenceRoot, FenceId,
    FenceSchema, Identity, IdentityBytes, OutputRoot, OwnerId, OwnerSchema, ReceiptId,
    ReceiptSchema, ReviewReceiptSchema, WorkKeySchema, append_identity, append_version,
};
use backend_version::{ObjectVersion, Schema};

/// Why an authority-owned attempt ended.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AttemptOutcome {
    /// The fenced implementation attempt produced a candidate.
    Succeeded,
    /// The owner reported a bounded failure.
    Failed,
    /// A controller or client cancelled the attempt.
    Cancelled,
    /// The attempt passed its lease deadline.
    Expired,
    /// The output was retained outside the reusable set.
    Quarantined,
}

impl AttemptOutcome {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Succeeded => 0,
            Self::Failed => 1,
            Self::Cancelled => 2,
            Self::Expired => 3,
            Self::Quarantined => 4,
        }
    }

    pub(crate) const fn from_tag(tag: u8) -> Result<Self, ControlError> {
        match tag {
            0 => Ok(Self::Succeeded),
            1 => Ok(Self::Failed),
            2 => Ok(Self::Cancelled),
            3 => Ok(Self::Expired),
            4 => Ok(Self::Quarantined),
            _ => Err(ControlError::Corrupt),
        }
    }
}

/// Authority class that is allowed to mint one receipt.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Authority {
    /// Coordinator-issued planning/context evidence.
    Controller,
    /// Evaluator-issued public or opaque test evidence.
    Evaluator,
    /// Independent reviewer-issued verdict evidence.
    Reviewer,
    /// Integration authority allowed to advance the train.
    Sol,
}

impl Authority {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Controller => 0,
            Self::Evaluator => 1,
            Self::Reviewer => 2,
            Self::Sol => 3,
        }
    }

    pub(crate) const fn from_tag(tag: u8) -> Result<Self, ControlError> {
        match tag {
            0 => Ok(Self::Controller),
            1 => Ok(Self::Evaluator),
            2 => Ok(Self::Reviewer),
            3 => Ok(Self::Sol),
            _ => Err(ControlError::Corrupt),
        }
    }
}

/// Candidate/evaluation bytes and their immutable receipt identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableReceipt {
    key: AgentWorkKey,
    owner: Identity<OwnerSchema>,
    fence: Identity<FenceSchema>,
    authority: Authority,
    output: Identity<crate::ids::OutputSchema>,
    evidence: Identity<crate::ids::EvidenceSchema>,
    outcome: AttemptOutcome,
    id: Identity<ReceiptSchema>,
}

impl DurableReceipt {
    /// Mints a receipt after the caller has checked the attempt fence.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] when the receipt's canonical identity
    /// preimage is oversized.
    pub(crate) fn new(
        key: AgentWorkKey,
        owner: Identity<OwnerSchema>,
        fence: Identity<FenceSchema>,
        authority: Authority,
        output: Identity<crate::ids::OutputSchema>,
        evidence: Identity<crate::ids::EvidenceSchema>,
        outcome: AttemptOutcome,
    ) -> Result<Self, ControlError> {
        let mut bytes = Vec::with_capacity(5 * backend_version::ID_BYTES + 2);
        append_version::<WorkKeySchema>(&mut bytes, key);
        append_identity::<OwnerSchema>(&mut bytes, &owner)?;
        append_identity::<FenceSchema>(&mut bytes, &fence)?;
        bytes.push(authority.tag());
        append_identity::<crate::ids::OutputSchema>(&mut bytes, &output)?;
        append_identity::<crate::ids::EvidenceSchema>(&mut bytes, &evidence)?;
        bytes.push(outcome.tag());
        let id = Identity::<ReceiptSchema>::from_bytes(bytes)?;
        Ok(Self {
            key,
            owner,
            fence,
            authority,
            output,
            evidence,
            outcome,
            id,
        })
    }

    /// Returns the immutable receipt identity.
    #[must_use]
    pub const fn id(&self) -> ReceiptId {
        self.id.id()
    }

    /// Returns the work key bound by this receipt.
    #[must_use]
    pub const fn key(&self) -> AgentWorkKey {
        self.key
    }

    /// Returns the fenced candidate owner.
    #[must_use]
    pub const fn owner(&self) -> OwnerId {
        self.owner.id()
    }

    pub(crate) const fn owner_identity(&self) -> &Identity<OwnerSchema> {
        &self.owner
    }

    /// Returns the attempt fence bound by this receipt.
    #[must_use]
    pub const fn fence(&self) -> FenceId {
        self.fence.id()
    }

    pub(crate) const fn fence_identity(&self) -> &Identity<FenceSchema> {
        &self.fence
    }

    /// Returns the receipt authority.
    #[must_use]
    pub const fn authority(&self) -> Authority {
        self.authority
    }

    /// Returns the candidate/output root.
    #[must_use]
    pub const fn output(&self) -> OutputRoot {
        self.output.id()
    }

    pub(crate) const fn output_identity(&self) -> &Identity<crate::ids::OutputSchema> {
        &self.output
    }

    /// Returns the evidence root.
    #[must_use]
    pub const fn evidence(&self) -> EvidenceRoot {
        self.evidence.id()
    }

    pub(crate) const fn evidence_identity(&self) -> &Identity<crate::ids::EvidenceSchema> {
        &self.evidence
    }

    /// Returns the terminal outcome represented by this receipt.
    #[must_use]
    pub const fn outcome(&self) -> AttemptOutcome {
        self.outcome
    }

    pub(crate) const fn receipt_identity(&self) -> &Identity<ReceiptSchema> {
        &self.id
    }
}

/// Verdict carried by an authority-owned custody attestation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CustodyVerdict {
    /// The subject satisfies the immutable contract for this stage.
    Accepted,
    /// The subject violated a required law or gate.
    Rejected,
    /// The subject disagreed with an independent oracle and is retained for diagnosis.
    Quarantined,
}

impl CustodyVerdict {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Accepted => 0,
            Self::Rejected => 1,
            Self::Quarantined => 2,
        }
    }

    pub(crate) const fn from_tag(tag: u8) -> Result<Self, ControlError> {
        match tag {
            0 => Ok(Self::Accepted),
            1 => Ok(Self::Rejected),
            2 => Ok(Self::Quarantined),
            _ => Err(ControlError::Corrupt),
        }
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A sealed stage in the candidate custody chain.
pub trait CustodyStage: Schema<Value = IdentityBytes> + sealed::Sealed {
    /// Authority that alone owns this stage.
    const AUTHORITY: Authority;
}

impl sealed::Sealed for EvaluationReceiptSchema {}
impl CustodyStage for EvaluationReceiptSchema {
    const AUTHORITY: Authority = Authority::Evaluator;
}

impl sealed::Sealed for ReviewReceiptSchema {}
impl CustodyStage for ReviewReceiptSchema {
    const AUTHORITY: Authority = Authority::Reviewer;
}

impl sealed::Sealed for DecisionReceiptSchema {}
impl CustodyStage for DecisionReceiptSchema {
    const AUTHORITY: Authority = Authority::Sol;
}

/// One domain-separated attestation that consumes the exact preceding receipt.
///
/// `P` is the predecessor receipt schema and `S` is the authority-owned output
/// schema. This makes skipping or reordering evaluation, review, and decision
/// unrepresentable in the public API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustodyReceipt<P, S>
where
    P: Schema<Value = IdentityBytes>,
    S: CustodyStage,
{
    key: AgentWorkKey,
    subject: ObjectVersion<P>,
    actor: Identity<OwnerSchema>,
    verdict: CustodyVerdict,
    evidence: Identity<crate::ids::EvidenceSchema>,
    id: Identity<S>,
}

impl<P, S> CustodyReceipt<P, S>
where
    P: Schema<Value = IdentityBytes>,
    S: CustodyStage,
{
    pub(crate) fn new(
        key: AgentWorkKey,
        subject: ObjectVersion<P>,
        actor: Identity<OwnerSchema>,
        verdict: CustodyVerdict,
        evidence: Identity<crate::ids::EvidenceSchema>,
    ) -> Result<Self, ControlError> {
        let mut bytes = Vec::with_capacity(4 * backend_version::ID_BYTES + 2);
        bytes.extend_from_slice(b"agent-custody-v1");
        append_version::<WorkKeySchema>(&mut bytes, key);
        append_version::<P>(&mut bytes, subject);
        bytes.push(S::AUTHORITY.tag());
        append_identity::<OwnerSchema>(&mut bytes, &actor)?;
        bytes.push(verdict.tag());
        append_identity::<crate::ids::EvidenceSchema>(&mut bytes, &evidence)?;
        let id = Identity::<S>::from_bytes(bytes)?;
        Ok(Self {
            key,
            subject,
            actor,
            verdict,
            evidence,
            id,
        })
    }

    /// Returns the immutable work key carried across the custody chain.
    #[must_use]
    pub const fn key(&self) -> AgentWorkKey {
        self.key
    }

    /// Returns the exact preceding receipt consumed by this stage.
    #[must_use]
    pub const fn subject(&self) -> ObjectVersion<P> {
        self.subject
    }

    /// Returns the authority class fixed by the output receipt schema.
    #[must_use]
    pub const fn authority(&self) -> Authority {
        S::AUTHORITY
    }

    /// Returns the actor that produced this attestation.
    #[must_use]
    pub const fn actor(&self) -> OwnerId {
        self.actor.id()
    }

    pub(crate) const fn actor_identity(&self) -> &Identity<OwnerSchema> {
        &self.actor
    }

    /// Returns the stage verdict.
    #[must_use]
    pub const fn verdict(&self) -> CustodyVerdict {
        self.verdict
    }

    /// Returns the review evidence root.
    #[must_use]
    pub const fn evidence(&self) -> EvidenceRoot {
        self.evidence.id()
    }

    pub(crate) const fn evidence_identity(&self) -> &Identity<crate::ids::EvidenceSchema> {
        &self.evidence
    }

    /// Returns the identity of this authority-owned receipt.
    #[must_use]
    pub const fn id(&self) -> ObjectVersion<S> {
        self.id.id()
    }

    pub(crate) const fn receipt_identity(&self) -> &Identity<S> {
        &self.id
    }
}

/// Evaluator evidence over an exact controller candidate receipt.
pub type EvaluationReceipt = CustodyReceipt<ReceiptSchema, EvaluationReceiptSchema>;
/// Reviewer evidence over an exact evaluator receipt.
pub type ReviewReceipt = CustodyReceipt<EvaluationReceiptSchema, ReviewReceiptSchema>;
/// Sol's final decision over an exact reviewer receipt.
pub type DecisionReceipt = CustodyReceipt<ReviewReceiptSchema, DecisionReceiptSchema>;
