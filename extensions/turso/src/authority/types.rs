//! Typed package-index selection identities and verification capabilities.

use super::AuthorityError;
use backend_store::{ArtifactBudget, ArtifactClosureClaim, FileStore};
use std::fmt;

/// Fixed-width logical digest used for exact inputs, candidates, roots, and packs.
pub type AuthorityHash = [u8; 32];

/// Caller intent when moving the selected head to a retained generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExistingGenerationSelection {
    /// Require the retained generation to match the latest source observation and attempt input.
    RequireCurrentObservation,
    /// Explicitly accept a retained generation built from older source or input facts.
    AcknowledgeHistorical,
}

/// Why the currently selected head points at its immutable candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionOrigin {
    /// A compiler attempt selected this candidate after exact fencing and verification.
    CompilerAttempt,
    /// A retained candidate matched the latest source observation and input digest.
    RetainedCurrentObservation,
    /// A retained candidate was selected with explicit acknowledgement of older source facts.
    RetainedHistoricalObservation,
}

impl SelectionOrigin {
    pub(super) const fn as_sql(self) -> i64 {
        match self {
            Self::CompilerAttempt => 0,
            Self::RetainedCurrentObservation => 1,
            Self::RetainedHistoricalObservation => 2,
        }
    }

    pub(super) fn from_sql(value: i64) -> Result<Self, AuthorityError> {
        match value {
            0 => Ok(Self::CompilerAttempt),
            1 => Ok(Self::RetainedCurrentObservation),
            2 => Ok(Self::RetainedHistoricalObservation),
            _ => Err(AuthorityError::CorruptRecord("selection_origin")),
        }
    }
}

/// Independent mutable selection plane within one package/source/branch/environment binding.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AuthorityPlane {
    /// Package-level metadata selection, independent of compiler profiles.
    PackageMetadata,
    /// One semantic compiler profile, including a stable language and stage key.
    SemanticProfile(Box<str>),
}

impl AuthorityPlane {
    /// Creates a profile plane such as `rust/2024/lower-ir`.
    pub fn semantic_profile(profile: impl Into<String>) -> Result<Self, AuthorityError> {
        let profile = profile.into();
        if profile.trim().is_empty()
            || profile.len() > 256
            || !profile.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(AuthorityError::InvalidNamespace);
        }
        Ok(Self::SemanticProfile(checked_name(profile)?))
    }

    /// Stable human-readable compiler profile, if this is a semantic plane.
    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        match self {
            Self::PackageMetadata => None,
            Self::SemanticProfile(profile) => Some(profile),
        }
    }

    pub(super) fn sql_parts(&self) -> (i64, &str) {
        match self {
            Self::PackageMetadata => (0, ""),
            Self::SemanticProfile(profile) => (1, profile),
        }
    }
}

/// Identity of one independent package/source/branch/environment/plane selection.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuthorityNamespace {
    pub(super) package: Box<str>,
    pub(super) source: Box<str>,
    pub(super) branch: Box<str>,
    pub(super) environment: Box<str>,
    pub(super) plane: AuthorityPlane,
}

impl AuthorityNamespace {
    /// Creates a package-metadata selection namespace.
    pub fn package_metadata(
        package: impl Into<String>,
        source: impl Into<String>,
        branch: impl Into<String>,
        environment: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_plane(
            package,
            source,
            branch,
            environment,
            AuthorityPlane::PackageMetadata,
        )
    }

    /// Creates a complete authority namespace with an explicit selection plane.
    pub fn with_plane(
        package: impl Into<String>,
        source: impl Into<String>,
        branch: impl Into<String>,
        environment: impl Into<String>,
        plane: AuthorityPlane,
    ) -> Result<Self, AuthorityError> {
        let package = checked_name(package.into())?;
        let source = checked_name(source.into())?;
        let branch = checked_name(branch.into())?;
        let environment = checked_name(environment.into())?;
        Ok(Self {
            package,
            source,
            branch,
            environment,
            plane,
        })
    }

    /// Creates one independently fenced semantic language/profile/stage selection.
    pub fn semantic_profile(
        package: impl Into<String>,
        source: impl Into<String>,
        branch: impl Into<String>,
        environment: impl Into<String>,
        profile: impl Into<String>,
    ) -> Result<Self, AuthorityError> {
        Self::with_plane(
            package,
            source,
            branch,
            environment,
            AuthorityPlane::semantic_profile(profile)?,
        )
    }

    /// Package coordinate selected by this authority scope.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Source identity selected by this authority scope.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Branch identity selected by this authority scope.
    #[must_use]
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Environment identity selected by this authority scope.
    #[must_use]
    pub fn environment(&self) -> &str {
        &self.environment
    }

    /// Explicit package metadata or semantic compiler profile plane.
    #[must_use]
    pub fn plane(&self) -> &AuthorityPlane {
        &self.plane
    }

