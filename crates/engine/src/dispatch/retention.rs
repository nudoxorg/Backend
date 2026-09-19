//! Retained output publication evidence and validator adapters.
//!
//! This module owns the output/CAS retention boundary separately from semantic
//! coverage. A validator must return a capability that retains the exact
//! canonical bytes used by publication; a digest-only check cannot mint one.

use super::DispatchError;
use super::coverage::{
    CompleteSemanticCoverage, SemanticCoverageAdmissionError, SemanticCoverageBinding,
    SemanticCoverageValidator, UntrustedSemanticCoverageClaim,
};
use backend_execution::OutputVersion;
use std::sync::Arc;

/// Engine-owned recipe/schema/CAS validator required by every dispatcher.
///
/// A successful call returns [`RetainedOutput`].  The capability is opaque and
/// can only be constructed by the validator adapters in this module; its
/// presence records that the validator accepted responsibility for retaining
/// or retrieving the exact output.  The scheduler also retains the canonical
/// bytes in the trusted receipt before installing a reusable entry.
pub trait OutputAdmissionValidator: Send + Sync + 'static {
    /// Validates canonical output bytes and an already admitted semantic
    /// dependency capability.
    ///
    /// Semantic admission is a separate mandatory step.  Receiving a
    /// [`CompleteSemanticCoverage`] here means this validator cannot turn a
    /// raw producer assertion into publication merely by checking a digest.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn validate(
        &self,
        output: OutputVersion,
        bytes: &[u8],
        semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError>;

    /// Validates a shared immutable output owner without copying its bytes.
    ///
    /// Remote admission uses this path after transport decoding. Validators
    /// that can retain the supplied owner should override it; the default is
    /// a compatibility bridge for validators that only inspect a slice.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn validate_shared(
        &self,
        output: OutputVersion,
        bytes: &Arc<Vec<u8>>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        // Slice-only validators still participate in the shared publication
        // path.  Once their check succeeds, retain the caller's immutable
        // owner rather than materialising a second multi-megabyte buffer.
        // Preserve the capability returned by the validator. A validator
        // that owns a CAS lease may deliberately return a different shared
        // owner (for example, one backed by a store pin); discarding it and
        // manufacturing an owner from the caller's slice would sever that
        // retention proof. Validators that can share the transport buffer
        // should override this method, while slice-only implementations keep
        // their own returned owner here.
        self.validate(output, bytes.as_slice(), semantic)
    }
}

/// Opaque publication evidence returned by an output/CAS validator.
///
/// The evidence owns or shares the canonical bytes that the validator checked.
/// A digest-only validator cannot satisfy the trait without also retaining the
/// bytes needed for local retrieval.  The public constructors are intended for
/// concrete validator implementations at the configured trust boundary.
#[must_use = "retain output publication evidence until receipt publication"]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedOutput {
    output: OutputVersion,
    canonical_bytes: Arc<Vec<u8>>,
}

impl RetainedOutput {
    /// Creates retained publication evidence from a validated byte slice.
    ///
    /// Implementors of [`OutputAdmissionValidator`] use this constructor
    /// after checking their store/CAS contract.  The returned value owns the
    /// canonical bytes, so scheduler publication cannot outlive the storage
    /// obligation represented by the validator result.
    pub fn from_bytes(output: OutputVersion, bytes: &[u8]) -> Self {
        Self {
            output,
            canonical_bytes: Arc::new(bytes.to_vec()),
        }
    }

    /// Creates retained publication evidence while sharing an immutable
    /// canonical byte owner with the validator's store/CAS path.
    pub fn from_shared(output: OutputVersion, canonical_bytes: Arc<Vec<u8>>) -> Self {
        Self {
            output,
            canonical_bytes,
        }
    }

    /// Returns the version accepted by the retaining validator.
    #[must_use]
    pub const fn output(&self) -> OutputVersion {
        self.output
    }

    /// Returns the retained canonical output bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        self.canonical_bytes.as_slice()
    }

    pub(super) fn canonical_bytes_arc(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.canonical_bytes)
    }
}

/// Explicit fail-closed capability useful only for wiring an owner before a
/// product validator is installed. It never admits a recipe result and is
/// therefore safe as a type default without becoming a production hash-only
/// validator.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnconfiguredOutputValidator;

