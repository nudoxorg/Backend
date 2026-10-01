//! Package/target placement and exact attempt fencing for private compiler clusters.
//!
//! This module owns no transport, compiler, CAS, or selected-head authority.  The
//! index owner admits exact input manifests and remote evidence, creates the
//! publication attempt, and supplies its fence.  This layer chooses a route,
//! bounds live assignments, rejects late completions, and returns a typed
//! stored-closure candidate for the index owner to validate and select.

use crate::CompletionCost;
use backend_replication::{AttemptId, Fence};
use backend_store::{ArtifactClosureClaim, StoredClosureReceipt};
use backend_version::{CompilationTargetDomain, CompileRecipeDomain, ContentId, GenerationId};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, Weak};
use thiserror::Error;

/// Maximum canonical byte length admitted for one package-lineage component.
pub const MAX_PACKAGE_LINEAGE_COMPONENT_BYTES: usize = 4_096;
/// Maximum lifetime of a remote preflight binding, in owner-local milliseconds.
pub const MAX_COMPILER_PROBE_WINDOW_MS: u64 = 60_000;

/// Stable package identity including source authority and branch lineage.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageLineageId([u8; 32]);

impl PackageLineageId {
    /// Decodes a nonzero package-lineage digest from a typed input manifest.
    ///
    /// This validates the fixed-width identity only. Callers that admit untrusted bytes must
    /// still verify the manifest and its source/package binding before using the identity as
    /// compiler work.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, CompilerIdentityError> {
        if bytes == [0; 32] {
            return Err(CompilerIdentityError::ZeroPackageLineage);
        }
        Ok(Self(bytes))
    }

    /// Creates a framed package lineage from its source authority, coordinate, and branch.
    ///
    /// This keeps the same coordinate from two package sources distinct and preserves
    /// local branches without manufacturing a release version.
    ///
    /// # Errors
    ///
    /// Returns [`CompilerIdentityError`] for an empty or oversized component.
    pub fn from_canonical_parts(
        source_authority: &[u8],
        coordinate: &[u8],
        branch: &[u8],
    ) -> Result<Self, CompilerIdentityError> {
        for (component, bytes) in [
            (PackageLineageComponent::SourceAuthority, source_authority),
            (PackageLineageComponent::Coordinate, coordinate),
            (PackageLineageComponent::Branch, branch),
        ] {
            if bytes.is_empty() {
                return Err(CompilerIdentityError::EmptyPackageLineageComponent(
                    component,
                ));
            }
            if bytes.len() > MAX_PACKAGE_LINEAGE_COMPONENT_BYTES {
                return Err(CompilerIdentityError::PackageLineageComponentTooLarge(
                    component,
                ));
            }
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.compiler.package-lineage.v1\0");
        for bytes in [source_authority, coordinate, branch] {
            let length =
                u64::try_from(bytes.len()).map_err(|_| CompilerIdentityError::LengthOverflow)?;
            hasher.update(&length.to_be_bytes());
            hasher.update(bytes);
        }
        Ok(Self(*hasher.finalize().as_bytes()))
    }

    /// Returns the exact package-lineage digest.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Package-lineage input rejected while constructing a typed identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageLineageComponent {
    /// Source authority that asserted the package coordinate.
    SourceAuthority,
    /// Canonical ecosystem package coordinate.
    Coordinate,
    /// Published or local branch marker.
    Branch,
}

/// Rejection while constructing an exact compiler placement identity.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CompilerIdentityError {
    /// A decoded package-lineage digest must be nonzero.
    #[error("package lineage digest is zero")]
    ZeroPackageLineage,
    /// One package-lineage field was empty.
    #[error("package lineage {0:?} is empty")]
    EmptyPackageLineageComponent(PackageLineageComponent),
    /// One package-lineage field exceeded the protocol bound.
    #[error("package lineage {0:?} exceeds its byte bound")]
    PackageLineageComponentTooLarge(PackageLineageComponent),
    /// A canonical length could not be represented.
    #[error("compiler identity length overflow")]
    LengthOverflow,
    /// The exact input evidence belongs to a different package, target, or recipe.
    #[error("compiler input evidence does not match package, target, and recipe")]
    InputIdentityMismatch,
    /// Full-workspace capture is only a fresh compile and cannot advance a selected base.
    #[error("full-workspace compiler input must have no selected base")]
    FullWorkspaceRequiresFresh,
    /// Incremental work requires an explicit selected base generation.
    #[error("incremental compiler input requires a selected base")]
    IncrementalRequiresBase,
    /// The exact output bound must be positive.
    #[error("compiler output bound is zero")]
    ZeroOutputBound,
}

/// Package, target, and recipe shared by fresh and incremental compiler inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerInputScope {
    /// Source-aware package lineage.
    pub package: PackageLineageId,
    /// Exact compiler target.
    pub target: ContentId<CompilationTargetDomain>,
    /// Exact compiler recipe.
    pub recipe: ContentId<CompileRecipeDomain>,
}

/// Exact identity shared by either a full workspace capture or a proven read set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerInputIdentityClaim {
    /// Package, target, and recipe scope.
    pub scope: CompilerInputScope,
    /// Root of the exact source/workspace inputs.
    pub input_root: GenerationId,
    /// Canonical identity of the input manifest.
    pub manifest: crate::ReadManifestId,
}

/// Exact package/target/input scope passed only to the incremental read-set authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactCompileReadSetClaim {
    identity: CompilerInputIdentityClaim,
}

impl ExactCompileReadSetClaim {
    /// Wraps the generic identity fields as a claim that requires complete-read evidence.
    #[must_use]
    pub const fn new(identity: CompilerInputIdentityClaim) -> Self {
        Self { identity }
    }

    /// Returns the fields that the configured read-set authority must verify.
    #[must_use]
    pub const fn identity(self) -> CompilerInputIdentityClaim {
        self.identity
    }
}

/// Authority that proves a compile read manifest is complete for its exact scope.
pub trait ExactCompileReadSetVerifier {
    /// Verifies positive and negative reads, configuration, toolchain, and environment inputs.
    ///
    /// # Errors
    ///
    /// Returns a rejection when the supplied manifest is partial or not bound to the exact
    /// package, target, recipe, and input root.
    fn verify_complete_read_set(
        &self,
        claim: ExactCompileReadSetClaim,
    ) -> Result<(), ExactCompileReadSetError>;
}

/// Why an exact compile read-set verifier rejected a claim.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ExactCompileReadSetError {
    /// The producer could not prove a complete positive and negative read set.
    #[error("compiler did not prove a complete read set")]
    Incomplete,
    /// The proof was not produced by the configured compiler-input authority.
    #[error("compiler read-set proof was rejected")]
    Rejected,
}

/// Exact read set admitted by the compiler-input authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedCompileReadSet {
    claim: ExactCompileReadSetClaim,
}

impl VerifiedCompileReadSet {
    /// Admits a complete read set only after the authority verifier accepts every scope field.
    ///
    /// # Errors
    ///
    /// Returns the verifier's exact failure. No public unchecked constructor exists.
    pub fn admit<V: ExactCompileReadSetVerifier>(
        claim: ExactCompileReadSetClaim,
        verifier: &V,
    ) -> Result<Self, ExactCompileReadSetError> {
        verifier.verify_complete_read_set(claim)?;
        Ok(Self { claim })
    }

    /// Returns the exact claim admitted by the compiler-input authority.
    #[must_use]
    pub const fn claim(self) -> ExactCompileReadSetClaim {
        self.claim
    }
}

/// Exact captured workspace closure and canonical manifest bound to the compiler identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FullWorkspaceInputClaim {
    /// Package, target, recipe, root, and manifest proven by the capture authority.
    pub identity: CompilerInputIdentityClaim,
    /// Durable full-workspace input closure, encoded in checked CAS identity form at the edge.
    pub input_closure_id: [u8; 32],
    /// Exact typed manifest object included in that closure.
    pub manifest_object_id: [u8; 32],
}

/// Authority that validates a complete immutable workspace capture against its content IDs.
pub trait FullWorkspaceInputVerifier {
    /// Checks the exact capture closure, manifest, tree/root derivation, and invocation binding.
    fn verify_full_workspace_capture(
        &self,
        claim: FullWorkspaceInputClaim,
    ) -> Result<(), FullWorkspaceInputError>;
}

/// Why a full-workspace snapshot was not admitted.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FullWorkspaceInputError {
    /// Capture evidence or one of its exact object identities was rejected.
    #[error("compiler full-workspace capture was rejected")]
    Rejected,
}

/// Records that the caller's configured capture verifier accepted a full-workspace input claim.
///
/// This wrapper does not authenticate the verifier implementation. Local services must provide
/// their trusted V2 capture authority and reverify the durable closure at the publication boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifierAcceptedFullWorkspaceInput {
    claim: FullWorkspaceInputClaim,
}

impl VerifierAcceptedFullWorkspaceInput {
    /// Admits one captured workspace only after checking its exact closure and manifest proof.
    pub fn admit<V: FullWorkspaceInputVerifier>(
        claim: FullWorkspaceInputClaim,
        verifier: &V,
    ) -> Result<Self, FullWorkspaceInputError> {
        if claim.input_closure_id == [0; 32] || claim.manifest_object_id == [0; 32] {
            return Err(FullWorkspaceInputError::Rejected);
        }
        verifier.verify_full_workspace_capture(claim)?;
        Ok(Self { claim })
    }

    /// Returns the exact invocation scope and immutable captured closure binding.
    #[must_use]
    pub const fn claim(self) -> FullWorkspaceInputClaim {
        self.claim
    }

    /// Returns the exact identity fields proven by the capture.
    #[must_use]
    pub const fn identity(self) -> CompilerInputIdentityClaim {
        self.claim.identity
    }
}

/// Closed compiler input modes. A workspace snapshot is always a fresh build; incremental
/// work always carries an authority-proven read set and selected base.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifiedCompilerInput {
    /// Immutable full-workspace snapshot; no selected-base reuse is claimed.
    FullWorkspaceFresh(VerifierAcceptedFullWorkspaceInput),
    /// Exact compiler positive/negative read set for an incremental advance.
    ProvenReadSetIncremental(VerifiedCompileReadSet),
}

impl VerifiedCompilerInput {
    /// Returns the exact package/target/recipe/input root and read-manifest identity.
    #[must_use]
    pub const fn identity(self) -> CompilerInputIdentityClaim {
        match self {
            Self::FullWorkspaceFresh(input) => input.identity(),
            Self::ProvenReadSetIncremental(input) => input.claim().identity(),
        }
    }

    /// Returns full-workspace closure and manifest IDs when this is a captured snapshot.
    #[must_use]
    pub const fn full_workspace(self) -> Option<VerifierAcceptedFullWorkspaceInput> {
        match self {
            Self::FullWorkspaceFresh(input) => Some(input),
            Self::ProvenReadSetIncremental(_) => None,
        }
    }

    /// Returns complete read-set evidence only for the incremental mode.
    #[must_use]
    pub const fn proven_read_set(self) -> Option<VerifiedCompileReadSet> {
        match self {
            Self::FullWorkspaceFresh(_) => None,
            Self::ProvenReadSetIncremental(read_set) => Some(read_set),
        }
    }
}

/// Exact content-addressed work identity for one package and compiler target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerWorkIdentity {
    package: PackageLineageId,
    target: ContentId<CompilationTargetDomain>,
    recipe: ContentId<CompileRecipeDomain>,
    input: VerifiedCompilerInput,
    selected_base: Option<GenerationId>,
    max_output_bytes: u64,
}

impl CompilerWorkIdentity {
    /// Admits package work against a fresh workspace capture or proven incremental read set.
    ///
    /// # Errors
    ///
    /// Returns an identity mismatch, invalid input mode/base pair, or invalid output bound.
    pub fn new(
        package: PackageLineageId,
        target: ContentId<CompilationTargetDomain>,
        recipe: ContentId<CompileRecipeDomain>,
        input: VerifiedCompilerInput,
        selected_base: Option<GenerationId>,
        max_output_bytes: u64,
    ) -> Result<Self, CompilerIdentityError> {
        let identity = input.identity();
        if identity.scope.package != package
            || identity.scope.target != target
            || identity.scope.recipe != recipe
        {
            return Err(CompilerIdentityError::InputIdentityMismatch);
        }
        match input {
            VerifiedCompilerInput::FullWorkspaceFresh(_) if selected_base.is_some() => {
                return Err(CompilerIdentityError::FullWorkspaceRequiresFresh);
            }
            VerifiedCompilerInput::ProvenReadSetIncremental(_) if selected_base.is_none() => {
                return Err(CompilerIdentityError::IncrementalRequiresBase);
            }
            _ => {}
        }
        if max_output_bytes == 0 {
            return Err(CompilerIdentityError::ZeroOutputBound);
        }
        Ok(Self {
            package,
            target,
            recipe,
            input,
            selected_base,
            max_output_bytes,
        })
    }

    /// Returns the source-aware package lineage.
    #[must_use]
    pub const fn package(self) -> PackageLineageId {
        self.package
    }

    /// Returns the exact compilation target.
    #[must_use]
    pub const fn target(self) -> ContentId<CompilationTargetDomain> {
        self.target
    }

    /// Returns the exact compiler recipe.
    #[must_use]
    pub const fn recipe(self) -> ContentId<CompileRecipeDomain> {
        self.recipe
    }

    /// Returns the exact captured or authority-proven input evidence.
    #[must_use]
    pub const fn input(self) -> VerifiedCompilerInput {
        self.input
    }

    /// Returns the exact input identity fields used by the compiler assignment.
    #[must_use]
    pub const fn input_identity(self) -> CompilerInputIdentityClaim {
        self.input.identity()
    }

    /// Returns the selected base generation, if this is an advance.
    #[must_use]
    pub const fn selected_base(self) -> Option<GenerationId> {
        self.selected_base
    }

    /// Returns the hard upper bound for emitted closure bytes.
    #[must_use]
    pub const fn max_output_bytes(self) -> u64 {
        self.max_output_bytes
    }

    /// Derives a stable transfer work ID from all exact package/target/input/base fields.
    #[must_use]
    pub fn transfer_work_id(self) -> [u8; 16] {
        let identity = self.input.identity();
        match self.input {
            VerifiedCompilerInput::FullWorkspaceFresh(input) => {
                let claim = input.claim();
                compiler_full_workspace_transfer_work_id(
                    identity.scope.package.as_bytes(),
                    *identity.scope.target.as_ref(),
                    *identity.scope.recipe.as_ref(),
                    *identity.manifest.as_bytes(),
                    *identity.input_root.as_ref(),
                    claim.input_closure_id,
                    claim.manifest_object_id,
                    self.max_output_bytes,
                )
            }
            VerifiedCompilerInput::ProvenReadSetIncremental(_) => compiler_transfer_work_id(
                self.package.as_bytes(),
                *self.target.as_ref(),
                *self.recipe.as_ref(),
                *identity.manifest.as_bytes(),
                *identity.input_root.as_ref(),
                self.selected_base.map(|base| *base.as_ref()),
                self.max_output_bytes,
            ),
        }
    }
}

/// Derives a fresh-build work ID from an exact full-workspace closure and typed manifest object.
#[must_use]
pub fn compiler_full_workspace_transfer_work_id(
    package_lineage: [u8; 32],
    target: [u8; 32],
    recipe: [u8; 32],
    read_manifest: [u8; 32],
    input_root: [u8; 32],
    input_closure_id: [u8; 32],
    manifest_object_id: [u8; 32],
    max_output_bytes: u64,
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.compiler.full-workspace.v2\0");
    hasher.update(&package_lineage);
    hasher.update(&target);
    hasher.update(&recipe);
    hasher.update(&read_manifest);
    hasher.update(&input_root);
    hasher.update(&input_closure_id);
    hasher.update(&manifest_object_id);
    hasher.update(&max_output_bytes.to_be_bytes());
    let digest = hasher.finalize();
    let mut id = [0; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    id
}

/// Derives the stable transfer work ID from the exact serialized invocation fields.
///
/// The worker uses the same function to check that a received input manifest names the
/// `AssignmentScope::work_id` accepted on its authenticated Offer stream.
#[must_use]
pub fn compiler_transfer_work_id(
    package_lineage: [u8; 32],
    target: [u8; 32],
    recipe: [u8; 32],
    read_manifest: [u8; 32],
    input_root: [u8; 32],
    selected_base: Option<[u8; 32]>,
    max_output_bytes: u64,
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.compiler.cluster-work.v1\0");
    hasher.update(&package_lineage);
    hasher.update(&target);
    hasher.update(&recipe);
    hasher.update(&read_manifest);
    hasher.update(&input_root);
    match selected_base {
        Some(base) => {
            hasher.update(&[1]);
            hasher.update(&base);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&max_output_bytes.to_be_bytes());
    let digest = hasher.finalize();
    let mut id = [0; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    id
}

/// Exact cluster endpoint identity after Iroh authenticated the peer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompilerPeerId([u8; 32]);

impl CompilerPeerId {
    /// Admits a nonzero authenticated endpoint identity.
    ///
    /// # Errors
    ///
    /// Returns [`CompilerRemoteEvidenceError::InvalidPeer`] for the reserved zero identity.
    pub fn new(bytes: [u8; 32]) -> Result<Self, CompilerRemoteEvidenceError> {
        if bytes == [0; 32] {
            return Err(CompilerRemoteEvidenceError::InvalidPeer);
        }
        Ok(Self(bytes))
    }

    /// Returns the exact endpoint ID bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Owner-minted nonce, exact database attempt, and deadline for one remote preflight.
///
/// A binding is useful only after the verifier admits the authenticated transport response.
/// It does not grant execution authority or publication authority by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerRemoteProbeBinding {
    namespace_id: [u8; 16],
    work_id: [u8; 16],
    attempt: u64,
    fence: [u8; 32],
    nonce: [u8; 16],
    observed_at: u64,
    expires_at: u64,
}

impl CompilerRemoteProbeBinding {
    /// Creates a checked probe binding from the live database attempt and owner-local clock.
    pub fn new(
        namespace_id: [u8; 16],
        work_id: [u8; 16],
        attempt: u64,
        fence: [u8; 32],
        nonce: [u8; 16],
        observed_at: u64,
        expires_at: u64,
    ) -> Result<Self, CompilerRemotePreflightError> {
        if namespace_id == [0; 16]
            || work_id == [0; 16]
            || attempt == 0
            || fence == [0; 32]
            || nonce == [0; 16]
            || expires_at <= observed_at
            || expires_at.saturating_sub(observed_at) > MAX_COMPILER_PROBE_WINDOW_MS
        {
            return Err(CompilerRemotePreflightError::InvalidProbeBinding);
        }
        Ok(Self {
            namespace_id,
            work_id,
            attempt,
            fence,
            nonce,
            observed_at,
            expires_at,
        })
    }

    /// Opaque authority namespace ID.
    #[must_use]
    pub const fn namespace_id(self) -> [u8; 16] {
        self.namespace_id
    }

    /// Exact transfer work identity.
    #[must_use]
    pub const fn work_id(self) -> [u8; 16] {
        self.work_id
    }

    /// Exact DB attempt ordinal.
    #[must_use]
    pub const fn attempt(self) -> u64 {
        self.attempt
    }

    /// Exact DB attempt fence.
    #[must_use]
    pub const fn fence(self) -> [u8; 32] {
        self.fence
    }

    /// One-time authenticated probe nonce.
    #[must_use]
    pub const fn nonce(self) -> [u8; 16] {
        self.nonce
    }

    /// Owner-local start of the observation window.
    #[must_use]
    pub const fn observed_at(self) -> u64 {
        self.observed_at
    }

    /// Exclusive owner-local end of the observation window.
    #[must_use]
    pub const fn expires_at(self) -> u64 {
        self.expires_at
    }

    /// Whether this probe is current in the caller's injected clock domain.
    #[must_use]
    pub const fn valid_at(self, now: u64) -> bool {
        now >= self.observed_at && now < self.expires_at
    }
}

/// Untrusted exact object-level Have summary awaiting transport and CAS-proof admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteHaveClaim {
    /// Exact typed input manifest requested by the assignment.
    pub input_manifest: crate::ReadManifestId,
    /// Exact full-workspace input closure challenged by the owner.
    pub input_closure_id: [u8; 32],
    /// Exact typed V2 manifest ObjectId included in the closure.
    pub manifest_object_id: [u8; 32],
    /// Digest over every sorted object ID and payload length in the challenged closure.
    pub inventory_digest: [u8; 32],
    /// Number of exact objects named by the complete challenged closure.
    pub required_objects: u32,
    /// Objects the authenticated worker verified in its durable CAS.
    pub verified_have_objects: u32,
    /// Missing objects the owner verified in the exact closure and can serve.
    pub owner_streamable_objects: u32,
    /// Exact payload bytes for the owner-streamable object set.
    pub missing_bytes: u64,
    /// Digest over all exact page responses, nonce, peer, scope, and inventory root.
    pub proof_digest: [u8; 32],
}

/// Untrusted portable compiler capability decision for one exact target and recipe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteCompilerCapabilityClaim {
    /// Exact target scope checked from the typed V2 package/unit descriptor.
    pub target: ContentId<CompilationTargetDomain>,
    /// Exact toolchain, environment, and invocation recipe checked by the worker.
    pub recipe: ContentId<CompileRecipeDomain>,
    /// False when this worker cannot execute the exact portable invocation.
    pub supported: bool,
    /// Digest of the configured capability identity, never a host fingerprint.
    pub capability_digest: [u8; 32],
}

/// Untrusted route-cost snapshot for one verified remote candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteCompilerCostClaim {
    /// Full transfer, queue, execution, and local validation critical path.
    pub completion: CompletionCost,
    /// Owner-derived confidence in per-mille.
    pub confidence_per_mille: u16,
    /// Owner-local measurement time.
    pub observed_at: u64,
    /// Exclusive owner-local validity deadline.
    pub expires_at: u64,
}

