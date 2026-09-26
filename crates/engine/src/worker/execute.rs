//! Admits and runs one pure worker invocation.

use super::{
    AdmittedInput, AdmittedInvocation, AdmittedWork, Arc, AuthorityVersion, Cancellation,
    CompleteSemanticCoverage, ExecutionRequestExpectation, ExecutionResultExpectation,
    ExpectedIdentity, OutputEquivalence, OutputVersion, PreparedOutput, PureRecipeExecutor,
    PureWorkContext, ReadManifestId, RecipeId, ResourceEnvelope, RevocationVersion,
    TransportLimits, WireAuthority, WireIdentity, WireRecipeRequest, WireRecipeResult, WorkKey,
    WorkerAttestationSigner, WorkerCall, WorkerCapabilities, WorkerEndpoint, WorkerError,
    WorkspaceRoot, charge, worker_receipt_id,
};

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
