//! Trusted output admission and typed result receipts.

use super::proof::{AuthorityVerifier, UntrustedAuthorityClaim, UntrustedOutputClaim};
use super::{
    AttemptError, AttemptFence, AttemptLease, BoundOutputValidator, ExecutionAuthorityEvidence,
    OutputValidationError, OutputValidator, ResultCoverage,
};
use crate::types::IdentityBinding;
use crate::{
    AuthorityVersion, OutputEquivalence, OutputVersion, ReadManifestId, RecipeId,
    VersionedWorkIdentity, WorkKey,
};
use backend_version::{IdContext, Relation, StateRoot, UntrustedId};
use std::{fmt, sync::Arc};

/// An output admission capability minted only after schema, coverage, and
/// engine-owned output validation have succeeded.
#[must_use = "pass the admitted output into receipt construction"]
pub struct OutputAdmission {
    output: OutputVersion,
    coverage: ResultCoverage,
    canonical_bytes: Arc<Vec<u8>>,
}

impl fmt::Debug for OutputAdmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutputAdmission")
            .field("output", &self.output)
            .field("coverage", &self.coverage)
            .field("canonical_bytes", &self.canonical_bytes.len())
            .finish()
    }
}

impl OutputAdmission {
    /// Validates and admits one fixed-width output claim.
    /// # Errors
    ///
    /// Returns [`OutputAdmissionError`] when the claimed bytes, schema,
    /// coverage, or engine validator do not admit the output.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the claim owns bounded canonical bytes while validation runs"
    )]
    pub fn admit<V: OutputValidator>(
        claim: UntrustedOutputClaim,
        validator: &V,
    ) -> Result<Self, OutputAdmissionError> {
        Self::admit_claim(&claim, |output, canonical_bytes, coverage| {
            validator.validate_output(output, canonical_bytes, coverage)
        })
    }

    /// Validates and admits one output claim against the complete typed work
    /// identity. Receipt admission uses this method so semantic coverage is
    /// checked with the exact read manifest and authority in scope.
    /// # Errors
    ///
    /// Returns [`OutputAdmissionError`] when the claimed bytes, schema,
    /// coverage, or engine validator do not admit the output.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the claim owns bounded canonical bytes while validation runs"
    )]
    pub fn admit_bound<R: Relation, V: BoundOutputValidator<R>>(
        identity: &VersionedWorkIdentity<R>,
        claim: UntrustedOutputClaim,
        validator: &V,
    ) -> Result<Self, OutputAdmissionError> {
        Self::admit_claim(&claim, |output, canonical_bytes, coverage| {
            validator.validate_bound_output(identity, output, canonical_bytes, coverage)
        })
    }

    fn admit_claim<F>(
        claim: &UntrustedOutputClaim,
        validator: F,
    ) -> Result<Self, OutputAdmissionError>
    where
        F: for<'a> FnOnce(
            OutputVersion,
            &'a [u8],
            ResultCoverage,
        ) -> Result<(), OutputValidationError>,
    {
        if !claim.coverage.is_complete() {
            return Err(OutputAdmissionError::IncompleteCoverage);
        }
        let output = OutputVersion::admit_value(
            UntrustedId::<crate::OutputSchema>::from_wire(
                &claim.output,
                IdContext::schema::<crate::OutputSchema>(),
            )
            .map_err(|_| OutputAdmissionError::InvalidWire)?,
            claim.canonical_bytes.as_slice(),
        )
        .map_err(|error| match error {
            backend_version::IdAdmissionError::ContextMismatch { .. }
            | backend_version::IdAdmissionError::UnverifiedDigest => {
                OutputAdmissionError::InvalidWire
            }
            backend_version::IdAdmissionError::DigestMismatch => {
                OutputAdmissionError::ContentMismatch
            }
        })?;
        validator(output, claim.canonical_bytes.as_slice(), claim.coverage)
            .map_err(OutputAdmissionError::Rejected)?;
        Ok(Self {
            output,
            coverage: claim.coverage,
            // The claim is consumed only after validation, so retaining its
            // canonical bytes moves the original Vec allocation into one
            // shared immutable owner instead of copying it for publication.
            canonical_bytes: Arc::clone(&claim.canonical_bytes),
        })
    }

    /// Returns the validated immutable output version.
    #[must_use]
    pub const fn output(&self) -> OutputVersion {
        self.output
    }

    /// Returns the validated coverage contract.
    #[must_use]
    pub const fn coverage(&self) -> ResultCoverage {
        self.coverage
    }

    /// Returns the canonical bytes retained by this admission capability.
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        self.canonical_bytes.as_slice()
    }
}