    /// Stable 128-bit namespace identifier for authenticated cluster transfer scopes.
    ///
    /// The identifier is the leading half of a domain-separated BLAKE3 digest
    /// over the five namespace components and the typed plane tag.
    #[must_use]
    pub fn namespace_id(&self) -> [u8; 16] {
        let digest = self.namespace_digest();
        let mut identity = [0_u8; 16];
        identity.copy_from_slice(&digest[..16]);
        identity
    }

    pub(super) fn namespace_digest(&self) -> AuthorityHash {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.turso.index-namespace.v2\0");
        let (plane_kind, plane_profile) = self.plane.sql_parts();
        hasher.update(&plane_kind.to_le_bytes());
        for value in [
            self.package.as_bytes(),
            self.source.as_bytes(),
            self.branch.as_bytes(),
            self.environment.as_bytes(),
            plane_profile.as_bytes(),
        ] {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value);
        }
        *hasher.finalize().as_bytes()
    }
}

/// Durable Turso proof that an exact worker `NoResult` scope is behind a safe
/// namespace-local maintenance barrier.
///
/// This is separate from [`CandidateAttempt`]: minting cleanup authority does
/// not create compiler work or a publishable candidate. `barrier_*` identifies
/// a unique authority-owned control scope, while `retired_through_epoch` is
/// the exact prefix the worker may forget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoResultRetirementBarrier {
    pub(super) namespace: AuthorityNamespace,
    pub(super) terminal_work_id: [u8; 16],
    pub(super) terminal_epoch: u64,
    pub(super) terminal_fence: AuthorityHash,
    pub(super) barrier_work_id: [u8; 16],
    pub(super) barrier_epoch: u64,
    pub(super) barrier_fence: AuthorityHash,
    pub(super) retired_through_epoch: u64,
}

impl NoResultRetirementBarrier {
    /// Canonical typed authority namespace for the terminal and barrier scopes.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Namespace-local identifier used by the authenticated cluster transport.
    #[must_use]
    pub fn namespace_id(&self) -> [u8; 16] {
        self.namespace.namespace_id()
    }

    /// Exact work identity from the worker terminal scope.
    #[must_use]
    pub const fn terminal_work_id(&self) -> &[u8; 16] {
        &self.terminal_work_id
    }

    /// Exact candidate attempt epoch from the worker terminal scope.
    #[must_use]
    pub const fn terminal_epoch(&self) -> u64 {
        self.terminal_epoch
    }

    /// Exact candidate attempt fence from the worker terminal scope.
    #[must_use]
    pub const fn terminal_fence(&self) -> &AuthorityHash {
        &self.terminal_fence
    }

    /// Turso-minted maintenance work identity for the barrier scope.
    #[must_use]
    pub const fn barrier_work_id(&self) -> &[u8; 16] {
        &self.barrier_work_id
    }

    /// Turso-minted namespace epoch for the barrier scope.
    #[must_use]
    pub const fn barrier_epoch(&self) -> u64 {
        self.barrier_epoch
    }

    /// Turso-minted fence for the barrier scope.
    #[must_use]
    pub const fn barrier_fence(&self) -> &AuthorityHash {
        &self.barrier_fence
    }

    /// Highest exact candidate epoch the worker may retire.
    #[must_use]
    pub const fn retired_through_epoch(&self) -> u64 {
        self.retired_through_epoch
    }
}

fn checked_name(value: String) -> Result<Box<str>, AuthorityError> {
    if value.trim().is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err(AuthorityError::InvalidNamespace);
    }
    Ok(value.into_boxed_str())
}

/// What a source observation was able to establish.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceObservationValue {
    /// A successful observation with an exact count; zero is a real known value.
    KnownCount(u64),
    /// The source was reached, but no count was knowable.
    Unknown,
    /// The source could not be observed, with a retained bounded reason.
    Unavailable(Box<str>),
}

/// One mutable source observation, stored separately from immutable generation bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceObservation {
    pub(super) namespace: AuthorityNamespace,
    pub(super) revision: Option<AuthorityHash>,
    pub(super) observed_at_ms: u64,
    pub(super) value: SourceObservationValue,
}

impl SourceObservation {
    /// Creates one observation. A known count of zero stays distinct from unknown.
    pub fn new(
        namespace: AuthorityNamespace,
        revision: Option<AuthorityHash>,
        observed_at_ms: u64,
        value: SourceObservationValue,
    ) -> Result<Self, AuthorityError> {
        u64_to_i64(observed_at_ms)?;
        if let SourceObservationValue::Unavailable(reason) = &value
            && (reason.trim().is_empty() || reason.len() > 4096 || reason.contains('\0'))
        {
            return Err(AuthorityError::InvalidObservation);
        }
        if let SourceObservationValue::KnownCount(count) = &value {
            u64_to_i64(*count)?;
        }
        Ok(Self {
            namespace,
            revision,
            observed_at_ms,
            value,
        })
    }

