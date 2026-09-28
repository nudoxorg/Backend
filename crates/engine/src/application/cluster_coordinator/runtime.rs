//! Typed input authority and local-first placement boundary for cluster dispatch.

use crate::compiler_input_capture_v2::CapturedFullWorkspaceV2;
use crate::compiler_input_manifest_v2::CompilerInputManifestV2;
use backend_execution::{
    CompilerAssignment, CompilerAssignmentError, CompilerAssignmentOutcome, CompilerAttemptToken,
    CompilerBalancedRemote, CompilerBalancingRequest, CompilerClusterScheduler, CompilerPeerId,
    CompilerPlacementPolicy, VerifiedCompilerInput,
};
use backend_store::ClosureId;

/// Facts handed to the source authority when it verifies the current owner fence.
///
/// The full-workspace tree itself is represented by the scheduler's opaque
/// `VerifiedFullWorkspaceInput` embedded in the assignment. These additional facts bind that
/// proof to the live source observation and the owner-configured worker trust policy.
#[derive(Clone, Debug)]
pub struct CompilerInputAdmissionEvidence {
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    capture: CapturedFullWorkspaceV2,
    source_observation_revision: [u8; 32],
    source_fence_digest: [u8; 32],
}

impl CompilerInputAdmissionEvidence {
    /// Binds one owner-minted assignment to its complete captured-workspace manifest and current
    /// source observation. A caller cannot create a full-workspace proof from a file list: the
    /// assignment must already contain scheduler's opaque `FullWorkspaceFresh` proof.
    pub fn new(
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        capture: CapturedFullWorkspaceV2,
        source_observation_revision: [u8; 32],
        source_fence_digest: [u8; 32],
    ) -> Result<Self, CompilerInputAdmissionError> {
        if namespace_id == [0; 16]
            || source_observation_revision == [0; 32]
            || source_fence_digest == [0; 32]
        {
            return Err(CompilerInputAdmissionError::EmptyIdentity);
        }
        let manifest = capture.manifest();
        manifest
            .encode()
            .map_err(|_| CompilerInputAdmissionError::ManifestInvalid)?;
        let work = assignment.work();
        let full_workspace = work
            .input()
            .full_workspace()
            .ok_or(CompilerInputAdmissionError::FreshOnly)?;
        let identity = manifest
            .identity_claim()
            .map_err(|_| CompilerInputAdmissionError::ManifestInvalid)?;
        let full_workspace_claim = full_workspace.claim();
        if work.selected_base().is_some()
            || identity != work.input_identity()
            || manifest.max_output_bytes() != work.max_output_bytes()
            || manifest.source_fence_digest() != source_fence_digest
            || full_workspace_claim.input_closure_id != *capture.closure().as_bytes()
            || full_workspace_claim.manifest_object_id != *capture.manifest_object_id().as_bytes()
        {
            return Err(CompilerInputAdmissionError::WorkMismatch);
        }
        Ok(Self {
            assignment,
            namespace_id,
            capture,
            source_observation_revision,
            source_fence_digest,
        })
    }

    /// Exact scheduled attempt.
    #[must_use]
    pub const fn assignment(&self) -> CompilerAssignment {
        self.assignment
    }

    /// Authority namespace bound to the Iroh assignment scope.
    #[must_use]
    pub const fn namespace_id(&self) -> [u8; 16] {
        self.namespace_id
    }

    /// Exact immutable input closure.
    #[must_use]
    pub const fn input_closure(&self) -> ClosureId {
        self.capture.closure()
    }

    /// Opaque, locally captured full workspace and its canonical V2 manifest.
    #[must_use]
    pub const fn capture(&self) -> &CapturedFullWorkspaceV2 {
        &self.capture
    }

    /// Canonical compiler invocation and workspace manifest.
    #[must_use]
    pub const fn manifest(&self) -> &CompilerInputManifestV2 {
        self.capture.manifest()
    }

    /// Source-observation revision the owner must still hold at selection time.
    #[must_use]
    pub const fn source_observation_revision(&self) -> [u8; 32] {
        self.source_observation_revision
    }