/// Failure while admitting a worker output claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputAdmissionError {
    /// The fixed-width claim did not carry the expected output schema.
    InvalidWire,
    /// Canonical bytes did not hash to the claimed output object version.
    ContentMismatch,
    /// The claim did not cover the requested dependency-closed scope.
    IncompleteCoverage,
    /// Engine/CAS validation rejected the claimed output.
    Rejected(OutputValidationError),
}

impl fmt::Display for OutputAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "output admission error: {self:?}")
    }
}

impl std::error::Error for OutputAdmissionError {}

/// Untrusted worker claims. The scheduler must pass this through
/// [`ResultReceipt::admit_wire`] before it can be accepted or cached.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedResultReceipt {
    /// Claimed work-key bytes.
    pub key: [u8; 32],
    /// Claimed input-root bytes.
    pub input: [u8; 32],
    /// Claimed recipe-version bytes.
    pub recipe: [u8; 32],
    /// Claimed read-manifest bytes.
    pub read_manifest: [u8; 32],
    /// Claimed authority-version bytes.
    pub authority: [u8; 32],
    /// Claimed authority epoch. The engine verifier decides whether it is
    /// current for the requested workspace and recipe.
    pub authority_epoch: u64,
    /// Claimed revocation observation. A raw value never authorizes a result;
    /// it is checked by [`AuthorityVerifier`].
    pub revocation_version: u64,
    /// Optional bounded worker attestation passed to the authority verifier.
    pub attestation: Option<[u8; 64]>,
    /// Claimed output-equivalence bytes.
    pub output_equivalence: [u8; 32],
    /// Claimed output-version bytes.
    pub output: [u8; 32],
    /// Canonical output bytes needed to verify the output version. Transport
    /// adapters must bound this allocation before constructing the claim.
    pub output_canonical_bytes: Arc<Vec<u8>>,
    /// Claimed result coverage.
    pub coverage: ResultCoverage,
    /// Claimed attempt ordinal.
    pub ordinal: u32,
    /// Claimed attempt-fence bytes.
    pub fence: [u8; 32],
}

/// A typed worker result receipt. `R` ties its input root to one relation
/// schema, and construction binds every echoed claim to the leased identity.
#[derive(Debug)]
pub struct ResultReceipt<R: Relation> {
    /// Semantic key claimed by the worker.
    pub(super) key: WorkKey,
    /// Complete typed identity echoed by the worker.
    pub(super) identity: VersionedWorkIdentity<R>,
    /// Input root echoed as a separately checked field for wire decoders.
    pub(super) input: StateRoot<R>,
    /// Recipe identity echoed by the worker.
    pub(super) recipe: RecipeId,
    /// Exact read manifest echoed by the worker.
    pub(super) read_manifest: ReadManifestId,
    /// Worker authority/capability revision.
    pub(super) authority: AuthorityVersion,
    /// Output equivalence contract echoed by the worker.
    pub(super) output_equivalence: OutputEquivalence,
    /// Immutable output object version.
    pub(super) result: OutputVersion,
    /// Canonical bytes retained for local retrieval and reusable publication.
    pub(super) canonical_bytes: Arc<Vec<u8>>,
    /// Coverage of the claimed result.
    pub(super) coverage: ResultCoverage,
    /// Attempt ordinal selected by the worker.
    pub(super) ordinal: u32,
    /// Opaque attempt fence selected by the scheduler.
    pub(super) fence: AttemptFence,
    /// Engine-verified authority, output, and semantic coverage evidence.
    pub(super) authority_evidence: ExecutionAuthorityEvidence,
}