    /// Exact namespace this mutable observation describes.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Optional source revision or immutable source snapshot identity.
    #[must_use]
    pub const fn revision(&self) -> Option<AuthorityHash> {
        self.revision
    }

    /// Observation time supplied by the source adapter in Unix milliseconds.
    #[must_use]
    pub const fn observed_at_ms(&self) -> u64 {
        self.observed_at_ms
    }

    /// Exact result, retaining the difference between known zero and unknown.
    #[must_use]
    pub fn value(&self) -> &SourceObservationValue {
        &self.value
    }
}

/// Persisted observation reference returned by Turso.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceObservationReceipt {
    pub(super) observation: SourceObservation,
    pub(super) sequence: u64,
}

impl SourceObservationReceipt {
    pub(super) fn new(observation: SourceObservation, sequence: u64) -> Self {
        Self {
            observation,
            sequence,
        }
    }

    /// Original typed observation.
    #[must_use]
    pub fn observation(&self) -> &SourceObservation {
        &self.observation
    }

    /// Monotonic per-namespace observation sequence assigned by Turso.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// One Turso-minted lease for an exact source/input attempt.
///
/// Its private epoch, random attempt ID, base frontier, and observation
/// sequence are carried into [`CandidateGeneration`]. Beginning a newer
/// attempt fences every earlier completion in the same namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateAttempt {
    pub(super) namespace: AuthorityNamespace,
    pub(super) epoch: u64,
    pub(super) attempt_id: [u8; 16],
    pub(super) fence: AuthorityHash,
    pub(super) input_digest: AuthorityHash,
    pub(super) base_generation: u64,
    pub(super) base_root: Option<AuthorityHash>,
    pub(super) observation: SourceObservationReceipt,
}

/// Untrusted persisted facts naming an attempt to reopen after an owner crash.
///
/// This is only a lookup claim. It cannot construct a [`CandidateAttempt`];
/// [`super::TursoAuthority::recover_candidate_attempt`] must compare every
/// field with the current transactional attempt, observation, and head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateAttemptRecoveryClaim {
    pub(super) namespace: AuthorityNamespace,
    pub(super) epoch: u64,
    pub(super) attempt_id: [u8; 16],
    pub(super) fence: AuthorityHash,
    pub(super) input_digest: AuthorityHash,
    pub(super) base_generation: u64,
    pub(super) base_root: Option<AuthorityHash>,
    pub(super) observation_sequence: u64,
}

impl CandidateAttemptRecoveryClaim {
    /// Names the exact durable attempt facts recorded before an Offer.
    #[must_use]
    pub fn new(
        namespace: AuthorityNamespace,
        epoch: u64,
        attempt_id: [u8; 16],
        fence: AuthorityHash,
        input_digest: AuthorityHash,
        base_generation: u64,
        base_root: Option<AuthorityHash>,
        observation_sequence: u64,
    ) -> Self {
        Self {
            namespace,
            epoch,
            attempt_id,
            fence,
            input_digest,
            base_generation,
            base_root,
            observation_sequence,
        }
    }
}

impl CandidateAttempt {
    pub(super) fn new(
        namespace: AuthorityNamespace,
        epoch: u64,
        attempt_id: [u8; 16],
        fence: AuthorityHash,
        input_digest: AuthorityHash,
        base_generation: u64,
        base_root: Option<AuthorityHash>,
        observation: SourceObservationReceipt,
    ) -> Self {
        Self {
            namespace,
            epoch,
            attempt_id,
            fence,
            input_digest,
            base_generation,
            base_root,
            observation,
        }
    }

    /// Exact namespace acquired by this attempt.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Monotonic namespace epoch held by this attempt.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Nonzero attempt ordinal for `backend_replication::AttemptId`.
    #[must_use]
    pub const fn scheduler_attempt_id(&self) -> u64 {
        self.epoch
    }

    /// Random per-attempt capability ID assigned by Turso.
    #[must_use]
    pub const fn attempt_id(&self) -> &[u8; 16] {
        &self.attempt_id
    }

    /// Opaque random database correlation nonce for this attempt.
    #[must_use]
    pub const fn attempt_nonce(&self) -> &[u8; 16] {
        &self.attempt_id
    }

    /// Exact nonzero 32-byte publication fence for scheduler/replication APIs.
    #[must_use]
    pub const fn fence_bytes(&self) -> AuthorityHash {
        self.fence
    }

    /// Exact digest of compiler/source inputs for this attempt.
    #[must_use]
    pub const fn input_digest(&self) -> &AuthorityHash {
        &self.input_digest
    }

