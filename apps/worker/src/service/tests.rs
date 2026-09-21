//! Worker service protocol and cancellation integration tests.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::similar_names,
    clippy::too_many_lines
)]

use super::*;
use backend_engine::{
    AttemptId, AuthorityEpoch, AuthorityExpectation, AuthorityScopeClaim, AuthorityVersion,
    Cancellation, CancellationId, CompleteSemanticCoverage, CoverageWitness,
    ExecutionRequestExpectation, ExpectedIdentity, Fence, ObjectVersion, OutputEquivalence,
    RecipeId, RecipeSchema, RelationBinding, ResourceEnvelope, Schema, SchemaDescriptor, ScopeRoot,
    SemanticCoverageAdmissionError, SemanticCoverageBinding, SemanticCoverageState,
    SemanticCoverageValidator, UntrustedProducerObservation, UntrustedSemanticCoverageClaim,
    VersionRange, VersionedWorkIdentity, WireAuthorityPolicy, WireIdentity, WorkKey, WorkerError,
    WorkspaceManifest, WorkspaceRootClaim, admit_producer_observation,
};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

struct FixtureCoverageProducer {
    scope: ScopeRoot,
}

impl backend_engine::ProducerObservationVerifier for FixtureCoverageProducer {
    type Error = &'static str;

    fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        if observation.producer_identity() == [0x71; 32]
            && observation.scope_root() == self.scope
            && observation.context() == [0x72; 32]
            && observation.evidence() == [0x73, 0x74]
        {
            Ok(())
        } else {
            Err("fixture producer rejected")
        }
    }
}

impl FixtureCoverageProducer {
    fn observation(&self) -> UntrustedProducerObservation {
        UntrustedProducerObservation::new([0x71; 32], self.scope, [0x72; 32], vec![0x73, 0x74])
    }
}

#[derive(Debug)]
struct Dummy;

impl PureRecipeExecutor for Dummy {
    fn recipe_id(&self) -> RecipeId {
        RecipeId::from_value(b"dummy")
    }

    fn execute(
        &self,
        _invocation: &backend_engine::AdmittedInvocation,
        _context: &mut backend_engine::PureWorkContext<'_>,
    ) -> Result<backend_engine::PreparedOutput, WorkerError> {
        Err(WorkerError::Capability)
    }
}

fn resource_limit() -> ResourceEnvelope {
    ResourceEnvelope {
        cpu_millis: 100,
        memory_bytes: 1024,
        network_bytes: 1024,
        storage_bytes: 1024,
        output_bytes: 1024,
        processes: 1,
        wall_millis: 100,
    }
}

fn manifest() -> CapabilityManifest {
    CapabilityManifest {
        protocol: VersionRange { min: 1, max: 1 },
        schemas: vec![SchemaDescriptor {
            domain: 1,
            type_id: 1,
            versions: VersionRange { min: 1, max: 1 },
        }],
        recipes: vec![backend_engine::RecipeCapability {
            recipe: WireIdentity::from_typed(&RecipeId::from_value(b"dummy")),
            versions: VersionRange { min: 1, max: 1 },
        }],
        max_object: 1024,
        max_chunk: 512,
        max_frame: 4096,
        max_ranges: 4,
        max_resources: resource_limit(),
    }
}

#[test]
fn worker_rejects_empty_registry_instead_of_selecting_a_default_recipe() {
    let mut empty = manifest();
    empty.recipes.clear();
    let capabilities = backend_engine::WorkerCapabilities {
        recipes: Vec::new(),
        max_scope: 1,
        max_resources: resource_limit(),
    };
    let result = WorkerService::new(capabilities, Dummy, empty, WorkerLimits::default());
    assert!(matches!(
        result,
        Err(WorkerProtocolError::Rejected(message)) if message.contains("no registered")
    ));
}

#[test]
fn worker_rejects_resource_advertisements_it_cannot_execute() {
    let capabilities = backend_engine::WorkerCapabilities {
        recipes: vec![RecipeId::from_value(b"dummy")],
        max_scope: 1,
        max_resources: resource_limit(),
    };
    let mut advertised = manifest();
    advertised.max_resources.cpu_millis = advertised.max_resources.cpu_millis.saturating_add(1);
    let result = WorkerService::new(capabilities, Dummy, advertised, WorkerLimits::default());
    assert!(matches!(
        result,
        Err(WorkerProtocolError::Rejected(message)) if message.contains("resource limits")
    ));
}