/// All untrusted live preflight facts used to admit one compiler peer for one exact attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteCompilerPreflightClaim {
    /// Iroh endpoint that answered the query.
    pub peer: CompilerPeerId,
    /// Exact package/target/recipe/input/base claim echoed by that peer.
    pub work: CompilerWorkIdentity,
    /// Exact owner challenge and DB attempt echoed by the authenticated probe stream.
    pub binding: CompilerRemoteProbeBinding,
    /// Process incarnation from the current worker snapshot.
    pub worker_incarnation: [u8; 32],
    /// Exact target and invocation capability before any Offer is sent.
    pub capability: RemoteCompilerCapabilityClaim,
    /// Object-level Have response and owner closure-supply observation.
    pub have: RemoteHaveClaim,
    /// Checked owner-observed completion estimate.
    pub cost: RemoteCompilerCostClaim,
}

/// Verifier for the authenticated probe, CAS Have responses, and owner-measured route cost.
pub trait CompilerRemotePreflightVerifier {
    /// Verifies that every probe fact came from the exact authenticated peer and current attempt.
    ///
    /// # Errors
    ///
    /// Returns a rejection if peer, nonce, page proofs, owner supply, or cost is not trusted.
    fn verify_remote_preflight(
        &self,
        expected: CompilerWorkIdentity,
        claim: RemoteCompilerPreflightClaim,
    ) -> Result<(), CompilerRemotePreflightError>;
}

/// Why a remote peer's live preflight facts were rejected.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CompilerRemotePreflightError {
    /// Iroh endpoint identity was the reserved zero value.
    #[error("compiler peer identity is invalid")]
    InvalidPeer,
    /// Work, manifest, or target identity did not match the local assignment.
    #[error("remote compiler evidence has a different work or input scope")]
    ScopeMismatch,
    /// Have counts do not cover every required input, locally or through authorized streaming.
    #[error("remote compiler does not have or cannot receive every exact input object")]
    IncompleteHave,
    /// Worker cannot execute the exact package/unit, profile, stage, and recipe.
    #[error("remote compiler does not support the exact invocation")]
    UnsupportedCapability,
    /// Probe binding lacks a current exact database attempt or nonce.
    #[error("remote compiler probe binding is invalid")]
    InvalidProbeBinding,
    /// Proof digest was absent.
    #[error("remote compiler proof digest is empty")]
    MissingProof,
    /// Cost observation has an invalid time window, confidence, or total.
    #[error("remote compiler cost observation is invalid")]
    InvalidCost,
    /// Configured authority rejected one or more preflight facts.
    #[error("remote compiler preflight was rejected by its authority")]
    Rejected,
}

/// Compatibility name for the preflight error family; it contains no output-coverage error.
pub type CompilerRemoteEvidenceError = CompilerRemotePreflightError;
/// Compatibility name for the authenticated preflight verifier.
pub use CompilerRemotePreflightVerifier as CompilerRemoteEvidenceVerifier;
/// Compatibility name for the preflight-only claim.
pub type RemoteCompilerEvidenceClaim = RemoteCompilerPreflightClaim;

/// Verified, work- and attempt-bound remote preflight eligible for placement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedRemoteCompiler {
    work_id: [u8; 16],
    peer: CompilerPeerId,
    binding: CompilerRemoteProbeBinding,
    worker_incarnation: [u8; 32],
    capability_digest: [u8; 32],
    completion: CompletionCost,
    confidence_per_mille: u16,
    observed_at: u64,
    expires_at: u64,
    transfer_bytes: u64,
}

impl VerifiedRemoteCompiler {
    /// Admits capability, current Have, and owner-measured cost before the result exists.
    ///
    /// # Errors
    ///
    /// Returns a typed scope, probe, capability, Have, proof, cost, or authority rejection.
    pub fn admit_preflight<V: CompilerRemotePreflightVerifier>(
        expected: CompilerWorkIdentity,
        claim: RemoteCompilerPreflightClaim,
        verifier: &V,
    ) -> Result<Self, CompilerRemotePreflightError> {
        let Some(full_workspace) = expected.input().full_workspace() else {
            return Err(CompilerRemotePreflightError::ScopeMismatch);
        };
        let input_claim = full_workspace.claim();
        if claim.work != expected
            || claim.binding.work_id != expected.transfer_work_id()
            || claim.binding.nonce == [0; 16]
            || claim.worker_incarnation == [0; 32]
            || claim.have.input_manifest != expected.input_identity().manifest
            || claim.have.input_closure_id != input_claim.input_closure_id
            || claim.have.manifest_object_id != input_claim.manifest_object_id
            || claim.have.inventory_digest == [0; 32]
            || claim.capability.target != expected.target
            || claim.capability.recipe != expected.recipe
        {
            return Err(CompilerRemotePreflightError::ScopeMismatch);
        }
        if !claim.capability.supported || claim.capability.capability_digest == [0; 32] {
            return Err(CompilerRemotePreflightError::UnsupportedCapability);
        }
        let supplied_objects = claim
            .have
            .verified_have_objects
            .checked_add(claim.have.owner_streamable_objects);
        if claim.have.required_objects == 0
            || supplied_objects != Some(claim.have.required_objects)
            || (claim.have.owner_streamable_objects == 0 && claim.have.missing_bytes != 0)
            || (claim.have.owner_streamable_objects != 0 && claim.have.missing_bytes == 0)
        {
            return Err(CompilerRemotePreflightError::IncompleteHave);
        }
        if claim.have.proof_digest == [0; 32] {
            return Err(CompilerRemotePreflightError::MissingProof);
        }
        if claim.binding.expires_at <= claim.binding.observed_at
            || claim.cost.observed_at != claim.binding.observed_at
            || claim.cost.expires_at != claim.binding.expires_at
            || claim.cost.expires_at <= claim.cost.observed_at
            || claim.cost.confidence_per_mille > 1_000
            || claim.cost.confidence_per_mille < 500
            || claim.cost.completion.checked_total().is_none()
        {
            return Err(CompilerRemotePreflightError::InvalidCost);
        }
        verifier.verify_remote_preflight(expected, claim)?;
        Ok(Self {
            work_id: expected.transfer_work_id(),
            peer: claim.peer,
            binding: claim.binding,
            worker_incarnation: claim.worker_incarnation,
            capability_digest: claim.capability.capability_digest,
            completion: claim.cost.completion,
            confidence_per_mille: claim.cost.confidence_per_mille,
            observed_at: claim.cost.observed_at,
            expires_at: claim.cost.expires_at,
            transfer_bytes: claim.have.missing_bytes,
        })
    }

    /// Compatibility wrapper for callers that already use the former method name.
    pub fn admit<V: CompilerRemotePreflightVerifier>(
        expected: CompilerWorkIdentity,
        claim: RemoteCompilerPreflightClaim,
        verifier: &V,
    ) -> Result<Self, CompilerRemotePreflightError> {
        Self::admit_preflight(expected, claim, verifier)
    }

    /// Returns the exact authenticated probe binding.
    #[must_use]
    pub const fn binding(self) -> CompilerRemoteProbeBinding {
        self.binding
    }

    /// Returns the worker process incarnation bound to the live probe.
    #[must_use]
    pub const fn worker_incarnation(self) -> [u8; 32] {
        self.worker_incarnation
    }

    /// Returns the configured portable capability digest.
    #[must_use]
    pub const fn capability_digest(self) -> [u8; 32] {
        self.capability_digest
    }

    /// Returns the peer proven to serve this exact work.
    #[must_use]
    pub const fn peer(self) -> CompilerPeerId {
        self.peer
    }

    /// Returns the owner-observed remote end-to-end estimate.
    #[must_use]
    pub const fn completion_cost(self) -> CompletionCost {
        self.completion
    }

    /// Returns missing input bytes that the owner must stream before execution.
    #[must_use]
    pub const fn input_transfer_bytes(self) -> u64 {
        self.transfer_bytes
    }
}

/// CPU credit measured in millicores. A value of `1_000` reserves one logical core.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct CompilerCpuCredits(u32);

impl CompilerCpuCredits {
    /// Creates a CPU credit quantity.
    #[must_use]
    pub const fn new(millicores: u32) -> Self {
        Self(millicores)
    }

    /// Returns the reserved millicore count.
    #[must_use]
    pub const fn millicores(self) -> u32 {
        self.0
    }
}

/// Memory credit measured in bytes.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct CompilerMemoryCredits(u64);

impl CompilerMemoryCredits {
    /// Creates a memory credit quantity.
    #[must_use]
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Returns the reserved memory bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

/// Network and artifact byte credit measured in bytes.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct CompilerByteCredits(u64);

impl CompilerByteCredits {
    /// Creates a byte credit quantity.
    #[must_use]
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Returns the reserved transfer bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

/// Typed CPU, memory, and network credits held for one live compile assignment.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompilerResourceCredits {
    /// CPU millicores.
    pub cpu: CompilerCpuCredits,
    /// Memory bytes.
    pub memory: CompilerMemoryCredits,
    /// Input plus bounded result bytes.
    pub transfer: CompilerByteCredits,
}

impl CompilerResourceCredits {
    /// Returns whether all three resource dimensions are nonzero.
    #[must_use]
    pub const fn is_positive(self) -> bool {
        self.cpu.0 > 0 && self.memory.0 > 0 && self.transfer.0 > 0
    }

    /// Returns whether this demand fits within all available dimensions.
    #[must_use]
    pub const fn fits_within(self, available: Self) -> bool {
        self.cpu.0 <= available.cpu.0
            && self.memory.0 <= available.memory.0
            && self.transfer.0 <= available.transfer.0
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            cpu: CompilerCpuCredits(self.cpu.0.checked_add(other.cpu.0)?),
            memory: CompilerMemoryCredits(self.memory.0.checked_add(other.memory.0)?),
            transfer: CompilerByteCredits(self.transfer.0.checked_add(other.transfer.0)?),
        })
    }

    fn saturating_sub(self, other: Self) -> Self {
        Self {
            cpu: CompilerCpuCredits(self.cpu.0.saturating_sub(other.cpu.0)),
            memory: CompilerMemoryCredits(self.memory.0.saturating_sub(other.memory.0)),
            transfer: CompilerByteCredits(self.transfer.0.saturating_sub(other.transfer.0)),
        }
    }
}

/// Warm compiler session key used to prefer a worker with matching loaded state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompilerSessionAffinity([u8; 32]);

impl CompilerSessionAffinity {
    /// Admits a nonzero session key.
    ///
    /// # Errors
    ///
    /// Returns `CompilerNodeCapacityError::InvalidSessionAffinity` for the reserved zero key.
    pub fn new(bytes: [u8; 32]) -> Result<Self, CompilerNodeCapacityError> {
        if bytes == [0; 32] {
            return Err(CompilerNodeCapacityError::InvalidSessionAffinity);
        }
        Ok(Self(bytes))
    }

    /// Returns the exact session affinity digest.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Untrusted worker capacity and warm-session observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerNodeCapacityClaim {
    /// Authenticated Iroh endpoint.
    pub peer: CompilerPeerId,
    /// Same one-time challenge and exact database attempt as the Have/capability response.
    pub binding: CompilerRemoteProbeBinding,
    /// Process incarnation; a change fences every assignment to the old worker process.
    pub worker_incarnation: [u8; 32],
    /// Opaque revision of this exact current credit snapshot.
    pub capacity_revision: [u8; 16],
    /// Configured hard resource limit.
    pub total: CompilerResourceCredits,
    /// Resources currently occupied outside this scheduler's assignments.
    pub busy: CompilerResourceCredits,
    /// Session keys already warm in this worker process.
    pub warm_sessions: Vec<CompilerSessionAffinity>,
    /// Owner-local observation time.
    pub observed_at: u64,
    /// Exclusive owner-local validity deadline.
    pub expires_at: u64,
}

/// Verifier for authenticated worker capacity, incarnation, and session claims.
pub trait CompilerNodeCapacityVerifier {
    /// Verifies the source and exact endpoint binding of a node-capacity claim.
    fn verify_node_capacity(
        &self,
        claim: &CompilerNodeCapacityClaim,
    ) -> Result<(), CompilerNodeCapacityError>;
}

/// Why a worker capacity claim was rejected.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CompilerNodeCapacityError {
    /// The worker process did not provide a nonzero incarnation.
    #[error("compiler worker incarnation is invalid")]
    InvalidIncarnation,
    /// The capacity record is not tied to a valid one-time probe binding.
    #[error("compiler worker capacity probe binding is invalid")]
    InvalidProbeBinding,
    /// The capacity revision is the reserved zero identity.
    #[error("compiler worker capacity revision is invalid")]
    InvalidCapacityRevision,
    /// Work, peer, attempt, or incarnation differs between preflight planes.
    #[error("compiler worker observations do not share one exact probe")]
    ProbeMismatch,
    /// Total capacity must include CPU and memory, while transfer may be zero.
    #[error("compiler worker capacity is invalid")]
    InvalidCapacity,
    /// Reported out-of-scheduler use exceeded a hard resource limit.
    #[error("compiler worker reports busy credits above capacity")]
    BusyAboveCapacity,
    /// Observation validity window is empty or reversed.
    #[error("compiler worker capacity observation has an invalid window")]
    InvalidWindow,
    /// Session-affinity key is the reserved zero value.
    #[error("compiler session affinity is invalid")]
    InvalidSessionAffinity,
    /// Session entries were duplicated or exceeded the protocol bound.
    #[error("compiler worker session inventory is not canonical")]
    InvalidSessionInventory,
    /// Configured capacity authority rejected the claim.
    #[error("compiler worker capacity claim was rejected")]
    Rejected,
}

/// Work-bound worker capacity admitted by the configured node authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedCompilerNodeCapacity {
    peer: CompilerPeerId,
    binding: CompilerRemoteProbeBinding,
    worker_incarnation: [u8; 32],
    capacity_revision: [u8; 16],
    total: CompilerResourceCredits,
    busy: CompilerResourceCredits,
    warm_sessions: Vec<CompilerSessionAffinity>,
    observed_at: u64,
    expires_at: u64,
}

impl VerifiedCompilerNodeCapacity {
    /// Admits the worker's resources and session inventory after checking provenance.
    pub fn admit<V: CompilerNodeCapacityVerifier>(
        claim: CompilerNodeCapacityClaim,
        verifier: &V,
    ) -> Result<Self, CompilerNodeCapacityError> {
        if claim.worker_incarnation == [0; 32] {
            return Err(CompilerNodeCapacityError::InvalidIncarnation);
        }
        if claim.binding.work_id == [0; 16]
            || claim.binding.attempt == 0
            || claim.binding.fence == [0; 32]
            || claim.binding.nonce == [0; 16]
            || claim.binding.expires_at <= claim.binding.observed_at
            || claim
                .binding
                .expires_at
                .saturating_sub(claim.binding.observed_at)
                > MAX_COMPILER_PROBE_WINDOW_MS
            || claim.observed_at != claim.binding.observed_at
            || claim.expires_at != claim.binding.expires_at
        {
            return Err(CompilerNodeCapacityError::InvalidProbeBinding);
        }
        if claim.capacity_revision == [0; 16] {
            return Err(CompilerNodeCapacityError::InvalidCapacityRevision);
        }
        if claim.total.cpu.0 == 0 || claim.total.memory.0 == 0 {
            return Err(CompilerNodeCapacityError::InvalidCapacity);
        }
        if !claim.busy.fits_within(claim.total) {
            return Err(CompilerNodeCapacityError::BusyAboveCapacity);
        }
        if claim.expires_at <= claim.observed_at {
            return Err(CompilerNodeCapacityError::InvalidWindow);
        }
        if claim.warm_sessions.len() > 512 {
            return Err(CompilerNodeCapacityError::InvalidSessionInventory);
        }
        let mut sessions = claim.warm_sessions.clone();
        sessions.sort_unstable();
        sessions.dedup();
        if sessions.len() != claim.warm_sessions.len() {
            return Err(CompilerNodeCapacityError::InvalidSessionInventory);
        }
        verifier.verify_node_capacity(&claim)?;
        Ok(Self {
            peer: claim.peer,
            binding: claim.binding,
            worker_incarnation: claim.worker_incarnation,
            capacity_revision: claim.capacity_revision,
            total: claim.total,
            busy: claim.busy,
            warm_sessions: sessions,
            observed_at: claim.observed_at,
            expires_at: claim.expires_at,
        })
    }