    /// Selected generation observed when this attempt was acquired; genesis is zero.
    #[must_use]
    pub const fn base_generation(&self) -> u64 {
        self.base_generation
    }

    /// Selected root observed when acquired, or `None` at genesis.
    #[must_use]
    pub const fn base_root(&self) -> Option<AuthorityHash> {
        self.base_root
    }

    /// Source observation this attempt consumed.
    #[must_use]
    pub fn observation(&self) -> &SourceObservationReceipt {
        &self.observation
    }

    /// Captures the exact untrusted facts needed to reopen this attempt after
    /// process loss. Only the authority can turn the claim back into a token.
    #[must_use]
    pub fn recovery_claim(&self) -> CandidateAttemptRecoveryClaim {
        CandidateAttemptRecoveryClaim::new(
            self.namespace.clone(),
            self.epoch,
            self.attempt_id,
            self.fence,
            self.input_digest,
            self.base_generation,
            self.base_root,
            self.observation.sequence,
        )
    }

    /// Binds an exact compiler result to this attempt and its input fence.
    #[must_use]
    pub fn candidate(
        self,
        candidate_id: AuthorityHash,
        target_root: AuthorityHash,
        pack_id: AuthorityHash,
        closure_id: AuthorityHash,
    ) -> CandidateGeneration {
        CandidateGeneration {
            attempt: self,
            candidate_id,
            target_root,
            pack_id,
            closure_id,
            semantic_manifest_root: None,
        }
    }
}

/// Exact immutable compiler result prepared by one Turso-minted attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateGeneration {
    pub(super) attempt: CandidateAttempt,
    pub(super) candidate_id: AuthorityHash,
    pub(super) target_root: AuthorityHash,
    pub(super) pack_id: AuthorityHash,
    pub(super) closure_id: AuthorityHash,
    pub(super) semantic_manifest_root: Option<AuthorityHash>,
}

impl CandidateGeneration {
    /// Turso-minted attempt capability and its exact input/base fence.
    #[must_use]
    pub fn attempt(&self) -> &CandidateAttempt {
        &self.attempt
    }

    /// Exact candidate generation identity.
    #[must_use]
    pub const fn candidate_id(&self) -> &AuthorityHash {
        &self.candidate_id
    }

    /// Logical target root. Physical repacking does not change this identity.
    #[must_use]
    pub const fn target_root(&self) -> &AuthorityHash {
        &self.target_root
    }

    /// Exact immutable physical pack identity for this candidate.
    #[must_use]
    pub const fn pack_id(&self) -> &AuthorityHash {
        &self.pack_id
    }

    /// Exact complete immutable closure identity for this candidate.
    #[must_use]
    pub const fn closure_id(&self) -> &AuthorityHash {
        &self.closure_id
    }

    /// Exact semantic-plane manifest root embedded in this compiler closure,
    /// if the candidate publishes versioned planes.
    #[must_use]
    pub const fn semantic_manifest_root(&self) -> Option<&AuthorityHash> {
        self.semantic_manifest_root.as_ref()
    }

    /// Aggregate root of every per-image semantic plane manifest in the
    /// candidate closure. The stored field retains its legacy name for
    /// SQLite compatibility.
    #[must_use]
    pub const fn semantic_catalog_root(&self) -> Option<&AuthorityHash> {
        self.semantic_manifest_root.as_ref()
    }

    pub(super) fn with_semantic_manifest_root(mut self, root: Option<AuthorityHash>) -> Self {
        self.semantic_manifest_root = root;
        self
    }
}

/// Opaque proof that one exact persisted attempt is no longer current.
///
/// The proof binds the Turso namespace, random attempt nonce, epoch, fence,
/// compiler input digest, and newer current epoch. It proves staleness only;
/// callers bind the arriving result's peer and closure before issuing a
/// terminal stale-result acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupersededAttemptProof {
    pub(super) namespace: AuthorityNamespace,
    pub(super) attempt_id: [u8; 16],
    pub(super) epoch: u64,
    pub(super) fence: AuthorityHash,
    pub(super) input_digest: AuthorityHash,
    pub(super) current_attempt_id: [u8; 16],
    pub(super) current_epoch: u64,
}

impl SupersededAttemptProof {
    pub(super) fn new(
        namespace: AuthorityNamespace,
        attempt_id: [u8; 16],
        epoch: u64,
        fence: AuthorityHash,
        input_digest: AuthorityHash,
        current_attempt_id: [u8; 16],
        current_epoch: u64,
    ) -> Self {
        Self {
            namespace,
            attempt_id,
            epoch,
            fence,
            input_digest,
            current_attempt_id,
            current_epoch,
        }
    }

    /// Exact authority namespace of the retired attempt.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Random Turso-minted attempt nonce.
    #[must_use]
    pub const fn attempt_id(&self) -> &[u8; 16] {
        &self.attempt_id
    }

