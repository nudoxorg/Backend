//! Deterministic slow worker used by the black-box cancellation journey.
//!
//! This fixture is intentionally a separate process.  It uses the production
//! worker listener, framing, admission, cancellation, and attestation seams;
//! only the pure recipe body is test-owned and waits for its authenticated
//! cancellation observation.
#![deny(unsafe_code)]
#![allow(clippy::panic)]

use backend_engine::{
    AuthorityEpoch, AuthorityExpectation, AuthorityVersion, Blake3WorkerSigner, CapabilityManifest,
    CompleteSemanticCoverage, ExecutionRequestExpectation, ExecutionScopeId, ExpectedIdentity,
    OutputEquivalence, PreparedOutput, PureRecipeExecutor, PureWorkContext, ReadManifestId,
    ResourceEnvelope, RevocationVersion, SemanticCoverageAdmissionError, SemanticCoverageBinding,
    SemanticCoverageState, SemanticCoverageValidator, TransportLimits,
    UntrustedSemanticCoverageClaim, VersionRange, WireAuthority, WireAuthorityPolicy, WireIdentity,
    WireRecipeRequest, WorkKey, WorkerCapabilities, WorkerError,
};
use backend_execution::{
    AuthorityVersionSchema, ReadManifestSchema, RecipeId, RecipeSchema, WorkKeySchema,
};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ObjectClosure, ProducerObservationClaims,
    ProducerObservationVerifier, Relation, RelationBinding, RelationState, StateRoot,
    UntrustedProducerObservation, WorkspaceManifest, admit_complete_scope,
    admit_producer_observation,
};
use backend_worker::{
    JobAdmission, WorkerJob, WorkerJobBindings, WorkerProcessConfig, WorkerService,
};
use std::marker::PhantomData;
use std::num::NonZeroU64;

const LEGACY_SCOPE_ONE: NonZeroU64 = NonZeroU64::MIN;

const RECIPE_BYTES: &[u8] = b"backend.journey.cancel.recipe.v1";
const READ_BYTES: &[u8] = b"backend.journey.cancel.reads.v1";
const AUTHORITY_BYTES: &[u8] = b"backend.journey.cancel.authority.v1";
const EQUIVALENCE_BYTES: &[u8] = b"backend.journey.cancel.equivalence.v1";
const WITNESS_BYTES: &[u8] = b"backend.journey.cancel.semantic.v1";
const AUTHORITY_SECRET: [u8; 32] = [0x33; 32];

struct CancelCoverageVerifier(backend_version::ScopeRoot);

impl ProducerObservationVerifier for CancelCoverageVerifier {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        let scope = self.0;
        let expected_identity = *scope.as_bytes();
        let expected_evidence = scope.as_bytes();
        if observation.producer_identity() != expected_identity
            || observation.context() != expected_identity
            || observation.scope_root() != scope
            || observation.evidence() != expected_evidence
        {
            return Err("invalid cancellation journey producer observation");
        }
        Ok(ProducerObservationClaims::new(
            expected_identity,
            scope,
            expected_identity,
            *blake3::hash(expected_evidence).as_bytes(),
        ))
    }
}

fn complete_scope(authority: AuthorityVersion) -> CoverageWitness {
    let declared = AuthorityScopeClaim::from_object_version(authority);
    let scope = declared.scope_root();
    let producer = admit_producer_observation(
        UntrustedProducerObservation::new(
            *scope.as_bytes(),
            scope,
            *scope.as_bytes(),
            scope.as_bytes().to_vec(),
        ),
        &CancelCoverageVerifier(scope),
    )
    .unwrap_or_else(|error| panic!("admit cancellation producer observation: {error}"));
    CoverageWitness::Complete(
        admit_complete_scope(declared, producer)
            .unwrap_or_else(|error| panic!("admit cancellation scope: {error}")),
    )
}

#[derive(Debug)]
struct CancelRelation;

impl Relation for CancelRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 2;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }
}

#[derive(Clone, Copy, Debug)]
struct SlowExecutor;

impl PureRecipeExecutor for SlowExecutor {
    fn recipe_id(&self) -> RecipeId {
        RecipeId::from_value(RECIPE_BYTES)
    }

