//! Untrusted semantic coverage claims and authority admitted capabilities.

use backend_replication::{
    AuthorityEpoch, AuthorityExpectation, ExecutionScopeId, ExpectedIdentity, RevocationVersion,
    SemanticCoverageExpectation, WireAuthority, WireIdentity,
};
use backend_semantic::DependencyManifest;
use backend_version::{ObjectVersion, Relation, Schema};
use std::{fmt, mem::size_of, sync::Arc};

mod claim;

/// Schema marker for execution-owned semantic coverage witnesses.
#[derive(Debug)]
pub struct SemanticCoverageSchema;

impl Schema for SemanticCoverageSchema {
    const DOMAIN: u8 = 0x84;
    const TYPE: u16 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// The state asserted by an untrusted semantic coverage claim.
///
/// This value is descriptive only.  In particular, `Complete` is a producer
/// assertion and does not authorize publication.  A claim must pass a
/// [`SemanticCoverageValidator`] before it can become a
/// [`CompleteSemanticCoverage`] capability.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticCoverageState {
    /// The producer asserts that its dependency scope is complete.
    Complete,
    /// The producer observed only part of its dependency scope.
    Partial,
    /// The producer could not observe the requested scope.
    Unavailable,
    /// The producer does not support the requested scope.
    Unsupported,
}

impl SemanticCoverageState {
    /// Returns whether the producer asserted complete coverage.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// An untrusted semantic coverage claim.
///
/// This is the value that can be decoded from a worker or assembled from
/// producer bytes.  Its `state`, scope, and witness are claims only; none of
/// them can authorize a scheduler completion.  Use
/// [`CompleteSemanticCoverage::admit`] with a recipe/scope authority
/// validator to obtain the opaque capability required by publication APIs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedSemanticCoverageClaim {
    identity: ObjectVersion<SemanticCoverageSchema>,
    scope: u64,
    read_manifest: backend_execution::ReadManifestId,
    authority: backend_execution::AuthorityVersion,
    authority_epoch: AuthorityEpoch,
    revocation_version: RevocationVersion,
    witness: Box<[u8]>,
    state: SemanticCoverageState,
}

/// Compatibility spelling for an untrusted semantic coverage claim.
pub type SemanticCoverage = UntrustedSemanticCoverageClaim;

/// Compatibility spelling emphasizing that a semantic claim is not trusted.
pub type SemanticCoverageClaim = UntrustedSemanticCoverageClaim;

/// Compatibility spelling for callers that name wire-derived claims directly.
pub type UntrustedSemanticCoverage = UntrustedSemanticCoverageClaim;

/// Exact typed recipe/input/manifest/authority material supplied to a
/// semantic coverage authority validator.
///
/// The relation marker is already included in `work_key`; this erased binding
/// lets a validator serve all relation schemas without weakening the exact
/// identity comparisons performed by the claim admission path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticCoverageBinding {
    recipe: backend_execution::RecipeId,
    work_key: backend_execution::WorkKey,
    read_manifest: backend_execution::ReadManifestId,
    authority: backend_execution::AuthorityVersion,
}

impl SemanticCoverageBinding {
    /// Builds the exact binding from one typed work identity.
    #[must_use]
    pub fn from_identity<R: Relation>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
    ) -> Self {
        Self {
            recipe: identity.recipe,
            work_key: identity.work_key(),
            read_manifest: identity.read_manifest,
            authority: identity.authority,
        }
    }

    /// Returns the exact recipe identity.
    #[must_use]
    pub const fn recipe(self) -> backend_execution::RecipeId {
        self.recipe
    }

    /// Returns the exact work-key identity.
    #[must_use]
    pub const fn work_key(self) -> backend_execution::WorkKey {
        self.work_key
    }

    /// Returns the exact read-manifest identity.
    #[must_use]
    pub const fn read_manifest(self) -> backend_execution::ReadManifestId {
        self.read_manifest
    }

    /// Returns the exact authority revision identity.
    #[must_use]
    pub const fn authority(self) -> backend_execution::AuthorityVersion {
        self.authority
    }
}

/// Authority-owned validator for recipe and semantic dependency coverage.
///
/// Implementations must check the claim's asserted scope and evidence against
/// the exact recipe, work key, read manifest, authority, epoch, and current
/// revocation state in the supplied claim.  Returning `Ok(())` does not expose
/// a constructor: the admission function below mints the capability with a
/// private field only after this method returns successfully.
pub trait SemanticCoverageValidator: Send + Sync + 'static {
    /// Validates one untrusted claim against exact recipe/scope authority
    /// material.
    ///
    /// # Errors
    ///
    /// Return [`SemanticCoverageAdmissionError::Rejected`] when producer bytes,
    /// scope, authority, or freshness are not sufficient for completion.
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError>;

    /// Validates a complete dependency manifest alongside its producer claim.
    ///
    /// Manifest admission is a separate authority operation. Authorities must
    /// override this hook and check every recipe, read selector, negative
    /// witness, and authority fact against their current registry. A claim-only
    /// validator is deliberately insufficient for a reusable dependency
    /// registration.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn validate_manifest(
        &self,
        _binding: &SemanticCoverageBinding,
        _claim: &UntrustedSemanticCoverageClaim,
        _manifest: &DependencyManifest,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        Err(SemanticCoverageAdmissionError::Rejected)
    }
}