    /// Monotonic epoch of the retired attempt.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Exact publication fence carried by the retired attempt.
    #[must_use]
    pub const fn fence(&self) -> &AuthorityHash {
        &self.fence
    }

    /// Exact compiler/source input digest acquired by the retired attempt.
    #[must_use]
    pub const fn input_digest(&self) -> &AuthorityHash {
        &self.input_digest
    }

    /// Random nonce of the currently persisted attempt.
    #[must_use]
    pub const fn current_attempt_id(&self) -> &[u8; 16] {
        &self.current_attempt_id
    }

    /// Epoch of the current attempt, strictly newer than this proof's epoch.
    #[must_use]
    pub const fn current_epoch(&self) -> u64 {
        self.current_epoch
    }
}

/// Sealed evidence that the latest, unselected attempt was invalidated by a
/// newer source observation before a replacement attempt was acquired.
///
/// This is deliberately distinct from [`SupersededAttemptProof`]: its epoch
/// has *not* advanced. It authorizes rejecting the old compiler result, but
/// never authorizes treating a new attempt as already acquired or selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptInvalidatedByObservationProof {
    pub(super) namespace: AuthorityNamespace,
    pub(super) attempt_id: [u8; 16],
    pub(super) epoch: u64,
    pub(super) fence: AuthorityHash,
    pub(super) input_digest: AuthorityHash,
    pub(super) attempt_observation_sequence: u64,
    pub(super) current_observation_sequence: u64,
    pub(super) current_revision: Option<AuthorityHash>,
}

impl AttemptInvalidatedByObservationProof {
    pub(super) fn new(
        claim: &CandidateAttemptRecoveryClaim,
        current_observation_sequence: u64,
        current_revision: Option<AuthorityHash>,
    ) -> Self {
        Self {
            namespace: claim.namespace.clone(),
            attempt_id: claim.attempt_id,
            epoch: claim.epoch,
            fence: claim.fence,
            input_digest: claim.input_digest,
            attempt_observation_sequence: claim.observation_sequence,
            current_observation_sequence,
            current_revision,
        }
    }

    /// Exact namespace of the invalidated attempt.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }
    /// Random nonce of the invalidated attempt.
    #[must_use]
    pub const fn attempt_id(&self) -> &[u8; 16] {
        &self.attempt_id
    }
    /// Unchanged attempt epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Exact attempt fence.
    #[must_use]
    pub const fn fence(&self) -> &AuthorityHash {
        &self.fence
    }
    /// Exact input digest of the invalidated attempt.
    #[must_use]
    pub const fn input_digest(&self) -> &AuthorityHash {
        &self.input_digest
    }
    /// Source observation consumed when the attempt was acquired.
    #[must_use]
    pub const fn attempt_observation_sequence(&self) -> u64 {
        self.attempt_observation_sequence
    }
    /// Strictly newer persisted source observation.
    #[must_use]
    pub const fn current_observation_sequence(&self) -> u64 {
        self.current_observation_sequence
    }
    /// Revision in the newer observation, when the source supplied one.
    #[must_use]
    pub const fn current_revision(&self) -> Option<AuthorityHash> {
        self.current_revision
    }
}

/// Immutable selected-generation record retained by the SQLite authority.
///
/// The current head is a mutable pointer to one of these records. Keeping each
/// successful selection append-only preserves exact history while payloads
/// remain in content-addressed storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedGeneration {
    pub(super) namespace: AuthorityNamespace,
    pub(super) generation: u64,
    pub(super) candidate_id: AuthorityHash,
    pub(super) attempt_id: [u8; 16],
    pub(super) attempt_epoch: u64,
    pub(super) attempt_fence: AuthorityHash,
    pub(super) input_digest: AuthorityHash,
    pub(super) target_root: AuthorityHash,
    pub(super) pack_id: AuthorityHash,
    pub(super) closure_id: AuthorityHash,
    pub(super) semantic_manifest_root: Option<AuthorityHash>,
    pub(super) observation: SourceObservationReceipt,
    pub(super) selection_origin: SelectionOrigin,
}

impl SelectedGeneration {
    pub(super) fn new(
        namespace: AuthorityNamespace,
        generation: u64,
        candidate_id: AuthorityHash,
        attempt_id: [u8; 16],
        attempt_epoch: u64,
        attempt_fence: AuthorityHash,
        input_digest: AuthorityHash,
        target_root: AuthorityHash,
        pack_id: AuthorityHash,
        closure_id: AuthorityHash,
        semantic_manifest_root: Option<AuthorityHash>,
        observation: SourceObservationReceipt,
        selection_origin: SelectionOrigin,
    ) -> Self {
        Self {
            namespace,
            generation,
            candidate_id,
            attempt_id,
            attempt_epoch,
            attempt_fence,
            input_digest,
            target_root,
            pack_id,
            closure_id,
            semantic_manifest_root,
            observation,
            selection_origin,
        }
    }

