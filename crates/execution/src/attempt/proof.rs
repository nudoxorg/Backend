//! Output, authority, and semantic proof interfaces.

use super::{AttemptFence, AttemptLease, OutputAdmission, ResultCoverage};
use crate::types::OutputVersion;
use crate::{AuthorityVersion, VersionedWorkIdentity, WorkKey};
use backend_version::Relation;
use std::fmt;
use std::sync::Arc;

/// Failure reported by a worker/store validator before an output is admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputValidationError {
    /// The claimed immutable output was not present in the authorized CAS.
    Missing,
    /// Canonical bytes did not hash to the claimed output version.
    ContentMismatch,
    /// The claimed coverage did not prove the requested dependency-closed
    /// scope.
    CoverageMismatch,
    /// The worker result did not satisfy the recipe's output contract.
    ContractMismatch,
}

impl fmt::Display for OutputValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "output validation error: {self:?}")
    }
}

impl std::error::Error for OutputValidationError {}

/// Validation seam owned by the engine/CAS adapter.
pub trait OutputValidator {
    /// Validates canonical bytes, CAS membership, and recipe-specific output
    /// evidence for an already schema-admitted output version. The bytes and
    /// coverage claim are supplied so an engine can check the exact object and
    /// dependency-closed scope that was admitted.
    ///
    /// # Errors
    ///
    /// Returns the validator's reason when the CAS or recipe evidence is not
    /// sufficient to admit the output.
    fn validate_output(
        &self,
        output: OutputVersion,
        canonical_bytes: &[u8],
        coverage: ResultCoverage,
    ) -> Result<(), OutputValidationError>;
}

impl<F> OutputValidator for F
where
    F: for<'a> Fn(OutputVersion, &'a [u8], ResultCoverage) -> Result<(), OutputValidationError>,
{
    fn validate_output(
        &self,
        output: OutputVersion,
        canonical_bytes: &[u8],
        coverage: ResultCoverage,
    ) -> Result<(), OutputValidationError> {
        self(output, canonical_bytes, coverage)
    }
}

/// Identity-bound output validation used for worker receipt admission.
///
/// The ordinary [`OutputValidator`] seam is useful for an already selected
/// output. A receipt additionally needs the exact recipe, input root, read
/// manifest, authority, and equivalence contract. This trait makes that
/// identity available to the engine validator before execution mints a
/// receipt, so a byte-range or bare `Complete` claim cannot stand in for
/// semantic coverage.
pub trait BoundOutputValidator<R: Relation> {
    /// Validates output bytes and semantic coverage for one exact identity.
    ///
    /// # Errors
    ///
    /// Returns the validator's reason when the output or dependency-closed
    /// scope is not authorized for the identity.
    fn validate_bound_output(
        &self,
        identity: &VersionedWorkIdentity<R>,
        output: OutputVersion,
        canonical_bytes: &[u8],
        coverage: ResultCoverage,
    ) -> Result<(), OutputValidationError>;
}

impl<R, F> BoundOutputValidator<R> for F
where
    R: Relation,
    F: for<'a> Fn(
        &'a VersionedWorkIdentity<R>,
        OutputVersion,
        &'a [u8],
        ResultCoverage,
    ) -> Result<(), OutputValidationError>,
{
    fn validate_bound_output(
        &self,
        identity: &VersionedWorkIdentity<R>,
        output: OutputVersion,
        canonical_bytes: &[u8],
        coverage: ResultCoverage,
    ) -> Result<(), OutputValidationError> {
        self(identity, output, canonical_bytes, coverage)
    }
}

/// Failure reported by the engine's authority/revocation verifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityValidationError {
    /// The worker is not authorized for this recipe, input, or workspace.
    Unauthorized,
    /// The authority or revocation observation is too old.
    Revoked,
    /// The evidence does not bind to the exact output or semantic coverage.
    BindingMismatch,
    /// The attestation or policy statement is malformed.
    Malformed,
}

impl fmt::Display for AuthorityValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "authority validation error: {self:?}")
    }
}

impl std::error::Error for AuthorityValidationError {}

/// Output claim awaiting bounded engine/CAS validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedOutputClaim {
    /// Claimed output object version bytes.
    pub output: [u8; 32],
    /// Canonical bytes associated with the claimed output. The transport
    /// adapter must enforce its own bounded object-size policy before creating
    /// this value.
    pub canonical_bytes: Arc<Vec<u8>>,
    /// Claimed coverage of the output.
    pub coverage: ResultCoverage,
}

/// Engine/authority evidence that remains untrusted until its verifier
/// accepts the exact identity, lease, output, and semantic coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedAuthorityClaim {
    /// Claimed authority-version bytes.
    pub authority: [u8; 32],
    /// Authority epoch at which the worker ran.
    pub authority_epoch: u64,
    /// Revocation observation carried by the worker or transport.
    pub revocation_version: u64,
    /// Optional bounded attestation statement. Its meaning belongs to the
    /// engine policy verifier.
    pub attestation: Option<[u8; 64]>,
}

