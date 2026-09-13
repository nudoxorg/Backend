//! Exact remote request contract and fence bindings.

use super::super::DispatchError;
use super::super::authority::RemoteAuthorityPolicy;
use super::super::coverage::CompleteSemanticCoverage;
use backend_execution::{AttemptFence, AttemptLease};
use backend_replication::{
    AuthorityEpoch, ExpectedIdentity, Fence, ResourceEnvelope, RevocationVersion, TransportLimits,
    WireIdentity,
};
use backend_version::{ObjectVersion, Schema, WorkspaceRoot};
use std::mem::size_of;

/// Exact input claim pair used to build a remote request expectation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpectedInput {
    /// Untrusted wire form sent to the peer.
    pub wire: WireIdentity,
    /// Caller-owned exact typed expectation.
    pub expected: ExpectedIdentity,
}

impl ExpectedInput {
    /// Creates an input pair from a typed identity whose schema is carried by
    /// its backend-version type.
    #[must_use]
    pub fn from_typed<T: backend_replication::TypedIdentity>(value: &T) -> Self {
        Self {
            wire: WireIdentity::from_typed(value),
            expected: ExpectedIdentity::from_typed(value),
        }
    }
}

/// All exact request material required for a remote execution admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteDispatchContract {
    /// Exact workspace root used to select inputs.
    pub input_basis: WorkspaceRoot,
    /// Exact input claims and typed expectations.
    pub inputs: Vec<ExpectedInput>,
    /// Semantic dependency capability admitted by a recipe/scope authority.
    pub semantic: CompleteSemanticCoverage,
    /// Full multidimensional hard resource envelope.
    pub resources: ResourceEnvelope,
    /// Minimum authority epoch.
    pub authority_epoch: AuthorityEpoch,
    /// Required current revocation observation.
    pub revocation_version: RevocationVersion,
    /// Remote trust class policy.
    pub authority_policy: RemoteAuthorityPolicy,
    /// Negotiated wire limits.
    pub limits: TransportLimits,
    /// Optional receipt identity supplied by the caller.
    pub expected_receipt: Option<WorkerReceiptId>,
}

impl RemoteDispatchContract {
    /// Returns the logical bytes retained by the contract's owned input
    /// vector and semantic capability. The pending owner uses this checked
    /// value instead of a resource-envelope estimate.
    pub(crate) fn retained_size(&self) -> Option<usize> {
        size_of::<Self>().checked_add(self.retained_payload_size()?)
    }

    /// Returns heap payload bytes owned by the contract. Inline contract and
    /// semantic records are charged by the affine ticket that contains them.
    pub(crate) fn retained_payload_size(&self) -> Option<usize> {
        let inputs = self
            .inputs
            .capacity()
            .checked_mul(size_of::<ExpectedInput>())?;
        inputs.checked_add(self.semantic.retained_payload_size()?)
    }
}

/// Full scheduler fence binding retained across replication.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FenceBinding {
    full: AttemptFence,
    wire: Fence,
}

impl FenceBinding {
    /// Binds the exact full scheduler fence to its wire representation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn from_lease(lease: &AttemptLease) -> Result<Self, DispatchError> {
        let wire = Fence::from_bytes(*lease.fence().as_bytes())
            .map_err(|_| DispatchError::InvalidAttempt)?;
        Ok(Self {
            full: lease.fence(),
            wire,
        })
    }

    /// Returns the full opaque scheduler fence.
    #[must_use]
    pub const fn full(self) -> AttemptFence {
        self.full
    }

    /// Returns the exact wire fence with no narrowing.
    #[must_use]
    pub const fn wire(self) -> Fence {
        self.wire
    }

    pub(crate) fn admits(self, lease: &AttemptLease, supplied: Fence) -> bool {
        self.full == lease.fence() && self.wire == supplied
    }
}

/// Schema marker for a pure worker receipt identity.
#[derive(Debug)]
pub struct WorkerReceiptSchema;

impl Schema for WorkerReceiptSchema {
    const DOMAIN: u8 = 0x83;
    const TYPE: u16 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Typed receipt identity for a pure worker response.
pub type WorkerReceiptId = ObjectVersion<WorkerReceiptSchema>;