    /// Selected namespace.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Monotonic generation number in this namespace.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Exact candidate identity.
    #[must_use]
    pub const fn candidate_id(&self) -> &AuthorityHash {
        &self.candidate_id
    }

    /// Exact attempt nonce and epoch that selected this generation.
    #[must_use]
    pub const fn attempt(&self) -> (&[u8; 16], u64) {
        (&self.attempt_id, self.attempt_epoch)
    }

    /// Exact scheduler attempt ordinal and publication fence.
    #[must_use]
    pub const fn scheduler_fence(&self) -> (u64, AuthorityHash) {
        (self.attempt_epoch, self.attempt_fence)
    }

    /// Exact compiler input digest.
    #[must_use]
    pub const fn input_digest(&self) -> &AuthorityHash {
        &self.input_digest
    }

    /// Logical target root, stable across physical repacking.
    #[must_use]
    pub const fn target_root(&self) -> &AuthorityHash {
        &self.target_root
    }

    /// Physical content pack selected for this generation.
    #[must_use]
    pub const fn pack_id(&self) -> &AuthorityHash {
        &self.pack_id
    }

    /// Complete immutable closure selected for this generation.
    #[must_use]
    pub const fn closure_id(&self) -> &AuthorityHash {
        &self.closure_id
    }

    /// Exact semantic-plane manifest root embedded in this selected closure,
    /// if the generation publishes versioned planes.
    #[must_use]
    pub const fn semantic_manifest_root(&self) -> Option<&AuthorityHash> {
        self.semantic_manifest_root.as_ref()
    }

    /// Aggregate root of every per-image semantic plane manifest in this
    /// selected generation. The stored field retains its legacy name for
    /// SQLite compatibility.
    #[must_use]
    pub const fn semantic_catalog_root(&self) -> Option<&AuthorityHash> {
        self.semantic_manifest_root.as_ref()
    }

    /// Exact source observation consumed by the generation.
    #[must_use]
    pub fn observation(&self) -> &SourceObservationReceipt {
        &self.observation
    }

    /// Whether this history event selected a compiler result or retained version.
    #[must_use]
    pub const fn selection_origin(&self) -> SelectionOrigin {
        self.selection_origin
    }
}

/// Complete identity a closure verifier must check before selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureClaim {
    pub(super) namespace: AuthorityNamespace,
    pub(super) attempt_id: [u8; 16],
    pub(super) attempt_epoch: u64,
    pub(super) attempt_fence: AuthorityHash,
    pub(super) input_digest: AuthorityHash,
    pub(super) candidate_id: AuthorityHash,
    pub(super) target_root: AuthorityHash,
    pub(super) pack_id: AuthorityHash,
    pub(super) closure_id: AuthorityHash,
    pub(super) semantic_manifest_root: Option<AuthorityHash>,
}

impl ClosureClaim {
    pub(super) fn for_candidate(candidate: &CandidateGeneration) -> Self {
        Self {
            namespace: candidate.attempt.namespace.clone(),
            attempt_id: candidate.attempt.attempt_id,
            attempt_epoch: candidate.attempt.epoch,
            attempt_fence: candidate.attempt.fence,
            input_digest: candidate.attempt.input_digest,
            candidate_id: candidate.candidate_id,
            target_root: candidate.target_root,
            pack_id: candidate.pack_id,
            closure_id: candidate.closure_id,
            semantic_manifest_root: candidate.semantic_manifest_root,
        }
    }

    pub(super) fn for_selected_generation(selected: &SelectedGeneration) -> Self {
        Self {
            namespace: selected.namespace.clone(),
            attempt_id: selected.attempt_id,
            attempt_epoch: selected.attempt_epoch,
            attempt_fence: selected.attempt_fence,
            input_digest: selected.input_digest,
            candidate_id: selected.candidate_id,
            target_root: selected.target_root,
            pack_id: selected.pack_id,
            closure_id: selected.closure_id,
            semantic_manifest_root: selected.semantic_manifest_root,
        }
    }

    /// Exact attempt namespace.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Attempt ID and epoch that the closure must bind.
    #[must_use]
    pub const fn attempt(&self) -> (&[u8; 16], u64) {
        (&self.attempt_id, self.attempt_epoch)
    }

    /// Exact scheduler attempt ordinal and publication fence.
    #[must_use]
    pub const fn scheduler_fence(&self) -> (u64, AuthorityHash) {
        (self.attempt_epoch, self.attempt_fence)
    }

    /// Exact compiler input digest.
    #[must_use]
    pub const fn input_digest(&self) -> &AuthorityHash {
        &self.input_digest
    }