impl<R: Relation> Clone for ResultReceipt<R> {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            identity: self.identity,
            input: self.input,
            recipe: self.recipe,
            read_manifest: self.read_manifest,
            authority: self.authority,
            output_equivalence: self.output_equivalence,
            result: self.result,
            canonical_bytes: Arc::clone(&self.canonical_bytes),
            coverage: self.coverage,
            ordinal: self.ordinal,
            fence: self.fence,
            authority_evidence: ExecutionAuthorityEvidence {
                key: self.authority_evidence.key,
                authority: self.authority_evidence.authority,
                authority_epoch: self.authority_evidence.authority_epoch,
                revocation_version: self.authority_evidence.revocation_version,
                output: self.authority_evidence.output,
                coverage: self.authority_evidence.coverage,
                ordinal: self.authority_evidence.ordinal,
                fence: self.authority_evidence.fence,
                incarnation: self.authority_evidence.incarnation,
            },
        }
    }
}

impl<R: Relation> PartialEq for ResultReceipt<R> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.identity == other.identity
            && self.input == other.input
            && self.recipe == other.recipe
            && self.read_manifest == other.read_manifest
            && self.authority == other.authority
            && self.output_equivalence == other.output_equivalence
            && self.result == other.result
            && self.canonical_bytes == other.canonical_bytes
            && self.coverage == other.coverage
            && self.ordinal == other.ordinal
            && self.fence == other.fence
            && self.authority_evidence.key == other.authority_evidence.key
            && self.authority_evidence.authority == other.authority_evidence.authority
            && self.authority_evidence.authority_epoch == other.authority_evidence.authority_epoch
            && self.authority_evidence.revocation_version
                == other.authority_evidence.revocation_version
            && self.authority_evidence.output == other.authority_evidence.output
            && self.authority_evidence.coverage == other.authority_evidence.coverage
            && self.authority_evidence.ordinal == other.authority_evidence.ordinal
            && self.authority_evidence.fence == other.authority_evidence.fence
    }
}

impl<R: Relation> Eq for ResultReceipt<R> {}

