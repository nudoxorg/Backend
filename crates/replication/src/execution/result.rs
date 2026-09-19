//! Typed result expectations, wire results, and bounded output admission.

use super::{
    AdmittedExecutionResult, AttemptId, Attestation, AttestationMaterial, AttestationMaterialView,
    AttestationVerifier, CancellationId, ExecutionRequestExpectation, ExecutionScopeId,
    ExpectedIdentity, Fence, PublishableExecutionResult, ReplicationError, ResourceEnvelope,
    RevocationVersion, SemanticCoverageExpectation, SparseCoverage, TransportLimits, WireAuthority,
    WireAuthorityPolicy, WireIdentity, WireSemanticCoverage, WorkspaceRootClaim,
};
use std::{mem::size_of, sync::Arc};

/// Caller-owned typed output and receipt identities for result admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionResultExpectation {
    /// Expected output object/facet identity.
    pub output: ExpectedIdentity,
    /// Expected result receipt identity.
    pub receipt: ExpectedIdentity,
    /// Exact canonical output length already checked by the execution owner.
    ///
    /// The bytes themselves remain owned by the wire result. Keeping only the
    /// length here prevents dispatch from cloning a large output solely to
    /// perform exact admission.
    pub output_len: u64,
    /// Byte/range coverage expected for the transferred output bytes.
    pub byte_coverage: SparseCoverage,
    /// Opaque execution-owned semantic coverage bound to the request.
    pub semantic_coverage: SemanticCoverageExpectation,
}
impl ExecutionResultExpectation {
    /// Creates an output expectation from its typed identity and exact wire
    /// length. The identity is normally produced from execution's typed
    /// `OutputVersion` with [`ExpectedIdentity::from_typed`].
    #[must_use]
    pub const fn new(
        output: ExpectedIdentity,
        receipt: ExpectedIdentity,
        output_len: u64,
        byte_coverage: SparseCoverage,
        semantic_coverage: SemanticCoverageExpectation,
    ) -> Self {
        Self {
            output,
            receipt,
            output_len,
            byte_coverage,
            semantic_coverage,
        }
    }

    /// Creates an expectation from a borrowed output slice without retaining
    /// or copying that slice.
    #[must_use]
    pub fn from_output_bytes(
        output: ExpectedIdentity,
        receipt: ExpectedIdentity,
        output_bytes: &[u8],
        byte_coverage: SparseCoverage,
        semantic_coverage: SemanticCoverageExpectation,
    ) -> Self {
        Self::new(
            output,
            receipt,
            output_bytes.len() as u64,
            byte_coverage,
            semantic_coverage,
        )
    }

    /// Returns a borrowing view of this expectation.
    #[must_use]
    pub fn as_ref(&self) -> ExecutionResultExpectationRef<'_> {
        ExecutionResultExpectationRef {
            output: self.output,
            receipt: self.receipt,
            output_len: self.output_len,
            byte_coverage: &self.byte_coverage,
            semantic_coverage: &self.semantic_coverage,
        }
    }

    fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.output_len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.byte_coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.byte_coverage.ranges() {
            if range.end()? > self.output_len {
                return Err(ReplicationError::Range);
            }
        }
        self.semantic_coverage.validate(limits)
    }
}

/// Borrowed result expectation used by consuming admission paths.
///
/// The wire result already owns its canonical bytes and transfer coverage.
/// Borrowing those fields here lets an owner perform exact comparison without
/// allocating a second copy before the result is moved into its admitted
/// `Arc` capability.
#[derive(Debug)]
pub struct ExecutionResultExpectationRef<'a> {
    /// Expected output object/facet identity.
    pub output: ExpectedIdentity,
    /// Expected result receipt identity.
    pub receipt: ExpectedIdentity,
    /// Exact canonical output length expected from the wire result.
    pub output_len: u64,
    /// Byte/range coverage borrowed from the wire result.
    pub byte_coverage: &'a SparseCoverage,
    /// Semantic coverage expectation borrowed from the caller's contract.
    pub semantic_coverage: &'a SemanticCoverageExpectation,
}

impl ExecutionResultExpectationRef<'_> {
    fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.output_len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        if self.byte_coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        for range in self.byte_coverage.ranges() {
            if range.end()? > self.output_len {
                return Err(ReplicationError::Range);
            }
        }
        self.semantic_coverage.validate(limits)
    }
}