    /// Returns the authenticated endpoint identity.
    #[must_use]
    pub const fn peer(&self) -> CompilerPeerId {
        self.peer
    }

    /// Returns the exact assignment challenge that produced this live snapshot.
    #[must_use]
    pub const fn binding(&self) -> CompilerRemoteProbeBinding {
        self.binding
    }

    /// Returns the worker process incarnation.
    #[must_use]
    pub const fn worker_incarnation(&self) -> [u8; 32] {
        self.worker_incarnation
    }

    /// Opaque revision for this exact current capacity snapshot.
    #[must_use]
    pub const fn capacity_revision(&self) -> [u8; 16] {
        self.capacity_revision
    }

    /// Returns whether this snapshot remains current at the injected owner clock.
    #[must_use]
    pub fn valid_at(&self, now: u64) -> bool {
        now >= self.observed_at && now < self.expires_at
    }

    fn available(&self) -> CompilerResourceCredits {
        self.total.saturating_sub(self.busy)
    }

    fn has_warm_session(&self, affinity: Option<CompilerSessionAffinity>) -> bool {
        affinity.is_some_and(|key| self.warm_sessions.binary_search(&key).is_ok())
    }
}

/// Verified remote work and the node resource observation used together for placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerBalancedRemote {
    work: VerifiedRemoteCompiler,
    /// Authenticated current worker capacity and session state.
    node: VerifiedCompilerNodeCapacity,
}

impl CompilerBalancedRemote {
    /// Pairs work preflight and capacity only when one exact worker probe produced both.
    pub fn new(
        work: VerifiedRemoteCompiler,
        node: VerifiedCompilerNodeCapacity,
    ) -> Result<Self, CompilerNodeCapacityError> {
        if work.peer != node.peer
            || work.binding != node.binding
            || work.worker_incarnation != node.worker_incarnation
        {
            return Err(CompilerNodeCapacityError::ProbeMismatch);
        }
        Ok(Self { work, node })
    }

    /// Verified exact-work preflight with no output-coverage claim.
    #[must_use]
    pub const fn work(&self) -> VerifiedRemoteCompiler {
        self.work
    }

    /// Authenticated current resource capacity from the same probe.
    #[must_use]
    pub const fn node(&self) -> &VerifiedCompilerNodeCapacity {
        &self.node
    }
}

/// Full placement input for one local-first compiler job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerBalancingRequest {
    /// Exact content-addressed compiler work.
    pub work: CompilerWorkIdentity,
    /// Interactive or background demand class.
    pub demand: CompilerDemand,
    /// Current local node availability.
    pub local: LocalCompilerAvailability,
    /// Local end-to-end estimate, including queue and contention.
    pub local_cost: CompletionCost,
    /// Estimated local start delay used by the local-first budget.
    pub local_start_delay: u64,
    /// Owner-local enqueue time.
    pub submitted_at: u64,
    /// How long an interactive request must start locally before remote optimization is allowed.
    pub local_first_budget: u64,
    /// Typed CPU/memory/network credits requested by this job.
    pub resources: CompilerResourceCredits,
    /// Optional warm-session affinity derived from exact toolchain and invocation identity.
    pub session_affinity: Option<CompilerSessionAffinity>,
    /// Optional hard end-to-end deadline in the owner-local clock domain.
    pub deadline_at: Option<u64>,
}

/// Versioned peer-failure/backoff record suitable for durable owner storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerPeerRetryRecord {
    /// Authenticated peer endpoint.
    pub peer: CompilerPeerId,
    /// Backoff belongs only to this exact worker process incarnation.
    pub worker_incarnation: [u8; 32],
    /// Consecutive failures recorded for this incarnation.
    pub failures: u32,
    /// Last failure time in the owner-local monotonic clock.
    pub last_failure_at: u64,
    /// Exclusive time at which another assignment may be attempted.
    pub retry_at: u64,
}

/// Owner-persisted retry snapshot. Live assignments are intentionally omitted and must
/// reacquire fresh authority attempts after process restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerRetrySnapshot {
    /// Current format version.
    pub version: u16,
    /// Canonically peer-sorted backoff records.
    pub peers: Vec<CompilerPeerRetryRecord>,
}

impl Default for CompilerRetrySnapshot {
    fn default() -> Self {
        Self {
            version: 1,
            peers: Vec::new(),
        }
    }
}

/// Exponential retry policy with deterministic peer-specific jitter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerBackoffPolicy {
    base_delay: u64,
    max_delay: u64,
}

impl CompilerBackoffPolicy {
    /// Creates a checked positive backoff policy.
    pub fn new(base_delay: u64, max_delay: u64) -> Result<Self, CompilerAssignmentError> {
        if base_delay == 0 || max_delay < base_delay {
            return Err(CompilerAssignmentError::InvalidBackoffPolicy);
        }
        Ok(Self {
            base_delay,
            max_delay,
        })
    }

    fn retry_at(
        self,
        peer: CompilerPeerId,
        incarnation: [u8; 32],
        failures: u32,
        now: u64,
    ) -> Option<u64> {
        let shift = failures.saturating_sub(1).min(63);
        let exponential = self.base_delay.checked_shl(shift).unwrap_or(u64::MAX);
        let capped = exponential.min(self.max_delay);
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.compiler.peer-backoff.v1\0");
        hasher.update(&peer.as_bytes());
        hasher.update(&incarnation);
        hasher.update(&failures.to_be_bytes());
        let digest = hasher.finalize();
        let mut jitter_bytes = [0; 8];
        jitter_bytes.copy_from_slice(&digest.as_bytes()[..8]);
        let jitter = u64::from_be_bytes(jitter_bytes) % (capped / 8 + 1);
        now.checked_add(capped.saturating_sub(capped / 8).saturating_add(jitter))
    }
}

/// Exact workers revoked after a process incarnation changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerPeerRestart {
    /// Previous process incarnation, if one was observed.
    pub previous: Option<[u8; 32]>,
    /// Newly authenticated process incarnation.
    pub current: [u8; 32],
    /// Assignments invalidated before the new process can receive work.
    pub invalidated: Vec<CompilerAssignment>,
}

/// The two executions admitted for one explicitly budgeted pure-work hedge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerHedgeAssignments {
    /// Local attempt; its authority token remains exact and independently fenced.
    pub local: CompilerAssignment,
    /// Remote attempt; its authority token remains exact and independently fenced.
    pub remote: CompilerAssignment,
}

/// One background task temporarily removed from its owner queue for bounded work stealing.
#[derive(Debug)]
pub struct CompilerStealLease {
    id: u64,
    request: CompilerBalancingRequest,
    peer: CompilerPeerId,
    peer_retry_at: u64,
    owner: Weak<Mutex<CompilerWorkQueueState>>,
}

impl CompilerStealLease {
    /// Returns the work and resource request protected by this queue lease.
    #[must_use]
    pub const fn request(&self) -> CompilerBalancingRequest {
        self.request
    }
}

impl Drop for CompilerStealLease {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let mut state = owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(request) = state.claimed.remove(&self.id) {
            let work_id = request.work.transfer_work_id();
            state.pending.insert(work_id, request);
            state
                .peer_work_retry_at
                .insert((self.peer, work_id), self.peer_retry_at);
        }
    }
}

/// Bounded local pending-work queue with age-based remote stealing.
///
/// The queue lock protects only enqueue/claim bookkeeping. Compilation and transport run after
/// releasing it. Steals are FIFO by owner-local enqueue time and work ID, so an older eligible
/// background task cannot be bypassed indefinitely by newer arrivals.
#[derive(Clone)]
pub struct CompilerWorkQueue {
    capacity: NonZeroUsize,
    state: Arc<Mutex<CompilerWorkQueueState>>,
}

#[derive(Debug, Default)]
struct CompilerWorkQueueState {
    pending: BTreeMap<[u8; 16], CompilerBalancingRequest>,
    claimed: BTreeMap<u64, CompilerBalancingRequest>,
    next_lease: u64,
    peer_windows: BTreeMap<CompilerPeerId, (u64, usize)>,
    peer_work_retry_at: BTreeMap<(CompilerPeerId, [u8; 16]), u64>,
}

/// Errors from bounded queue and work-stealing operations.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CompilerWorkQueueError {
    /// Pending plus claimed work reached the configured hard bound.
    #[error("compiler work queue is full")]
    AtCapacity,
    /// This exact work is already pending or held by a steal lease.
    #[error("compiler work is already queued")]
    DuplicateWork,
    /// The steal lease is stale, committed, or belongs to another queue.
    #[error("compiler work-steal lease is stale")]
    StaleLease,
    /// The owner-local clock regressed relative to a queue or peer window.
    #[error("compiler work queue clock regressed")]
    TimeRegression,
    /// Steal window length must be positive.
    #[error("compiler work-steal window is invalid")]
    InvalidWindow,
    /// Claim identity sequence overflowed.
    #[error("compiler work-steal lease sequence overflow")]
    LeaseOverflow,
}

impl CompilerWorkQueue {
    /// Creates a bounded pending queue.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            state: Arc::new(Mutex::new(CompilerWorkQueueState::default())),
        }
    }

    /// Adds an exact compiler request while preserving the caller's original enqueue time.
    pub fn enqueue(&self, request: CompilerBalancingRequest) -> Result<(), CompilerWorkQueueError> {
        let work_id = request.work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.pending.contains_key(&work_id)
            || state
                .claimed
                .values()
                .any(|request| request.work.transfer_work_id() == work_id)
        {
            return Err(CompilerWorkQueueError::DuplicateWork);
        }
        if state.pending.len().saturating_add(state.claimed.len()) >= self.capacity.get() {
            return Err(CompilerWorkQueueError::AtCapacity);
        }
        state.pending.insert(work_id, request);
        Ok(())
    }

    /// Claims the oldest sufficiently aged background request for an eligible peer.
    ///
    /// Steals are limited per peer and time window. The caller must commit the returned lease
    /// only after `place_balanced_and_assign` succeeds; on failure it requeues the same request,
    /// retaining its original age so a failed dispatch does not lose fairness.
    pub fn claim_oldest_stealable(
        &self,
        peer: CompilerPeerId,
        remotes: &[CompilerBalancedRemote],
        now: u64,
        minimum_age: u64,
        window: u64,
        per_peer_limit: NonZeroUsize,
    ) -> Result<Option<CompilerStealLease>, CompilerWorkQueueError> {
        if window == 0 {
            return Err(CompilerWorkQueueError::InvalidWindow);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((window_started, count)) = state.peer_windows.get(&peer).copied() else {
            state.peer_windows.insert(peer, (now, 0));
            return self.claim_oldest_locked(
                &mut state,
                peer,
                remotes,
                now,
                minimum_age,
                window,
                per_peer_limit,
            );
        };
        if now < window_started {
            return Err(CompilerWorkQueueError::TimeRegression);
        }
        if now.saturating_sub(window_started) >= window {
            state.peer_windows.insert(peer, (now, 0));
        } else if count >= per_peer_limit.get() {
            return Ok(None);
        }
        self.claim_oldest_locked(
            &mut state,
            peer,
            remotes,
            now,
            minimum_age,
            window,
            per_peer_limit,
        )
    }

    fn claim_oldest_locked(
        &self,
        state: &mut CompilerWorkQueueState,
        peer: CompilerPeerId,
        remotes: &[CompilerBalancedRemote],
        now: u64,
        minimum_age: u64,
        window: u64,
        per_peer_limit: NonZeroUsize,
    ) -> Result<Option<CompilerStealLease>, CompilerWorkQueueError> {
        let eligible = state
            .pending
            .iter()
            .filter_map(|(work_id, request)| {
                if request.demand != CompilerDemand::Background
                    || now < request.submitted_at
                    || now.saturating_sub(request.submitted_at) < minimum_age
                    || state
                        .peer_work_retry_at
                        .get(&(peer, *work_id))
                        .is_some_and(|retry_at| now < *retry_at)
                {
                    return None;
                }
                let has_peer = remotes.iter().any(|candidate| {
                    candidate.node.peer == peer
                        && predicted_remote_cost(*request, candidate, now).is_some()
                });
                has_peer.then_some((request.submitted_at, *work_id))
            })
            .min();
        let Some((_, work_id)) = eligible else {
            return Ok(None);
        };
        let id = state.next_lease;
        let next_lease = state
            .next_lease
            .checked_add(1)
            .ok_or(CompilerWorkQueueError::LeaseOverflow)?;
        let peer_retry_at = window
            .checked_mul(2)
            .and_then(|delay| now.checked_add(delay))
            .ok_or(CompilerWorkQueueError::LeaseOverflow)?;
        let request = state
            .pending
            .remove(&work_id)
            .ok_or(CompilerWorkQueueError::StaleLease)?;
        state.next_lease = next_lease;
        let lease = CompilerStealLease {
            id,
            request,
            peer,
            peer_retry_at,
            owner: Arc::downgrade(&self.state),
        };
        state.claimed.insert(id, request);
        let (started, count) = state.peer_windows.get(&peer).copied().unwrap_or((now, 0));
        state.peer_windows.insert(
            peer,
            (started, count.saturating_add(1).min(per_peer_limit.get())),
        );
        Ok(Some(lease))
    }

    /// Commits a steal after the scheduler has admitted its fresh fenced assignment.
    pub fn commit_steal(&self, lease: CompilerStealLease) -> Result<(), CompilerWorkQueueError> {
        if !lease
            .owner
            .upgrade()
            .is_some_and(|owner| Arc::ptr_eq(&owner, &self.state))
        {
            return Err(CompilerWorkQueueError::StaleLease);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.claimed.get(&lease.id) != Some(&lease.request) {
            return Err(CompilerWorkQueueError::StaleLease);
        }
        state.claimed.remove(&lease.id);
        Ok(())
    }

    /// Returns an uncommitted steal to the queue without resetting its fairness age.
    pub fn requeue_steal(&self, lease: CompilerStealLease) -> Result<(), CompilerWorkQueueError> {
        if !lease
            .owner
            .upgrade()
            .is_some_and(|owner| Arc::ptr_eq(&owner, &self.state))
        {
            return Err(CompilerWorkQueueError::StaleLease);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.claimed.get(&lease.id) != Some(&lease.request) {
            return Err(CompilerWorkQueueError::StaleLease);
        }
        state.claimed.remove(&lease.id);
        state
            .pending
            .insert(lease.request.work.transfer_work_id(), lease.request);
        state.peer_work_retry_at.insert(
            (lease.peer, lease.request.work.transfer_work_id()),
            lease.peer_retry_at,
        );
        Ok(())
    }

    /// Returns the current pending plus in-flight queue length.
    #[must_use]
    pub fn len(&self) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending.len().saturating_add(state.claimed.len())
    }

    /// Returns whether the queue has no pending or claimed work.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Interactive latency-critical compile versus background freshness work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerDemand {
    /// User-visible work; a ready local compiler starts before remote optimization.
    Interactive,
    /// Background work may move to a cheaper verified peer.
    Background,
}

/// Availability of the local embedded/standalone compiler node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCompilerAvailability {
    /// Local inputs and node capacity are ready.
    Ready,
    /// Local inputs are ready, but the node is currently contended.
    Contended,
    /// Local node or required local closure is unavailable.
    Unavailable,
}

/// Deterministic route choice for one exact owner-fenced attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerPlacement {
    /// Run the embedded/local compiler immediately.
    Local,
    /// Assign the exact package/target closure to this verified peer.
    Remote(CompilerPeerId),
    /// Neither local execution nor an eligible verified peer is available.
    OfflineUnavailable,
}

/// Stateless package compiler placement policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompilerPlacementPolicy;

impl CompilerPlacementPolicy {
    /// Selects local or remote execution from exact verified work, current Have, capability, and
    /// cost facts. Output coverage is admitted only after the worker returns its result closure.
    ///
    /// Interactive work always takes a ready local route. A remote route is considered only
    /// when its complete verified input evidence is current and its full critical path beats
    /// local work, or no local route exists.
    #[must_use]
    pub fn choose(
        &self,
        work: CompilerWorkIdentity,
        demand: CompilerDemand,
        local: LocalCompilerAvailability,
        local_cost: u64,
        now: u64,
        remotes: &[VerifiedRemoteCompiler],
    ) -> CompilerPlacement {
        let remote = remotes
            .iter()
            .copied()
            .filter(|candidate| {
                candidate.work_id == work.transfer_work_id()
                    && candidate.confidence_per_mille >= 500
                    && now >= candidate.observed_at
                    && now < candidate.expires_at
            })
            .min_by_key(|candidate| (candidate.completion.total(), candidate.peer));
        match (demand, local) {
            (_, LocalCompilerAvailability::Unavailable) => remote
                .map_or(CompilerPlacement::OfflineUnavailable, |candidate| {
                    CompilerPlacement::Remote(candidate.peer)
                }),
            (CompilerDemand::Interactive, LocalCompilerAvailability::Ready)
            | (CompilerDemand::Interactive, LocalCompilerAvailability::Contended) => {
                CompilerPlacement::Local
            }
            (CompilerDemand::Background, LocalCompilerAvailability::Ready)
            | (CompilerDemand::Background, LocalCompilerAvailability::Contended) => remote
                .filter(|candidate| candidate.completion.total() < local_cost)
                .map_or(CompilerPlacement::Local, |candidate| {
                    CompilerPlacement::Remote(candidate.peer)
                }),
        }
    }

