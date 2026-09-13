//! Authenticated cancellation control claims.

use crate::{
    AttemptId, CancellationId, ExpectedIdentity, Fence, ReplicationError, TransportLimits,
    WireIdentity,
};

/// An untrusted cancellation command for one active pure execution attempt.
///
/// The command intentionally carries the attempt, execution-owned work-key
/// claim, exact cancellation identity, and publication fence together. A
/// receiver must compare all four fields against the caller-owned active job
/// before tripping its cancellation handle; matching only the cancellation
/// token would permit an old command to cancel a newer attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelAttempt {
    /// Attempt identity being cancelled.
    pub attempt: AttemptId,
    /// Untrusted execution-owned work-key claim.
    pub work_key: WireIdentity,
    /// Cancellation identity generated for this attempt.
    pub cancellation: CancellationId,
    /// Exact scheduler publication fence for this attempt.
    pub fence: Fence,
}

/// Caller-owned active execution material used to admit a cancellation
/// command. Replication only compares claims; it never constructs an
/// execution work-key or cancellation handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelAttemptExpectation {
    /// Active attempt identity.
    pub attempt: AttemptId,
    /// Exact typed work-key identity expected for the active attempt.
    pub work_key: ExpectedIdentity,
    /// Active attempt's cancellation identity.
    pub cancellation: CancellationId,
    /// Active attempt's publication fence.
    pub fence: Fence,
}

impl CancelAttempt {
    /// Validates command shape without admitting its work-key claim.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidIdentifier`] when an attempt,
    /// cancellation, or fence is zero.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.attempt.get() == 0 || self.cancellation.is_zero() || self.fence.is_zero() {
            return Err(ReplicationError::InvalidIdentifier);
        }
        Ok(())
    }

    /// Admits a cancellation command against one caller-owned active job.
    ///
    /// Every identity is compared exactly. A stale attempt, cancellation
    /// token, or fence returns [`ReplicationError::StaleFence`]; a work-key
    /// context or digest mismatch returns the corresponding identity error.
    /// The caller may trip its own cancellation handle only after this method
    /// succeeds.
    ///
    /// # Errors
    ///
    /// Returns an identifier, stale-fence, or identity error when the command
    /// does not exactly identify the active caller-owned job.
    pub fn admit_against(
        &self,
        expected: CancelAttemptExpectation,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        self.validate(limits)?;
        if self.attempt != expected.attempt
            || self.cancellation != expected.cancellation
            || self.fence != expected.fence
        {
            return Err(ReplicationError::StaleFence);
        }
        expected.work_key.matches(self.work_key)
    }
}