/// A wire execution result/receipt with explicitly untrusted identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireRecipeResult {
    /// Attempt identity.
    pub attempt: AttemptId,
    /// Untrusted recipe identity.
    pub recipe: WireIdentity,
    /// Untrusted semantic work identity.
    pub work_key: WireIdentity,
    /// Untrusted source basis root.
    pub input_basis: WorkspaceRootClaim,
    /// Untrusted input object/facet identities.
    pub inputs: Vec<WireIdentity>,
    /// Untrusted read-manifest identity.
    pub read_manifest: WireIdentity,
    /// Untrusted output object/facet identity.
    pub output: WireIdentity,
    /// Canonical output bytes carried by the untrusted result envelope.
    ///
    /// The shared immutable owner lets decode, attestation verification,
    /// admission, and lower receipt publication retain one allocation.
    pub output_bytes: Arc<Vec<u8>>,
    /// Scope echoed from the request.
    pub scope: ExecutionScopeId,
    /// Full resource envelope echoed from the request.
    pub resources: ResourceEnvelope,
    /// Byte/range coverage for the output bytes. This never claims semantic
    /// completeness.
    pub byte_coverage: SparseCoverage,
    /// Opaque semantic coverage claim bound to scope/read/authority.
    pub semantic_coverage: WireSemanticCoverage,
    /// Untrusted worker authority.
    pub authority: WireAuthority,
    /// Authority policy echoed from the request.
    pub authority_policy: WireAuthorityPolicy,
    /// Revocation observation.
    pub revocation_version: RevocationVersion,
    /// Publication fence echoed from the request.
    pub fence: Fence,
    /// Cancellation identity echoed from the request.
    pub cancellation: CancellationId,
    /// Optional authenticated worker statement.
    pub attestation: Option<Attestation>,
    /// Untrusted receipt statement identity.
    pub receipt: WireIdentity,
}
/// Generic name for a wire execution result receipt.
pub type WireExecutionResult = WireRecipeResult;
impl WireRecipeResult {
    /// Returns the checked logical bytes retained by this result envelope.
    /// Shared output bytes are charged exactly once here; cloning or moving
    /// the result preserves the same `Arc` allocation and does not increase
    /// this charge until an owner stores a distinct envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Overflow`] when a retained allocation
    /// cannot be represented by the platform `usize`.
    pub fn retained_size(&self) -> Result<usize, ReplicationError> {
        let inputs = self
            .inputs
            .capacity()
            .checked_mul(size_of::<WireIdentity>())
            .ok_or(ReplicationError::Overflow)?;
        let coverage = self
            .byte_coverage
            .retained_payload_size()
            .map_err(|_| ReplicationError::Overflow)?;
        size_of::<Self>()
            .checked_add(inputs)
            .and_then(|size| size.checked_add(self.output_bytes.capacity()))
            .and_then(|size| size.checked_add(coverage))
            .ok_or(ReplicationError::Overflow)
    }