    /// Selects an eligible worker from verified work, resource, session, and cost evidence.
    ///
    /// Interactive work starts locally during its explicit local-first window when the local
    /// node can admit the typed credits. After that window, the same work may move remotely if
    /// its predicted end-to-end cost is lower and it can still meet its deadline. A worker's
    /// cold session retains the verified warmup cost; matching affinity removes it. Peer and
    /// work identifiers provide deterministic tie breaking.
    #[must_use]
    pub fn choose_balanced(
        &self,
        request: CompilerBalancingRequest,
        local_resources_available: bool,
        now: u64,
        remotes: &[CompilerBalancedRemote],
    ) -> CompilerPlacement {
        let remote = remotes
            .iter()
            .filter_map(|candidate| {
                let cost = predicted_remote_cost(request, candidate, now)?;
                Some((cost, candidate.node.peer()))
            })
            .min_by_key(|(cost, peer)| (*cost, *peer));
        let local_available = request.local != LocalCompilerAvailability::Unavailable
            && local_resources_available
            && request.local_cost.checked_total().is_some()
            && request
                .deadline_at
                .is_none_or(|deadline| now.saturating_add(request.local_cost.total()) <= deadline);
        if request.demand == CompilerDemand::Interactive
            && local_available
            && now >= request.submitted_at
            && now.saturating_sub(request.submitted_at) < request.local_first_budget
            && request.local_start_delay <= request.local_first_budget
        {
            return CompilerPlacement::Local;
        }
        let Some((remote_cost, peer)) = remote else {
            return if local_available {
                CompilerPlacement::Local
            } else {
                CompilerPlacement::OfflineUnavailable
            };
        };
        if !local_available || remote_cost < request.local_cost.total() {
            CompilerPlacement::Remote(peer)
        } else {
            CompilerPlacement::Local
        }
    }
}

fn predicted_remote_cost(
    request: CompilerBalancingRequest,
    candidate: &CompilerBalancedRemote,
    now: u64,
) -> Option<u64> {
    let remote = candidate.work;
    let node = &candidate.node;
    if remote.work_id != request.work.transfer_work_id()
        || remote.binding.work_id != request.work.transfer_work_id()
        || remote.peer != node.peer
        || remote.binding != node.binding
        || remote.worker_incarnation != node.worker_incarnation
        || !remote.binding.valid_at(now)
        || !node.valid_at(now)
        || now < remote.observed_at
        || now >= remote.expires_at
        || remote.confidence_per_mille < 500
        || !request.resources.is_positive()
        || !request.resources.fits_within(node.available())
    {
        return None;
    }
    let required_transfer = remote
        .transfer_bytes
        .checked_add(request.work.max_output_bytes)?;
    if u64::from(request.resources.transfer.bytes()) < required_transfer {
        return None;
    }
    let mut completion = remote.completion;
    if node.has_warm_session(request.session_affinity) {
        completion.warmup = 0;
    }
    let pressure = max_resource_pressure_per_mille(node.total, node.busy);
    completion.contention = completion
        .contention
        .saturating_add(completion.execution.saturating_mul(u64::from(pressure)) / 1_000);
    let total = completion.checked_total()?;
    if request
        .deadline_at
        .is_some_and(|deadline| now.saturating_add(total) > deadline)
    {
        return None;
    }
    Some(total)
}

fn max_resource_pressure_per_mille(
    total: CompilerResourceCredits,
    busy: CompilerResourceCredits,
) -> u16 {
    let ratios = [
        (u64::from(busy.cpu.0) * 1_000) / u64::from(total.cpu.0.max(1)),
        busy.memory.0.saturating_mul(1_000) / total.memory.0.max(1),
        busy.transfer.0.saturating_mul(1_000) / total.transfer.0.max(1),
    ];
    ratios.into_iter().max().unwrap_or(0).min(1_000) as u16
}

/// Exact DB/index-owner attempt correlation supplied by the publication authority.
///
/// The fields are private so callers cannot bypass the nonzero `AttemptId` and `Fence` checks.
/// Construction still belongs at the authority adapter boundary: these value types validate
/// representation, while Turso remains responsible for proving the attempt is current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerAttemptToken {
    /// Attempt ordinal minted by the publication owner.
    attempt: AttemptId,
    /// Exact publication fence minted by the publication owner.
    fence: Fence,
}

impl CompilerAttemptToken {
    /// Constructs a scheduler token from already checked authority identifier types.
    #[must_use]
    pub const fn new(attempt: AttemptId, fence: Fence) -> Self {
        Self { attempt, fence }
    }

    /// Returns the checked nonzero DB attempt ordinal.
    #[must_use]
    pub const fn attempt(self) -> AttemptId {
        self.attempt
    }

    /// Returns the checked full-width DB attempt fence.
    #[must_use]
    pub const fn fence(self) -> Fence {
        self.fence
    }
}

/// Scheduled route for one exact package/target attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerAssignmentRoute {
    /// Local embedded or standalone compiler node.
    Local,
    /// Authenticated remote Iroh endpoint.
    Remote(CompilerPeerId),
}

/// Immutable assignment passed from index owner to compiler-node adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerAssignment {
    work: CompilerWorkIdentity,
    token: CompilerAttemptToken,
    route: CompilerAssignmentRoute,
}

impl CompilerAssignment {
    /// Rebuilds the exact remote assignment facts from already revalidated durable authority
    /// state after an owner restart.
    ///
    /// This constructor does not validate Turso or worker trust. It accepts only a fully typed
    /// work identity with a verifier-accepted complete workspace input, then checks that the
    /// derived transfer ID and checked attempt/fence equal the durable scope. Locald must call it
    /// only after its sealed recovery path rechecks the current Turso attempt, source observation,
    /// and persisted worker grant.
    pub fn recover_exact(
        work: CompilerWorkIdentity,
        token: CompilerAttemptToken,
        peer: CompilerPeerId,
        expected_work_id: [u8; 16],
        expected_attempt: AttemptId,
        expected_fence: Fence,
    ) -> Result<Self, CompilerAssignmentError> {
        let Some(full_workspace) = work.input().full_workspace() else {
            return Err(CompilerAssignmentError::InvalidRecoveredAssignment);
        };
        let claim = full_workspace.claim();
        if expected_work_id == [0; 16]
            || claim.input_closure_id == [0; 32]
            || claim.manifest_object_id == [0; 32]
            || work.transfer_work_id() != expected_work_id
            || token.attempt() != expected_attempt
            || token.fence() != expected_fence
        {
            return Err(CompilerAssignmentError::InvalidRecoveredAssignment);
        }
        Ok(Self {
            work,
            token,
            route: CompilerAssignmentRoute::Remote(peer),
        })
    }

    /// Reconstructs the storage-only candidate for a cold, owner-verified remote completion.
    ///
    /// This is intentionally independent of the in-memory scheduler table so an owner can
    /// re-admit a durable result after restart. It proves only exact assignment/peer/closure
    /// consistency; locald must first revalidate the durable Turso attempt, source observation,
    /// worker grant, worker receipt, and pinned CAS closure. It grants no selection authority.
    pub fn admit_recovered_completion(
        self,
        claim: RemoteCompilerCompletionClaim,
        stored: StoredClosureReceipt,
    ) -> Result<StoredCompilerCandidate, CompilerAssignmentError> {
        let CompilerAssignmentRoute::Remote(expected_peer) = self.route else {
            return Err(CompilerAssignmentError::WrongRoute);
        };
        if claim.work != self.work
            || claim.token != self.token
            || claim.peer != expected_peer
            || claim.closure != ArtifactClosureClaim::from_id(stored.closure())
        {
            return Err(CompilerAssignmentError::CompletionMismatch);
        }
        Ok(StoredCompilerCandidate {
            work: self.work,
            token: self.token,
            peer: expected_peer,
            closure: stored,
        })
    }

    /// Returns the exact package/target/read identity.
    #[must_use]
    pub const fn work(self) -> CompilerWorkIdentity {
        self.work
    }

    /// Returns the publication owner's exact attempt token.
    #[must_use]
    pub const fn token(self) -> CompilerAttemptToken {
        self.token
    }

    /// Returns the assigned local or remote route.
    #[must_use]
    pub const fn route(self) -> CompilerAssignmentRoute {
        self.route
    }
}

/// Remote completion claim awaiting exact assignment and durable CAS comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteCompilerCompletionClaim {
    /// Exact work identity echoed by the worker.
    pub work: CompilerWorkIdentity,
    /// Exact DB/index-owner attempt echoed by the worker.
    pub token: CompilerAttemptToken,
    /// Iroh endpoint that returned the closure.
    pub peer: CompilerPeerId,
    /// Closure claimed by the worker and independently checked by the receiving CAS.
    pub closure: ArtifactClosureClaim,
}

/// Candidate emitted after a remote closure is stored and exact attempt checks pass.
///
/// This type proves only that the received closure is durably reachable and belongs to the
/// currently assigned scheduler attempt. It has no compare-and-select or selected-head method;
/// the index owner's Turso authority still validates the candidate and its complete input fence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredCompilerCandidate {
    work: CompilerWorkIdentity,
    token: CompilerAttemptToken,
    peer: CompilerPeerId,
    closure: StoredClosureReceipt,
}

impl StoredCompilerCandidate {
    /// Returns the exact package/target/input assignment.
    #[must_use]
    pub const fn work(self) -> CompilerWorkIdentity {
        self.work
    }

    /// Returns the exact publication owner's attempt token.
    #[must_use]
    pub const fn token(self) -> CompilerAttemptToken {
        self.token
    }

    /// Returns the authenticated endpoint that produced this candidate.
    #[must_use]
    pub const fn peer(self) -> CompilerPeerId {
        self.peer
    }

    /// Returns the storage-only durable CAS receipt.
    #[must_use]
    pub const fn closure_receipt(self) -> StoredClosureReceipt {
        self.closure
    }
}

/// Assignment-control rejection, including capacity and exact-attempt fencing.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CompilerAssignmentError {
    /// The configured bounded assignment table is full.
    #[error("compiler assignment capacity is full")]
    AtCapacity,
    /// Another attempt for this exact package/target work is already live.
    #[error("compiler work already has a live assignment")]
    AlreadyAssigned,
    /// The supplied attempt is no longer the exact active attempt.
    #[error("compiler attempt is stale or already completed")]
    StaleAttempt,
    /// Completion came from a different peer, route, identity, or closure.
    #[error("remote compiler completion does not match its exact assignment")]
    CompletionMismatch,
    /// A local assignment cannot be completed through the remote closure path.
    #[error("assignment is not remote")]
    WrongRoute,
    /// A reservation was already committed, cancelled, or superseded.
    #[error("compiler assignment reservation is stale")]
    StaleReservation,
    /// Checked assignment reservation sequence overflowed.
    #[error("compiler assignment reservation sequence overflow")]
    ReservationOverflow,
    /// Durable work, attempt, fence, or full-workspace capture did not reproduce one exact
    /// remote assignment after owner restart.
    #[error("recovered compiler assignment does not match its exact durable scope")]
    InvalidRecoveredAssignment,
    /// Balanced placement requires an explicit local CPU/memory/byte budget.
    #[error("balanced placement has no configured local resource budget")]
    MissingLocalResourceBudget,
    /// The requested resource credits are zero or arithmetic overflowed.
    #[error("compiler resource credits are invalid")]
    InvalidResourceCredits,
    /// A verified worker snapshot is older than the latest owner observation.
    #[error("compiler worker capacity observation is stale")]
    StaleCapacityObservation,
    /// A peer failure record does not match its current process incarnation.
    #[error("compiler peer failure belongs to a stale worker incarnation")]
    StalePeerIncarnation,
    /// Retry snapshot schema or rows are not canonical.
    #[error("compiler peer retry snapshot is invalid")]
    InvalidRetrySnapshot,
    /// Backoff base/cap values are invalid.
    #[error("compiler peer backoff policy is invalid")]
    InvalidBackoffPolicy,
    /// A speculative hedge lacks a positive explicit budget or repeatable work.
    #[error("compiler hedge is not admitted by the repeatability or resource budget")]
    HedgeNotAdmitted,
    /// A hedge's local and remote attempts do not have distinct owner fences.
    #[error("compiler hedge attempts must use distinct attempt IDs and fences")]
    HedgeFenceMismatch,
}

#[derive(Clone, Copy)]
struct ActiveAssignment {
    assignment: CompilerAssignment,
    resources: CompilerResourceCredits,
    worker_incarnation: Option<[u8; 32]>,
}

#[derive(Default)]
struct AssignmentState {
    active: BTreeMap<[u8; 16], ActiveAssignment>,
    local_active: BTreeMap<[u8; 16], ActiveAssignment>,
    hedged: BTreeMap<([u8; 16], u64), ActiveAssignment>,
    reserved: BTreeMap<[u8; 16], u64>,
    next_reservation: u64,
    peer_incarnations: BTreeMap<CompilerPeerId, ([u8; 32], u64)>,
    peer_retries: BTreeMap<CompilerPeerId, CompilerPeerRetryRecord>,
}

/// Bounded remote-assignment table shared by the GUI embedded node and standalone service.
///
/// Local work is handed to the compiler runtime's own bounded lane queue. This table's slot
/// budget is reserved for network assignments so background remotes can never consume the
/// capacity needed to start an interactive local compile.
#[derive(Clone)]
pub struct CompilerClusterScheduler {
    capacity: NonZeroUsize,
    local_capacity: NonZeroUsize,
    local_resources: Option<CompilerResourceCredits>,
    max_hedged_assignments: usize,
    state: Arc<Mutex<AssignmentState>>,
}

impl std::fmt::Debug for CompilerClusterScheduler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        formatter
            .debug_struct("CompilerClusterScheduler")
            .field("remote_capacity", &self.capacity)
            .field("local_capacity", &self.local_capacity)
            .field("remote_active", &state.active.len())
            .field("local_active", &state.local_active.len())
            .field("hedged_active", &state.hedged.len())
            .field("reserved", &state.reserved.len())
            .finish()
    }
}