impl<F> SemanticCoverageValidator for F
where
    F: Fn(
            &SemanticCoverageBinding,
            &UntrustedSemanticCoverageClaim,
        ) -> Result<(), SemanticCoverageAdmissionError>
        + Send
        + Sync
        + 'static,
{
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        self(binding, claim)
    }
}

/// Compatibility spelling for recipe/scope authority validation.
pub use SemanticCoverageValidator as RecipeScopeAuthorityValidator;

/// Failure while admitting an untrusted semantic coverage claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticCoverageAdmissionError {
    /// The claim did not assert complete coverage.
    Incomplete,
    /// The requested scope was empty or otherwise invalid.
    InvalidScope,
    /// The claim was not bound to the exact scheduled identity.
    BindingMismatch,
    /// The recipe/scope authority rejected the producer evidence, including a
    /// stale authority epoch or revocation observation.
    Rejected,
}

impl fmt::Display for SemanticCoverageAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "semantic coverage admission error: {self:?}")
    }
}

impl std::error::Error for SemanticCoverageAdmissionError {}

/// Opaque semantic capability admitted by a recipe/scope authority validator.
///
/// The field is private and there is no public constructor.  This prevents a
/// raw digest, producer `Complete` assertion, or deserialized wire value from
/// entering [`crate::dispatch::Dispatcher::complete_local`] or
/// [`crate::dispatch::RemoteDispatchContract`].
#[must_use = "retain complete semantic coverage until publication or cancellation"]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteSemanticCoverage {
    claim: UntrustedSemanticCoverageClaim,
    scope_id: ExecutionScopeId,
    dependency_manifest: Option<Arc<DependencyManifest>>,
}

/// Explicit capability spelling for APIs that want to distinguish the
/// admitted value from an untrusted coverage claim at the call site.
pub type CompleteSemanticCoverageCapability = CompleteSemanticCoverage;

impl CompleteSemanticCoverage {
    /// Returns only heap payload bytes owned by this capability. The enclosing
    /// ticket accounts for its inline capability record separately, avoiding
    /// double charging when a manifest is shared by the contract and witness
    /// indexes.
    pub(crate) fn retained_payload_size(&self) -> Option<usize> {
        let claim = self.claim.witness.len();
        let manifest = match self.dependency_manifest.as_deref() {
            Some(manifest) => {
                size_of::<DependencyManifest>().checked_add(manifest.canonical_len())?
            }
            None => 0,
        };
        claim.checked_add(manifest)
    }