    /// Exact compiled candidate identity.
    #[must_use]
    pub const fn candidate_id(&self) -> &AuthorityHash {
        &self.candidate_id
    }

    /// Logical target root.
    #[must_use]
    pub const fn target_root(&self) -> &AuthorityHash {
        &self.target_root
    }

    /// Physical pack identity.
    #[must_use]
    pub const fn pack_id(&self) -> &AuthorityHash {
        &self.pack_id
    }

    /// Complete closure identity.
    #[must_use]
    pub const fn closure_id(&self) -> &AuthorityHash {
        &self.closure_id
    }

    /// Exact semantic-plane manifest root embedded in the candidate closure,
    /// if one is present.
    #[must_use]
    pub const fn semantic_manifest_root(&self) -> Option<&AuthorityHash> {
        self.semantic_manifest_root.as_ref()
    }
}

/// Injected verifier for durable local or remote immutable closure bytes.
///
/// The authority mints [`ClosureReceipt`] only after this method has checked
/// the exact claim. Implementations must verify the complete closure and its
/// relationship to the target root and pack; an object-store acknowledgement
/// alone is not sufficient.
pub(super) trait DurableClosureVerifier {
    /// Error returned by the implementation's independent closure check.
    type Error: fmt::Display;

    /// Checks every required immutable member named by `claim`.
    fn verify_closure(&self, claim: &ClosureClaim) -> Result<(), Self::Error>;
}

/// Independent local CAS closure verifier backed by `FileStore`.
#[cfg(test)]
pub(super) struct FileStoreClosureVerifier<'store> {
    store: &'store FileStore,
    budget: ArtifactBudget,
}

#[cfg(test)]
impl<'store> FileStoreClosureVerifier<'store> {
    /// Uses a caller-bounded full closure reopen check.
    #[must_use]
    pub const fn new(store: &'store FileStore, budget: ArtifactBudget) -> Self {
        Self { store, budget }
    }
}

#[cfg(test)]
impl DurableClosureVerifier for FileStoreClosureVerifier<'_> {
    type Error = String;

    fn verify_closure(&self, claim: &ClosureClaim) -> Result<(), Self::Error> {
        let receipt = self
            .store
            .reopen_stored_closure(
                ArtifactClosureClaim::from_bytes(claim.closure_id),
                self.budget,
            )
            .map_err(|error| format!("{error:?}"))?;
        if receipt.closure().as_bytes() != &claim.closure_id {
            return Err("reopened closure identity mismatch".to_owned());
        }
        Ok(())
    }
}

/// One-use authority capability proving an exact candidate's immutable closure.
///
/// Its fields and constructor are private. It can be created from a store's
/// checked receipt or by [`TursoAuthority::verify_closure`](super::TursoAuthority::verify_closure)
/// with an injected verifier, then consumed by compare-and-select.
#[derive(Debug, Eq, PartialEq)]
pub struct ClosureReceipt {
    pub(super) claim: ClosureClaim,
}

/// Which derived index has reached the exact current selected root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionKind {
    /// Mutable package catalog view.
    Catalog,
    /// Package dependency graph projection.
    Graph,
    /// Lexical Tantivy projection.
    Lexical,
}

impl ProjectionKind {
    pub(super) const fn as_sql(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Graph => "graph",
            Self::Lexical => "lexical",
        }
    }

    pub(super) fn from_sql(value: &str) -> Result<Self, AuthorityError> {
        match value {
            "catalog" => Ok(Self::Catalog),
            "graph" => Ok(Self::Graph),
            "lexical" => Ok(Self::Lexical),
            _ => Err(AuthorityError::CorruptRecord("projector")),
        }
    }
}

/// Exact progress of one derived projection for the selected frontier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionWatermark {
    pub(super) projector: ProjectionKind,
    pub(super) selected_generation: u64,
    pub(super) selected_root: AuthorityHash,
    pub(super) projected_generation: Option<u64>,
    pub(super) projected_root: Option<AuthorityHash>,
}

impl ProjectionWatermark {
    pub(super) fn new(
        projector: ProjectionKind,
        selected_generation: u64,
        selected_root: AuthorityHash,
        projected_generation: Option<u64>,
        projected_root: Option<AuthorityHash>,
    ) -> Self {
        Self {
            projector,
            selected_generation,
            selected_root,
            projected_generation,
            projected_root,
        }
    }

    /// Projection lane.
    #[must_use]
    pub const fn projector(self) -> ProjectionKind {
        self.projector
    }

    /// Generation whose completion is required.
    #[must_use]
    pub const fn selected_generation(self) -> u64 {
        self.selected_generation
    }

    /// Root whose completion is required.
    #[must_use]
    pub const fn selected_root(self) -> AuthorityHash {
        self.selected_root
    }