impl CompilerClusterScheduler {
    /// Creates a bounded table for live local and remote package/target assignments.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self::with_capacities(capacity, capacity)
    }

    /// Creates separately bounded local and remote assignment tables.
    ///
    /// The local bound should match the compiler runtime's admitted local jobs;
    /// the remote bound protects network slots independently so background work
    /// cannot occupy local interactive capacity.
    #[must_use]
    pub fn with_capacities(remote_capacity: NonZeroUsize, local_capacity: NonZeroUsize) -> Self {
        Self {
            capacity: remote_capacity,
            local_capacity,
            local_resources: None,
            max_hedged_assignments: 0,
            state: Arc::new(Mutex::new(AssignmentState::default())),
        }
    }

    /// Creates a scheduler with explicit local resources and a bounded speculative-hedge lane.
    ///
    /// The default constructors disable hedging. A caller must opt in with a nonzero hedge
    /// limit and still provide repeatable work plus distinct owner-minted attempt fences.
    #[must_use]
    pub fn with_balancing_limits(
        remote_capacity: NonZeroUsize,
        local_capacity: NonZeroUsize,
        local_resources: CompilerResourceCredits,
        max_hedged_assignments: usize,
    ) -> Self {
        Self {
            capacity: remote_capacity,
            local_capacity,
            local_resources: Some(local_resources),
            max_hedged_assignments,
            state: Arc::new(Mutex::new(AssignmentState::default())),
        }
    }

    /// Observes an authenticated process incarnation and invalidates any old-worker assignments.
    ///
    /// The returned assignments should receive transport cancellation messages. A changed
    /// incarnation also clears its old backoff and session affinity; the new process is cold.
    ///
    /// # Errors
    ///
    /// Returns a stale-observation error when the owner clock regresses or the snapshot is older
    /// than the latest observation for this peer.
    pub fn observe_peer_incarnation(
        &self,
        peer: CompilerPeerId,
        incarnation: [u8; 32],
        observed_at: u64,
    ) -> Result<CompilerPeerRestart, CompilerAssignmentError> {
        if incarnation == [0; 32] {
            return Err(CompilerAssignmentError::StalePeerIncarnation);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = state.peer_incarnations.get(&peer).copied();
        if previous.is_some_and(|(_, last_seen)| observed_at < last_seen) {
            return Err(CompilerAssignmentError::StaleCapacityObservation);
        }
        let old_incarnation = previous.map(|(value, _)| value);
        let changed = old_incarnation.is_some_and(|value| value != incarnation);
        let mut invalidated = Vec::new();
        if changed {
            let old = old_incarnation.unwrap_or([0; 32]);
            let remote_ids: Vec<_> = state
                .active
                .iter()
                .filter_map(|(work_id, active)| {
                    (active.worker_incarnation == Some(old)
                        && active.assignment.route == CompilerAssignmentRoute::Remote(peer))
                    .then_some(*work_id)
                })
                .collect();
            for work_id in remote_ids {
                if let Some(active) = state.active.remove(&work_id) {
                    invalidated.push(active.assignment);
                }
            }
            let stale_hedge_work: Vec<_> = state
                .hedged
                .iter()
                .filter_map(|((work_id, _), active)| {
                    (active.worker_incarnation == Some(old)
                        && active.assignment.route == CompilerAssignmentRoute::Remote(peer))
                    .then_some(*work_id)
                })
                .collect();
            let hedge_keys: Vec<_> = state
                .hedged
                .keys()
                .filter(|(work_id, _)| stale_hedge_work.contains(work_id))
                .copied()
                .collect();
            for key in hedge_keys {
                if let Some(active) = state.hedged.remove(&key) {
                    invalidated.push(active.assignment);
                }
            }
            state.peer_retries.remove(&peer);
        }
        state
            .peer_incarnations
            .insert(peer, (incarnation, observed_at));
        invalidated.sort_by_key(|assignment| assignment.token.attempt());
        Ok(CompilerPeerRestart {
            previous: old_incarnation,
            current: incarnation,
            invalidated,
        })
    }

    /// Records a peer failure with bounded exponential backoff for its current incarnation.
    ///
    /// Persist the returned record through [`Self::retry_snapshot`] alongside the owner's
    /// durable retry state. A different worker incarnation starts with a clean failure history.
    ///
    /// # Errors
    ///
    /// Returns a stale-incarnation, time-regression, or checked-backoff error.
    pub fn record_peer_failure(
        &self,
        peer: CompilerPeerId,
        incarnation: [u8; 32],
        now: u64,
        policy: CompilerBackoffPolicy,
    ) -> Result<CompilerPeerRetryRecord, CompilerAssignmentError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((current_incarnation, _)) = state.peer_incarnations.get(&peer).copied() else {
            return Err(CompilerAssignmentError::StalePeerIncarnation);
        };
        if current_incarnation != incarnation || incarnation == [0; 32] {
            return Err(CompilerAssignmentError::StalePeerIncarnation);
        }
        let previous = state.peer_retries.get(&peer).copied();
        if previous.is_some_and(|record| {
            record.worker_incarnation == incarnation && now < record.last_failure_at
        }) {
            return Err(CompilerAssignmentError::StaleCapacityObservation);
        }
        let failures = previous
            .filter(|record| record.worker_incarnation == incarnation)
            .map_or(1, |record| record.failures.saturating_add(1));
        let retry_at = policy
            .retry_at(peer, incarnation, failures, now)
            .ok_or(CompilerAssignmentError::ReservationOverflow)?;
        let record = CompilerPeerRetryRecord {
            peer,
            worker_incarnation: incarnation,
            failures,
            last_failure_at: now,
            retry_at,
        };
        state.peer_retries.insert(peer, record);
        Ok(record)
    }

    /// Clears a peer's backoff after successful admitted work on the current incarnation.
    pub fn record_peer_success(
        &self,
        peer: CompilerPeerId,
        incarnation: [u8; 32],
    ) -> Result<(), CompilerAssignmentError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .peer_incarnations
            .get(&peer)
            .map(|(current, _)| *current)
            != Some(incarnation)
        {
            return Err(CompilerAssignmentError::StalePeerIncarnation);
        }
        if state
            .peer_retries
            .get(&peer)
            .is_some_and(|record| record.worker_incarnation == incarnation)
        {
            state.peer_retries.remove(&peer);
        }
        Ok(())
    }

    /// Returns a canonical versioned snapshot of peer backoff state for durable persistence.
    #[must_use]
    pub fn retry_snapshot(&self) -> CompilerRetrySnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        CompilerRetrySnapshot {
            version: 1,
            peers: state.peer_retries.values().copied().collect(),
        }
    }

    /// Restores owner-persisted retry state before dispatch resumes after restart.
    ///
    /// Active work is never restored: callers must obtain a new DB attempt/fence and re-offer
    /// after peer observation. This makes a process restart a fencing boundary even when retry
    /// history survives it.
    pub fn restore_retry_snapshot(
        &self,
        snapshot: CompilerRetrySnapshot,
    ) -> Result<(), CompilerAssignmentError> {
        if snapshot.version != 1
            || snapshot.peers.len() > 4_096
            || snapshot
                .peers
                .windows(2)
                .any(|pair| pair[0].peer >= pair[1].peer)
            || snapshot.peers.iter().any(|record| {
                record.worker_incarnation == [0; 32]
                    || record.failures == 0
                    || record.retry_at < record.last_failure_at
            })
        {
            return Err(CompilerAssignmentError::InvalidRetrySnapshot);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.active.is_empty()
            || !state.local_active.is_empty()
            || !state.hedged.is_empty()
            || !state.reserved.is_empty()
            || !state.peer_retries.is_empty()
        {
            return Err(CompilerAssignmentError::AlreadyAssigned);
        }
        for record in snapshot.peers {
            state.peer_incarnations.insert(
                record.peer,
                (record.worker_incarnation, record.last_failure_at),
            );
            state.peer_retries.insert(record.peer, record);
        }
        Ok(())
    }

    /// Re-adopts one exact owner-verified assignment so a restarted owner can finish validating a
    /// retained worker result through the regular bounded result receiver.
    ///
    /// This does not mint publication authority or prove that the Turso attempt is still current.
    /// The local authority must revalidate those facts and seal the assignment before calling
    /// this method. `resources` and `worker_incarnation` are the persisted claims admitted for
    /// the original assignment, so the recovered row occupies normal global and per-worker
    /// capacity until result admission completes or the exact assignment is cancelled.
    pub fn adopt_recovered_remote(
        &self,
        assignment: CompilerAssignment,
        resources: CompilerResourceCredits,
        worker_incarnation: [u8; 32],
    ) -> Result<(), CompilerAssignmentError> {
        let CompilerAssignmentRoute::Remote(peer) = assignment.route else {
            return Err(CompilerAssignmentError::WrongRoute);
        };
        if !resources.is_positive() || worker_incarnation == [0; 32] {
            return Err(CompilerAssignmentError::InvalidResourceCredits);
        }
        if assignment.work.input().full_workspace().is_none() {
            return Err(CompilerAssignmentError::InvalidRecoveredAssignment);
        }
        let work_id = assignment.work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active.contains_key(&work_id)
            || state.local_active.contains_key(&work_id)
            || state.hedged.keys().any(|(id, _)| *id == work_id)
            || state.reserved.contains_key(&work_id)
        {
            return Err(CompilerAssignmentError::AlreadyAssigned);
        }
        let remote_slots = state
            .active
            .len()
            .saturating_add(state.reserved.len())
            .saturating_add(
                state
                    .hedged
                    .values()
                    .filter(|active| {
                        matches!(active.assignment.route, CompilerAssignmentRoute::Remote(_))
                    })
                    .count(),
            );
        if remote_slots >= self.capacity.get() {
            return Err(CompilerAssignmentError::AtCapacity);
        }
        state.active.insert(
            work_id,
            ActiveAssignment {
                assignment,
                resources,
                worker_incarnation: Some(worker_incarnation),
            },
        );
        // Preserve the peer association for subsequent bounded credit accounting. An older
        // recovered incarnation may later be superseded by a fresh live probe, but the result
        // receiver still fences by this assignment's exact peer and attempt.
        state
            .peer_incarnations
            .entry(peer)
            .or_insert((worker_incarnation, 0));
        Ok(())
    }

    /// Chooses and reserves a local or remote assignment with typed resource admission.
    ///
    /// The worker snapshot must already have passed [`Self::observe_peer_incarnation`]. All
    /// capacity checks and the assignment commit happen under the scheduler's short per-instance
    /// lock, so concurrent callers cannot oversubscribe a peer's CPU, memory, bytes, or slots.
    ///
    /// # Errors
    ///
    /// Returns an assignment conflict, stale peer snapshot, missing local resource budget, or
    /// invalid typed demand. Lack of a viable route is returned as `OfflineUnavailable`.
    pub fn place_balanced_and_assign(
        &self,
        policy: &CompilerPlacementPolicy,
        request: CompilerBalancingRequest,
        now: u64,
        remotes: &[CompilerBalancedRemote],
        token: CompilerAttemptToken,
    ) -> Result<CompilerAssignmentOutcome, CompilerAssignmentError> {
        if !request.resources.is_positive() || now < request.submitted_at {
            return Err(CompilerAssignmentError::InvalidResourceCredits);
        }
        let local_budget = self
            .local_resources
            .ok_or(CompilerAssignmentError::MissingLocalResourceBudget)?;
        let work_id = request.work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active.contains_key(&work_id)
            || state.local_active.contains_key(&work_id)
            || state.hedged.keys().any(|(id, _)| *id == work_id)
            || state.reserved.contains_key(&work_id)
        {
            return Err(CompilerAssignmentError::AlreadyAssigned);
        }

        let local_used =
            state
                .local_active
                .values()
                .chain(state.hedged.values().filter(|active| {
                    matches!(active.assignment.route, CompilerAssignmentRoute::Local)
                }))
                .try_fold(CompilerResourceCredits::default(), |sum, active| {
                    sum.checked_add(active.resources)
                })
                .ok_or(CompilerAssignmentError::InvalidResourceCredits)?;
        let local_credit_available = request
            .resources
            .fits_within(local_budget.saturating_sub(local_used));
        let local_slot_available = state.local_active.len().saturating_add(
            state
                .hedged
                .values()
                .filter(|active| matches!(active.assignment.route, CompilerAssignmentRoute::Local))
                .count(),
        ) < self.local_capacity.get();
        let local_available = local_credit_available && local_slot_available;

        let remote_slots_used = state
            .active
            .len()
            .saturating_add(state.reserved.len())
            .saturating_add(
                state
                    .hedged
                    .values()
                    .filter(|active| {
                        matches!(active.assignment.route, CompilerAssignmentRoute::Remote(_))
                    })
                    .count(),
            );
        let mut eligible = Vec::new();
        for candidate in remotes {
            let node = &candidate.node;
            if candidate.work.binding.attempt != token.attempt().get()
                || candidate.work.binding.fence != token.fence().as_bytes()
                || candidate.work.binding.work_id != work_id
                || !node.valid_at(now)
                || state
                    .peer_incarnations
                    .get(&node.peer)
                    .is_none_or(|(incarnation, observed)| {
                        *incarnation != node.worker_incarnation || node.observed_at < *observed
                    })
                || state.peer_retries.get(&node.peer).is_some_and(|retry| {
                    retry.worker_incarnation == node.worker_incarnation && now < retry.retry_at
                })
            {
                continue;
            }
            let allocated = state
                .active
                .values()
                .chain(state.hedged.values())
                .filter(|active| {
                    active.assignment.route == CompilerAssignmentRoute::Remote(node.peer)
                        && active.worker_incarnation == Some(node.worker_incarnation)
                })
                .try_fold(CompilerResourceCredits::default(), |sum, active| {
                    sum.checked_add(active.resources)
                })
                .ok_or(CompilerAssignmentError::InvalidResourceCredits)?;
            let current_free = node.available().saturating_sub(allocated);
            if !request.resources.fits_within(current_free) {
                continue;
            }
            let mut adjusted = candidate.clone();
            adjusted.node.busy = node
                .busy
                .checked_add(allocated)
                .ok_or(CompilerAssignmentError::InvalidResourceCredits)?;
            eligible.push(adjusted);
        }
        let mut placement = policy.choose_balanced(request, local_available, now, &eligible);
        if remote_slots_used >= self.capacity.get()
            && matches!(placement, CompilerPlacement::Remote(_))
        {
            placement = if local_available {
                CompilerPlacement::Local
            } else {
                CompilerPlacement::OfflineUnavailable
            };
        }
        let assignment = match placement {
            CompilerPlacement::Local => {
                if !local_available {
                    return Ok(CompilerAssignmentOutcome::OfflineUnavailable);
                }
                let assignment = CompilerAssignment {
                    work: request.work,
                    token,
                    route: CompilerAssignmentRoute::Local,
                };
                state.local_active.insert(
                    work_id,
                    ActiveAssignment {
                        assignment,
                        resources: request.resources,
                        worker_incarnation: None,
                    },
                );
                assignment
            }
            CompilerPlacement::Remote(peer) => {
                let Some(candidate) = eligible
                    .iter()
                    .find(|candidate| candidate.node.peer == peer)
                else {
                    return Ok(CompilerAssignmentOutcome::OfflineUnavailable);
                };
                let assignment = CompilerAssignment {
                    work: request.work,
                    token,
                    route: CompilerAssignmentRoute::Remote(peer),
                };
                state.active.insert(
                    work_id,
                    ActiveAssignment {
                        assignment,
                        resources: request.resources,
                        worker_incarnation: Some(candidate.node.worker_incarnation),
                    },
                );
                assignment
            }
            CompilerPlacement::OfflineUnavailable => {
                return Ok(CompilerAssignmentOutcome::OfflineUnavailable);
            }
        };
        Ok(CompilerAssignmentOutcome::Assigned(assignment))
    }

    /// Admits a local/remote hedge only with repeatable work and an explicit caller budget.
    ///
    /// Both branches receive separate owner-minted attempt IDs and fences. Results remain
    /// storage-only candidates until the authority accepts the exact current token; the
    /// scheduler never chooses a publication winner. Hedge capacity is disabled by default and
    /// consumes both local and remote assignment/resource budgets when enabled.
    pub fn assign_hedged(
        &self,
        request: CompilerBalancingRequest,
        candidate: &CompilerBalancedRemote,
        local_token: CompilerAttemptToken,
        remote_token: CompilerAttemptToken,
        repeatable: bool,
        explicit_hedge_budget: NonZeroUsize,
        now: u64,
    ) -> Result<CompilerHedgeAssignments, CompilerAssignmentError> {
        if !repeatable || self.max_hedged_assignments < 2 {
            return Err(CompilerAssignmentError::HedgeNotAdmitted);
        }
        if local_token.attempt() == remote_token.attempt()
            || local_token.fence() == remote_token.fence()
        {
            return Err(CompilerAssignmentError::HedgeFenceMismatch);
        }
        if !request.resources.is_positive() || now < request.submitted_at {
            return Err(CompilerAssignmentError::InvalidResourceCredits);
        }
        let local_budget = self
            .local_resources
            .ok_or(CompilerAssignmentError::MissingLocalResourceBudget)?;
        let work_id = request.work.transfer_work_id();
        let peer = candidate.node.peer();
        if candidate.work.peer != peer
            || candidate.work.work_id != work_id
            || candidate.work.binding.work_id() != work_id
            || candidate.work.binding.attempt() != remote_token.attempt().get()
            || candidate.work.binding.fence() != remote_token.fence().as_bytes()
            || !candidate.node.valid_at(now)
            || now < candidate.work.observed_at
            || now >= candidate.work.expires_at
        {
            return Err(CompilerAssignmentError::CompletionMismatch);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active.contains_key(&work_id)
            || state.local_active.contains_key(&work_id)
            || state.hedged.keys().any(|(id, _)| *id == work_id)
            || state.reserved.contains_key(&work_id)
        {
            return Err(CompilerAssignmentError::AlreadyAssigned);
        }
        let budget_limit = explicit_hedge_budget.get().min(self.max_hedged_assignments);
        if state.hedged.len().saturating_add(2) > budget_limit {
            return Err(CompilerAssignmentError::HedgeNotAdmitted);
        }
        let current_incarnation = state.peer_incarnations.get(&peer).copied();
        if current_incarnation.is_none_or(|(incarnation, observed)| {
            incarnation != candidate.node.worker_incarnation
                || candidate.node.observed_at < observed
        }) {
            return Err(CompilerAssignmentError::StaleCapacityObservation);
        }
        if state.peer_retries.get(&peer).is_some_and(|retry| {
            retry.worker_incarnation == candidate.node.worker_incarnation && now < retry.retry_at
        }) {
            return Err(CompilerAssignmentError::AtCapacity);
        }
        let local_used =
            state
                .local_active
                .values()
                .chain(state.hedged.values().filter(|active| {
                    matches!(active.assignment.route, CompilerAssignmentRoute::Local)
                }))
                .try_fold(CompilerResourceCredits::default(), |sum, active| {
                    sum.checked_add(active.resources)
                })
                .ok_or(CompilerAssignmentError::InvalidResourceCredits)?;
        if state.local_active.len().saturating_add(
            state
                .hedged
                .values()
                .filter(|active| matches!(active.assignment.route, CompilerAssignmentRoute::Local))
                .count(),
        ) >= self.local_capacity.get()
            || !request
                .resources
                .fits_within(local_budget.saturating_sub(local_used))
        {
            return Err(CompilerAssignmentError::AtCapacity);
        }
        let remote_slots = state
            .active
            .len()
            .saturating_add(state.reserved.len())
            .saturating_add(
                state
                    .hedged
                    .values()
                    .filter(|active| {
                        matches!(active.assignment.route, CompilerAssignmentRoute::Remote(_))
                    })
                    .count(),
            );
        if remote_slots >= self.capacity.get() {
            return Err(CompilerAssignmentError::AtCapacity);
        }
        let remote_used = state
            .active
            .values()
            .chain(state.hedged.values())
            .filter(|active| {
                active.assignment.route == CompilerAssignmentRoute::Remote(peer)
                    && active.worker_incarnation == Some(candidate.node.worker_incarnation)
            })
            .try_fold(CompilerResourceCredits::default(), |sum, active| {
                sum.checked_add(active.resources)
            })
            .ok_or(CompilerAssignmentError::InvalidResourceCredits)?;
        if !request
            .resources
            .fits_within(candidate.node.available().saturating_sub(remote_used))
            || predicted_remote_cost(request, candidate, now).is_none()
        {
            return Err(CompilerAssignmentError::AtCapacity);
        }
        let local = CompilerAssignment {
            work: request.work,
            token: local_token,
            route: CompilerAssignmentRoute::Local,
        };
        let remote = CompilerAssignment {
            work: request.work,
            token: remote_token,
            route: CompilerAssignmentRoute::Remote(peer),
        };
        state.hedged.insert(
            (work_id, local_token.attempt().get()),
            ActiveAssignment {
                assignment: local,
                resources: request.resources,
                worker_incarnation: None,
            },
        );
        state.hedged.insert(
            (work_id, remote_token.attempt().get()),
            ActiveAssignment {
                assignment: remote,
                resources: request.resources,
                worker_incarnation: Some(candidate.node.worker_incarnation),
            },
        );
        Ok(CompilerHedgeAssignments { local, remote })
    }

    /// Reserves a slot before the caller asks Turso to mint the exact attempt fence.
    ///
    /// The reservation keeps the table bound while the publication authority is consulted.
    /// Dropping it releases the slot. The owner must call [`CompilerAssignmentReservation::commit`]
    /// only with the `CandidateAttempt` token returned for this exact work identity.
    ///
    /// # Errors
    ///
    /// Returns [`CompilerAssignmentError::AtCapacity`] or
    /// [`CompilerAssignmentError::AlreadyAssigned`] before side effects are scheduled.
    pub(crate) fn reserve(
        &self,
        work: CompilerWorkIdentity,
        route: CompilerAssignmentRoute,
    ) -> Result<CompilerAssignmentReservation, CompilerAssignmentError> {
        if !matches!(route, CompilerAssignmentRoute::Remote(_)) {
            return Err(CompilerAssignmentError::WrongRoute);
        }
        let work_id = work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active.contains_key(&work_id)
            || state.local_active.contains_key(&work_id)
            || state.hedged.keys().any(|(id, _)| *id == work_id)
            || state.reserved.contains_key(&work_id)
        {
            return Err(CompilerAssignmentError::AlreadyAssigned);
        }
        let hedged_remote = state
            .hedged
            .values()
            .filter(|active| matches!(active.assignment.route, CompilerAssignmentRoute::Remote(_)))
            .count();
        if state
            .active
            .len()
            .saturating_add(state.reserved.len())
            .saturating_add(hedged_remote)
            >= self.capacity.get()
        {
            return Err(CompilerAssignmentError::AtCapacity);
        }
        let reservation_id = state.next_reservation;
        state.next_reservation = state
            .next_reservation
            .checked_add(1)
            .ok_or(CompilerAssignmentError::ReservationOverflow)?;
        state.reserved.insert(work_id, reservation_id);
        Ok(CompilerAssignmentReservation {
            state: Arc::clone(&self.state),
            work,
            work_id,
            reservation_id,
            route,
            committed: false,
        })
    }

    /// Chooses a route and begins one exact assignment using the owner's DB-minted token.
    ///
    /// If remote placement is selected but capacity is full, an available local compiler is
    /// selected deterministically. A ready local interactive route is never delayed by a peer.
    /// The authority token must already have been created for this work identity by the index
    /// owner; the scheduler never mints semantic publication authority.
    ///
    /// # Errors
    ///
    /// Returns a bounded assignment-table error. [`CompilerPlacement::OfflineUnavailable`] is
    /// returned as a successful route value because it is an expected offline state.
    pub fn place_and_assign(
        &self,
        policy: &CompilerPlacementPolicy,
        work: CompilerWorkIdentity,
        demand: CompilerDemand,
        local: LocalCompilerAvailability,
        local_cost: u64,
        now: u64,
        remotes: &[VerifiedRemoteCompiler],
        token: CompilerAttemptToken,
    ) -> Result<CompilerAssignmentOutcome, CompilerAssignmentError> {
        let work_id = work.transfer_work_id();
        let current_remotes: Vec<_> = remotes
            .iter()
            .copied()
            .filter(|candidate| {
                let binding = candidate.binding();
                binding.work_id() == work_id
                    && binding.attempt() == token.attempt().get()
                    && binding.fence() == token.fence().as_bytes()
                    && binding.valid_at(now)
            })
            .collect();
        let placement = policy.choose(work, demand, local, local_cost, now, &current_remotes);
        let route = match placement {
            CompilerPlacement::Local => CompilerAssignmentRoute::Local,
            CompilerPlacement::Remote(peer) => CompilerAssignmentRoute::Remote(peer),
            CompilerPlacement::OfflineUnavailable => {
                return Ok(CompilerAssignmentOutcome::OfflineUnavailable);
            }
        };
        if route == CompilerAssignmentRoute::Local {
            return match self.assign_local(work, token) {
                Ok(assignment) => Ok(CompilerAssignmentOutcome::Assigned(assignment)),
                Err(CompilerAssignmentError::AtCapacity) => {
                    Ok(CompilerAssignmentOutcome::OfflineUnavailable)
                }
                Err(error) => Err(error),
            };
        }
        match self.reserve(work, route) {
            Ok(reservation) => {
                return reservation
                    .commit(token)
                    .map(CompilerAssignmentOutcome::Assigned);
            }
            Err(CompilerAssignmentError::AtCapacity)
                if matches!(route, CompilerAssignmentRoute::Remote(_))
                    && local != LocalCompilerAvailability::Unavailable =>
            {
                return match self.assign_local(work, token) {
                    Ok(assignment) => Ok(CompilerAssignmentOutcome::Assigned(assignment)),
                    Err(CompilerAssignmentError::AtCapacity) => {
                        Ok(CompilerAssignmentOutcome::OfflineUnavailable)
                    }
                    Err(error) => Err(error),
                };
            }
            Err(error) => return Err(error),
        }
    }

    /// Cancels one exact assignment and frees its bounded slot.
    ///
    /// The caller should propagate the returned token to the index owner's cancellation
    /// authority and to the transport's `Cancel` control message.
    ///
    /// # Errors
    ///
    /// Returns [`CompilerAssignmentError::StaleAttempt`] if the work or fence changed.
    pub fn cancel(
        &self,
        assignment: CompilerAssignment,
    ) -> Result<CompilerAssignment, CompilerAssignmentError> {
        let work_id = assignment.work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hedge_key = (work_id, assignment.token.attempt().get());
        if state
            .hedged
            .get(&hedge_key)
            .is_some_and(|active| active.assignment == assignment)
        {
            state.hedged.remove(&hedge_key);
            return Ok(assignment);
        }
        match assignment.route {
            CompilerAssignmentRoute::Local => {
                if state
                    .local_active
                    .get(&work_id)
                    .map(|active| active.assignment)
                    != Some(assignment)
                {
                    return Err(CompilerAssignmentError::StaleAttempt);
                }
                state.local_active.remove(&work_id);
            }
            CompilerAssignmentRoute::Remote(_) => {
                let Some(current) = state.active.get(&work_id).copied() else {
                    return Err(CompilerAssignmentError::StaleAttempt);
                };
                if current.assignment != assignment {
                    return Err(CompilerAssignmentError::StaleAttempt);
                }
                state.active.remove(&work_id);
            }
        }
        Ok(assignment)
    }

    /// Checks that one exact local or remote assignment is still live.
    ///
    /// Transport adapters call this immediately before offering work, minting range grants,
    /// or serving bytes. The remote result path repeats its checks while accepting completion.
    ///
    /// # Errors
    ///
    /// Returns [`CompilerAssignmentError::StaleAttempt`] if the assignment was cancelled,
    /// completed, or superseded.
    pub fn validate_assignment(
        &self,
        assignment: CompilerAssignment,
    ) -> Result<(), CompilerAssignmentError> {
        let work_id = assignment.work.transfer_work_id();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = match assignment.route {
            CompilerAssignmentRoute::Local => state
                .local_active
                .get(&work_id)
                .map(|active| active.assignment),
            CompilerAssignmentRoute::Remote(_) => {
                state.active.get(&work_id).map(|active| active.assignment)
            }
        };
        let hedged_current = state
            .hedged
            .get(&(work_id, assignment.token.attempt().get()))
            .is_some_and(|active| active.assignment == assignment);
        if current == Some(assignment) || hedged_current {
            Ok(())
        } else {
            Err(CompilerAssignmentError::StaleAttempt)
        }
    }

    /// Supersedes a remote assignment with an owner-fenced local fallback.
    ///
    /// The replacement token must come from a newer authority attempt. The old assignment is
    /// removed before returning, so a late remote `Complete` is rejected while the caller starts
    /// its local fallback. The index owner remains responsible for cancelling/replacing its DB
    /// candidate attempt and publishing the local result.
    ///
    /// # Errors
    ///
    /// Returns stale/wrong-route errors when the supplied remote attempt is no longer current.
    pub fn fallback_to_local(
        &self,
        remote: CompilerAssignment,
        replacement: CompilerAttemptToken,
    ) -> Result<CompilerAssignment, CompilerAssignmentError> {
        if !matches!(remote.route, CompilerAssignmentRoute::Remote(_)) {
            return Err(CompilerAssignmentError::WrongRoute);
        }
        if replacement.attempt() <= remote.token.attempt()
            || replacement.fence() == remote.token.fence()
        {
            return Err(CompilerAssignmentError::StaleAttempt);
        }
        let work_id = remote.work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(current) = state.active.get(&work_id).copied() else {
            return Err(CompilerAssignmentError::StaleAttempt);
        };
        if current.assignment != remote {
            return Err(CompilerAssignmentError::StaleAttempt);
        }
        if state.local_active.contains_key(&work_id)
            || state.hedged.keys().any(|(id, _)| *id == work_id)
        {
            return Err(CompilerAssignmentError::AlreadyAssigned);
        }
        let local_used =
            state
                .local_active
                .values()
                .chain(state.hedged.values().filter(|active| {
                    matches!(active.assignment.route, CompilerAssignmentRoute::Local)
                }))
                .try_fold(CompilerResourceCredits::default(), |sum, active| {
                    sum.checked_add(active.resources)
                })
                .ok_or(CompilerAssignmentError::InvalidResourceCredits)?;
        let local_slots = state.local_active.len().saturating_add(
            state
                .hedged
                .values()
                .filter(|active| matches!(active.assignment.route, CompilerAssignmentRoute::Local))
                .count(),
        );
        if local_slots >= self.local_capacity.get()
            || (current.resources.is_positive()
                && self.local_resources.is_none_or(|budget| {
                    !current
                        .resources
                        .fits_within(budget.saturating_sub(local_used))
                }))
        {
            return Err(CompilerAssignmentError::AtCapacity);
        }
        let local = CompilerAssignment {
            work: remote.work,
            token: replacement,
            route: CompilerAssignmentRoute::Local,
        };
        state.active.remove(&work_id);
        state.local_active.insert(
            work_id,
            ActiveAssignment {
                assignment: local,
                resources: current.resources,
                worker_incarnation: None,
            },
        );
        Ok(local)
    }

    /// Retires one exact local compiler assignment after its result has been handed to the
    /// index owner's closure verifier, or after a local failure has been recorded.
    ///
    /// This releases only the scheduler's duplicate-work fence. Local execution capacity and
    /// durable result validation remain owned by the compiler runtime and index authority.
    ///
    /// # Errors
    ///
    /// Returns [`CompilerAssignmentError::StaleAttempt`] when the local token was superseded.
    pub fn complete_local(
        &self,
        assignment: CompilerAssignment,
    ) -> Result<(), CompilerAssignmentError> {
        if !matches!(assignment.route, CompilerAssignmentRoute::Local) {
            return Err(CompilerAssignmentError::WrongRoute);
        }
        let work_id = assignment.work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hedge_key = (work_id, assignment.token.attempt().get());
        if state
            .local_active
            .get(&work_id)
            .map(|active| active.assignment)
            == Some(assignment)
        {
            state.local_active.remove(&work_id);
            return Ok(());
        }
        if state
            .hedged
            .get(&hedge_key)
            .is_some_and(|active| active.assignment == assignment)
        {
            state.hedged.remove(&hedge_key);
            return Ok(());
        }
        Err(CompilerAssignmentError::StaleAttempt)
    }

    fn assign_local(
        &self,
        work: CompilerWorkIdentity,
        token: CompilerAttemptToken,
    ) -> Result<CompilerAssignment, CompilerAssignmentError> {
        let work_id = work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active.contains_key(&work_id)
            || state.local_active.contains_key(&work_id)
            || state.hedged.keys().any(|(id, _)| *id == work_id)
            || state.reserved.contains_key(&work_id)
        {
            return Err(CompilerAssignmentError::AlreadyAssigned);
        }
        if state.local_active.len().saturating_add(
            state
                .hedged
                .values()
                .filter(|active| matches!(active.assignment.route, CompilerAssignmentRoute::Local))
                .count(),
        ) >= self.local_capacity.get()
        {
            return Err(CompilerAssignmentError::AtCapacity);
        }
        let assignment = CompilerAssignment {
            work,
            token,
            route: CompilerAssignmentRoute::Local,
        };
        state.local_active.insert(
            work_id,
            ActiveAssignment {
                assignment,
                resources: CompilerResourceCredits::default(),
                worker_incarnation: None,
            },
        );
        Ok(assignment)
    }

    /// Accepts a matching remote closure as a stored candidate and frees its assignment slot.
    ///
    /// The exact target/input/base work, owner attempt/fence, authenticated peer, and claimed
    /// closure root must match. `StoredClosureReceipt` proves only durable CAS closure reachability;
    /// the returned candidate still must pass Turso closure admission and compare-and-select.
    ///
    /// # Errors
    ///
    /// Returns stale, wrong-route, or completion-mismatch errors without accepting the receipt.
    pub fn complete_remote(
        &self,
        assignment: CompilerAssignment,
        claim: RemoteCompilerCompletionClaim,
        stored: StoredClosureReceipt,
    ) -> Result<StoredCompilerCandidate, CompilerAssignmentError> {
        let CompilerAssignmentRoute::Remote(expected_peer) = assignment.route else {
            return Err(CompilerAssignmentError::WrongRoute);
        };
        let work_id = assignment.work.transfer_work_id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hedge_key = (work_id, assignment.token.attempt().get());
        let active_current = state.active.get(&work_id).copied();
        let hedged_current = state.hedged.get(&hedge_key).copied();
        if active_current.map(|current| current.assignment) != Some(assignment)
            && hedged_current.map(|current| current.assignment) != Some(assignment)
        {
            return Err(CompilerAssignmentError::StaleAttempt);
        }
        if claim.work != assignment.work
            || claim.token != assignment.token
            || claim.peer != expected_peer
            || claim.closure != ArtifactClosureClaim::from_id(stored.closure())
        {
            return Err(CompilerAssignmentError::CompletionMismatch);
        }
        if active_current.is_some() {
            state.active.remove(&work_id);
        } else {
            state.hedged.remove(&hedge_key);
        }
        Ok(StoredCompilerCandidate {
            work: assignment.work,
            token: assignment.token,
            peer: expected_peer,
            closure: stored,
        })
    }

    /// Checks a remote control completion before receiving its bulk closure bytes.
    ///
    /// The receiver still calls [`Self::complete_remote`] after CAS admission, which repeats
    /// these checks under the assignment lock to close cancellation/completion races.
    ///
    /// # Errors
    ///
    /// Returns stale, wrong-route, or identity/peer mismatch before opening a result sink.
    pub fn validate_remote_completion(
        &self,
        assignment: CompilerAssignment,
        claim: RemoteCompilerCompletionClaim,
    ) -> Result<(), CompilerAssignmentError> {
        let CompilerAssignmentRoute::Remote(expected_peer) = assignment.route else {
            return Err(CompilerAssignmentError::WrongRoute);
        };
        let work_id = assignment.work.transfer_work_id();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hedge_key = (work_id, assignment.token.attempt().get());
        let active_current = state.active.get(&work_id).copied();
        let hedged_current = state.hedged.get(&hedge_key).copied();
        if active_current.map(|current| current.assignment) != Some(assignment)
            && hedged_current.map(|current| current.assignment) != Some(assignment)
        {
            return Err(CompilerAssignmentError::StaleAttempt);
        }
        if claim.work != assignment.work
            || claim.token != assignment.token
            || claim.peer != expected_peer
        {
            return Err(CompilerAssignmentError::CompletionMismatch);
        }
        Ok(())
    }

    /// Returns the current local and remote assignment count for diagnostics.
    #[must_use]
    pub fn live_assignments(&self) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .active
            .len()
            .saturating_add(state.local_active.len())
            .saturating_add(state.hedged.len())
    }

    /// Returns the number of slots still available to new assignments.
    #[must_use]
    pub fn available_slots(&self) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.capacity.get().saturating_sub(
            state
                .active
                .len()
                .saturating_add(state.reserved.len())
                .saturating_add(
                    state
                        .hedged
                        .values()
                        .filter(|active| {
                            matches!(active.assignment.route, CompilerAssignmentRoute::Remote(_))
                        })
                        .count(),
                ),
        )
    }

    /// Returns the number of slots still available to local compiler assignments.
    #[must_use]
    pub fn available_local_slots(&self) -> usize {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.local_capacity.get().saturating_sub(
            state.local_active.len().saturating_add(
                state
                    .hedged
                    .values()
                    .filter(|active| {
                        matches!(active.assignment.route, CompilerAssignmentRoute::Local)
                    })
                    .count(),
            ),
        )
    }
}