    /// Checks bounded structure without admitting execution identities.
    ///
    /// # Errors
    ///
    /// Returns a message, coverage, or identifier error when the envelope is
    /// malformed or exceeds negotiated limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        let output_len =
            u64::try_from(self.output_bytes.len()).map_err(|_| ReplicationError::Overflow)?;
        if self.attempt.get() == 0 || self.fence.is_zero() || self.cancellation.is_zero() {
            return Err(ReplicationError::InvalidIdentifier);
        }
        if self.inputs.len() > limits.max_inputs {
            return Err(ReplicationError::MessageTooLarge);
        }
        self.resources.validate(limits)?;
        if self.byte_coverage.ranges().len() > limits.max_ranges {
            return Err(ReplicationError::CoverageLimit);
        }
        if output_len > limits.max_object {
            return Err(ReplicationError::ObjectTooLarge);
        }
        for range in self.byte_coverage.ranges() {
            if range.end()? > output_len {
                return Err(ReplicationError::Range);
            }
        }
        self.semantic_coverage.validate(limits)?;
        Ok(())
    }

    /// Builds the complete statement that a trust verifier must authenticate.
    #[must_use]
    pub fn attestation_material(&self) -> AttestationMaterial {
        self.attestation_material_view().to_owned_material()
    }

    /// Returns complete attestation material as a borrowing view.
    #[must_use]
    pub fn attestation_material_view(&self) -> AttestationMaterialView<'_> {
        AttestationMaterialView {
            attempt: self.attempt,
            cancellation: self.cancellation,
            recipe: self.recipe,
            work_key: self.work_key,
            input_basis: self.input_basis,
            inputs: &self.inputs,
            read_manifest: self.read_manifest,
            scope: self.scope,
            resources: self.resources,
            fence: self.fence,
            output: self.output,
            output_bytes: self.output_bytes.as_slice(),
            byte_coverage: &self.byte_coverage,
            semantic_coverage: &self.semantic_coverage,
            authority_policy: self.authority_policy,
            authority: self.authority,
            revocation_version: self.revocation_version,
            receipt: self.receipt,
        }
    }

    fn validate_against(
        &self,
        request: &ExecutionRequestExpectation,
        result: &ExecutionResultExpectation,
        limits: TransportLimits,
        observed_revocation: RevocationVersion,
    ) -> Result<(), ReplicationError> {
        result.validate(limits)?;
        self.validate_against_ref(request, &result.as_ref(), limits, observed_revocation)
    }

    fn validate_against_ref(
        &self,
        request: &ExecutionRequestExpectation,
        result: &ExecutionResultExpectationRef<'_>,
        limits: TransportLimits,
        observed_revocation: RevocationVersion,
    ) -> Result<(), ReplicationError> {
        self.validate(limits)?;
        request.validate(limits)?;
        result.validate(limits)?;
        if self.attempt != request.attempt
            || self.fence != request.fence
            || self.cancellation != request.cancellation
        {
            return Err(ReplicationError::StaleFence);
        }
        if self.scope != request.scope || self.resources != request.resources {
            return Err(ReplicationError::IdentityMismatch);
        }
        if self.revocation_version < request.authority.revocation_version
            || observed_revocation < self.revocation_version
        {
            return Err(ReplicationError::RevokedAuthority);
        }
        request.recipe.matches(self.recipe)?;
        request.work_key.matches(self.work_key)?;
        if self.inputs.len() != request.inputs.len() {
            return Err(ReplicationError::IdentityMismatch);
        }
        for (claim, expected_input) in self.inputs.iter().zip(&request.inputs) {
            expected_input.matches(*claim)?;
        }
        request.read_manifest.matches(self.read_manifest)?;
        self.input_basis.admit(request.input_basis)?;
        self.authority_policy
            .admit_against(request.authority, observed_revocation)?;
        if self.semantic_coverage.scope != request.scope
            || self.semantic_coverage.read_manifest != self.read_manifest
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        self.semantic_coverage
            .admit_against(result.semantic_coverage, observed_revocation)?;
        result.output.matches(self.output)?;
        // A matching output claim is still only a caller-owned expectation
        // until the bytes in this result reproduce that claim.  This check is
        // deliberately performed over the borrowed wire allocation so a
        // remote result cannot smuggle a different payload behind an
        // otherwise valid recipe/output identity pair.
        self.output
            .admit_canonical_value(self.output_bytes.as_slice())?;
        result.receipt.matches(self.receipt)?;
        let output_len =
            u64::try_from(self.output_bytes.len()).map_err(|_| ReplicationError::Overflow)?;
        if output_len != result.output_len || &self.byte_coverage != result.byte_coverage {
            return Err(ReplicationError::IdentityMismatch);
        }
        request.authority.admit(self.authority, observed_revocation)
    }

    /// Admits the result by exact comparison against caller-owned typed
    /// request, output, receipt, and semantic coverage material.
    ///
    /// The returned value proves only structural and exact-claim admission. It
    /// is intentionally not publishable until [`Self::admit_publishable`]
    /// authenticates the complete statement with a caller-owned verifier.
    ///
    /// # Errors
    ///
    /// Returns an identity, source-root, authority, revocation, fence, or
    /// bounds error when the result does not match the expected request.
    pub fn admit_against(
        &self,
        request: &ExecutionRequestExpectation,
        result: &ExecutionResultExpectation,
        limits: TransportLimits,
        observed_revocation: RevocationVersion,
    ) -> Result<AdmittedExecutionResult, ReplicationError> {
        self.validate_against(request, result, limits, observed_revocation)?;
        Ok(AdmittedExecutionResult {
            wire: Arc::new(self.clone()),
        })
    }

    /// Admits an owned result while retaining its original output allocation.
    ///
    /// The borrowed [`Self::admit_against`] API remains available for callers
    /// that need to retain the wire value. Callers that are finished with the
    /// wire envelope should use this consuming form: metadata is checked by
    /// borrow, then the complete result moves into the admitted shared owner
    /// without cloning its output bytes.
    ///
    /// # Errors
    ///
    /// Returns the same identity, authority, fence, coverage, and limit errors
    /// as [`Self::admit_against`].
    pub fn admit_against_owned(
        self,
        request: &ExecutionRequestExpectation,
        result: &ExecutionResultExpectation,
        limits: TransportLimits,
        observed_revocation: RevocationVersion,
    ) -> Result<AdmittedExecutionResult, ReplicationError> {
        self.validate_against(request, result, limits, observed_revocation)?;
        Ok(AdmittedExecutionResult {
            wire: Arc::new(self),
        })
    }

    /// Admits and authenticates a result as publishable under the supplied
    /// verifier. Absent, invalid, or diagnostic-only attestations cannot mint
    /// this capability.
    ///
    /// # Errors
    ///
    /// Returns an admission, authority, revocation, or attestation error when
    /// the result is not safe to publish.
    pub fn admit_publishable(
        &self,
        request: &ExecutionRequestExpectation,
        result: &ExecutionResultExpectation,
        limits: TransportLimits,
        observed_revocation: RevocationVersion,
        verifier: &dyn AttestationVerifier,
    ) -> Result<PublishableExecutionResult, ReplicationError> {
        let admitted = self.admit_against(request, result, limits, observed_revocation)?;
        if self.attestation.is_none() {
            return Err(ReplicationError::AttestationRequired);
        }
        let class = verifier.verify_view(self.attestation, &self.attestation_material_view())?;
        if !class.is_publishable() {
            return Err(ReplicationError::UntrustedAttestation);
        }
        Ok(PublishableExecutionResult { admitted, class })
    }

    /// Admits and authenticates an owned result without copying its output
    /// bytes. The verifier receives a borrowing view over the same output
    /// allocation, then the complete result moves into its admitted shared
    /// owner.
    ///
    /// # Errors
    ///
    /// Returns the same admission errors as [`Self::admit_publishable`],
    /// including [`ReplicationError::AttestationRequired`] when no statement
    /// is present and [`ReplicationError::InvalidAttestation`] when the
    /// verifier rejects it.
    pub fn admit_publishable_owned(
        self,
        request: &ExecutionRequestExpectation,
        result: &ExecutionResultExpectation,
        limits: TransportLimits,
        observed_revocation: RevocationVersion,
        verifier: &dyn AttestationVerifier,
    ) -> Result<PublishableExecutionResult, ReplicationError> {
        self.admit_publishable_owned_ref(
            request,
            &result.as_ref(),
            limits,
            observed_revocation,
            verifier,
        )
    }

    /// Admits and authenticates an owned result against a borrowed result
    /// expectation. This is the allocation-preserving entry point for owners
    /// whose expectation fields already live in the wire envelope.
    ///
    /// # Errors
    ///
    /// Returns the same admission, authority, fence, coverage, and attestation
    /// errors as [`Self::admit_publishable`].
    pub fn admit_publishable_owned_ref(
        self,
        request: &ExecutionRequestExpectation,
        result: &ExecutionResultExpectationRef<'_>,
        limits: TransportLimits,
        observed_revocation: RevocationVersion,
        verifier: &dyn AttestationVerifier,
    ) -> Result<PublishableExecutionResult, ReplicationError> {
        self.validate_against_ref(request, result, limits, observed_revocation)?;
        if self.attestation.is_none() {
            return Err(ReplicationError::AttestationRequired);
        }
        let class = verifier.verify_view(self.attestation, &self.attestation_material_view())?;
        if !class.is_publishable() {
            return Err(ReplicationError::UntrustedAttestation);
        }
        Ok(PublishableExecutionResult {
            admitted: AdmittedExecutionResult {
                wire: Arc::new(self),
            },
            class,
        })
    }
}