impl<R: Relation> ResultReceipt<R> {
    /// Creates a receipt bound to an exact typed work identity and admitted
    /// output. This constructor remains crate-private so callers must pass
    /// through the public wire/admission seam.
    /// # Errors
    ///
    /// Returns [`AttemptError`] when any identity, lease fence, or output
    /// admission field differs from the typed request.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "receipt construction consumes the already admitted capabilities"
    )]
    pub(crate) fn from_admission(
        identity: VersionedWorkIdentity<R>,
        lease: &AttemptLease,
        admission: OutputAdmission,
        authority_evidence: ExecutionAuthorityEvidence,
    ) -> Result<Self, AttemptError> {
        let binding = IdentityBinding::from(identity);
        if binding.key != lease.key
            || lease.binding != binding
            || !authority_evidence.matches(identity, lease, &admission)
        {
            return Err(AttemptError::AuthorityRejected);
        }
        if admission.coverage() != ResultCoverage::Complete {
            return Err(AttemptError::IncompleteCoverage);
        }
        Ok(Self {
            key: lease.key,
            input: identity.input,
            recipe: identity.recipe,
            read_manifest: identity.read_manifest,
            authority: identity.authority,
            output_equivalence: identity.output_equivalence,
            identity,
            result: admission.output,
            canonical_bytes: admission.canonical_bytes,
            coverage: ResultCoverage::Complete,
            ordinal: lease.ordinal,
            fence: lease.fence,
            authority_evidence,
        })
    }

    /// Admits fixed-width worker claims against the exact typed identity and
    /// lease. No raw claim becomes a trusted receipt unless every schema and
    /// publication binding agrees.
    /// # Errors
    ///
    /// Returns [`AttemptError`] when the wire claim has a stale fence or an
    /// identity mismatch, or when output admission rejects its bytes or
    /// coverage.
    pub fn admit_wire<V: BoundOutputValidator<R>, A: AuthorityVerifier<R>>(
        identity: VersionedWorkIdentity<R>,
        lease: &AttemptLease,
        wire: UntrustedResultReceipt,
        validator: &V,
        authority_verifier: &A,
    ) -> Result<Self, AttemptError> {
        let key = identity.work_key();
        if wire.key != key.to_bytes() {
            return Err(AttemptError::KeyMismatch);
        }
        if wire.ordinal != lease.ordinal || wire.fence != *lease.fence.as_bytes() {
            return Err(AttemptError::Stale);
        }
        if wire.input != identity.input.to_bytes() {
            return Err(AttemptError::InputMismatch);
        }
        if wire.recipe != identity.recipe.to_bytes() {
            return Err(AttemptError::RecipeMismatch);
        }
        if wire.read_manifest != identity.read_manifest.to_bytes() {
            return Err(AttemptError::ReadManifestMismatch);
        }
        if wire.authority != identity.authority.to_bytes() {
            return Err(AttemptError::AuthorityMismatch);
        }
        if wire.output_equivalence != identity.output_equivalence.to_bytes() {
            return Err(AttemptError::OutputEquivalenceMismatch);
        }
        let admission = OutputAdmission::admit_bound(
            &identity,
            UntrustedOutputClaim {
                output: wire.output,
                canonical_bytes: wire.output_canonical_bytes,
                coverage: wire.coverage,
            },
            validator,
        )
        .map_err(|error| match error {
            OutputAdmissionError::IncompleteCoverage => AttemptError::IncompleteCoverage,
            OutputAdmissionError::InvalidWire
            | OutputAdmissionError::ContentMismatch
            | OutputAdmissionError::Rejected(_) => AttemptError::InvalidResult,
        })?;
        let authority_claim = UntrustedAuthorityClaim {
            authority: wire.authority,
            authority_epoch: wire.authority_epoch,
            revocation_version: wire.revocation_version,
            attestation: wire.attestation,
        };
        authority_verifier
            .verify_authority(&identity, lease, &authority_claim, &admission)
            .map_err(|_| AttemptError::AuthorityRejected)?;
        let evidence =
            ExecutionAuthorityEvidence::mint(identity, lease, &authority_claim, &admission);
        Self::from_admission(identity, lease, admission, evidence)
    }

    /// Returns the semantic key claimed by this receipt.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns the complete identity echoed by this receipt.
    #[must_use]
    pub const fn identity(&self) -> VersionedWorkIdentity<R> {
        self.identity
    }

    /// Returns the immutable output claimed by this receipt.
    #[must_use]
    pub const fn result(&self) -> OutputVersion {
        self.result
    }

    /// Returns the canonical bytes retained by this trusted receipt.
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        self.canonical_bytes.as_slice()
    }

    pub(crate) fn canonical_bytes_arc(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.canonical_bytes)
    }

    /// Returns the allocation capacity retained by this trusted receipt.
    /// Capacity is the ownership measure used by bounded publication caches;
    /// it can exceed the logical payload length when a caller supplied a
    /// reserved buffer.
    #[must_use]
    pub fn canonical_bytes_capacity(&self) -> usize {
        self.canonical_bytes.capacity()
    }

    /// Returns the attempt ordinal carried by this receipt.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the opaque publication fence carried by this receipt.
    #[must_use]
    pub const fn fence(&self) -> AttemptFence {
        self.fence
    }

    /// Returns the worker's declared result coverage.
    #[must_use]
    pub const fn coverage(&self) -> ResultCoverage {
        self.coverage
    }

    /// Returns the verified authority evidence bound to this receipt.
    pub const fn authority_evidence(&self) -> &ExecutionAuthorityEvidence {
        &self.authority_evidence
    }

    pub(crate) fn output_admission(&self) -> OutputAdmission {
        OutputAdmission {
            output: self.result,
            coverage: self.coverage,
            canonical_bytes: Arc::clone(&self.canonical_bytes),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_input(&mut self, input: StateRoot<R>) {
        self.input = input;
    }

    #[cfg(test)]
    pub(crate) fn test_set_read_manifest(&mut self, read_manifest: ReadManifestId) {
        self.read_manifest = read_manifest;
    }

    #[cfg(test)]
    pub(crate) fn test_set_authority(&mut self, authority: AuthorityVersion) {
        self.authority = authority;
    }
}