/// Result of a placement request when the owner has no eligible execution route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerAssignmentOutcome {
    /// One exact assignment owns a bounded scheduler slot.
    Assigned(CompilerAssignment),
    /// Local and verified remote compiler nodes are currently unavailable.
    OfflineUnavailable,
}

/// Reserved bounded assignment slot awaiting a DB-minted attempt token.
pub(crate) struct CompilerAssignmentReservation {
    state: Arc<Mutex<AssignmentState>>,
    work: CompilerWorkIdentity,
    work_id: [u8; 16],
    reservation_id: u64,
    route: CompilerAssignmentRoute,
    committed: bool,
}

impl CompilerAssignmentReservation {
    /// Commits this slot with the exact attempt token minted by the index owner.
    ///
    /// # Errors
    ///
    /// Returns [`CompilerAssignmentError::StaleReservation`] if the reservation was cancelled
    /// or superseded before commit.
    pub(crate) fn commit(
        mut self,
        token: CompilerAttemptToken,
    ) -> Result<CompilerAssignment, CompilerAssignmentError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.reserved.get(&self.work_id) != Some(&self.reservation_id) {
            self.committed = true;
            return Err(CompilerAssignmentError::StaleReservation);
        }
        state.reserved.remove(&self.work_id);
        let assignment = CompilerAssignment {
            work: self.work,
            token,
            route: self.route,
        };
        state.active.insert(
            self.work_id,
            ActiveAssignment {
                assignment,
                resources: CompilerResourceCredits::default(),
                worker_incarnation: None,
            },
        );
        self.committed = true;
        Ok(assignment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct AcceptReadSet;

    impl ExactCompileReadSetVerifier for AcceptReadSet {
        fn verify_complete_read_set(
            &self,
            claim: ExactCompileReadSetClaim,
        ) -> Result<(), ExactCompileReadSetError> {
            if claim.identity().manifest.as_bytes() == &[0; 32] {
                return Err(ExactCompileReadSetError::Incomplete);
            }
            Ok(())
        }
    }

    struct CapturedWorkspaceVerifier {
        expected: FullWorkspaceInputClaim,
        captured_tree: &'static [u8],
    }

    impl FullWorkspaceInputVerifier for CapturedWorkspaceVerifier {
        fn verify_full_workspace_capture(
            &self,
            claim: FullWorkspaceInputClaim,
        ) -> Result<(), FullWorkspaceInputError> {
            let captured_root = GenerationId::from_canonical_bytes(self.captured_tree);
            if claim == self.expected && claim.identity.input_root == captured_root {
                Ok(())
            } else {
                Err(FullWorkspaceInputError::Rejected)
            }
        }
    }

    struct AcceptRemoteEvidence;

    impl CompilerRemotePreflightVerifier for AcceptRemoteEvidence {
        fn verify_remote_preflight(
            &self,
            _expected: CompilerWorkIdentity,
            claim: RemoteCompilerPreflightClaim,
        ) -> Result<(), CompilerRemotePreflightError> {
            if claim.have.proof_digest == [0; 32]
                || claim.have.inventory_digest == [0; 32]
                || claim.capability.capability_digest == [0; 32]
            {
                return Err(CompilerRemoteEvidenceError::Rejected);
            }
            Ok(())
        }
    }

    struct AcceptCapturedWorkspace {
        expected: FullWorkspaceInputClaim,
    }

    impl FullWorkspaceInputVerifier for AcceptCapturedWorkspace {
        fn verify_full_workspace_capture(
            &self,
            claim: FullWorkspaceInputClaim,
        ) -> Result<(), FullWorkspaceInputError> {
            if claim == self.expected {
                Ok(())
            } else {
                Err(FullWorkspaceInputError::Rejected)
            }
        }
    }

    struct AcceptCapacity;

    impl CompilerNodeCapacityVerifier for AcceptCapacity {
        fn verify_node_capacity(
            &self,
            _claim: &CompilerNodeCapacityClaim,
        ) -> Result<(), CompilerNodeCapacityError> {
            Ok(())
        }
    }

    fn work() -> Result<CompilerWorkIdentity, Box<dyn std::error::Error>> {
        work_with_branch(b"branch:main")
    }

    fn work_with_branch(branch: &[u8]) -> Result<CompilerWorkIdentity, Box<dyn std::error::Error>> {
        let package = PackageLineageId::from_canonical_parts(
            b"registry:test",
            b"pkg://rust/crates/acme",
            branch,
        )?;
        let target = ContentId::<CompilationTargetDomain>::from_canonical_bytes(b"rust-target");
        let recipe = ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"rust-recipe");
        let input_root = GenerationId::from_canonical_bytes(b"exact-source-root");
        let manifest = crate::ReadManifestId::from_value(b"full-workspace-manifest");
        let identity = CompilerInputIdentityClaim {
            scope: CompilerInputScope {
                package,
                target,
                recipe,
            },
            input_root,
            manifest,
        };
        let mut closure_hasher = blake3::Hasher::new();
        closure_hasher.update(b"execution-test-input-closure.v1\0");
        closure_hasher.update(branch);
        let input_closure_id = *closure_hasher.finalize().as_bytes();
        let mut manifest_hasher = blake3::Hasher::new();
        manifest_hasher.update(b"execution-test-manifest-object.v1\0");
        manifest_hasher.update(branch);
        let manifest_object_id = *manifest_hasher.finalize().as_bytes();
        let claim = FullWorkspaceInputClaim {
            identity,
            input_closure_id,
            manifest_object_id,
        };
        let captured = VerifierAcceptedFullWorkspaceInput::admit(
            claim,
            &AcceptCapturedWorkspace { expected: claim },
        )?;
        Ok(CompilerWorkIdentity::new(
            package,
            target,
            recipe,
            VerifiedCompilerInput::FullWorkspaceFresh(captured),
            None,
            1_000_000,
        )?)
    }

