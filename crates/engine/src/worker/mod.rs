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

impl<E: PureRecipeExecutor, S: WorkerAttestationSigner> WorkerEndpoint<E, S> {
    /// Creates a worker endpoint with an owner-supplied statement signer.
    #[must_use]
    pub fn with_signer(
        capabilities: WorkerCapabilities,
        executor: E,
        signer: S,
        limits: TransportLimits,
    ) -> Self {
        Self {
            capabilities,
            executor: Arc::new(executor),
            signer,
            limits,
        }
    }

    /// Returns immutable advertised capabilities.
    #[must_use]
    pub const fn capabilities(&self) -> &WorkerCapabilities {
        &self.capabilities
    }

    /// Admits a request, runs the pure recipe, and constructs a fully bound
    /// result. Cancellation and the multidimensional envelope are supplied by
    /// the scheduler/owner; no scalar advisory resource is accepted.
    ///
    /// This borrowed compatibility entry point clones the request envelope so
    /// existing callers can retain it. Stream owners that are finished with
    /// the request should use [`Self::execute_owned`] to move its input claims
    /// into the result without another allocation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    #[allow(
        clippy::too_many_arguments,
        reason = "The public worker boundary keeps each owner-supplied capability explicit."
    )]
    pub fn execute(
        &self,
        request: &WireRecipeRequest,
        expected: &ExecutionRequestExpectation,
        input_basis: WorkspaceRoot,
        recipe: RecipeId,
        read_manifest: ReadManifestId,
        authority: AuthorityVersion,
        output_equivalence: OutputEquivalence,
        work_key: WorkKey,
        inputs: Box<[AdmittedInput]>,
        semantic: CompleteSemanticCoverage,
        observed_revocation: RevocationVersion,
        cancellation: &Cancellation,
        resources: ResourceEnvelope,
    ) -> Result<WireRecipeResult, WorkerError> {
        self.execute_owned(
            request.clone(),
            expected,
            input_basis,
            recipe,
            read_manifest,
            authority,
            output_equivalence,
            work_key,
            inputs,
            semantic,
            observed_revocation,
            cancellation,
            resources,
        )
    }

    /// Admits an owned request, runs the pure recipe, and constructs a fully
    /// bound result. The request's input claims move into the result, so this
    /// path preserves one allocation for a stream handoff. Cancellation and
    /// the multidimensional envelope are supplied by the scheduler/owner; no
    /// scalar advisory resource is accepted.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    #[allow(
        clippy::too_many_arguments,
        reason = "The public worker boundary keeps each owner-supplied capability explicit."
    )]
    pub fn execute_owned(
        &self,
        request: WireRecipeRequest,
        expected: &ExecutionRequestExpectation,
        input_basis: WorkspaceRoot,
        recipe: RecipeId,
        read_manifest: ReadManifestId,
        authority: AuthorityVersion,
        output_equivalence: OutputEquivalence,
        work_key: WorkKey,
        inputs: Box<[AdmittedInput]>,
        semantic: CompleteSemanticCoverage,
        observed_revocation: RevocationVersion,
        cancellation: &Cancellation,
        resources: ResourceEnvelope,
    ) -> Result<WireRecipeResult, WorkerError> {
        let mut work = self.admit_invocation(WorkerCall {
            request,
            expected,
            input_basis,
            recipe,
            read_manifest,
            authority,
            output_equivalence,
            work_key,
            inputs,
            semantic,
            observed_revocation,
            cancellation,
            resources,
        })?;
        let output = self.execute_bounded(&mut work)?;
        self.finish_result(work, output)
    }

    fn admit_invocation<'a>(&self, call: WorkerCall<'a>) -> Result<AdmittedWork<'a>, WorkerError> {
        let input_bytes = self.validate_call(&call)?;
        let WorkerCall {
            request,
            expected,
            input_basis,
            recipe,
            read_manifest,
            authority,
            output_equivalence,
            work_key,
            inputs,
            semantic,
            observed_revocation,
            cancellation,
            resources,
        } = call;
        let invocation = AdmittedInvocation {
            work_key,
            recipe,
            read_manifest,
            authority,
            output_equivalence,
            input_basis,
            inputs,
            scope: request.scope,
            resources,
            attempt: request.attempt,
            fence: request.fence,
            cancellation: request.cancellation,
        };
        let mut remaining = resources;
        if resources.processes == 0 {
            return Err(WorkerError::ResourceOverrun);
        }
        // Charge fixed process/input costs before entering user code. These
        // checks are outside recipe cooperation, so a recipe that ignores its
        // context still cannot exceed the owner's hard envelope.
        charge(&mut remaining.cpu_millis, 1)?;
        charge(&mut remaining.memory_bytes, input_bytes)?;
        // Input transfer is part of the worker's network budget. Charge it
        // before invoking user code so a recipe cannot consume an over-sized
        // input and still report a successful execution merely because its
        // output was small.
        charge(&mut remaining.network_bytes, input_bytes)?;
        remaining.processes = remaining
            .processes
            .checked_sub(1)
            .ok_or(WorkerError::ResourceOverrun)?;
        Ok(AdmittedWork {
            request,
            expected,
            invocation,
            semantic,
            observed_revocation,
            cancellation,
            remaining,
        })
    }

    fn validate_call(&self, call: &WorkerCall<'_>) -> Result<u64, WorkerError> {
        call.request
            .admit_against(call.expected, self.limits)
            .map_err(WorkerError::Replication)?;
        if call.cancellation.is_cancelled() {
            return Err(WorkerError::Cancelled);
        }
        let recipe_claim = WireIdentity::from_typed(&call.recipe);
        let read_manifest_claim = WireIdentity::from_typed(&call.read_manifest);
        let semantic_scope = backend_replication::ExecutionScopeId::try_from(call.semantic.scope())
            .map_err(WorkerError::Replication)?;
        if !self.capabilities.recipes.contains(&call.recipe)
            || self.executor.recipe_id() != call.recipe
            || call.expected.work_key != ExpectedIdentity::from_typed(&call.work_key)
            || call.request.recipe != recipe_claim
            || call.request.read_manifest != read_manifest_claim
            || call.request.scope != semantic_scope
            || call.request.resources != call.resources
        {
            return Err(WorkerError::Capability);
        }
        call.request
            .input_basis
            .admit(call.input_basis)
            .map_err(WorkerError::Replication)?;
        call.request
            .authority
            .admit_against(call.expected.authority, call.observed_revocation)
            .map_err(WorkerError::Replication)?;
        if call.inputs.len() != call.request.inputs.len()
            || call
                .inputs
                .iter()
                .zip(&call.request.inputs)
                .any(|(input, claim)| input.identity != *claim)
        {
            return Err(WorkerError::InputMismatch);
        }
        if call.semantic.scope() > self.capabilities.max_scope
            || !within(call.resources, self.capabilities.max_resources)
        {
            return Err(WorkerError::ResourceLimit);
        }
        call.resources
            .validate(self.limits)
            .map_err(WorkerError::Replication)?;
        call.inputs.iter().try_fold(0_u64, |total, input| {
            total
                .checked_add(
                    u64::try_from(input.bytes.len()).map_err(|_| WorkerError::ResourceOverrun)?,
                )
                .ok_or(WorkerError::ResourceOverrun)
        })
    }

    fn execute_bounded(&self, work: &mut AdmittedWork<'_>) -> Result<PreparedOutput, WorkerError> {
        let cancelled = || work.cancellation.is_cancelled();
        let start = std::time::Instant::now();
        let output = {
            let mut context = PureWorkContext {
                cancelled: &cancelled,
                resources: &mut work.remaining,
            };
            self.executor.execute(&work.invocation, &mut context)?
        };
        if work.cancellation.is_cancelled() {
            return Err(WorkerError::Cancelled);
        }
        let elapsed = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        if elapsed > work.invocation.resources.wall_millis {
            return Err(WorkerError::ResourceOverrun);
        }
        let output_len =
            u64::try_from(output.bytes.len()).map_err(|_| WorkerError::OutputTooLarge)?;
        if output_len > work.invocation.resources.output_bytes
            || output_len > self.limits.max_object
        {
            return Err(WorkerError::OutputTooLarge);
        }
        charge(&mut work.remaining.storage_bytes, output_len)?;
        charge(&mut work.remaining.network_bytes, output_len)?;
        if !output.coverage.is_complete(output_len) {
            return Err(WorkerError::Coverage);
        }
        Ok(output)
    }

    fn finish_result(
        &self,
        work: AdmittedWork<'_>,
        output: PreparedOutput,
    ) -> Result<WireRecipeResult, WorkerError> {
        let AdmittedWork {
            request,
            expected,
            invocation,
            semantic,
            observed_revocation,
            remaining: _,
            cancellation: _,
        } = work;
        let recipe_claim = WireIdentity::from_typed(&invocation.recipe);
        let read_manifest_claim = WireIdentity::from_typed(&invocation.read_manifest);
        let authority_claim =
            WireAuthority::from_typed(&invocation.authority, request.authority.minimum_epoch);
        let output_version = OutputVersion::from_value(&output.bytes);
        let receipt = worker_receipt_id(&request, output_version, &output.bytes);
        let result = WireRecipeResult {
            attempt: request.attempt,
            recipe: recipe_claim,
            work_key: WireIdentity::from_typed(&invocation.work_key),
            input_basis: backend_replication::WorkspaceRootClaim::from_bytes(
                *invocation.input_basis.as_bytes(),
            ),
            inputs: request.inputs,
            read_manifest: read_manifest_claim,
            output: WireIdentity::from_typed(&output_version),
            output_bytes: Arc::new(output.bytes.into_vec()),
            scope: request.scope,
            resources: invocation.resources,
            byte_coverage: output.coverage,
            semantic_coverage: semantic.wire(),
            authority: authority_claim,
            authority_policy: request.authority,
            revocation_version: observed_revocation,
            fence: request.fence,
            cancellation: request.cancellation,
            attestation: None,
            receipt: WireIdentity::from_typed(&receipt),
        };
        let attestation = self.signer.sign_view(&result.attestation_material_view())?;
        let result = WireRecipeResult {
            attestation: Some(attestation),
            ..result
        };
        let result_expectation = ExecutionResultExpectation {
            output: ExpectedIdentity::from_typed(&output_version),
            receipt: ExpectedIdentity::from_typed(&receipt),
            output_len: u64::try_from(result.output_bytes.len())
                .map_err(|_| WorkerError::OutputTooLarge)?,
            byte_coverage: result.byte_coverage.clone(),
            semantic_coverage: semantic.replication_expectation(),
        };
        result
            .admit_against_owned(
                expected,
                &result_expectation,
                self.limits,
                observed_revocation,
            )
            .map(backend_replication::AdmittedExecutionResult::into_wire)
            .map_err(WorkerError::Replication)
    }
}

fn within(actual: ResourceEnvelope, limit: ResourceEnvelope) -> bool {
    actual.cpu_millis <= limit.cpu_millis
        && actual.memory_bytes <= limit.memory_bytes
        && actual.network_bytes <= limit.network_bytes
        && actual.storage_bytes <= limit.storage_bytes
        && actual.output_bytes <= limit.output_bytes
        && actual.processes <= limit.processes
        && actual.wall_millis <= limit.wall_millis
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