impl OutputAdmissionValidator for UnconfiguredOutputValidator {
    fn validate(
        &self,
        _output: OutputVersion,
        _bytes: &[u8],
        _semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        Err(DispatchError::AuthorityRejected)
    }
}

impl SemanticCoverageValidator for UnconfiguredOutputValidator {
    fn validate(
        &self,
        _binding: &SemanticCoverageBinding,
        _claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        Err(SemanticCoverageAdmissionError::Rejected)
    }
}

// A closure that merely checks a slice is a useful unit-test fixture, but it
// cannot communicate a store/CAS lease or retrieval obligation. Keep this
// compatibility adapter out of production so a hash-only closure cannot be
// mistaken for the required retaining validator.
#[cfg(test)]
impl<F> OutputAdmissionValidator for F
where
    F: Fn(OutputVersion, &[u8], &CompleteSemanticCoverage) -> Result<(), DispatchError>
        + Send
        + Sync
        + 'static,
{
    fn validate(
        &self,
        output: OutputVersion,
        bytes: &[u8],
        semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        self(output, bytes, semantic)?;
        Ok(RetainedOutput::from_bytes(output, bytes))
    }

    fn validate_shared(
        &self,
        output: OutputVersion,
        bytes: &Arc<Vec<u8>>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        self(output, bytes.as_slice(), semantic)?;
        Ok(RetainedOutput::from_shared(output, Arc::clone(bytes)))
    }
}

/// Composes independent output and recipe/scope authority validators into the
/// single mandatory validator capability held by a [`crate::dispatch::Dispatcher`].
#[derive(Clone, Debug)]
pub struct CompositeAdmissionValidator<O, S> {
    output: O,
    semantic: S,
}

impl<O, S> CompositeAdmissionValidator<O, S> {
    /// Creates a composed validator from an output validator and a semantic
    /// coverage authority.
    #[must_use]
    pub const fn new(output: O, semantic: S) -> Self {
        Self { output, semantic }
    }

    /// Borrows the output validator.
    #[must_use]
    pub const fn output(&self) -> &O {
        &self.output
    }

    /// Borrows the semantic coverage authority validator.
    #[must_use]
    pub const fn semantic(&self) -> &S {
        &self.semantic
    }
}

impl<O, S> OutputAdmissionValidator for CompositeAdmissionValidator<O, S>
where
    O: OutputAdmissionValidator,
    S: Send + Sync + 'static,
{
    fn validate(
        &self,
        output: OutputVersion,
        bytes: &[u8],
        semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        self.output.validate(output, bytes, semantic)
    }

    fn validate_shared(
        &self,
        output: OutputVersion,
        bytes: &Arc<Vec<u8>>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<RetainedOutput, DispatchError> {
        self.output.validate_shared(output, bytes, semantic)
    }
}

impl<O, S> SemanticCoverageValidator for CompositeAdmissionValidator<O, S>
where
    O: Send + Sync + 'static,
    S: SemanticCoverageValidator,
{
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        self.semantic.validate(binding, claim)
    }

    fn validate_manifest(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
        manifest: &backend_semantic::DependencyManifest,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        self.semantic.validate_manifest(binding, claim, manifest)
    }
}

/// Explicit fixtures for tests that need deterministic byte hashing and a
/// deliberately small semantic authority.  Production code must provide its
/// own recipe/schema/CAS and scope validator.
pub mod test_util {
    use super::super::coverage::{
        CompleteSemanticCoverage, SemanticCoverageAdmissionError, SemanticCoverageBinding,
        SemanticCoverageValidator, UntrustedSemanticCoverageClaim,
    };
    use super::{DispatchError, OutputAdmissionValidator, RetainedOutput};
    use backend_execution::{OutputVersion, VersionedWorkIdentity};
    use backend_version::Relation;

    /// Hash-only output validator for explicit unit-test fixtures.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct HashOnlyOutputValidator;