    #[test]
    fn full_workspace_proof_rechecks_the_captured_tree_and_has_a_fresh_only_identity() -> TestResult
    {
        let package = PackageLineageId::from_canonical_parts(
            b"registry:test",
            b"pkg://rust/crates/acme",
            b"branch:main",
        )?;
        let target = ContentId::<CompilationTargetDomain>::from_canonical_bytes(b"rust-target");
        let recipe = ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"rust-recipe");
        let tree = b"src/lib.rs=fn main() {}";
        let claim = FullWorkspaceInputClaim {
            identity: CompilerInputIdentityClaim {
                scope: CompilerInputScope {
                    package,
                    target,
                    recipe,
                },
                input_root: GenerationId::from_canonical_bytes(tree),
                manifest: crate::ReadManifestId::from_value(b"full-workspace-manifest"),
            },
            input_closure_id: [0x61; 32],
            manifest_object_id: [0x62; 32],
        };
        let verifier = CapturedWorkspaceVerifier {
            expected: claim,
            captured_tree: tree,
        };
        let captured = VerifierAcceptedFullWorkspaceInput::admit(claim, &verifier)?;
        let fresh = CompilerWorkIdentity::new(
            package,
            target,
            recipe,
            VerifiedCompilerInput::FullWorkspaceFresh(captured),
            None,
            1_000_000,
        )?;
        assert_eq!(fresh.input_identity(), claim.identity);
        assert_eq!(
            fresh
                .input()
                .full_workspace()
                .map(VerifierAcceptedFullWorkspaceInput::claim),
            Some(claim)
        );
        assert_eq!(
            CompilerWorkIdentity::new(
                package,
                target,
                recipe,
                VerifiedCompilerInput::FullWorkspaceFresh(captured),
                Some(GenerationId::from_canonical_bytes(b"old-base")),
                1_000_000,
            ),
            Err(CompilerIdentityError::FullWorkspaceRequiresFresh)
        );

        // Reuse the exact opaque IDs and scope while substituting a different captured tree.
        // The configured verifier must recompute the root from its own capture and reject it.
        let changed_tree_verifier = CapturedWorkspaceVerifier {
            expected: claim,
            captured_tree: b"src/lib.rs=fn main() { changed }",
        };
        assert_eq!(
            VerifierAcceptedFullWorkspaceInput::admit(claim, &changed_tree_verifier),
            Err(FullWorkspaceInputError::Rejected)
        );

        let changed_closure_claim = FullWorkspaceInputClaim {
            input_closure_id: [0x63; 32],
            ..claim
        };
        let changed_closure = VerifierAcceptedFullWorkspaceInput::admit(
            changed_closure_claim,
            &CapturedWorkspaceVerifier {
                expected: changed_closure_claim,
                captured_tree: tree,
            },
        )?;
        let different_snapshot = CompilerWorkIdentity::new(
            package,
            target,
            recipe,
            VerifiedCompilerInput::FullWorkspaceFresh(changed_closure),
            None,
            1_000_000,
        )?;
        assert_ne!(
            fresh.transfer_work_id(),
            different_snapshot.transfer_work_id()
        );
        assert_ne!(
            fresh.transfer_work_id(),
            compiler_transfer_work_id(
                package.as_bytes(),
                *target.as_ref(),
                *recipe.as_ref(),
                *claim.identity.manifest.as_bytes(),
                *claim.identity.input_root.as_ref(),
                None,
                1_000_000,
            )
        );