#[test]
fn cancellation_requires_the_exact_active_attempt_and_fence() {
    let capabilities = backend_engine::WorkerCapabilities {
        recipes: vec![RecipeId::from_value(b"dummy")],
        max_scope: 1,
        max_resources: resource_limit(),
    };
    let mut service = WorkerService::new(capabilities, Dummy, manifest(), WorkerLimits::default())
        .unwrap_or_else(|error| panic!("worker: {error}"));
    let recipe = RecipeId::from_value(b"dummy");
    let expected_work_key = ExpectedIdentity::from_typed(&recipe);
    let work_key = WireIdentity::from_typed(&recipe);
    let fence = Fence::from_bytes([3; 32]).unwrap_or_else(|error| panic!("fence: {error}"));
    let cancellation =
        CancellationId::new([4; 32]).unwrap_or_else(|error| panic!("cancellation: {error}"));
    let (observation, raw_handle) = Cancellation::new();
    let handle = CancellationHandle::from_handle(raw_handle);
    let key = ActiveJobKey {
        attempt: AttemptId::new(7).expect("attempt"),
        work_key,
        fence,
        cancellation,
    };
    service.active.insert(
        key,
        ActiveJob {
            handle: handle.clone(),
            cancellation: CancelAttemptExpectation {
                attempt: key.attempt,
                work_key: expected_work_key,
                cancellation,
                fence,
            },
        },
    );

    let valid = CancelAttempt {
        attempt: key.attempt,
        work_key,
        cancellation,
        fence,
    };
    let stale = CancelAttempt {
        cancellation: CancellationId::new([5; 32])
            .unwrap_or_else(|error| panic!("stale cancellation: {error}")),
        ..valid
    };
    assert!(matches!(
        service.admit_cancel_attempt(key, stale),
        Err(WorkerProtocolError::Replication(
            backend_engine::ReplicationError::StaleFence
        ))
    ));
    assert!(!observation.is_cancelled());
    service
        .admit_cancel_attempt(key, valid)
        .unwrap_or_else(|error| panic!("cancel: {error}"));
    assert!(observation.is_cancelled());

    let unmatched = CancelAttempt {
        attempt: AttemptId::new(8).expect("attempt"),
        ..valid
    };
    assert!(matches!(
        service.admit_cancel_attempt(key, unmatched),
        Err(WorkerProtocolError::Rejected(_))
    ));
    assert!(observation.is_cancelled());
}

#[derive(Debug)]
struct CancelRelation;

impl Relation for CancelRelation {
    const DOMAIN: u8 = 0x94;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }
}

#[derive(Debug)]
struct CancelAuthoritySchema;

impl Schema for CancelAuthoritySchema {
    const DOMAIN: u8 = 0x94;
    const TYPE: u16 = 2;
    type Value = u64;

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }
}

fn complete_coverage() -> CoverageWitness {
    let authority = ObjectVersion::<CancelAuthoritySchema>::from_value(&1_u64);
    let declared = AuthorityScopeClaim::from_object_version(authority);
    let producer = FixtureCoverageProducer {
        scope: declared.scope_root(),
    };
    let admitted = admit_producer_observation(producer.observation(), &producer)
        .unwrap_or_else(|error| panic!("producer: {error}"));
    CoverageWitness::Complete(
        backend_engine::admit_complete_scope(declared, admitted)
            .unwrap_or_else(|error| panic!("coverage: {error}")),
    )
}

#[derive(Clone, Copy, Debug)]
struct CancelSemanticAuthority {
    binding: SemanticCoverageBinding,
}

impl SemanticCoverageValidator for CancelSemanticAuthority {
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        if *binding == self.binding
            && claim.state() == SemanticCoverageState::Complete
            && claim.scope() == 1
            && claim.authority_epoch() == AuthorityEpoch(1)
            && claim.revocation_version() == backend_engine::RevocationVersion(1)
            && claim.witness() == b"cancel-witness"
        {
            Ok(())
        } else {
            Err(SemanticCoverageAdmissionError::Rejected)
        }
    }
}

#[derive(Clone, Debug)]
struct BlockingExecutor {
    recipe: RecipeId,
    entered: Arc<AtomicBool>,
}

impl PureRecipeExecutor for BlockingExecutor {
    fn recipe_id(&self) -> RecipeId {
        self.recipe
    }

    fn execute(
        &self,
        _invocation: &backend_engine::AdmittedInvocation,
        context: &mut backend_engine::PureWorkContext<'_>,
    ) -> Result<backend_engine::PreparedOutput, WorkerError> {
        self.entered.store(true, Ordering::Release);
        while !context.is_cancelled() {
            thread::yield_now();
        }
        Err(WorkerError::Cancelled)
    }
}

