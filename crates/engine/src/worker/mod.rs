//! Pure worker capability boundary.
//!
//! A worker admits one exact request, receives immutable input bytes, executes
//! one registered pure recipe, and returns a wire result whose output identity
//! is recomputed from the bytes it actually produced.  It has no workspace
//! owner, journal, effect sink, or publication capability.

use crate::dispatch::{CompleteSemanticCoverage, worker_receipt_id};
use backend_execution::{
    AuthorityVersion, Cancellation, OutputEquivalence, OutputVersion, ReadManifestId, RecipeId,
    WorkKey,
};
use backend_replication::{
    Attestation, AttestationMaterial, AttestationMaterialView, ExecutionRequestExpectation,
    ExecutionResultExpectation, ExpectedIdentity, Fence, ReplicationError, ResourceEnvelope,
    RevocationVersion, SparseCoverage, TransportLimits, WireAuthority, WireIdentity,
    WireRecipeRequest, WireRecipeResult,
};
use backend_version::{WorkspaceError, WorkspaceRoot};
use std::fmt;
use std::sync::Arc;

mod execute;

/// Input object made available after the transfer adapter checked its exact
/// identity and canonical bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedInput {
    /// Exact admitted wire identity.
    pub identity: WireIdentity,
    /// Canonical immutable object bytes.
    pub bytes: Box<[u8]>,
}

/// Invocation passed to a pure recipe implementation.
#[derive(Clone, Debug)]
pub struct AdmittedInvocation {
    /// Exact work key.
    pub work_key: WorkKey,
    /// Registered recipe ABI identity.
    pub recipe: RecipeId,
    /// Exact read manifest identity.
    pub read_manifest: ReadManifestId,
    /// Workspace authority identity.
    pub authority: AuthorityVersion,
    /// Output-equivalence contract.
    pub output_equivalence: OutputEquivalence,
    /// Exact workspace root used to select inputs.
    pub input_basis: WorkspaceRoot,
    /// Admitted immutable inputs.
    pub inputs: Box<[AdmittedInput]>,
    /// Exact requested semantic scope.
    pub scope: backend_replication::ExecutionScopeId,
    /// Multidimensional hard resource envelope.
    pub resources: ResourceEnvelope,
    /// Nonzero attempt identity.
    pub attempt: backend_replication::AttemptId,
    /// Full-width scheduler fence.
    pub fence: Fence,
    /// Scheduler cancellation identity.
    pub cancellation: backend_replication::CancellationId,
}

/// Bounded context supplied to a pure recipe.
///
/// The executor may charge dimensions as it works, but the endpoint also
/// checks the returned bytes, elapsed wall time, cancellation, and declared
/// envelope after the call.  A recipe cannot increase the owner-supplied
/// envelope or mint semantic/authority evidence.
pub struct PureWorkContext<'a> {
    /// Latest scheduler cancellation observation.
    cancelled: &'a dyn Fn() -> bool,
    /// Remaining multidimensional allowance.
    resources: &'a mut ResourceEnvelope,
}

impl PureWorkContext<'_> {
    /// Charges CPU credits with checked arithmetic.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn charge_cpu(&mut self, amount: u64) -> Result<(), WorkerError> {
        charge(&mut self.resources.cpu_millis, amount)
    }

    /// Charges mutable-memory credits with checked arithmetic.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn charge_memory(&mut self, amount: u64) -> Result<(), WorkerError> {
        charge(&mut self.resources.memory_bytes, amount)
    }

    /// Charges network credits with checked arithmetic.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn charge_network(&mut self, amount: u64) -> Result<(), WorkerError> {
        charge(&mut self.resources.network_bytes, amount)
    }

    /// Charges temporary-storage credits with checked arithmetic.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn charge_storage(&mut self, amount: u64) -> Result<(), WorkerError> {
        charge(&mut self.resources.storage_bytes, amount)
    }

    /// Returns the latest cancellation state.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        (self.cancelled)()
    }
}

fn charge(remaining: &mut u64, amount: u64) -> Result<(), WorkerError> {
    *remaining = remaining
        .checked_sub(amount)
        .ok_or(WorkerError::ResourceOverrun)?;
    Ok(())
}

/// Output returned by a pure executor.  The executor cannot self-attest or
/// claim semantic completeness; both are supplied by the owner endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedOutput {
    /// Canonical output bytes.
    pub bytes: Box<[u8]>,
    /// Transfer byte coverage only.
    pub coverage: SparseCoverage,
}

/// Pure recipe implementation. It has no owner, sink, or publication method.
pub trait PureRecipeExecutor: Send + Sync + 'static {
    /// Recipe identity implemented by this executor.
    fn recipe_id(&self) -> RecipeId;
    /// Executes using only admitted immutable inputs and a bounded context.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn execute(
        &self,
        invocation: &AdmittedInvocation,
        context: &mut PureWorkContext<'_>,
    ) -> Result<PreparedOutput, WorkerError>;
}

/// Owner-supplied signer for the complete request/result statement.
pub trait WorkerAttestationSigner: Send + Sync + 'static {
    /// Signs the complete lower replication attestation material.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn sign(&self, material: &AttestationMaterial) -> Result<Attestation, WorkerError>;

    /// Signs complete material through a borrowing view. Large worker
    /// results can override this to avoid cloning canonical output bytes.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn sign_view(
        &self,
        material: &AttestationMaterialView<'_>,
    ) -> Result<Attestation, WorkerError> {
        self.sign(&material.to_owned_material())
    }
}