    /// Owner fence digest for revalidation before candidate selection.
    #[must_use]
    pub const fn source_fence_digest(&self) -> [u8; 32] {
        self.source_fence_digest
    }
}

/// Owner-side source-observation and trusted-peer checks beyond scheduler's captured-input proof.
/// Implementations compare the source observation/fence with the live owner and confirm an
/// explicit trusted-execution grant for the exact peer and invocation.
pub trait CompilerInputAdmissionVerifier {
    /// Rechecks that the exact source observation and fence remain current for dispatch.
    fn verify_source_observation(
        &self,
        evidence: &CompilerInputAdmissionEvidence,
    ) -> Result<(), CompilerInputAdmissionError>;

    /// Confirms explicit trust for this exact peer, namespace, recipe, toolchain, environment and
    /// target platform. This must be owner configuration, not worker or storage metadata.
    fn authorize_trusted_worker(
        &self,
        peer: CompilerPeerId,
        namespace_id: [u8; 16],
        manifest: &CompilerInputManifestV2,
    ) -> bool;
}

/// Explicit owner grant for one worker and one exact host-execution identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerTrustedExecutionGrant {
    peer: CompilerPeerId,
    namespace_id: [u8; 16],
    package_lineage: [u8; 32],
    target: [u8; 32],
    recipe: [u8; 32],
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
}

impl CompilerTrustedExecutionGrant {
    /// Exact authenticated worker endpoint covered by the grant.
    #[must_use]
    pub const fn peer(self) -> CompilerPeerId {
        self.peer
    }

    /// Exact authority namespace covered by the grant.
    #[must_use]
    pub const fn namespace_id(self) -> [u8; 16] {
        self.namespace_id
    }

    /// Exact package lineage, target and recipe covered by the grant.
    #[must_use]
    pub const fn work_facts(self) -> ([u8; 32], [u8; 32], [u8; 32]) {
        (self.package_lineage, self.target, self.recipe)
    }

    /// Exact executable, compiler environment and target platform covered by the grant.
    #[must_use]
    pub const fn execution_facts(self) -> ([u8; 32], [u8; 32], [u8; 32]) {
        (self.toolchain, self.environment, self.target_platform)
    }
}

/// Captured input evidence after source-authority and remote-execution policy admission.
/// Its fields are private so callers cannot turn an indexed-file list into a workspace proof.
#[derive(Clone, Debug)]
pub struct VerifiedCompilerInputAdmission {
    evidence: CompilerInputAdmissionEvidence,
    execution_grant: CompilerTrustedExecutionGrant,
}

impl VerifiedCompilerInputAdmission {
    /// Admits only a fresh full-workspace assignment under the owner's current source fence and
    /// trusted peer policy. Incremental read-set assignments are not dispatched by this path.
    pub fn admit(
        evidence: CompilerInputAdmissionEvidence,
        verifier: &impl CompilerInputAdmissionVerifier,
    ) -> Result<Self, CompilerInputAdmissionError> {
        if !matches!(
            evidence.assignment.work().input(),
            VerifiedCompilerInput::FullWorkspaceFresh(_)
        ) || evidence.assignment.work().selected_base().is_some()
        {
            return Err(CompilerInputAdmissionError::FreshOnly);
        }
        verifier.verify_source_observation(&evidence)?;
        let peer = match evidence.assignment.route() {
            backend_execution::CompilerAssignmentRoute::Remote(peer) => peer,
            backend_execution::CompilerAssignmentRoute::Local => {
                return Err(CompilerInputAdmissionError::RemoteAssignmentRequired);
            }
        };
        if !verifier.authorize_trusted_worker(
            peer,
            evidence.namespace_id,
            evidence.capture.manifest(),
        ) {
            return Err(CompilerInputAdmissionError::WorkerNotTrusted);
        }
        let manifest = evidence.capture.manifest();
        let execution_grant = CompilerTrustedExecutionGrant {
            peer,
            namespace_id: evidence.namespace_id,
            package_lineage: manifest.package_lineage(),
            target: *manifest.package_target().target().as_ref(),
            recipe: manifest.recipe(),
            toolchain: manifest.toolchain(),
            environment: manifest.environment(),
            target_platform: manifest.target_platform(),
        };
        Ok(Self {
            evidence,
            execution_grant,
        })
    }

