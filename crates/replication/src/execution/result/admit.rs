//! Admission and byte accounting for a wire execution result.

use super::super::{
    AdmittedExecutionResult, AttestationMaterial, AttestationMaterialView, AttestationVerifier,
    ExecutionRequestExpectation, PublishableExecutionResult, ReplicationError, RevocationVersion,
    TransportLimits, WireIdentity,
};
use super::*;
use std::{mem::size_of, sync::Arc};

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