#[derive(Clone, Debug)]
struct CancelAdmission {
    recipe: RecipeId,
    read_manifest: backend_engine::ReadManifestId,
    authority: AuthorityVersion,
    equivalence: OutputEquivalence,
    input_basis: backend_engine::WorkspaceRoot,
    work_key: WorkKey,
    semantic: CompleteSemanticCoverage,
}

impl JobAdmission<CancelRelation> for CancelAdmission {
    fn admit(
        &mut self,
        request: &WireRecipeRequest,
    ) -> Result<WorkerJob<CancelRelation>, WorkerError> {
        let expected = ExecutionRequestExpectation {
            attempt: request.attempt,
            recipe: ExpectedIdentity::from_typed(&self.recipe),
            work_key: ExpectedIdentity::from_typed(&self.work_key),
            inputs: Vec::new(),
            read_manifest: ExpectedIdentity::from_typed(&self.read_manifest),
            scope: 1,
            authority: AuthorityExpectation::from_typed(
                &self.authority,
                AuthorityEpoch(1),
                backend_engine::RevocationVersion(1),
            ),
            resources: request.resources,
            fence: request.fence,
            input_basis: self.input_basis,
            cancellation: request.cancellation,
        };
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
                semantic: self.semantic.clone(),
                observed_revocation: backend_engine::RevocationVersion(1),
                resources: request.resources,
                relation: core::marker::PhantomData,
            },
            JobCancellation::new(),
        ))
    }
}

fn cancellation_fixture() -> (
    WorkerService<BlockingExecutor>,
    CancelAdmission,
    WireRecipeRequest,
    Arc<AtomicBool>,
    WorkerLimits,
) {
    let limits = WorkerLimits {
        transport: backend_engine::TransportLimits {
            max_frame: 4096,
            max_chunk: 1024,
            max_object: 4096,
            ..backend_engine::TransportLimits::default()
        },
        max_frames_per_connection: 4,
        io_timeout: std::time::Duration::from_millis(50),
    };
    let recipe = RecipeId::from_value(b"worker-cancellation-test");
    let read_manifest = backend_engine::ReadManifestId::from_value(b"cancel-read");
    let authority = AuthorityVersion::from_value(b"cancel-authority");
    let equivalence = OutputEquivalence::from_value(b"cancel-equivalence");
    let relation = backend_engine::RelationState::<CancelRelation>::from_entries(
        [(1_u64, 1_u64)],
        complete_coverage(),
    )
    .unwrap_or_else(|error| panic!("relation: {error}"));
    let workspace = WorkspaceManifest::from_versions(
        1,
        vec![RelationBinding::from_state(&relation)],
        Vec::new(),
        ObjectVersion::<CancelAuthoritySchema>::from_value(&1_u64),
        complete_coverage(),
    )
    .unwrap_or_else(|error| panic!("workspace: {error}"));
    let identity = VersionedWorkIdentity::new(
        recipe,
        relation.root(),
        read_manifest,
        authority,
        equivalence,
    );
    let semantic_authority = CancelSemanticAuthority {
        binding: SemanticCoverageBinding::from_identity(&identity),
    };
    let claim = UntrustedSemanticCoverageClaim::complete_claim(
        &identity,
        1,
        b"cancel-witness".to_vec().into_boxed_slice(),
        AuthorityEpoch(1),
        backend_engine::RevocationVersion(1),
    );
    let semantic = CompleteSemanticCoverage::admit(&identity, claim, &semantic_authority)
        .unwrap_or_else(|error| panic!("semantic coverage: {error}"));
    let fence = Fence::from_bytes([0x33; 32]).unwrap_or_else(|error| panic!("fence: {error}"));
    let cancellation =
        CancellationId::new([0x44; 32]).unwrap_or_else(|error| panic!("cancellation: {error}"));
    let resources = ResourceEnvelope {
        cpu_millis: 100,
        memory_bytes: 1024,
        network_bytes: 1024,
        storage_bytes: 1024,
        output_bytes: 1024,
        processes: 1,
        wall_millis: 4000,
    };
    let request = WireRecipeRequest {
        attempt: AttemptId::new(1).expect("attempt"),
        recipe: WireIdentity::from_typed(&recipe),
        work_key: WireIdentity::from_typed(&identity.work_key()),
        inputs: Vec::new(),
        read_manifest: WireIdentity::from_typed(&read_manifest),
        scope: 1,
        authority: WireAuthorityPolicy {
            id: WireIdentity::from_typed(&authority),
            minimum_epoch: AuthorityEpoch(1),
            revocation_version: backend_engine::RevocationVersion(1),
        },
        resources,
        fence,
        cancellation,
        input_basis: WorkspaceRootClaim::from_bytes(*workspace.root().as_bytes()),
    };
    let schemas = vec![SchemaDescriptor::of::<RecipeSchema>()];
    let manifest = CapabilityManifest {
        protocol: VersionRange { min: 1, max: 1 },
        schemas,
        recipes: vec![backend_engine::RecipeCapability {
            recipe: request.recipe,
            versions: VersionRange { min: 1, max: 1 },
        }],
        max_object: 4096,
        max_chunk: 1024,
        max_frame: 4096,
        max_ranges: 4,
        max_resources: resources,
    };
    let entered = Arc::new(AtomicBool::new(false));
    let worker = WorkerService::new(
        backend_engine::WorkerCapabilities {
            recipes: vec![recipe],
            max_scope: 1,
            max_resources: resources,
        },
        BlockingExecutor {
            recipe,
            entered: Arc::clone(&entered),
        },
        manifest,
        limits,
    )
    .unwrap_or_else(|error| panic!("worker: {error}"));
    let admission = CancelAdmission {
        recipe,
        read_manifest,
        authority,
        equivalence,
        input_basis: workspace.root(),
        work_key: identity.work_key(),
        semantic,
    };
    (worker, admission, request, entered, limits)
}