    /// Scheduler's opaque full-workspace proof bound to this assignment.
    #[must_use]
    pub const fn compiler_input(&self) -> VerifiedCompilerInput {
        self.evidence.assignment.work().input()
    }

    /// Re-opened exact input facts bound to the source authority.
    #[must_use]
    pub const fn evidence(&self) -> &CompilerInputAdmissionEvidence {
        &self.evidence
    }

    /// Owner-configured trust grant for the assigned worker.
    #[must_use]
    pub const fn execution_grant(&self) -> CompilerTrustedExecutionGrant {
        self.execution_grant
    }

    /// Confirms assignment, namespace, closure and manifest identity before CAS admission.
    pub fn matches(
        &self,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        capture: &CapturedFullWorkspaceV2,
    ) -> bool {
        let evidence = &self.evidence;
        evidence.assignment == assignment
            && evidence.namespace_id == namespace_id
            && evidence.capture.same_capture(capture)
            && evidence.source_fence_digest == capture.source_fence_digest()
            && self.execution_grant.namespace_id == namespace_id
            && self.execution_grant.package_lineage == capture.manifest().package_lineage()
            && self.execution_grant.target == *capture.manifest().package_target().target().as_ref()
            && self.execution_grant.recipe == capture.manifest().recipe()
            && self.execution_grant.toolchain == capture.manifest().toolchain()
            && self.execution_grant.environment == capture.manifest().environment()
            && self.execution_grant.target_platform == capture.manifest().target_platform()
            && match assignment.route() {
                backend_execution::CompilerAssignmentRoute::Remote(peer) => {
                    self.execution_grant.peer == peer
                }
                backend_execution::CompilerAssignmentRoute::Local => false,
            }
    }
}

/// Constructs measured local-first placement for one owner-fenced compiler invocation.
pub struct CompilerClusterCoordinator<'scheduler> {
    scheduler: &'scheduler CompilerClusterScheduler,
    policy: &'scheduler CompilerPlacementPolicy,
}

impl<'scheduler> CompilerClusterCoordinator<'scheduler> {
    /// Binds a scheduler and placement policy for a production dispatch call.
    #[must_use]
    pub const fn new(
        scheduler: &'scheduler CompilerClusterScheduler,
        policy: &'scheduler CompilerPlacementPolicy,
    ) -> Self {
        Self { scheduler, policy }
    }

    /// Places locally first using live measured resource/Have evidence and bounded slots.
    /// The scheduler returns a local assignment immediately when it can answer within policy;
    /// remote peer measurements never gate that available local route.
    pub fn place(
        &self,
        request: CompilerBalancingRequest,
        now: u64,
        remotes: &[CompilerBalancedRemote],
        token: CompilerAttemptToken,
    ) -> Result<CompilerAssignmentOutcome, CompilerAssignmentError> {
        self.scheduler
            .place_balanced_and_assign(self.policy, request, now, remotes, token)
    }

    /// Revalidates an owner-minted live assignment before any remote Offer or result work.
    pub fn validate_assignment(
        &self,
        assignment: CompilerAssignment,
        input: &VerifiedCompilerInputAdmission,
    ) -> Result<(), CompilerAssignmentError> {
        self.scheduler.validate_assignment(assignment)?;
        if input.evidence.assignment != assignment {
            return Err(CompilerAssignmentError::StaleAttempt);
        }
        Ok(())
    }
}

/// Closed failure returned while admitting owner source or execution policy evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerInputAdmissionError {
    /// An identity required for fencing was empty.
    EmptyIdentity,
    /// The manifest could not be canonically encoded and validated.
    ManifestInvalid,
    /// Manifest facts differ from the exact assignment or capture claim.
    WorkMismatch,
    /// This coordinator path permits only fresh full-workspace proof.
    FreshOnly,
    /// The source authority rejected the current observation or fence.
    SourceAuthorityRejected,
    /// Host execution is not authorized for this exact remote peer and invocation.
    WorkerNotTrusted,
    /// Only a remote assignment can carry a trusted-worker execution grant.
    RemoteAssignmentRequired,
}