    fn execute(
        &self,
        _invocation: &backend_engine::AdmittedInvocation,
        context: &mut PureWorkContext<'_>,
    ) -> Result<PreparedOutput, WorkerError> {
        if let Ok(path) = std::env::var("BACKEND_JOURNEY_STARTED") {
            let _ = std::fs::write(path, b"started");
        }
        // The loop is cooperative and has no wall-clock sleep.  The test
        // sends a real authenticated CancelAttempt while this call is live.
        loop {
            if context.is_cancelled() {
                if let Ok(path) = std::env::var("BACKEND_JOURNEY_CANCELLED") {
                    let _ = std::fs::write(path, b"cancelled");
                }
                return Err(WorkerError::Cancelled);
            }
            std::thread::yield_now();
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CancelSemanticAuthority;

impl SemanticCoverageValidator for CancelSemanticAuthority {
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        if claim.state() != SemanticCoverageState::Complete
            || claim.scope() != 1
            || claim.witness() != WITNESS_BYTES
            || claim.read_manifest() != binding.read_manifest()
            || claim.authority() != binding.authority()
        {
            return Err(SemanticCoverageAdmissionError::Rejected);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct CancelAdmission {
    recipe: RecipeId,
    read_manifest: ReadManifestId,
    authority: AuthorityVersion,
    equivalence: OutputEquivalence,
    input_root: StateRoot<CancelRelation>,
    input_basis: backend_version::WorkspaceRoot,
    work_key: WorkKey,
    authority_claim: WireAuthority,
    authority_expectation: AuthorityExpectation,
    semantic_authority: CancelSemanticAuthority,
}

impl CancelAdmission {
    fn new() -> Result<Self, String> {
        let recipe = RecipeId::from_value(RECIPE_BYTES);
        let read_manifest = ReadManifestId::from_value(READ_BYTES);
        let authority = AuthorityVersion::from_value(AUTHORITY_BYTES);
        let equivalence = OutputEquivalence::from_value(EQUIVALENCE_BYTES);
        let relation = RelationState::<CancelRelation>::from_entries(
            [(1_u64, 1_u64)],
            complete_scope(authority),
        )
        .map_err(|error| error.to_string())?;
        let manifest = WorkspaceManifest::new_checked(
            1,
            vec![RelationBinding::from_state(&relation)],
            Vec::new(),
            ObjectClosure::from_version(authority),
            relation.coverage(),
        )
        .map_err(|error| error.to_string())?;
        let input_basis = manifest.root();
        let input_root = relation.root();
        let identity = backend_engine::VersionedWorkIdentity::new(
            recipe,
            input_root,
            read_manifest,
            authority,
            equivalence,
        );
        let authority_claim = WireAuthority::from_typed(&authority, AuthorityEpoch(1));
        let authority_expectation =
            AuthorityExpectation::from_typed(&authority, AuthorityEpoch(1), RevocationVersion(1));
        Ok(Self {
            recipe,
            read_manifest,
            authority,
            equivalence,
            input_root,
            input_basis,
            work_key: identity.work_key(),
            authority_claim,
            authority_expectation,
            semantic_authority: CancelSemanticAuthority,
        })
    }

    fn admit_request(
        &self,
        request: &WireRecipeRequest,
    ) -> Result<WorkerJob<CancelRelation>, WorkerError> {
        let recipe_claim = WireIdentity::from_typed(&self.recipe);
        let read_claim = WireIdentity::from_typed(&self.read_manifest);
        let work_claim = WireIdentity::from_typed(&self.work_key);
        let expected_authority = WireAuthorityPolicy {
            id: self.authority_claim.id,
            minimum_epoch: AuthorityEpoch(1),
            revocation_version: RevocationVersion(1),
        };
        if request.recipe != recipe_claim
            || request.read_manifest != read_claim
            || request.work_key != work_claim
            || request.input_basis.as_bytes() != self.input_basis.to_bytes()
            || !request.inputs.is_empty()
            || request.scope != ExecutionScopeId::from_legacy_ordinal(LEGACY_SCOPE_ONE)
            || request.authority != expected_authority
        {
            return Err(WorkerError::Capability);
        }
        let identity = backend_engine::VersionedWorkIdentity::new(
            self.recipe,
            self.input_root,
            self.read_manifest,
            self.authority,
            self.equivalence,
        );
        let claim = UntrustedSemanticCoverageClaim::complete_claim(
            &identity,
            1,
            WITNESS_BYTES,
            AuthorityEpoch(1),
            RevocationVersion(1),
        );
        let semantic = CompleteSemanticCoverage::admit(&identity, claim, &self.semantic_authority)
            .map_err(|_| WorkerError::SemanticCoverage)?;
        let expected = ExecutionRequestExpectation {
            attempt: request.attempt,
            recipe: ExpectedIdentity::from_typed(&self.recipe),
            work_key: ExpectedIdentity::from_typed(&self.work_key),
            inputs: Vec::new(),
            read_manifest: ExpectedIdentity::from_typed(&self.read_manifest),
            scope: ExecutionScopeId::from_legacy_ordinal(LEGACY_SCOPE_ONE),
            authority: self.authority_expectation,
            resources: request.resources,
            fence: request.fence,
            input_basis: self.input_basis,
            cancellation: request.cancellation,
        };
        let cancellation = backend_worker::JobCancellation::new();
        Ok(WorkerJob::with_cancellation(
            WorkerJobBindings {
                expected,
                input_basis: self.input_basis,
                recipe: self.recipe,
                read_manifest: self.read_manifest,
                authority: self.authority,
                output_equivalence: self.equivalence,
                work_key: self.work_key,
                inputs: Vec::new().into_boxed_slice(),
                semantic,
                observed_revocation: RevocationVersion(1),
                resources: request.resources,
                relation: PhantomData,
            },
            cancellation,
        ))
    }
}

impl JobAdmission<CancelRelation> for CancelAdmission {
    fn admit(
        &mut self,
        request: &WireRecipeRequest,
    ) -> Result<WorkerJob<CancelRelation>, WorkerError> {
        self.admit_request(request)
    }
}

fn manifest(limits: TransportLimits) -> Result<CapabilityManifest, String> {
    let recipe = RecipeId::from_value(RECIPE_BYTES);
    let mut schemas = vec![
        backend_replication::SchemaDescriptor::of::<RecipeSchema>(),
        backend_replication::SchemaDescriptor::of::<ReadManifestSchema>(),
        backend_replication::SchemaDescriptor::of::<AuthorityVersionSchema>(),
        backend_replication::SchemaDescriptor::of::<WorkKeySchema>(),
    ];
    schemas.sort_by_key(|schema| (schema.domain, schema.type_id));
    let max_frame = u32::try_from(limits.max_frame).map_err(|error| error.to_string())?;
    Ok(CapabilityManifest {
        protocol: VersionRange { min: 1, max: 1 },
        schemas,
        recipes: vec![backend_replication::RecipeCapability {
            recipe: WireIdentity::from_typed(&recipe),
            versions: VersionRange { min: 1, max: 1 },
        }],
        max_object: limits.max_object,
        max_chunk: u32::try_from(limits.max_chunk).map_err(|error| error.to_string())?,
        max_frame,
        max_ranges: u32::try_from(limits.max_ranges).map_err(|error| error.to_string())?,
        max_resources: max_resources(limits),
    })
}

fn max_resources(limits: TransportLimits) -> ResourceEnvelope {
    ResourceEnvelope {
        cpu_millis: limits.max_object.min(10_000),
        memory_bytes: limits.max_object.min(1024 * 1024),
        network_bytes: limits.max_object,
        storage_bytes: limits.max_object,
        output_bytes: limits.max_object,
        processes: 1,
        wall_millis: limits.max_object.min(10_000),
    }
}

fn run() -> Result<std::process::ExitCode, String> {
    let config =
        WorkerProcessConfig::parse(std::env::args().skip(1)).map_err(|error| error.to_string())?;
    let worker_limits = config.limits;
    let admission = CancelAdmission::new()?;
    let capabilities = WorkerCapabilities {
        recipes: vec![admission.recipe],
        max_scope: 1,
        max_resources: max_resources(worker_limits.transport),
    };
    let worker_manifest = manifest(worker_limits.transport)?;
    let signer = Blake3WorkerSigner::new(admission.authority.to_bytes(), AUTHORITY_SECRET);
    let worker = WorkerService::with_signer(
        capabilities,
        SlowExecutor,
        signer,
        worker_manifest,
        worker_limits,
    )
    .map_err(|error| error.to_string())?;
    Ok(backend_worker::run_process::<_, _, CancelRelation, _>(
        worker, admission, config,
    ))
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("backend-journey-cancel-worker: {error}");
            std::process::ExitCode::from(70)
        }
    }
}
