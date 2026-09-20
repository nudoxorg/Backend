//! Typed request expectations and wire admission.

use super::{
    AttemptId, AuthorityExpectation, CancellationId, ExpectedIdentity, Fence, ReplicationError,
    ResourceEnvelope, TransportLimits, WireAuthorityPolicy, WireIdentity, WorkspaceRoot,
    WorkspaceRootClaim,
};

/// Caller-owned typed execution material used to admit a wire request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequestExpectation {
    /// Attempt identity expected for this request.
    pub attempt: AttemptId,
    /// Expected recipe identity.
    pub recipe: ExpectedIdentity,
    /// Expected semantic work identity.
    pub work_key: ExpectedIdentity,
    /// Expected exact input object/facet identities.
    pub inputs: Vec<ExpectedIdentity>,
    /// Expected complete read-manifest identity.
    pub read_manifest: ExpectedIdentity,
    /// Requested key/range/SCC scope.
    pub scope: u64,
    /// Expected authority and revocation policy.
    pub authority: AuthorityExpectation,
    /// Full resource envelope declared for the pure operation.
    pub resources: ResourceEnvelope,
    /// Publication fence expected from the scheduler.
    pub fence: Fence,
    /// Exact source workspace root expected by the execution owner.
    pub input_basis: WorkspaceRoot,
    /// Cancellation identity the worker must observe for this attempt.
    pub cancellation: CancellationId,
}
impl ExecutionRequestExpectation {
    /// Validates the caller-provided expected material and allocation bounds.
    ///
    /// # Errors
    ///
    /// Returns an identifier, ordering, or input-count error when the
    /// expectation cannot be used for admission.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.attempt.get() == 0
            || self.fence.is_zero()
            || self.cancellation.is_zero()
            || self.inputs.len() > limits.max_inputs
        {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.inputs.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ReplicationError::Unsorted);
        }
        self.resources.validate(limits)?;
        Ok(())
    }
}

/// A wire execution request with no trusted execution identity fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireRecipeRequest {
    /// Attempt identity.
    pub attempt: AttemptId,
    /// Untrusted recipe identity and context.
    pub recipe: WireIdentity,
    /// Untrusted semantic work identity and context.
    pub work_key: WireIdentity,
    /// Untrusted input object/facet identities.
    pub inputs: Vec<WireIdentity>,
    /// Untrusted complete read-manifest identity.
    pub read_manifest: WireIdentity,
    /// Requested key/range/SCC scope.
    pub scope: u64,
    /// Untrusted authority policy.
    pub authority: WireAuthorityPolicy,
    /// Full declared pure resource envelope.
    pub resources: ResourceEnvelope,
    /// Publication fence.
    pub fence: Fence,
    /// Cancellation identity observed by the worker.
    pub cancellation: CancellationId,
    /// Untrusted source workspace root claim.
    pub input_basis: WorkspaceRootClaim,
}
/// Generic name for a wire execution request.
pub type WireExecutionRequest = WireRecipeRequest;
impl WireRecipeRequest {
    /// Checks bounds without admitting identities.
    ///
    /// # Errors
    ///
    /// Returns a message or identifier error when the envelope is too large
    /// or carries a zero attempt/fence.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.attempt.get() == 0 || self.fence.is_zero() || self.cancellation.is_zero() {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.inputs.len() > limits.max_inputs {
            return Err(ReplicationError::MessageTooLarge);
        }
        self.resources.validate(limits)?;
        Ok(())
    }

    /// Admits the request only by exact comparison against caller-owned typed
    /// execution material.
    ///
    /// # Errors
    ///
    /// Returns an identity, source-root, authority, fence, or bounds error
    /// when the wire request does not exactly match the expected request.
    pub fn admit_against(
        &self,
        expected: &ExecutionRequestExpectation,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        self.validate(limits)?;
        expected.validate(limits)?;
        if self.attempt != expected.attempt
            || self.fence != expected.fence
            || self.cancellation != expected.cancellation
        {
            return Err(ReplicationError::StaleFence);
        }
        if self.scope != expected.scope || self.resources != expected.resources {
            return Err(ReplicationError::IdentityMismatch);
        }
        expected.recipe.matches(self.recipe)?;
        expected.work_key.matches(self.work_key)?;
        if self.inputs.len() != expected.inputs.len() {
            return Err(ReplicationError::IdentityMismatch);
        }
        for (claim, expected_input) in self.inputs.iter().zip(&expected.inputs) {
            expected_input.matches(*claim)?;
        }
        expected.read_manifest.matches(self.read_manifest)?;
        self.input_basis.admit(expected.input_basis)?;
        self.authority
            .admit_against(expected.authority, expected.authority.revocation_version)
    }
}