    /// Admits a claim through the supplied recipe/scope authority validator.
    ///
    /// All structural bindings are checked by the engine before the validator
    /// is called.  The capability is minted only inside this function.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn admit<R: Relation, V: SemanticCoverageValidator>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
        validator: &V,
    ) -> Result<Self, SemanticCoverageAdmissionError> {
        if !claim.state.is_complete() {
            return Err(SemanticCoverageAdmissionError::Incomplete);
        }
        let scope_id = ExecutionScopeId::try_from(claim.scope)
            .map_err(|_| SemanticCoverageAdmissionError::InvalidScope)?;
        if !claim.binds(identity) {
            return Err(SemanticCoverageAdmissionError::BindingMismatch);
        }
        let binding = SemanticCoverageBinding::from_identity(identity);
        validator.validate(&binding, &claim)?;
        Ok(Self {
            claim,
            scope_id,
            dependency_manifest: None,
        })
    }

    /// Admits a claim and a complete dependency manifest through the same
    /// recipe/scope authority. The manifest is retained by the capability so
    /// publication can register its exact dependency graph transactionally.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticCoverageAdmissionError::Rejected`] when the manifest
    /// is not reuse-ready or the authority rejects its facts.
    pub fn admit_with_manifest<R: Relation, V: SemanticCoverageValidator>(
        identity: &backend_execution::VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
        manifest: DependencyManifest,
        validator: &V,
    ) -> Result<Self, SemanticCoverageAdmissionError> {
        if !manifest.is_reuse_ready() {
            return Err(SemanticCoverageAdmissionError::Rejected);
        }
        let binding = SemanticCoverageBinding::from_identity(identity);
        if !claim.state.is_complete() {
            return Err(SemanticCoverageAdmissionError::Incomplete);
        }
        let scope_id = ExecutionScopeId::try_from(claim.scope)
            .map_err(|_| SemanticCoverageAdmissionError::InvalidScope)?;
        if !claim.binds(identity) {
            return Err(SemanticCoverageAdmissionError::BindingMismatch);
        }
        // Manifest admission is an additional authority check, never a
        // replacement for the base recipe/scope/freshness validation.  A
        // validator that implements the manifest hook must still prove the
        // producer claim itself before the capability can retain a reusable
        // dependency graph.
        validator.validate(&binding, &claim)?;
        validator.validate_manifest(&binding, &claim, &manifest)?;
        Ok(Self {
            claim,
            scope_id,
            dependency_manifest: Some(Arc::new(manifest)),
        })
    }

    /// Returns the typed semantic coverage identity.
    #[must_use]
    pub const fn identity(&self) -> ObjectVersion<SemanticCoverageSchema> {
        self.claim.identity
    }

    /// Returns the validated recipe dependency scope.
    #[must_use]
    pub const fn scope(&self) -> u64 {
        self.claim.scope
    }

    /// Returns the exact read-manifest identity.
    #[must_use]
    pub const fn read_manifest(&self) -> backend_execution::ReadManifestId {
        self.claim.read_manifest
    }

    /// Returns the exact authority revision identity.
    #[must_use]
    pub const fn authority(&self) -> backend_execution::AuthorityVersion {
        self.claim.authority
    }

    /// Returns the authority epoch accepted by the validator.
    #[must_use]
    pub const fn authority_epoch(&self) -> AuthorityEpoch {
        self.claim.authority_epoch
    }

    /// Returns the revocation observation accepted by the validator.
    #[must_use]
    pub const fn revocation_version(&self) -> RevocationVersion {
        self.claim.revocation_version
    }

    /// Returns the authority evidence bytes retained by this capability.
    #[must_use]
    pub fn witness(&self) -> &[u8] {
        self.claim.witness()
    }

    /// Returns whether this capability represents complete coverage.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        true
    }

    /// Returns the untrusted claim used to mint this capability.
    #[must_use]
    pub const fn claim(&self) -> &UntrustedSemanticCoverageClaim {
        &self.claim
    }

    /// Returns the complete dependency manifest retained by this capability,
    /// when the authority admitted one at the semantic boundary.
    #[must_use]
    pub fn dependency_manifest(&self) -> Option<&DependencyManifest> {
        self.dependency_manifest.as_deref()
    }

    /// Checks that this capability is bound to one exact scheduled identity.
    #[must_use]
    pub fn binds<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
    ) -> bool {
        self.claim.binds(identity)
    }

    /// Converts this capability into lower replication expectation material.
    #[must_use]
    pub fn expectation<R: Relation>(
        &self,
        identity: &backend_execution::VersionedWorkIdentity<R>,
    ) -> SemanticCoverageExpectation {
        SemanticCoverageExpectation {
            identity: ExpectedIdentity::from_typed(&self.claim.identity),
            scope: self.scope_id,
            read_manifest: ExpectedIdentity::from_typed(&identity.read_manifest),
            authority: AuthorityExpectation::from_typed(
                &identity.authority,
                self.claim.authority_epoch,
                self.claim.revocation_version,
            ),
        }
    }

    /// Converts the capability's own exact bindings into lower replication
    /// expectation material. This erased form is used by the pure worker
    /// endpoint after the owner has already admitted the capability; it does
    /// not reconstruct any identity from a wire digest.
    #[must_use]
    pub fn replication_expectation(&self) -> SemanticCoverageExpectation {
        SemanticCoverageExpectation {
            identity: ExpectedIdentity::from_typed(&self.claim.identity),
            scope: self.scope_id,
            read_manifest: ExpectedIdentity::from_typed(&self.claim.read_manifest),
            authority: AuthorityExpectation::from_typed(
                &self.claim.authority,
                self.claim.authority_epoch,
                self.claim.revocation_version,
            ),
        }
    }

    /// Converts this capability into an untrusted wire claim.
    #[must_use]
    pub fn wire(&self) -> backend_replication::WireSemanticCoverage {
        backend_replication::WireSemanticCoverage {
            identity: WireIdentity::from_typed(&self.claim.identity),
            scope: self.scope_id,
            read_manifest: WireIdentity::from_typed(&self.claim.read_manifest),
            authority: WireAuthority::from_typed(&self.claim.authority, self.claim.authority_epoch),
        }
    }
}

/// Admits one untrusted semantic claim through a mandatory recipe/scope
/// authority validator.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn admit_semantic_coverage<R: Relation, V: SemanticCoverageValidator>(
    identity: &backend_execution::VersionedWorkIdentity<R>,
    claim: UntrustedSemanticCoverageClaim,
    validator: &V,
) -> Result<CompleteSemanticCoverage, SemanticCoverageAdmissionError> {
    CompleteSemanticCoverage::admit(identity, claim, validator)
}
