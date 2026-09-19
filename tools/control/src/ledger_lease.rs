//! Fenced lease capabilities and identity construction.

use core::marker::PhantomData;

use crate::ControlError;
use crate::ids::{
    AgentWorkKey, EvaluationReceiptId, EvaluationReceiptSchema, FenceId, FenceSchema, Identity,
    IdentityBytes, OwnerId, OwnerSchema, ReviewReceiptId, ReviewReceiptSchema, WorkKeySchema,
    append_identity, append_version,
};
use crate::record::AttemptData;
use backend_version::{ObjectVersion, Schema};

pub(crate) fn ensure_fence<S: LeaseMarker>(
    attempt: &AttemptData,
    lease: &Lease<S>,
) -> Result<(), ControlError> {
    if attempt.owner() != lease.attempt.owner()
        || attempt.epoch() != lease.attempt.epoch()
        || attempt.fence() != lease.attempt.fence()
    {
        return Err(ControlError::StaleFence);
    }
    Ok(())
}

pub(crate) fn fence_identity(
    key: AgentWorkKey,
    owner: &Identity<OwnerSchema>,
    epoch: u64,
) -> Result<Identity<FenceSchema>, ControlError> {
    let mut bytes = Vec::with_capacity(4 + 2 * backend_version::ID_BYTES + 8);
    bytes.extend_from_slice(b"agent-fence-v1");
    append_version::<WorkKeySchema>(&mut bytes, key);
    append_identity::<OwnerSchema>(&mut bytes, owner)?;
    bytes.extend_from_slice(&epoch.to_be_bytes());
    Identity::<FenceSchema>::from_bytes(bytes)
}

/// A lease state that is only constructible through the control plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Held;
/// A renewed lease state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Renewed;
/// A frozen candidate state awaiting review.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frozen;

mod sealed {
    pub trait Sealed {}
}

/// Marker for an accepted evaluator receipt awaiting independent review.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Evaluated;
/// Marker for an accepted reviewer receipt awaiting Sol's final decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reviewed;

/// Sealed custody stage used by [`Candidate`] capabilities.
pub trait CandidateStage: sealed::Sealed {
    /// Receipt schema that proves this exact stage.
    type Receipt: Schema<Value = IdentityBytes> + Clone + core::fmt::Debug + Eq;
    /// Relation status represented by this capability.
    const STATUS: crate::WorkStatus;
}

impl sealed::Sealed for Evaluated {}
impl CandidateStage for Evaluated {
    type Receipt = EvaluationReceiptSchema;
    const STATUS: crate::WorkStatus = crate::WorkStatus::Evaluated;
}

impl sealed::Sealed for Reviewed {}
impl CandidateStage for Reviewed {
    type Receipt = ReviewReceiptSchema;
    const STATUS: crate::WorkStatus = crate::WorkStatus::Reviewed;
}

/// Marker implemented by lease typestates minted by the control plane.
pub trait LeaseMarker {}
impl LeaseMarker for Held {}
impl LeaseMarker for Renewed {}
impl LeaseMarker for Frozen {}

/// Marker implemented by leases that still own execution custody.
///
/// A frozen lease intentionally does not implement this trait: only the
/// independent review operation may consume it.  This keeps expiry/failure
/// APIs from accidentally accepting a candidate that has already crossed the
/// authority boundary.
pub trait ActiveLeaseMarker: LeaseMarker {}
impl ActiveLeaseMarker for Held {}
impl ActiveLeaseMarker for Renewed {}

/// Affine-by-convention capability for one exact accepted custody stage.
///
/// The marker selects the only receipt schema accepted by the next transition;
/// callers cannot pass a candidate receipt where a review receipt is required.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate<S: CandidateStage> {
    key: AgentWorkKey,
    receipt: ObjectVersion<S::Receipt>,
}

impl<S: CandidateStage> Candidate<S> {
    pub(crate) const fn from_parts(key: AgentWorkKey, receipt: ObjectVersion<S::Receipt>) -> Self {
        Self { key, receipt }
    }

    /// Returns the immutable work key carried by this custody capability.
    #[must_use]
    pub const fn key(&self) -> AgentWorkKey {
        self.key
    }

    /// Returns the exact accepted receipt consumed by the next stage.
    #[must_use]
    pub const fn receipt(&self) -> ObjectVersion<S::Receipt> {
        self.receipt
    }
}

impl Candidate<Evaluated> {
    /// Returns the evaluator receipt identity.
    #[must_use]
    pub const fn evaluation_receipt(&self) -> EvaluationReceiptId {
        self.receipt
    }
}

impl Candidate<Reviewed> {
    /// Returns the reviewer receipt identity.
    #[must_use]
    pub const fn review_receipt(&self) -> ReviewReceiptId {
        self.receipt
    }
}

/// Typestate capability for one fenced attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lease<S: LeaseMarker> {
    pub(super) key: AgentWorkKey,
    pub(super) attempt: AttemptData,
    pub(super) marker: PhantomData<fn() -> S>,
}

impl<S: LeaseMarker> Lease<S> {
    pub(crate) const fn from_parts(key: AgentWorkKey, attempt: AttemptData) -> Self {
        Self {
            key,
            attempt,
            marker: PhantomData,
        }
    }

    /// Returns the immutable work key held by this lease.
    #[must_use]
    pub const fn key(&self) -> AgentWorkKey {
        self.key
    }

    /// Returns the current fencing token and expiry metadata.
    #[must_use]
    pub fn fence(&self) -> LeaseFence {
        LeaseFence::from_attempt(self.key, &self.attempt)
    }
}

/// Public wire-safe view of an admitted lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeaseFence {
    key: AgentWorkKey,
    owner: OwnerId,
    epoch: u64,
    fence: FenceId,
    expires_at: u64,
    attempt: u16,
}

impl LeaseFence {
    fn from_attempt(key: AgentWorkKey, attempt: &AttemptData) -> Self {
        Self {
            key,
            owner: attempt.owner(),
            epoch: attempt.epoch(),
            fence: attempt.fence(),
            expires_at: attempt.expires_at(),
            attempt: attempt.attempt(),
        }
    }

    /// Returns the work key bound by the fence.
    #[must_use]
    pub const fn key(self) -> AgentWorkKey {
        self.key
    }

    /// Returns the owner identity bound by the fence.
    #[must_use]
    pub const fn owner(self) -> OwnerId {
        self.owner
    }

    /// Returns the monotonically increasing attempt epoch.
    #[must_use]
    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    /// Returns the opaque fencing identity.
    #[must_use]
    pub const fn fence(self) -> FenceId {
        self.fence
    }

    /// Returns the lease expiry in the caller's clock.
    #[must_use]
    pub const fn expires_at(self) -> u64 {
        self.expires_at
    }

    /// Returns the one-based retry attempt number.
    #[must_use]
    pub const fn attempt(self) -> u16 {
        self.attempt
    }
}