/// A non-forgeable authority capability minted only after an engine verifier
/// accepts a complete output admission. The private binding prevents callers
/// from manufacturing a receipt by copying a raw authority ID or enum.
#[must_use = "retain authority evidence until the result receipt is built"]
pub struct ExecutionAuthorityEvidence {
    pub(super) key: WorkKey,
    pub(super) authority: AuthorityVersion,
    pub(super) authority_epoch: u64,
    pub(super) revocation_version: u64,
    pub(super) output: OutputVersion,
    pub(super) coverage: ResultCoverage,
    pub(super) ordinal: u32,
    pub(super) fence: AttemptFence,
    pub(super) incarnation: [u8; 32],
}

impl fmt::Debug for ExecutionAuthorityEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionAuthorityEvidence")
            .field("key", &self.key)
            .field("authority", &self.authority)
            .field("authority_epoch", &self.authority_epoch)
            .field("revocation_version", &self.revocation_version)
            .field("output", &self.output)
            .field("coverage", &self.coverage)
            .field("ordinal", &self.ordinal)
            .field("fence", &self.fence)
            .finish_non_exhaustive()
    }
}

impl ExecutionAuthorityEvidence {
    /// Returns the exact semantic work key covered by this evidence.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns the authority version accepted by the engine policy.
    #[must_use]
    pub const fn authority(&self) -> AuthorityVersion {
        self.authority
    }

    /// Returns the accepted worker authority epoch.
    #[must_use]
    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    /// Returns the accepted revocation observation.
    #[must_use]
    pub const fn revocation_version(&self) -> u64 {
        self.revocation_version
    }

    /// Returns the exact output bound by this evidence.
    #[must_use]
    pub const fn output(&self) -> OutputVersion {
        self.output
    }

    /// Returns the complete semantic coverage bound by this evidence.
    #[must_use]
    pub const fn coverage(&self) -> ResultCoverage {
        self.coverage
    }

    /// Returns the attempt ordinal covered by this evidence.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the opaque fence covered by this evidence.
    #[must_use]
    pub const fn fence(&self) -> AttemptFence {
        self.fence
    }

    /// Returns the durable process incarnation bound to this evidence.
    #[must_use]
    pub const fn incarnation(&self) -> [u8; 32] {
        self.incarnation
    }

    pub(crate) fn mint<R: Relation>(
        identity: VersionedWorkIdentity<R>,
        lease: &AttemptLease,
        claim: &UntrustedAuthorityClaim,
        admission: &OutputAdmission,
    ) -> Self {
        let key = identity.work_key();
        Self {
            key,
            authority: identity.authority,
            authority_epoch: claim.authority_epoch,
            revocation_version: claim.revocation_version,
            output: admission.output(),
            coverage: admission.coverage(),
            ordinal: lease.ordinal,
            fence: lease.fence,
            incarnation: lease.incarnation,
        }
    }

    pub(super) fn matches<R: Relation>(
        &self,
        identity: VersionedWorkIdentity<R>,
        lease: &AttemptLease,
        admission: &OutputAdmission,
    ) -> bool {
        let key = identity.work_key();
        self.key == key
            && self.authority == identity.authority
            && self.output == admission.output()
            && self.coverage == admission.coverage()
            && self.ordinal == lease.ordinal
            && self.fence == lease.fence
            && self.incarnation == lease.incarnation
    }
}

/// Engine-owned authority and semantic coverage verifier.
pub trait AuthorityVerifier<R: Relation> {
    /// Verifies worker authority, revocation, exact identity, complete
    /// semantic coverage, and the output admission already performed by
    /// execution. Returning success lets execution mint a private evidence
    /// capability bound to every supplied field.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityValidationError`] when policy, revocation, worker
    /// attestation, identity, or coverage evidence is insufficient.
    fn verify_authority(
        &self,
        identity: &VersionedWorkIdentity<R>,
        lease: &AttemptLease,
        claim: &UntrustedAuthorityClaim,
        admission: &OutputAdmission,
    ) -> Result<(), AuthorityValidationError>;
}

impl<R, F> AuthorityVerifier<R> for F
where
    R: Relation,
    F: for<'a> Fn(
        &'a VersionedWorkIdentity<R>,
        &'a AttemptLease,
        &'a UntrustedAuthorityClaim,
        &'a OutputAdmission,
    ) -> Result<(), AuthorityValidationError>,
{
    fn verify_authority(
        &self,
        identity: &VersionedWorkIdentity<R>,
        lease: &AttemptLease,
        claim: &UntrustedAuthorityClaim,
        admission: &OutputAdmission,
    ) -> Result<(), AuthorityValidationError> {
        self(identity, lease, claim, admission)
    }
}