        let read_set = VerifiedCompileReadSet::admit(
            ExactCompileReadSetClaim::new(claim.identity),
            &AcceptReadSet,
        )?;
        let incremental = CompilerWorkIdentity::new(
            package,
            target,
            recipe,
            VerifiedCompilerInput::ProvenReadSetIncremental(read_set),
            Some(GenerationId::from_canonical_bytes(b"old-base")),
            1_000_000,
        )?;
        assert_ne!(fresh.transfer_work_id(), incremental.transfer_work_id());
        assert_eq!(
            CompilerWorkIdentity::new(
                package,
                target,
                recipe,
                VerifiedCompilerInput::ProvenReadSetIncremental(read_set),
                None,
                1_000_000,
            ),
            Err(CompilerIdentityError::IncrementalRequiresBase)
        );
        Ok(())
    }

    fn peer(value: u8) -> Result<CompilerPeerId, CompilerRemoteEvidenceError> {
        let mut bytes = [0; 32];
        bytes[0] = value;
        CompilerPeerId::new(bytes)
    }

    fn probe_binding(
        identity: CompilerWorkIdentity,
        peer: CompilerPeerId,
        now: u64,
        attempt: u64,
        fence: u64,
    ) -> Result<CompilerRemoteProbeBinding, Box<dyn std::error::Error>> {
        let token = token(attempt, fence)?;
        let mut nonce_hasher = blake3::Hasher::new();
        nonce_hasher.update(b"compiler-execution-probe-fixture.v1\0");
        nonce_hasher.update(&identity.transfer_work_id());
        nonce_hasher.update(&peer.as_bytes());
        nonce_hasher.update(&attempt.to_be_bytes());
        nonce_hasher.update(&token.fence().as_bytes());
        let mut nonce = [0; 16];
        nonce.copy_from_slice(&nonce_hasher.finalize().as_bytes()[..16]);
        CompilerRemoteProbeBinding::new(
            [0xA1; 16],
            identity.transfer_work_id(),
            attempt,
            token.fence().as_bytes(),
            nonce,
            now,
            now + 100,
        )
        .map_err(Into::into)
    }

    fn remote_with_binding(
        identity: CompilerWorkIdentity,
        peer: CompilerPeerId,
        completion: CompletionCost,
        binding: CompilerRemoteProbeBinding,
        worker_incarnation: [u8; 32],
    ) -> Result<VerifiedRemoteCompiler, CompilerRemotePreflightError> {
        let full_workspace = identity
            .input()
            .full_workspace()
            .expect("test compiler inputs are full-workspace captures")
            .claim();
        let claim = RemoteCompilerPreflightClaim {
            peer,
            work: identity,
            binding,
            worker_incarnation,
            capability: RemoteCompilerCapabilityClaim {
                target: identity.target(),
                recipe: identity.recipe(),
                supported: true,
                capability_digest: [2; 32],
            },
            have: RemoteHaveClaim {
                input_manifest: identity.input_identity().manifest,
                input_closure_id: full_workspace.input_closure_id,
                manifest_object_id: full_workspace.manifest_object_id,
                inventory_digest: [3; 32],
                required_objects: 3,
                verified_have_objects: 2,
                owner_streamable_objects: 1,
                missing_bytes: 64,
                proof_digest: [1; 32],
            },
            cost: RemoteCompilerCostClaim {
                completion,
                confidence_per_mille: 900,
                observed_at: binding.observed_at(),
                expires_at: binding.expires_at(),
            },
        };
        VerifiedRemoteCompiler::admit_preflight(identity, claim, &AcceptRemoteEvidence)
    }

    fn remote(
        identity: CompilerWorkIdentity,
        peer: CompilerPeerId,
        completion: CompletionCost,
        now: u64,
    ) -> Result<VerifiedRemoteCompiler, Box<dyn std::error::Error>> {
        let binding = probe_binding(identity, peer, now, 1, 1)?;
        let worker_incarnation = [peer.as_bytes()[0]; 32];
        Ok(remote_with_binding(
            identity,
            peer,
            completion,
            binding,
            worker_incarnation,
        )?)
    }

    fn credits(
        cpu_millicores: u32,
        memory_bytes: u64,
        transfer_bytes: u64,
    ) -> CompilerResourceCredits {
        CompilerResourceCredits {
            cpu: CompilerCpuCredits::new(cpu_millicores),
            memory: CompilerMemoryCredits::new(memory_bytes),
            transfer: CompilerByteCredits::new(transfer_bytes),
        }
    }

    fn node_capacity(
        peer: CompilerPeerId,
        incarnation: [u8; 32],
        binding: CompilerRemoteProbeBinding,
        busy: CompilerResourceCredits,
        warm_sessions: Vec<CompilerSessionAffinity>,
    ) -> Result<VerifiedCompilerNodeCapacity, CompilerNodeCapacityError> {
        VerifiedCompilerNodeCapacity::admit(
            CompilerNodeCapacityClaim {
                peer,
                binding,
                worker_incarnation: incarnation,
                capacity_revision: [0xB2; 16],
                total: credits(4_000, 8 * 1024 * 1024 * 1024, 32 * 1024 * 1024),
                busy,
                warm_sessions,
                observed_at: binding.observed_at(),
                expires_at: binding.expires_at(),
            },
            &AcceptCapacity,
        )
    }

    fn balanced_remote(
        identity: CompilerWorkIdentity,
        peer: CompilerPeerId,
        incarnation: [u8; 32],
        now: u64,
        busy: CompilerResourceCredits,
        warm_sessions: Vec<CompilerSessionAffinity>,
    ) -> Result<CompilerBalancedRemote, Box<dyn std::error::Error>> {
        balanced_remote_for_token(identity, peer, incarnation, now, busy, warm_sessions, 1, 1)
    }

    fn balanced_remote_for_token(
        identity: CompilerWorkIdentity,
        peer: CompilerPeerId,
        incarnation: [u8; 32],
        now: u64,
        busy: CompilerResourceCredits,
        warm_sessions: Vec<CompilerSessionAffinity>,
        attempt: u64,
        fence: u64,
    ) -> Result<CompilerBalancedRemote, Box<dyn std::error::Error>> {
        let binding = probe_binding(identity, peer, now, attempt, fence)?;
        let work = remote_with_binding(
            identity,
            peer,
            CompletionCost {
                input_transfer: 10,
                warmup: 300,
                execution: 100,
                ..CompletionCost::default()
            },
            binding,
            incarnation,
        )?;
        let node = node_capacity(peer, incarnation, binding, busy, warm_sessions)?;
        Ok(CompilerBalancedRemote::new(work, node)?)
    }

    fn balancing_request(
        identity: CompilerWorkIdentity,
        demand: CompilerDemand,
        submitted_at: u64,
        local_start_delay: u64,
        session_affinity: Option<CompilerSessionAffinity>,
    ) -> CompilerBalancingRequest {
        CompilerBalancingRequest {
            work: identity,
            demand,
            local: LocalCompilerAvailability::Ready,
            local_cost: CompletionCost {
                execution: 10_000,
                ..CompletionCost::default()
            },
            local_start_delay,
            submitted_at,
            local_first_budget: 100,
            resources: credits(1_000, 1024 * 1024 * 1024, 2 * 1024 * 1024),
            session_affinity,
            deadline_at: None,
        }
    }

    fn token(
        attempt: u64,
        fence: u64,
    ) -> Result<CompilerAttemptToken, backend_replication::ReplicationError> {
        Ok(CompilerAttemptToken::new(
            AttemptId::new(attempt)?,
            Fence::from_u64(fence)?,
        ))
    }

    #[test]
    fn balanced_policy_uses_warm_sessions_and_local_first_latency_budget() -> TestResult {
        let identity = work()?;
        let cold_peer = peer(1)?;
        let warm_peer = peer(2)?;
        let affinity = CompilerSessionAffinity::new([0x91; 32])?;
        let cold = balanced_remote(
            identity,
            cold_peer,
            [0x11; 32],
            150,
            CompilerResourceCredits::default(),
            Vec::new(),
        )?;
        let warm = balanced_remote(
            identity,
            warm_peer,
            [0x22; 32],
            150,
            credits(2_500, 0, 0),
            vec![affinity],
        )?;
        let candidates = [cold, warm];
        let mut request =
            balancing_request(identity, CompilerDemand::Background, 0, 0, Some(affinity));
        assert_eq!(
            CompilerPlacementPolicy.choose_balanced(request, true, 160, &candidates),
            CompilerPlacement::Remote(warm_peer)
        );

        request.demand = CompilerDemand::Interactive;
        request.local_start_delay = 10;
        assert_eq!(
            CompilerPlacementPolicy.choose_balanced(request, true, 20, &candidates),
            CompilerPlacement::Local
        );
        assert_eq!(
            CompilerPlacementPolicy.choose_balanced(request, true, 150, &candidates),
            CompilerPlacement::Remote(warm_peer)
        );
        Ok(())
    }

    #[test]
    fn balanced_remote_rejects_capacity_from_another_worker_incarnation() -> TestResult {
        let identity = work()?;
        let peer = peer(9)?;
        let binding = probe_binding(identity, peer, 5, 1, 1)?;
        let preflight = remote_with_binding(
            identity,
            peer,
            CompletionCost::default(),
            binding,
            [0x91; 32],
        )?;
        let capacity = node_capacity(
            peer,
            [0x92; 32],
            binding,
            CompilerResourceCredits::default(),
            Vec::new(),
        )?;
        assert_eq!(
            CompilerBalancedRemote::new(preflight, capacity),
            Err(CompilerNodeCapacityError::ProbeMismatch)
        );
        Ok(())
    }

    #[test]
    fn balanced_assignment_reserves_typed_credits_and_preserves_local_slot() -> TestResult {
        let peer = peer(3)?;
        let incarnation = [0x31; 32];
        let local_budget = credits(1_000, 1024 * 1024 * 1024, 2 * 1024 * 1024);
        let scheduler = CompilerClusterScheduler::with_balancing_limits(
            NonZeroUsize::new(1).ok_or("zero")?,
            NonZeroUsize::new(1).ok_or("zero")?,
            local_budget,
            0,
        );
        scheduler.observe_peer_incarnation(peer, incarnation, 5)?;

        let first_work = work()?;
        let first_remote = balanced_remote(
            first_work,
            peer,
            incarnation,
            5,
            CompilerResourceCredits::default(),
            Vec::new(),
        )?;
        let first = scheduler.place_balanced_and_assign(
            &CompilerPlacementPolicy,
            balancing_request(first_work, CompilerDemand::Background, 0, 0, None),
            10,
            &[first_remote],
            token(1, 1)?,
        )?;
        let CompilerAssignmentOutcome::Assigned(first) = first else {
            return Err("first balanced job was offline".into());
        };
        assert_eq!(first.route(), CompilerAssignmentRoute::Remote(peer));
        assert_eq!(scheduler.available_slots(), 0);

        let second_work = work_with_branch(b"branch:second-balanced")?;
        let second_remote = balanced_remote_for_token(
            second_work,
            peer,
            incarnation,
            5,
            CompilerResourceCredits::default(),
            Vec::new(),
            2,
            2,
        )?;
        let second = scheduler.place_balanced_and_assign(
            &CompilerPlacementPolicy,
            balancing_request(second_work, CompilerDemand::Background, 0, 0, None),
            10,
            &[second_remote],
            token(2, 2)?,
        )?;
        let CompilerAssignmentOutcome::Assigned(second) = second else {
            return Err("local slot was lost to remote capacity".into());
        };
        assert_eq!(second.route(), CompilerAssignmentRoute::Local);
        assert_eq!(scheduler.available_local_slots(), 0);

        let third_work = work_with_branch(b"branch:third-balanced")?;
        assert_eq!(
            scheduler.place_balanced_and_assign(
                &CompilerPlacementPolicy,
                balancing_request(third_work, CompilerDemand::Background, 0, 0, None),
                10,
                &[],
                token(3, 3)?,
            )?,
            CompilerAssignmentOutcome::OfflineUnavailable
        );
        scheduler.cancel(first)?;
        scheduler.complete_local(second)?;
        assert_eq!(scheduler.live_assignments(), 0);
        Ok(())
    }

    #[test]
    fn node_resource_credits_never_oversubscribe_under_deterministic_admission_model() -> TestResult
    {
        let first_peer = peer(4)?;
        let second_peer = peer(5)?;
        let first_incarnation = [0x41; 32];
        let second_incarnation = [0x51; 32];
        let scheduler = CompilerClusterScheduler::with_balancing_limits(
            NonZeroUsize::new(16).ok_or("zero")?,
            NonZeroUsize::new(1).ok_or("zero")?,
            credits(1_000, 1024 * 1024 * 1024, 2 * 1024 * 1024),
            0,
        );
        scheduler.observe_peer_incarnation(first_peer, first_incarnation, 5)?;
        scheduler.observe_peer_incarnation(second_peer, second_incarnation, 5)?;

        let mut accepted = Vec::new();
        let mut per_peer = BTreeMap::new();
        for index in 0..10_u64 {
            let identity = work_with_branch(format!("branch:model-{index}").as_bytes())?;
            let first = balanced_remote_for_token(
                identity,
                first_peer,
                first_incarnation,
                5,
                CompilerResourceCredits::default(),
                Vec::new(),
                index + 1,
                index + 1,
            )?;
            let second = balanced_remote_for_token(
                identity,
                second_peer,
                second_incarnation,
                5,
                CompilerResourceCredits::default(),
                Vec::new(),
                index + 1,
                index + 1,
            )?;
            let mut request = balancing_request(identity, CompilerDemand::Background, 0, 0, None);
            request.local = LocalCompilerAvailability::Unavailable;
            let outcome = scheduler.place_balanced_and_assign(
                &CompilerPlacementPolicy,
                request,
                10,
                &[first, second],
                token(index + 1, index + 1)?,
            )?;
            if let CompilerAssignmentOutcome::Assigned(assignment) = outcome {
                let CompilerAssignmentRoute::Remote(assigned_peer) = assignment.route() else {
                    return Err("unavailable local work was assigned locally".into());
                };
                *per_peer.entry(assigned_peer).or_insert(0_usize) += 1;
                accepted.push(assignment);
            }
            assert!(scheduler.available_slots() <= 16);
            assert!(per_peer.values().all(|count| *count <= 4));
        }
        assert_eq!(accepted.len(), 8);
        assert_eq!(per_peer.get(&first_peer), Some(&4));
        assert_eq!(per_peer.get(&second_peer), Some(&4));
        for assignment in accepted {
            scheduler.cancel(assignment)?;
        }
        assert_eq!(scheduler.available_slots(), 16);
        Ok(())
    }

    #[test]
    fn failed_steal_preserves_age_without_starving_younger_work() -> TestResult {
        let peer = peer(6)?;
        let incarnation = [0x61; 32];
        let queue = CompilerWorkQueue::new(NonZeroUsize::new(4).ok_or("zero")?);
        let oldest = balancing_request(work()?, CompilerDemand::Background, 0, 0, None);
        let younger = balancing_request(
            work_with_branch(b"branch:younger")?,
            CompilerDemand::Background,
            1,
            0,
            None,
        );
        queue.enqueue(oldest)?;
        queue.enqueue(younger)?;
        let remotes = [
            balanced_remote(
                oldest.work,
                peer,
                incarnation,
                0,
                CompilerResourceCredits::default(),
                Vec::new(),
            )?,
            balanced_remote(
                younger.work,
                peer,
                incarnation,
                0,
                CompilerResourceCredits::default(),
                Vec::new(),
            )?,
        ];
        let peer_limit = NonZeroUsize::new(1).ok_or("zero")?;
        let failed = queue
            .claim_oldest_stealable(peer, &remotes, 10, 5, 50, peer_limit)?
            .ok_or("oldest work was not claimed")?;
        assert_eq!(failed.request().work, oldest.work);
        drop(failed);

        let next = queue
            .claim_oldest_stealable(peer, &remotes, 60, 5, 50, peer_limit)?
            .ok_or("failed steal starved the younger request in the next bounded window")?;
        assert_eq!(next.request().work, younger.work);
        queue.commit_steal(next)?;
        assert_eq!(queue.len(), 1);

        let refreshed_oldest = balanced_remote_for_token(
            oldest.work,
            peer,
            incarnation,
            110,
            CompilerResourceCredits::default(),
            Vec::new(),
            2,
            2,
        )?;
        let retry = queue
            .claim_oldest_stealable(peer, &[refreshed_oldest], 110, 5, 50, peer_limit)?
            .ok_or("old request did not retain its age after retry window")?;
        assert_eq!(retry.request().work, oldest.work);
        queue.requeue_steal(retry)?;
        assert_eq!(queue.len(), 1);
        Ok(())
    }

    #[test]
    fn cold_restart_restores_backoff_but_invalidates_old_attempts() -> TestResult {
        let peer = peer(7)?;
        let incarnation = [0x71; 32];
        let policy = CompilerBackoffPolicy::new(100, 100)?;
        let scheduler = CompilerClusterScheduler::with_balancing_limits(
            NonZeroUsize::new(2).ok_or("zero")?,
            NonZeroUsize::new(1).ok_or("zero")?,
            credits(1_000, 1024 * 1024 * 1024, 2 * 1024 * 1024),
            0,
        );
        scheduler.observe_peer_incarnation(peer, incarnation, 5)?;
        let old_work = work()?;
        let old_remote = balanced_remote(
            old_work,
            peer,
            incarnation,
            5,
            CompilerResourceCredits::default(),
            Vec::new(),
        )?;
        let CompilerAssignmentOutcome::Assigned(old_assignment) = scheduler
            .place_balanced_and_assign(
                &CompilerPlacementPolicy,
                balancing_request(old_work, CompilerDemand::Background, 0, 0, None),
                10,
                &[old_remote],
                token(1, 1)?,
            )?
        else {
            return Err("initial remote attempt was offline".into());
        };
        assert_eq!(
            old_assignment.route(),
            CompilerAssignmentRoute::Remote(peer)
        );
        let failure = scheduler.record_peer_failure(peer, incarnation, 20, policy)?;
        assert!(failure.retry_at > 25);

        let restarted_owner = CompilerClusterScheduler::with_balancing_limits(
            NonZeroUsize::new(2).ok_or("zero")?,
            NonZeroUsize::new(1).ok_or("zero")?,
            credits(1_000, 1024 * 1024 * 1024, 2 * 1024 * 1024),
            0,
        );
        restarted_owner.restore_retry_snapshot(scheduler.retry_snapshot())?;
        assert_eq!(
            restarted_owner.validate_assignment(old_assignment),
            Err(CompilerAssignmentError::StaleAttempt)
        );

        let retry_work = work_with_branch(b"branch:backoff")?;
        let retry_remote = balanced_remote_for_token(
            retry_work,
            peer,
            incarnation,
            21,
            CompilerResourceCredits::default(),
            Vec::new(),
            2,
            2,
        )?;
        let CompilerAssignmentOutcome::Assigned(local_fallback) = restarted_owner
            .place_balanced_and_assign(
                &CompilerPlacementPolicy,
                balancing_request(retry_work, CompilerDemand::Background, 20, 0, None),
                25,
                &[retry_remote],
                token(2, 2)?,
            )?
        else {
            return Err("local fallback was not admitted during peer backoff".into());
        };
        assert_eq!(local_fallback.route(), CompilerAssignmentRoute::Local);
        restarted_owner.complete_local(local_fallback)?;

        let new_incarnation = [0x72; 32];
        let restart = restarted_owner.observe_peer_incarnation(peer, new_incarnation, 30)?;
        assert_eq!(restart.previous, Some(incarnation));
        assert!(restarted_owner.retry_snapshot().peers.is_empty());
        let next_work = work_with_branch(b"branch:restarted-worker")?;
        let next_remote = balanced_remote_for_token(
            next_work,
            peer,
            new_incarnation,
            31,
            CompilerResourceCredits::default(),
            Vec::new(),
            3,
            3,
        )?;
        let CompilerAssignmentOutcome::Assigned(new_assignment) = restarted_owner
            .place_balanced_and_assign(
                &CompilerPlacementPolicy,
                balancing_request(next_work, CompilerDemand::Background, 30, 0, None),
                32,
                &[next_remote],
                token(3, 3)?,
            )?
        else {
            return Err("restarted worker did not receive eligible work".into());
        };
        assert_eq!(
            new_assignment.route(),
            CompilerAssignmentRoute::Remote(peer)
        );
        assert_eq!(
            restarted_owner.validate_remote_completion(
                old_assignment,
                RemoteCompilerCompletionClaim {
                    work: old_work,
                    token: old_assignment.token(),
                    peer,
                    closure: ArtifactClosureClaim::from_bytes([0x73; 32]),
                },
            ),
            Err(CompilerAssignmentError::StaleAttempt)
        );
        Ok(())
    }

    #[test]
    fn hedged_branches_accept_out_of_order_evidence_but_reject_retired_attempts() -> TestResult {
        let peer = peer(8)?;
        let incarnation = [0x81; 32];
        let identity = work()?;
        let candidate = balanced_remote_for_token(
            identity,
            peer,
            incarnation,
            5,
            CompilerResourceCredits::default(),
            Vec::new(),
            2,
            0x202,
        )?;
        let scheduler = CompilerClusterScheduler::with_balancing_limits(
            NonZeroUsize::new(1).ok_or("zero")?,
            NonZeroUsize::new(1).ok_or("zero")?,
            credits(2_000, 2 * 1024 * 1024 * 1024, 4 * 1024 * 1024),
            2,
        );
        scheduler.observe_peer_incarnation(peer, incarnation, 5)?;
        let wrong_candidate = balanced_remote_for_token(
            identity,
            peer,
            incarnation,
            5,
            CompilerResourceCredits::default(),
            Vec::new(),
            3,
            0x303,
        )?;
        assert_eq!(
            scheduler.assign_hedged(
                balancing_request(identity, CompilerDemand::Background, 0, 0, None),
                &wrong_candidate,
                token(1, 0x101)?,
                token(2, 0x202)?,
                true,
                NonZeroUsize::new(2).ok_or("zero")?,
                10,
            ),
            Err(CompilerAssignmentError::CompletionMismatch)
        );
        let branches = scheduler.assign_hedged(
            balancing_request(identity, CompilerDemand::Background, 0, 0, None),
            &candidate,
            token(1, 0x101)?,
            token(2, 0x202)?,
            true,
            NonZeroUsize::new(2).ok_or("zero")?,
            10,
        )?;
        assert_eq!(scheduler.live_assignments(), 2);
        scheduler.complete_local(branches.local)?;
        assert!(scheduler.validate_assignment(branches.remote).is_ok());
        let late_remote = RemoteCompilerCompletionClaim {
            work: identity,
            token: branches.remote.token(),
            peer,
            closure: ArtifactClosureClaim::from_bytes([0x82; 32]),
        };
        scheduler.validate_remote_completion(branches.remote, late_remote)?;

        // The authority chooses the local candidate and retires the remote branch. The
        // scheduler never infers a winner from arrival order or completion cost.
        scheduler.cancel(branches.remote)?;
        assert_eq!(
            scheduler.validate_remote_completion(branches.remote, late_remote),
            Err(CompilerAssignmentError::StaleAttempt)
        );
        assert_eq!(scheduler.live_assignments(), 0);
        Ok(())
    }

    #[test]
    fn package_lineage_frames_authority_coordinate_and_branch() {
        let first = PackageLineageId::from_canonical_parts(b"registry:a", b"pkg", b"main")
            .expect("valid package lineage");
        let other_source = PackageLineageId::from_canonical_parts(b"registry:b", b"pkg", b"main")
            .expect("valid package lineage");
        let other_branch = PackageLineageId::from_canonical_parts(b"registry:a", b"pkg", b"local")
            .expect("valid package lineage");
        assert_ne!(first, other_source);
        assert_ne!(first, other_branch);
        assert_eq!(PackageLineageId::from_bytes(first.as_bytes()), Ok(first));
        assert_eq!(
            PackageLineageId::from_bytes([0; 32]),
            Err(CompilerIdentityError::ZeroPackageLineage)
        );
        assert_eq!(
            PackageLineageId::from_canonical_parts(b"", b"pkg", b"main"),
            Err(CompilerIdentityError::EmptyPackageLineageComponent(
                PackageLineageComponent::SourceAuthority
            ))
        );
    }

    #[test]
    fn interactive_local_precedence_ignores_a_cheaper_remote_peer() -> TestResult {
        let work = work()?;
        let peer = peer(1)?;
        let remote = remote(
            work,
            peer,
            CompletionCost {
                execution: 1,
                ..CompletionCost::default()
            },
            5,
        )?;
        let selected = CompilerPlacementPolicy.choose(
            work,
            CompilerDemand::Interactive,
            LocalCompilerAvailability::Ready,
            50_000,
            5,
            &[remote],
        );
        assert_eq!(selected, CompilerPlacement::Local);
        Ok(())
    }

    #[test]
    fn background_remote_requires_complete_verified_have_and_lower_cost() -> TestResult {
        let work = work()?;
        let peer = peer(1)?;
        let remote = remote(
            work,
            peer,
            CompletionCost {
                input_transfer: 10,
                execution: 20,
                ..CompletionCost::default()
            },
            5,
        )?;
        assert_eq!(
            CompilerPlacementPolicy.choose(
                work,
                CompilerDemand::Background,
                LocalCompilerAvailability::Ready,
                100,
                5,
                &[remote],
            ),
            CompilerPlacement::Remote(peer)
        );
        assert_eq!(
            CompilerPlacementPolicy.choose(
                work,
                CompilerDemand::Background,
                LocalCompilerAvailability::Ready,
                20,
                5,
                &[remote],
            ),
            CompilerPlacement::Local
        );
        assert_eq!(
            CompilerPlacementPolicy.choose(
                work,
                CompilerDemand::Background,
                LocalCompilerAvailability::Ready,
                100,
                105,
                &[remote],
            ),
            CompilerPlacement::Local
        );
        Ok(())
    }

    #[test]
    fn preflight_requires_complete_input_have_but_no_result_coverage() -> TestResult {
        let work = work()?;
        let peer = peer(1)?;
        let manifest = work.input_identity().manifest;
        let full_workspace = work
            .input()
            .full_workspace()
            .ok_or("missing full workspace")?;
        let input_claim = full_workspace.claim();
        let binding = probe_binding(work, peer, 1, 1, 1)?;
        let mut claim = RemoteCompilerPreflightClaim {
            peer,
            work,
            binding,
            worker_incarnation: [1; 32],
            capability: RemoteCompilerCapabilityClaim {
                target: work.target(),
                recipe: work.recipe(),
                supported: true,
                capability_digest: [2; 32],
            },
            have: RemoteHaveClaim {
                input_manifest: manifest,
                input_closure_id: input_claim.input_closure_id,
                manifest_object_id: input_claim.manifest_object_id,
                inventory_digest: [3; 32],
                required_objects: 3,
                verified_have_objects: 1,
                owner_streamable_objects: 1,
                missing_bytes: 64,
                proof_digest: [1; 32],
            },
            cost: RemoteCompilerCostClaim {
                completion: CompletionCost {
                    execution: 1,
                    ..CompletionCost::default()
                },
                confidence_per_mille: 800,
                observed_at: binding.observed_at(),
                expires_at: binding.expires_at(),
            },
        };
        assert_eq!(
            VerifiedRemoteCompiler::admit_preflight(work, claim, &AcceptRemoteEvidence),
            Err(CompilerRemoteEvidenceError::IncompleteHave)
        );
        claim.have.owner_streamable_objects = 2;
        assert!(
            VerifiedRemoteCompiler::admit_preflight(work, claim, &AcceptRemoteEvidence).is_ok(),
            "complete preflight is eligible before any output coverage exists"
        );
        claim.capability.supported = false;
        assert_eq!(
            VerifiedRemoteCompiler::admit_preflight(work, claim, &AcceptRemoteEvidence),
            Err(CompilerRemoteEvidenceError::UnsupportedCapability)
        );
        Ok(())
    }

    #[test]
    fn remote_assignment_capacity_falls_back_local_without_blocking_interactive_work() -> TestResult
    {
        let work = work()?;
        let remote_peer = peer(1)?;
        let evidence = remote(
            work,
            remote_peer,
            CompletionCost {
                execution: 1,
                ..CompletionCost::default()
            },
            5,
        )?;
        let scheduler = CompilerClusterScheduler::with_capacities(
            NonZeroUsize::new(1).ok_or("zero")?,
            NonZeroUsize::new(2).ok_or("zero")?,
        );
        assert_eq!(
            scheduler.place_and_assign(
                &CompilerPlacementPolicy,
                work,
                CompilerDemand::Background,
                LocalCompilerAvailability::Unavailable,
                100,
                5,
                &[evidence],
                token(2, 2)?,
            )?,
            CompilerAssignmentOutcome::OfflineUnavailable,
            "preflight from an older attempt cannot place a newer token"
        );
        let first = scheduler.place_and_assign(
            &CompilerPlacementPolicy,
            work,
            CompilerDemand::Background,
            LocalCompilerAvailability::Ready,
            100,
            5,
            &[evidence],
            token(1, 1)?,
        )?;
        let CompilerAssignmentOutcome::Assigned(first) = first else {
            return Err("first assignment was offline".into());
        };
        assert_eq!(first.route(), CompilerAssignmentRoute::Remote(remote_peer));
        assert_eq!(scheduler.live_assignments(), 1);

        let interactive = scheduler.place_and_assign(
            &CompilerPlacementPolicy,
            work,
            CompilerDemand::Interactive,
            LocalCompilerAvailability::Ready,
            100,
            5,
            &[evidence],
            token(2, 2)?,
        );
        assert_eq!(interactive, Err(CompilerAssignmentError::AlreadyAssigned));
        let second_work = work_with_branch(b"branch:other")?;
        let local = scheduler.place_and_assign(
            &CompilerPlacementPolicy,
            second_work,
            CompilerDemand::Interactive,
            LocalCompilerAvailability::Ready,
            100,
            5,
            &[],
            token(3, 3)?,
        )?;
        let CompilerAssignmentOutcome::Assigned(local) = local else {
            return Err("interactive local assignment was offline".into());
        };
        assert_eq!(local.route(), CompilerAssignmentRoute::Local);
        assert_eq!(scheduler.live_assignments(), 2);

        let third_work = work_with_branch(b"branch:third")?;
        let third_evidence = remote(
            third_work,
            remote_peer,
            CompletionCost {
                execution: 1,
                ..CompletionCost::default()
            },
            5,
        )?;
        let fallback = scheduler.place_and_assign(
            &CompilerPlacementPolicy,
            third_work,
            CompilerDemand::Background,
            LocalCompilerAvailability::Ready,
            100,
            5,
            &[third_evidence],
            token(4, 4)?,
        )?;
        let CompilerAssignmentOutcome::Assigned(fallback) = fallback else {
            return Err("remote capacity fallback was offline".into());
        };
        assert_eq!(fallback.route(), CompilerAssignmentRoute::Local);
        assert_eq!(scheduler.available_slots(), 0);
        assert_eq!(scheduler.available_local_slots(), 0);

        let fourth_work = work_with_branch(b"branch:fourth")?;
        assert_eq!(
            scheduler.place_and_assign(
                &CompilerPlacementPolicy,
                fourth_work,
                CompilerDemand::Interactive,
                LocalCompilerAvailability::Ready,
                100,
                5,
                &[],
                token(5, 5)?,
            )?,
            CompilerAssignmentOutcome::OfflineUnavailable
        );
        Ok(())
    }

    #[test]
    fn cancelled_or_superseded_remote_receipt_is_fenced_before_cas_admission() -> TestResult {
        let work = work()?;
        let peer = peer(1)?;
        let evidence = remote(
            work,
            peer,
            CompletionCost {
                execution: 1,
                ..CompletionCost::default()
            },
            5,
        )?;
        let scheduler = CompilerClusterScheduler::new(NonZeroUsize::new(2).ok_or("zero")?);
        let assignment = scheduler.place_and_assign(
            &CompilerPlacementPolicy,
            work,
            CompilerDemand::Background,
            LocalCompilerAvailability::Ready,
            100,
            5,
            &[evidence],
            token(1, 1)?,
        )?;
        let CompilerAssignmentOutcome::Assigned(remote) = assignment else {
            return Err("remote assignment was offline".into());
        };
        let claim = RemoteCompilerCompletionClaim {
            work,
            token: remote.token(),
            peer,
            closure: ArtifactClosureClaim::from_bytes([9; 32]),
        };
        let fallback = scheduler.fallback_to_local(remote, token(2, 2)?)?;
        assert_eq!(fallback.route(), CompilerAssignmentRoute::Local);
        assert_eq!(
            scheduler.validate_assignment(remote),
            Err(CompilerAssignmentError::StaleAttempt)
        );
        scheduler.validate_assignment(fallback)?;
        assert_eq!(
            scheduler.validate_remote_completion(remote, claim),
            Err(CompilerAssignmentError::StaleAttempt)
        );
        assert_eq!(scheduler.live_assignments(), 1);
        scheduler.complete_local(fallback)?;
        assert_eq!(
            scheduler.validate_assignment(fallback),
            Err(CompilerAssignmentError::StaleAttempt)
        );
        assert_eq!(scheduler.live_assignments(), 0);
        Ok(())
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;
}

impl Drop for CompilerAssignmentReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.reserved.get(&self.work_id) == Some(&self.reservation_id) {
            state.reserved.remove(&self.work_id);
        }
    }
}