    impl OutputAdmissionValidator for HashOnlyOutputValidator {
        fn validate(
            &self,
            output: OutputVersion,
            bytes: &[u8],
            _semantic: &CompleteSemanticCoverage,
        ) -> Result<RetainedOutput, DispatchError> {
            if OutputVersion::from_value(bytes) == output {
                Ok(RetainedOutput::from_bytes(output, bytes))
            } else {
                Err(DispatchError::OutputMismatch)
            }
        }

        fn validate_shared(
            &self,
            output: OutputVersion,
            bytes: &std::sync::Arc<Vec<u8>>,
            _semantic: &CompleteSemanticCoverage,
        ) -> Result<RetainedOutput, DispatchError> {
            if OutputVersion::from_value(bytes.as_slice()) != output {
                return Err(DispatchError::OutputMismatch);
            }
            Ok(RetainedOutput::from_shared(
                output,
                std::sync::Arc::clone(bytes),
            ))
        }
    }

    /// Compatibility spelling for the explicit hash-only test fixture.
    pub type CanonicalOutputValidator = HashOnlyOutputValidator;

    /// Minimal semantic authority for dispatch unit tests.
    ///
    /// It checks the immutable identity binding and requires a complete
    /// producer assertion.  Real authorities must additionally validate the
    /// recipe's declared reads, witness bytes, epoch, and revocation state.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct HashOnlySemanticCoverageValidator;

    impl SemanticCoverageValidator for HashOnlySemanticCoverageValidator {
        fn validate(
            &self,
            binding: &SemanticCoverageBinding,
            claim: &UntrustedSemanticCoverageClaim,
        ) -> Result<(), SemanticCoverageAdmissionError> {
            if !claim.asserted_complete() || claim.scope() == 0 {
                return Err(SemanticCoverageAdmissionError::Incomplete);
            }
            if claim.read_manifest() != binding.read_manifest()
                || claim.authority() != binding.authority()
            {
                return Err(SemanticCoverageAdmissionError::BindingMismatch);
            }
            Ok(())
        }

        fn validate_manifest(
            &self,
            binding: &SemanticCoverageBinding,
            claim: &UntrustedSemanticCoverageClaim,
            manifest: &backend_semantic::DependencyManifest,
        ) -> Result<(), SemanticCoverageAdmissionError> {
            if !manifest.is_reuse_ready() {
                return Err(SemanticCoverageAdmissionError::Rejected);
            }
            self.validate(binding, claim)
        }
    }

    /// Combined fixture validator used by dispatcher tests.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct HashOnlyAdmissionValidator;

    impl OutputAdmissionValidator for HashOnlyAdmissionValidator {
        fn validate(
            &self,
            output: OutputVersion,
            bytes: &[u8],
            semantic: &CompleteSemanticCoverage,
        ) -> Result<RetainedOutput, DispatchError> {
            HashOnlyOutputValidator.validate(output, bytes, semantic)
        }

        fn validate_shared(
            &self,
            output: OutputVersion,
            bytes: &std::sync::Arc<Vec<u8>>,
            semantic: &CompleteSemanticCoverage,
        ) -> Result<RetainedOutput, DispatchError> {
            HashOnlyOutputValidator.validate_shared(output, bytes, semantic)
        }
    }

    impl SemanticCoverageValidator for HashOnlyAdmissionValidator {
        fn validate(
            &self,
            binding: &SemanticCoverageBinding,
            claim: &UntrustedSemanticCoverageClaim,
        ) -> Result<(), SemanticCoverageAdmissionError> {
            HashOnlySemanticCoverageValidator.validate(binding, claim)
        }

        fn validate_manifest(
            &self,
            binding: &SemanticCoverageBinding,
            claim: &UntrustedSemanticCoverageClaim,
            manifest: &backend_semantic::DependencyManifest,
        ) -> Result<(), SemanticCoverageAdmissionError> {
            HashOnlySemanticCoverageValidator.validate_manifest(binding, claim, manifest)
        }
    }

    /// Produces a complete capability for a test identity using the explicit
    /// test authority above.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn admit_complete<R: Relation>(
        identity: &VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
    ) -> Result<CompleteSemanticCoverage, SemanticCoverageAdmissionError> {
        CompleteSemanticCoverage::admit(identity, claim, &HashOnlySemanticCoverageValidator)
    }
}