    /// Last generation durably completed by this projection, if current.
    #[must_use]
    pub const fn projected_generation(self) -> Option<u64> {
        self.projected_generation
    }

    /// Root last durably completed by this projection, if current.
    #[must_use]
    pub const fn projected_root(self) -> Option<AuthorityHash> {
        self.projected_root
    }

    /// Whether the exact selected root has reached this projection.
    #[must_use]
    pub fn is_current(self) -> bool {
        self.projected_generation == Some(self.selected_generation)
            && matches!(self.projected_root, Some(root) if root == self.selected_root)
    }
}

/// Reopenable exact selected answer for one package/source/branch/environment/plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedFrontier {
    pub(super) namespace: AuthorityNamespace,
    pub(super) generation: u64,
    pub(super) candidate_id: AuthorityHash,
    pub(super) attempt_id: [u8; 16],
    pub(super) attempt_epoch: u64,
    pub(super) attempt_fence: AuthorityHash,
    pub(super) input_digest: AuthorityHash,
    pub(super) target_root: AuthorityHash,
    pub(super) pack_id: AuthorityHash,
    pub(super) closure_id: AuthorityHash,
    pub(super) semantic_manifest_root: Option<AuthorityHash>,
    pub(super) observation: SourceObservationReceipt,
    pub(super) projections: Box<[ProjectionWatermark]>,
    pub(super) selection_origin: SelectionOrigin,
}

impl SelectedFrontier {
    pub(super) fn new(
        namespace: AuthorityNamespace,
        generation: u64,
        candidate_id: AuthorityHash,
        attempt_id: [u8; 16],
        attempt_epoch: u64,
        attempt_fence: AuthorityHash,
        input_digest: AuthorityHash,
        target_root: AuthorityHash,
        pack_id: AuthorityHash,
        closure_id: AuthorityHash,
        semantic_manifest_root: Option<AuthorityHash>,
        observation: SourceObservationReceipt,
        projections: Box<[ProjectionWatermark]>,
        selection_origin: SelectionOrigin,
    ) -> Self {
        Self {
            namespace,
            generation,
            candidate_id,
            attempt_id,
            attempt_epoch,
            attempt_fence,
            input_digest,
            target_root,
            pack_id,
            closure_id,
            semantic_manifest_root,
            observation,
            projections,
            selection_origin,
        }
    }

    /// Selected namespace.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Monotonic selected generation in this namespace.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Selected candidate identity.
    #[must_use]
    pub const fn candidate_id(&self) -> &AuthorityHash {
        &self.candidate_id
    }

    /// Exact attempt token that selected this generation.
    #[must_use]
    pub const fn attempt(&self) -> (&[u8; 16], u64) {
        (&self.attempt_id, self.attempt_epoch)
    }

    /// Exact scheduler attempt ordinal and publication fence that selected this root.
    #[must_use]
    pub const fn scheduler_fence(&self) -> (u64, AuthorityHash) {
        (self.attempt_epoch, self.attempt_fence)
    }

    /// Exact compiler input digest consumed by the selection.
    #[must_use]
    pub const fn input_digest(&self) -> &AuthorityHash {
        &self.input_digest
    }

    /// Logical root; it remains stable across physical pack repacking.
    #[must_use]
    pub const fn target_root(&self) -> &AuthorityHash {
        &self.target_root
    }

    /// Physical pack selected for this generation.
    #[must_use]
    pub const fn pack_id(&self) -> &AuthorityHash {
        &self.pack_id
    }

    /// Complete immutable closure selected for this generation.
    #[must_use]
    pub const fn closure_id(&self) -> &AuthorityHash {
        &self.closure_id
    }

    /// Exact semantic-plane manifest root embedded in this current closure,
    /// if the selected generation publishes versioned planes.
    #[must_use]
    pub const fn semantic_manifest_root(&self) -> Option<&AuthorityHash> {
        self.semantic_manifest_root.as_ref()
    }

    /// Exact source observation this generation was built from.
    #[must_use]
    pub fn observation(&self) -> &SourceObservationReceipt {
        &self.observation
    }

    /// Why this selected generation became the current head.
    #[must_use]
    pub const fn selection_origin(&self) -> SelectionOrigin {
        self.selection_origin
    }

    /// Per-projector exact readiness fences.
    #[must_use]
    pub fn projections(&self) -> &[ProjectionWatermark] {
        &self.projections
    }
}

pub(super) fn u64_to_i64(value: u64) -> Result<i64, AuthorityError> {
    i64::try_from(value).map_err(|_| AuthorityError::IntegerOverflow)
}

pub(super) fn i64_to_u64(value: i64, field: &'static str) -> Result<u64, AuthorityError> {
    u64::try_from(value).map_err(|_| AuthorityError::CorruptRecord(field))
}