/// Explicit signer that makes a worker useful for local memoization but never
/// publishable as a remote authority.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoAttestationSigner;

impl WorkerAttestationSigner for NoAttestationSigner {
    fn sign(&self, _material: &AttestationMaterial) -> Result<Attestation, WorkerError> {
        Err(WorkerError::AttestationUnavailable)
    }
}

/// Deterministic keyed BLAKE3 signer used by a configured trusted executor.
#[derive(Clone, Debug)]
pub struct Blake3WorkerSigner {
    authority: [u8; 32],
    secret: [u8; 32],
}

impl Blake3WorkerSigner {
    /// Creates a signer bound to one configured authority identity.
    #[must_use]
    pub const fn new(authority: [u8; 32], secret: [u8; 32]) -> Self {
        Self { authority, secret }
    }

    /// Returns the configured authority identity.
    #[must_use]
    pub const fn authority(&self) -> [u8; 32] {
        self.authority
    }
}

impl WorkerAttestationSigner for Blake3WorkerSigner {
    fn sign(&self, material: &AttestationMaterial) -> Result<Attestation, WorkerError> {
        self.sign_view(&material.as_view())
    }

    fn sign_view(
        &self,
        material: &AttestationMaterialView<'_>,
    ) -> Result<Attestation, WorkerError> {
        if material.authority.id.as_bytes() != self.authority {
            return Err(WorkerError::AttestationAuthority);
        }
        Ok(material.keyed_authority_statement(&self.secret))
    }
}

/// Worker capability advertisement and hard admission limits.
#[derive(Clone, Debug)]
pub struct WorkerCapabilities {
    /// Recipes this process is permitted to execute.
    pub recipes: Vec<RecipeId>,
    /// Maximum requested semantic scope.
    pub max_scope: u64,
    /// Maximum multidimensional resource envelope.
    pub max_resources: ResourceEnvelope,
}

/// A worker endpoint owns only a pure executor, capabilities, and an owner
/// supplied attestation signer.
pub struct WorkerEndpoint<E, S = NoAttestationSigner> {
    capabilities: WorkerCapabilities,
    executor: Arc<E>,
    signer: S,
    limits: TransportLimits,
}

struct WorkerCall<'a> {
    request: WireRecipeRequest,
    expected: &'a ExecutionRequestExpectation,
    input_basis: WorkspaceRoot,
    recipe: RecipeId,
    read_manifest: ReadManifestId,
    authority: AuthorityVersion,
    output_equivalence: OutputEquivalence,
    work_key: WorkKey,
    inputs: Box<[AdmittedInput]>,
    semantic: CompleteSemanticCoverage,
    observed_revocation: RevocationVersion,
    cancellation: &'a Cancellation,
    resources: ResourceEnvelope,
}

struct AdmittedWork<'a> {
    request: WireRecipeRequest,
    expected: &'a ExecutionRequestExpectation,
    invocation: AdmittedInvocation,
    semantic: CompleteSemanticCoverage,
    observed_revocation: RevocationVersion,
    cancellation: &'a Cancellation,
    remaining: ResourceEnvelope,
}

impl<E, S> fmt::Debug for WorkerEndpoint<E, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerEndpoint")
            .field("capabilities", &self.capabilities)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl<E: PureRecipeExecutor> WorkerEndpoint<E, NoAttestationSigner> {
    /// Creates an explicitly nonpublishable worker endpoint.
    #[must_use]
    pub fn new(capabilities: WorkerCapabilities, executor: E, limits: TransportLimits) -> Self {
        Self {
            capabilities,
            executor: Arc::new(executor),
            signer: NoAttestationSigner,
            limits,
        }
    }
}

/// Worker failure, including exact replication admission errors.
#[derive(Debug)]
pub enum WorkerError {
    /// Request or result failed transport admission.
    Replication(ReplicationError),
    /// Recipe/schema capability was not advertised.
    Capability,
    /// Resource declaration exceeded a hard worker limit.
    ResourceLimit,
    /// Resource usage or elapsed wall time exceeded the hard envelope.
    ResourceOverrun,
    /// Recipe output byte coverage was incomplete.
    Coverage,
    /// Output exceeded the negotiated or requested object limit.
    OutputTooLarge,
    /// Input identities supplied by the transfer adapter did not match.
    InputMismatch,
    /// A specific authenticated input-proof stage failed. The static stage
    /// label carries no peer bytes and is safe for operator diagnostics.
    InputProof(&'static str),
    /// A workspace manifest failed typed closure admission.
    Workspace(WorkspaceError),
    /// Owner semantic coverage was absent or not bound to this request.
    SemanticCoverage,
    /// Scheduler cancellation was observed before publication.
    Cancelled,
    /// No owner-supplied attestation signer was configured.
    AttestationUnavailable,
    /// Signer authority did not match the result authority.
    AttestationAuthority,
    /// Pure executor failed.
    Executor(String),
}

impl fmt::Display for WorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "worker error: {self:?}")
    }
}
impl std::error::Error for WorkerError {}