/// Returns two connected local streams for the streamed-cancellation test.
///
/// Unix keeps `socketpair`, which needs no filesystem entry. Windows has no
/// `socketpair` for AF_UNIX, so the pair is formed by binding a private
/// endpoint and accepting one connection; the path is unlinked as soon as both
/// ends exist.
#[cfg(unix)]
fn connected_pair() -> (backend_platform::LocalStream, backend_platform::LocalStream) {
    std::os::unix::net::UnixStream::pair()
        .unwrap_or_else(|error| panic!("UnixStream::pair: {error}"))
}

#[cfg(windows)]
fn connected_pair() -> (backend_platform::LocalStream, backend_platform::LocalStream) {
    let path = std::env::temp_dir().join(format!(
        "backend-worker-pair-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&path);
    let listener = backend_platform::LocalListener::bind(&path)
        .unwrap_or_else(|error| panic!("local pair bind: {error}"));
    let client = backend_platform::LocalStream::connect(&path)
        .unwrap_or_else(|error| panic!("local pair connect: {error}"));
    let (server, _) = listener
        .accept()
        .unwrap_or_else(|error| panic!("local pair accept: {error}"));
    drop(listener);
    let _ = std::fs::remove_file(&path);
    (server, client)
}

#[test]
fn stream_cancel_keeps_control_loop_live_and_releases_active_capacity() {
    let (mut worker, mut admission, request, entered, limits) = cancellation_fixture();
    let cancel = CancelAttempt {
        attempt: request.attempt,
        work_key: request.work_key,
        cancellation: request.cancellation,
        fence: request.fence,
    };
    let (mut server, mut client) = connected_pair();
    let client_thread = thread::spawn(move || {
        let request = crate::protocol::frame(&TransportMessage::WireRecipeRequest(request), limits)
            .unwrap_or_else(|error| panic!("request frame: {error}"));
        client
            .write_all(&request)
            .unwrap_or_else(|error| panic!("request write: {error}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !entered.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "worker execution did not start"
            );
            thread::yield_now();
        }
        let cancel = crate::protocol::frame(&TransportMessage::CancelAttempt(cancel), limits)
            .unwrap_or_else(|error| panic!("cancel frame: {error}"));
        client
            .write_all(&cancel)
            .unwrap_or_else(|error| panic!("cancel write: {error}"));
        let _ = client.shutdown(std::net::Shutdown::Write);
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .unwrap_or_else(|error| panic!("response read: {error}"));
        response
    });
    let served = worker
        .serve_stream::<CancelRelation, _, _>(&mut server, &mut admission)
        .unwrap_or_else(|error| panic!("serve stream: {error}"));
    assert_eq!(served, 1);
    assert_eq!(worker.active_jobs(), 0);
    drop(server);
    let response = client_thread
        .join()
        .unwrap_or_else(|_| panic!("client thread panicked"));
    assert!(
        response.is_empty(),
        "cancelled jobs must not publish a result"
    );
}
