//! Adversarial trust-boundary and authority transaction tests.

use super::authority::statement_id;
use super::*;
use crate::worker::{Blake3WorkerSigner, WorkerAttestationSigner};
use backend_execution::{
    AuthorityVersion, CompletionCost, CostObservation, CostSnapshot, EnvelopeBudgets,
    LocalCapability, LocalState, ObservationError, OutputEquivalence, OutputVersion,
    ReadManifestId, RecipeId, RemoteCapability, RemoteState, ResourceVector, Scheduler,
    VersionedWorkIdentity,
};
use backend_replication::{
    AttestationClass, AttestationVerifier, AuthorityEpoch, Fence, NegotiatedCapabilities,
    ReplicationError, ResourceEnvelope, RevocationVersion, SchemaDescriptor, SparseCoverage,
    TransportLimits, WireAuthority, WireIdentity, WireRecipeResult,
};
use backend_semantic::DependencyManifest;
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ObjectVersion, ProducerObservationClaims,
    ProducerObservationVerifier, Relation, RelationState, Schema, UntrustedProducerObservation,
    WorkspaceManifest, WorkspaceRoot, admit_complete_scope, admit_producer_observation,
};
use std::io::{self, Read, Write};
use std::sync::{Arc, Barrier};
use std::thread;

#[derive(Debug)]
struct TestRelation;

impl Relation for TestRelation {
    const DOMAIN: u8 = 0x71;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &u64, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &u64, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }
}

#[derive(Debug)]
struct TestObjectSchema;

struct IdleStream;

impl Read for IdleStream {
    fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::TimedOut))
    }
}

impl Write for IdleStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Schema for TestObjectSchema {
    const DOMAIN: u8 = 0x72;
    const TYPE: u16 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

#[derive(Clone, Copy, Debug)]
struct ScopeAuthority {
    binding: SemanticCoverageBinding,
    scope: u64,
    epoch: AuthorityEpoch,
    revocation: RevocationVersion,
    witness: &'static [u8],
}

impl ScopeAuthority {
    fn for_identity<R: Relation>(
        identity: &VersionedWorkIdentity<R>,
        scope: u64,
        epoch: AuthorityEpoch,
        revocation: RevocationVersion,
        witness: &'static [u8],
    ) -> Self {
        Self {
            binding: SemanticCoverageBinding::from_identity(identity),
            scope,
            epoch,
            revocation,
            witness,
        }
    }
}

impl SemanticCoverageValidator for ScopeAuthority {
    fn validate(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        if !claim.asserted_complete() {
            return Err(SemanticCoverageAdmissionError::Incomplete);
        }
        if *binding != self.binding
            || claim.scope() != self.scope
            || claim.authority_epoch() < self.epoch
            || claim.revocation_version() < self.revocation
            || claim.witness() != self.witness
        {
            return Err(SemanticCoverageAdmissionError::Rejected);
        }
        Ok(())
    }

    fn validate_manifest(
        &self,
        binding: &SemanticCoverageBinding,
        claim: &UntrustedSemanticCoverageClaim,
        manifest: &DependencyManifest,
    ) -> Result<(), SemanticCoverageAdmissionError> {
        // The fixture's empty manifest represents a complete dependency
        // closure whose selectors are already bound by the exact identity.
        // Keeping this hook explicit exercises the production rule that only
        // a manifest-backed capability may populate reusable lookup state.
        if manifest.facts().is_empty() {
            self.validate(binding, claim)
        } else {
            Err(SemanticCoverageAdmissionError::Rejected)
        }
    }
}

type TestDispatcher = Dispatcher<
    CompositeAdmissionValidator<test_util::HashOnlyOutputValidator, ScopeAuthority>,
    Blake3AuthorityVerifier,
>;

#[allow(
    clippy::unnecessary_wraps,
    reason = "the placement verifier callback ABI is fallible"
)]
fn accept_local(
    _identity: &VersionedWorkIdentity<TestRelation>,
    _state: LocalState,
) -> Result<(), ObservationError> {
    Ok(())
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "the placement verifier callback ABI is fallible"
)]
fn accept_remote(
    _identity: &VersionedWorkIdentity<TestRelation>,
    _capabilities: &NegotiatedCapabilities,
) -> Result<RemoteState, ObservationError> {
    Ok(RemoteState::Available)
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "the placement verifier callback ABI is fallible"
)]
fn accept_cost(
    _identity: &VersionedWorkIdentity<TestRelation>,
    _observation: &CostObservation,
) -> Result<(), ObservationError> {
    Ok(())
}

fn complete_witness(label: &'static [u8]) -> CoverageWitness {
    let version = ObjectVersion::<TestObjectSchema>::from_value(label);
    let declaration = AuthorityScopeClaim::from_object_version(version);
    let observation = UntrustedProducerObservation::new(
        [0x71; 32],
        declaration.scope_root(),
        [0x72; 32],
        label.to_vec(),
    );
    let verifier = ExactProducerObservation(observation.clone());
    let admitted = admit_producer_observation(observation, &verifier)
        .unwrap_or_else(|_| unreachable!("test producer evidence is linked"));
    CoverageWitness::Complete(
        admit_complete_scope(declaration, admitted)
            .unwrap_or_else(|_| unreachable!("test coverage evidence is linked")),
    )
}

#[derive(Clone, Debug)]
struct ExactProducerObservation(UntrustedProducerObservation);

impl ProducerObservationVerifier for ExactProducerObservation {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        (observation == &self.0)
            .then(|| {
                ProducerObservationClaims::new(
                    self.0.producer_identity(),
                    self.0.scope_root(),
                    self.0.context(),
                    *blake3::hash(self.0.evidence()).as_bytes(),
                )
            })
            .ok_or("test producer observation mismatch")
    }
}

fn identity(value: u64) -> (VersionedWorkIdentity<TestRelation>, WorkspaceRoot) {
    let state = RelationState::<TestRelation>::from_entries(
        [(value, value)],
        complete_witness(b"relation-scope"),
    )
    .unwrap_or_else(|_| unreachable!("test state is canonical"));
    let authority = ObjectVersion::<TestObjectSchema>::from_value(b"workspace-authority");
    let workspace = WorkspaceManifest::from_versions(
        1,
        vec![backend_version::RelationBinding::from_state(&state)],
        Vec::new(),
        authority,
        complete_witness(b"workspace-scope"),
    )
    .unwrap_or_else(|_| unreachable!("test workspace is checked"));
    let identity = VersionedWorkIdentity::new(
        RecipeId::from_value(b"recipe"),
        state.root(),
        ReadManifestId::from_value(b"read-manifest"),
        AuthorityVersion::from_value(b"authority"),
        OutputEquivalence::from_value(b"output-equivalence"),
    );
    (identity, workspace.root())
}

fn semantic_claim(
    identity: &VersionedWorkIdentity<TestRelation>,
    scope: u64,
    epoch: u64,
    revocation: u64,
    state: SemanticCoverageState,
    witness: &'static [u8],
) -> UntrustedSemanticCoverageClaim {
    UntrustedSemanticCoverageClaim::with_state(
        identity,
        scope,
        witness,
        AuthorityEpoch(epoch),
        RevocationVersion(revocation),
        state,
    )
}

#[test]
fn complete_output_bytes_do_not_admit_partial_semantics() {
    let (identity, _) = identity(1);
    let authority = ScopeAuthority::for_identity(
        &identity,
        7,
        AuthorityEpoch(3),
        RevocationVersion(4),
        b"witness",
    );
    let partial = semantic_claim(
        &identity,
        7,
        3,
        4,
        SemanticCoverageState::Partial,
        b"witness",
    );
    let result = CompleteSemanticCoverage::admit(&identity, partial, &authority);
    assert_eq!(result, Err(SemanticCoverageAdmissionError::Incomplete));

    let complete = semantic_claim(
        &identity,
        7,
        3,
        4,
        SemanticCoverageState::Complete,
        b"witness",
    );
    let capability = CompleteSemanticCoverage::admit(&identity, complete, &authority)
        .unwrap_or_else(|_| unreachable!("authority admits exact complete claim"));
    let bytes = b"complete output bytes";
    assert!(
        test_util::HashOnlyOutputValidator
            .validate(OutputVersion::from_value(bytes), bytes, &capability)
            .is_ok()
    );
}

#[test]
fn semantic_admission_rejects_wrong_scope_manifest_authority_and_revocation() {
    let (identity_value, _) = identity(2);
    let authority = ScopeAuthority::for_identity(
        &identity_value,
        7,
        AuthorityEpoch(3),
        RevocationVersion(4),
        b"witness",
    );

    let wrong_scope = semantic_claim(
        &identity_value,
        8,
        3,
        4,
        SemanticCoverageState::Complete,
        b"witness",
    );
    assert_eq!(
        CompleteSemanticCoverage::admit(&identity_value, wrong_scope, &authority),
        Err(SemanticCoverageAdmissionError::Rejected)
    );

    let (other_read, _) = identity(3);
    let wrong_manifest = semantic_claim(
        &other_read,
        7,
        3,
        4,
        SemanticCoverageState::Complete,
        b"witness",
    );
    assert_eq!(
        CompleteSemanticCoverage::admit(&identity_value, wrong_manifest, &authority),
        Err(SemanticCoverageAdmissionError::BindingMismatch)
    );

    let mut wrong_authority_identity = identity_value;
    wrong_authority_identity.authority = AuthorityVersion::from_value(b"other-authority");
    let wrong_authority = semantic_claim(
        &wrong_authority_identity,
        7,
        3,
        4,
        SemanticCoverageState::Complete,
        b"witness",
    );
    assert_eq!(
        CompleteSemanticCoverage::admit(&identity_value, wrong_authority, &authority),
        Err(SemanticCoverageAdmissionError::BindingMismatch)
    );

    let stale = semantic_claim(
        &identity_value,
        7,
        2,
        3,
        SemanticCoverageState::Complete,
        b"witness",
    );
    assert_eq!(
        CompleteSemanticCoverage::admit(&identity_value, stale, &authority),
        Err(SemanticCoverageAdmissionError::Rejected)
    );
}

#[test]
fn authority_rotation_is_rejected_before_any_remote_reservation() {
    let (dispatcher, plan, contract, wire) = remote_fixture(b"rotated authority");

    let mut stale_epoch = contract.clone();
    stale_epoch.authority_epoch = AuthorityEpoch(2);
    assert!(matches!(
        dispatcher.admit_remote(&plan, &wire, &stale_epoch),
        Err(DispatchError::IncompleteSemanticCoverage)
    ));

    let mut stale_revocation = contract.clone();
    stale_revocation.revocation_version = RevocationVersion(3);
    assert!(matches!(
        dispatcher.admit_remote(&plan, &wire, &stale_revocation),
        Err(DispatchError::IncompleteSemanticCoverage)
    ));

    assert!(
        dispatcher
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    assert!(dispatcher.pending_authority.is_empty());
    assert!(
        dispatcher
            .replayed
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    dispatcher.cancel(plan);
}

fn request_for(
    identity: VersionedWorkIdentity<TestRelation>,
    local_state: LocalState,
    class: backend_execution::PlacementClass,
    pure: bool,
    hedge: bool,
) -> ScheduleRequest<TestRelation> {
    let local = LocalCapability::admit(&identity, local_state, 0, 100, &accept_local)
        .unwrap_or_else(|_| unreachable!("test local observation"));
    let remote = RemoteCapability::admit(
        &identity,
        &NegotiatedCapabilities {
            protocol: 1,
            schemas: vec![SchemaDescriptor::of::<SemanticCoverageSchema>()],
            recipes: Vec::new(),
            limits: TransportLimits::default(),
            max_resources: ResourceEnvelope::UNBOUNDED,
        },
        0,
        100,
        &accept_remote,
    )
    .unwrap_or_else(|_| unreachable!("test remote observation"));
    let costs = CostSnapshot::admit(
        &identity,
        CostObservation {
            local: 1,
            remote: CompletionCost::default(),
            observed_at: 0,
            expires_at: 100,
            confidence_per_mille: 1_000,
        },
        &accept_cost,
    )
    .unwrap_or_else(|_| unreachable!("test cost observation"));
    ScheduleRequest::new(identity, 1, 1, class, local, remote, costs)
        .with_charge(32, ResourceVector::zero())
        .pure(pure)
        .hedge(hedge)
}

fn remote_request(identity: VersionedWorkIdentity<TestRelation>) -> ScheduleRequest<TestRelation> {
    request_for(
        identity,
        LocalState::Unavailable,
        backend_execution::PlacementClass::RemoteRequired,
        false,
        false,
    )
}

fn remote_fixture_with_request(
    bytes: &'static [u8],
    request: impl FnOnce(VersionedWorkIdentity<TestRelation>) -> ScheduleRequest<TestRelation>,
) -> (
    TestDispatcher,
    DispatchPlan<TestRelation>,
    RemoteDispatchContract,
    WireRecipeResult,
) {
    let (identity, workspace_root) = identity(4);
    let semantic_authority = ScopeAuthority::for_identity(
        &identity,
        7,
        AuthorityEpoch(3),
        RevocationVersion(4),
        b"witness",
    );
    let claim = semantic_claim(
        &identity,
        7,
        3,
        4,
        SemanticCoverageState::Complete,
        b"witness",
    );
    let semantic = CompleteSemanticCoverage::admit_with_manifest(
        &identity,
        claim,
        DependencyManifest::new(Vec::new())
            .unwrap_or_else(|_| unreachable!("empty dependency manifest is canonical")),
        &semantic_authority,
    )
    .unwrap_or_else(|_| unreachable!("test authority admits exact claim and manifest"));
    let validator =
        CompositeAdmissionValidator::new(test_util::HashOnlyOutputValidator, semantic_authority);
    let authority_bytes = identity.authority.to_bytes();
    let mut verifier = Blake3AuthorityVerifier::new();
    verifier.insert_key(authority_bytes, [0x42; 32]);
    let signer = verifier.clone();
    let dispatcher = Dispatcher::with_scheduler(
        Scheduler::with_envelopes(EnvelopeBudgets::uniform(Budget {
            operations: 2,
            bytes: 16 * 1024,
            hedges: 1,
            ..Budget::zero()
        })),
        validator,
        verifier,
        RemoteAuthorityPolicy::Signed,
    );
    let plan = dispatcher
        .plan(request(identity))
        .unwrap_or_else(|error| panic!("test remote route: {error:?}"));
    let scheduled = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let semantic_wire = semantic.wire();
    let contract = RemoteDispatchContract {
        input_basis: workspace_root,
        inputs: Vec::new(),
        semantic,
        resources: ResourceEnvelope::zero(),
        authority_epoch: AuthorityEpoch(3),
        revocation_version: RevocationVersion(4),
        authority_policy: RemoteAuthorityPolicy::Signed,
        limits: TransportLimits::default(),
        expected_receipt: None,
    };
    let expected = request_expectation(scheduled, &contract)
        .unwrap_or_else(|_| unreachable!("test request expectation"));
    let request = wire_request(scheduled, &contract, &expected);
    let output = OutputVersion::from_value(bytes);
    let receipt = worker_receipt_id(&request, output, bytes);
    let mut wire = WireRecipeResult {
        attempt: expected.attempt,
        recipe: request.recipe,
        work_key: request.work_key,
        input_basis: request.input_basis,
        inputs: request.inputs,
        read_manifest: request.read_manifest,
        output: WireIdentity::from_typed(&output),
        output_bytes: Arc::new(bytes.to_vec()),
        scope: request.scope,
        resources: request.resources,
        byte_coverage: SparseCoverage::complete(bytes.len() as u64)
            .unwrap_or_else(|_| unreachable!("nonempty test output")),
        semantic_coverage: semantic_wire,
        authority: WireAuthority {
            id: request.authority.id,
            epoch: AuthorityEpoch(3),
        },
        authority_policy: request.authority,
        revocation_version: RevocationVersion(4),
        fence: request.fence,
        cancellation: request.cancellation,
        attestation: None,
        receipt: WireIdentity::from_typed(&receipt),
    };
    wire.attestation = signer.sign(authority_bytes, &wire.attestation_material());
    (dispatcher, plan, contract, wire)
}

fn remote_fixture(
    bytes: &'static [u8],
) -> (
    TestDispatcher,
    DispatchPlan<TestRelation>,
    RemoteDispatchContract,
    WireRecipeResult,
) {
    remote_fixture_with_request(bytes, remote_request)
}

static LARGE_REMOTE_OUTPUT: [u8; 8192] = [0x5a; 8192];

#[test]
fn worker_receipt_commits_complete_request_material() {
    let (_dispatcher, plan, contract, wire) = remote_fixture(b"receipt binding");
    let scheduled = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh fixture"),
    };
    let expected = request_expectation(scheduled, &contract)
        .unwrap_or_else(|_| unreachable!("fixture request expectation"));
    let request = wire_request(scheduled, &contract, &expected);
    let output = OutputVersion::from_value(wire.output_bytes.as_slice());
    let original = worker_receipt_id(&request, output, &wire.output_bytes);

    let mut changed_basis = request.clone();
    let mut basis = changed_basis.input_basis.as_bytes();
    basis[0] ^= 1;
    changed_basis.input_basis = backend_replication::WorkspaceRootClaim::from_bytes(basis);
    assert_ne!(
        original,
        worker_receipt_id(&changed_basis, output, &wire.output_bytes)
    );

    let mut changed_authority = request.clone();
    changed_authority.authority.minimum_epoch.0 += 1;
    assert_ne!(
        original,
        worker_receipt_id(&changed_authority, output, &wire.output_bytes)
    );

    let mut changed_inputs = request;
    changed_inputs.inputs.push(changed_inputs.recipe);
    assert_ne!(
        original,
        worker_receipt_id(&changed_inputs, output, &wire.output_bytes)
    );
}

#[test]
fn dispatch_ticket_owns_exact_remote_correlation() {
    let (dispatcher, plan, contract, wire) = remote_fixture(b"ticket output");
    let ticket = dispatcher
        .dispatch_ticket(plan, contract)
        .unwrap_or_else(|error| panic!("ticket admission: {error:?}"));
    assert_eq!(ticket.attempt().unwrap(), wire.attempt);
    assert_eq!(ticket.fence().unwrap(), wire.fence);
    assert_eq!(ticket.cancellation().unwrap(), wire.cancellation);
    assert_eq!(ticket.wire_request().unwrap().work_key, wire.work_key);
    let completion = dispatcher
        .complete_remote_ticket(ticket, wire, 1)
        .unwrap_or_else(|error| panic!("ticket completion: {error:?}"));
    assert!(matches!(completion, DispatchCompletion::Accepted(_)));
}

#[test]
fn consumed_ticket_returns_typed_state_error() {
    let (dispatcher, plan, contract, _wire) = remote_fixture(b"consumed ticket");
    let mut ticket = dispatcher
        .dispatch_ticket(plan, contract)
        .unwrap_or_else(|error| panic!("ticket admission: {error:?}"));
    let (plan, _contract, _expected, _cancellation) = ticket
        .take_parts()
        .unwrap_or_else(|error| panic!("ticket extraction: {error:?}"));
    dispatcher.cancel(plan);
    assert!(matches!(
        ticket.take_parts(),
        Err(DispatchError::TicketConsumed)
    ));
}

#[test]
fn pending_envelope_rejects_bad_fence_before_completion() {
    let (dispatcher, plan, contract, mut wire) = remote_fixture(b"pending output");
    let ticket = dispatcher
        .dispatch_ticket(plan, contract)
        .unwrap_or_else(|error| panic!("ticket admission: {error:?}"));
    let (mut pending, request) = dispatcher
        .begin_remote(ticket)
        .unwrap_or_else(|error| panic!("begin remote: {error:?}"));
    assert_eq!(pending.key().attempt(), request.attempt);
    let expected_fence = wire.fence;
    wire.fence = Fence::from_u64(0xfeed).unwrap_or_else(|_| unreachable!("nonzero fence"));
    assert!(!pending.matches(&wire));
    assert!(matches!(
        pending.complete(&dispatcher, wire.clone(), 1),
        Err(DispatchError::TicketMismatch)
    ));
    wire.fence = expected_fence;
    assert!(pending.matches(&wire));
}

#[test]
fn borrowed_ticket_retries_after_correlated_invalid_frame() {
    let (dispatcher, plan, contract, mut wire) = remote_fixture(b"ticket retry");
    let mut ticket = dispatcher
        .dispatch_ticket(plan, contract)
        .unwrap_or_else(|error| panic!("ticket admission: {error:?}"));
    let valid = wire.clone();
    wire.fence = Fence::from_u64(0xbeef).unwrap_or_else(|_| unreachable!("nonzero fence"));
    assert!(matches!(
        dispatcher.complete_remote_ticket_checked(&mut ticket, wire, 1),
        Err(DispatchError::TicketMismatch)
    ));
    let completion = dispatcher
        .complete_remote_ticket_checked(&mut ticket, valid, 1)
        .unwrap_or_else(|error| panic!("ticket retry completion: {error:?}"));
    assert!(matches!(completion, DispatchCompletion::Accepted(_)));
}

#[test]
fn pending_envelope_retains_ticket_after_invalid_attestation() {
    let (dispatcher, plan, contract, mut wire) = remote_fixture(b"attestation retry");
    let ticket = dispatcher
        .dispatch_ticket(plan, contract)
        .unwrap_or_else(|error| panic!("ticket admission: {error:?}"));
    let (mut pending, _) = dispatcher
        .begin_remote(ticket)
        .unwrap_or_else(|error| panic!("begin remote: {error:?}"));
    let valid = wire.clone();
    let mut attestation = wire
        .attestation
        .unwrap_or_else(|| unreachable!("fixture signs the result"));
    attestation.0[0] ^= 0x80;
    wire.attestation = Some(attestation);
    assert!(matches!(
        pending.complete(&dispatcher, wire, 1),
        Err(DispatchError::Replication(
            ReplicationError::InvalidAttestation
        ))
    ));
    assert!(pending.matches(&valid));
    pending.cancel();
}

#[test]
fn dropping_remote_ticket_releases_active_semantic_registration() {
    let (dispatcher, plan, contract, _wire) = remote_fixture(b"ticket reservation");
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    dispatcher.cancel(plan);
    let semantic = contract.semantic.clone();
    let plan = dispatcher
        .plan_with_semantic(remote_request(identity), &semantic)
        .unwrap_or_else(|error| panic!("semantic plan: {error:?}"));
    assert_eq!(dispatcher.semantic.registration_count(), 1);
    let ticket = dispatcher
        .dispatch_ticket(plan, contract)
        .unwrap_or_else(|error| panic!("ticket admission: {error:?}"));
    drop(ticket);
    assert_eq!(dispatcher.semantic.registration_count(), 0);
}

#[test]
fn remote_ticket_rejects_a_local_only_schedule() {
    let (dispatcher, plan, contract, _wire) =
        remote_fixture_with_request(b"local route", |identity| {
            request_for(
                identity,
                LocalState::Ready,
                backend_execution::PlacementClass::LocalPreferred,
                false,
                false,
            )
        });
    assert!(matches!(
        &plan,
        DispatchPlan::Scheduled(schedule)
            if schedule.decision() == backend_execution::PlacementDecision::Local
    ));
    assert!(matches!(
        dispatcher.dispatch_ticket(plan, contract),
        Err(DispatchError::AuthorityRejected)
    ));
}

#[test]
fn owned_remote_admission_retains_large_output_buffer() {
    let (dispatcher, plan, contract, wire) = remote_fixture(&LARGE_REMOTE_OUTPUT);
    let source = wire.output_bytes.as_ptr();
    let statement = authority::statement_id_view(&wire.attestation_material_view());
    let admission = dispatcher
        .admit_remote_owned(&plan, wire, &contract)
        .unwrap_or_else(|_| unreachable!("large signed result admission"));
    assert_eq!(admission.wire().output_bytes.as_ptr(), source);
    assert!(
        dispatcher
            .replayed
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&statement)
    );
    drop(admission);
    dispatcher.cancel(plan);
}

#[test]
fn accepted_output_bytes_survive_wire_drop_and_reuse() {
    let (dispatcher, plan, contract, wire) = remote_fixture(&LARGE_REMOTE_OUTPUT);
    let source = wire.output_bytes.as_ptr();
    let source_owner = Arc::clone(&wire.output_bytes);
    let source_weak = Arc::downgrade(&source_owner);
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let completion = dispatcher
        .complete_remote_wire_owned(plan, wire, &contract, 1)
        .unwrap_or_else(|error| panic!("signed result completion: {error:?}"));
    let DispatchCompletion::Accepted(receipt) = completion else {
        unreachable!("fresh remote route must publish")
    };
    let receipt_owner = receipt
        .reusable()
        .unwrap_or_else(|| unreachable!("manifest-backed receipt is reusable"))
        .canonical_bytes_arc();
    assert!(Arc::ptr_eq(&receipt_owner, &source_owner));
    let retained_context = receipt
        .reusable()
        .unwrap_or_else(|| unreachable!("manifest-backed receipt is reusable"))
        .context();
    let retained = dispatcher
        .scheduler
        .output_lookup()
        .lookup(identity.work_key(), &retained_context)
        .unwrap_or_else(|| unreachable!("accepted output is retained"));
    assert!(Arc::ptr_eq(&retained.canonical_bytes_arc(), &source_owner));
    assert_eq!(retained.canonical_bytes().as_ptr(), source);
    assert_eq!(retained.canonical_bytes(), LARGE_REMOTE_OUTPUT.as_slice());
    drop(retained);

    let reused_plan = dispatcher
        .plan_with_semantic(remote_request(identity), &contract.semantic)
        .unwrap_or_else(|error| panic!("accepted output should be reusable: {error:?}"));
    let reused = match &reused_plan {
        DispatchPlan::Reused(reused) => reused,
        DispatchPlan::Scheduled(_) | DispatchPlan::Waiting(_) => {
            unreachable!("the exact retained output must be selected")
        }
    };
    assert!(Arc::ptr_eq(&reused.canonical_bytes_arc(), &source_owner));
    assert_eq!(reused.canonical_bytes().as_ptr(), source);
    let reused_completion = dispatcher
        .complete_local(reused_plan, b"ignored", contract.semantic, 1)
        .unwrap_or_else(|error| panic!("reused completion: {error:?}"));
    let DispatchCompletion::Reused(reused) = reused_completion else {
        unreachable!("reused completion must carry the retained output");
    };
    assert!(Arc::ptr_eq(&reused.canonical_bytes_arc(), &source_owner));
    assert_eq!(reused.canonical_bytes().as_ptr(), source);
    drop(reused);
    drop(receipt_owner);
    drop(receipt);
    drop(source_owner);
    assert!(source_weak.upgrade().is_some());

    // Dispatcher-owned invalidation retires the durable publication proof as
    // well as the lower lookup entry. Calling the lower index directly would
    // leave the daemon hand-off owner alive by design.
    dispatcher.revoke_published_output(identity.work_key());
    assert!(dispatcher.scheduler.output_lookup().is_empty());
    assert!(source_weak.upgrade().is_none());
}

#[test]
fn claim_only_semantic_completion_is_ephemeral_and_cannot_reuse() {
    let (dispatcher, plan, mut contract, wire) = remote_fixture(b"ephemeral output");
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    // Re-admit the exact producer claim without its dependency manifest. It is
    // still sufficient for this one completion, but the dispatcher must keep
    // it out of the reusable lookup.
    contract.semantic = dispatcher
        .admit_semantic(&identity, contract.semantic.claim().clone())
        .unwrap_or_else(|error| panic!("claim-only admission: {error:?}"));
    let completion = dispatcher
        .complete_remote_wire_owned(plan, wire, &contract, 1)
        .unwrap_or_else(|error| panic!("claim-only completion: {error:?}"));
    assert!(matches!(completion, DispatchCompletion::Accepted(_)));
    assert!(dispatcher.scheduler.output_lookup().is_empty());

    let replacement = dispatcher
        .plan(remote_request(identity))
        .unwrap_or_else(|error| panic!("claim-only result must not be reused: {error:?}"));
    assert!(matches!(replacement, DispatchPlan::Scheduled(_)));
    dispatcher.cancel(replacement);
}

#[test]
fn newer_revocation_for_stable_authority_invalidates_reuse_context() {
    let (dispatcher, plan, contract, wire) = remote_fixture(b"freshness output");
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    dispatcher
        .complete_remote_wire_owned(plan, wire, &contract, 1)
        .unwrap_or_else(|error| panic!("initial publication: {error:?}"));
    assert_eq!(dispatcher.scheduler.output_lookup().len(), 1);

    // The authority version remains stable while its revocation observation
    // advances. Admission of that engine-validated claim retires the old
    // output before a scheduler lookup can reuse it.
    let newer_claim = semantic_claim(
        &identity,
        7,
        3,
        5,
        SemanticCoverageState::Complete,
        b"witness",
    );
    let _newer_semantic = dispatcher
        .admit_semantic(&identity, newer_claim)
        .unwrap_or_else(|error| panic!("new revocation admission: {error:?}"));
    assert!(dispatcher.scheduler.output_lookup().is_empty());
    let replacement = dispatcher
        .plan(remote_request(identity))
        .unwrap_or_else(|error| panic!("fresh authority must schedule: {error:?}"));
    assert!(matches!(replacement, DispatchPlan::Scheduled(_)));
    dispatcher.cancel(replacement);
}

#[test]
fn explicit_authority_transition_fences_durable_and_hot_reuse() {
    let (dispatcher, plan, contract, wire) = remote_fixture(b"transition output");
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let old_claim = contract.semantic.claim().clone();
    dispatcher
        .complete_remote_wire_owned(plan, wire, &contract, 1)
        .unwrap_or_else(|error| panic!("initial publication: {error:?}"));
    assert_eq!(dispatcher.scheduler.output_lookup().len(), 1);

    let transition = dispatcher
        .admit_authority_transition(identity.authority, AuthorityEpoch(4), RevocationVersion(5))
        .unwrap_or_else(|error| panic!("authority transition: {error:?}"));
    assert_eq!(transition.affected_work_keys(), &[identity.work_key()]);
    assert!(dispatcher.scheduler.output_lookup().is_empty());
    assert!(dispatcher.admit_semantic(&identity, old_claim).is_err());
    assert!(matches!(
        dispatcher.admit_authority_transition(
            identity.authority,
            AuthorityEpoch(3),
            RevocationVersion(4),
        ),
        Err(DispatchError::AuthorityRejected)
    ));
}

#[test]
fn remote_attestation_is_required_and_tamper_evident() {
    let (dispatcher, plan, contract, mut wire) = remote_fixture(b"signed output");
    wire.attestation = None;
    assert!(matches!(
        dispatcher.admit_remote(&plan, &wire, &contract),
        Err(DispatchError::Replication(
            ReplicationError::AttestationRequired
        ))
    ));
    dispatcher.cancel(plan);

    let (dispatcher, plan, contract, mut wire) = remote_fixture(b"signed output");
    let mut attestation = wire
        .attestation
        .unwrap_or_else(|| unreachable!("fixture is signed"));
    attestation.0[0] ^= 1;
    wire.attestation = Some(attestation);
    assert!(matches!(
        dispatcher.admit_remote(&plan, &wire, &contract),
        Err(DispatchError::Replication(
            ReplicationError::InvalidAttestation
        ))
    ));
    dispatcher.cancel(plan);
}

#[test]
fn worker_and_owner_share_authority_statement_grammar() {
    let (dispatcher, plan, contract, mut wire) = remote_fixture(b"shared grammar");
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let authority_bytes = identity.authority.to_bytes();
    let secret = [0x42; 32];
    let worker = Blake3WorkerSigner::new(authority_bytes, secret);
    let mut owner = Blake3AuthorityVerifier::new();
    owner.insert_key(authority_bytes, secret);
    let worker_signature = worker
        .sign_view(&wire.attestation_material_view())
        .unwrap_or_else(|_| unreachable!("configured worker signer"));
    let owner_signature = owner
        .sign(authority_bytes, &wire.attestation_material())
        .unwrap_or_else(|| unreachable!("configured owner verifier signer"));
    assert_eq!(worker_signature, owner_signature);
    wire.output_bytes = Arc::new(b"tampered shared grammar".to_vec());
    assert_ne!(
        worker_signature,
        worker
            .sign_view(&wire.attestation_material_view())
            .unwrap_or_else(|_| unreachable!("configured worker signer"))
    );
    assert!(
        owner
            .verify(Some(worker_signature), &wire.attestation_material(),)
            .is_err()
    );
    dispatcher.cancel(plan);
    drop(contract);
}

#[test]
fn remote_replay_and_disagreement_are_rejected() {
    let (dispatcher, plan, contract, wire) = remote_fixture(b"first output");
    let first = dispatcher
        .admit_remote(&plan, &wire, &contract)
        .unwrap_or_else(|_| unreachable!("first signed result"));
    assert_eq!(first.authority_class(), AttestationClass::TrustedSigned);
    assert!(matches!(
        dispatcher.admit_remote(&plan, &wire, &contract),
        Err(DispatchError::AuthorityRejected)
    ));

    let (dispatcher, plan, contract, wire) = remote_fixture(b"first output");
    let first = dispatcher
        .admit_remote(&plan, &wire, &contract)
        .unwrap_or_else(|_| unreachable!("first signed result"));
    let mut disagreement = wire;
    disagreement.output_bytes = Arc::new(b"different output".to_vec());
    disagreement.byte_coverage = SparseCoverage::complete(disagreement.output_bytes.len() as u64)
        .unwrap_or_else(|_| unreachable!("nonempty disagreement output"));
    let output = OutputVersion::from_value(disagreement.output_bytes.as_slice());
    let scheduled = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let expected = request_expectation(scheduled, &contract)
        .unwrap_or_else(|_| unreachable!("test request expectation"));
    let request = wire_request(scheduled, &contract, &expected);
    disagreement.output = WireIdentity::from_typed(&output);
    disagreement.receipt = WireIdentity::from_typed(&worker_receipt_id(
        &request,
        output,
        disagreement.output_bytes.as_slice(),
    ));
    disagreement.attestation = None;
    let signer = {
        let mut verifier = Blake3AuthorityVerifier::new();
        verifier.insert_key(scheduled.identity().authority.to_bytes(), [0x42; 32]);
        verifier
    };
    disagreement.attestation = signer.sign(
        scheduled.identity().authority.to_bytes(),
        &disagreement.attestation_material(),
    );
    let disagreement_result = dispatcher.admit_remote(&plan, &disagreement, &contract);
    assert!(
        matches!(&disagreement_result, Err(DispatchError::AuthorityConflict)),
        "unexpected disagreement result: {disagreement_result:?}"
    );
    assert!(
        dispatcher
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    drop(first);
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    dispatcher.cancel(plan);
    let replacement = dispatcher
        .plan(remote_request(identity))
        .unwrap_or_else(|error| panic!("replacement route: {error:?}"));
    assert!(matches!(replacement, DispatchPlan::Scheduled(_)));
    dispatcher.cancel(replacement);
}

#[test]
fn remote_stale_fence_and_cancellation_block_publication() {
    let (dispatcher, plan, contract, mut wire) = remote_fixture(b"fenced output");
    wire.fence = Fence::from_u64(99).unwrap_or_else(|_| unreachable!("nonzero test fence"));
    assert!(matches!(
        dispatcher.admit_remote(&plan, &wire, &contract),
        Err(DispatchError::Replication(ReplicationError::StaleFence))
    ));
    dispatcher.cancel(plan);

    let (dispatcher, plan, contract, wire) = remote_fixture(b"cancelled output");
    let cancellation = dispatcher
        .cancellation(&plan, HedgeSide::Remote)
        .unwrap_or_else(|| unreachable!("remote route has cancellation"));
    dispatcher.cancel(plan);
    assert!(cancellation.is_cancelled());
    // A cancellation test retains the wire/contract to make clear that
    // the publication attempt has been abandoned with the affine plan.
    let _ = (contract, wire);
}

#[test]
fn scheduler_failure_rolls_back_replay_and_authority_reservations() {
    let (dispatcher, plan, contract, wire) = remote_fixture(b"expired output");
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let statement = statement_id(&wire.attestation_material());
    let admission = dispatcher
        .admit_remote(&plan, &wire, &contract)
        .unwrap_or_else(|_| unreachable!("signed result admission"));

    // The fixture's owner clock is one and the default attempt lease expires
    // at two. Scheduler rejection occurs after authority admission but before
    // publication, so the RAII reservation must release every pending entry.
    assert!(matches!(
        dispatcher.complete_remote(plan, admission, 2),
        Err(DispatchError::Schedule(
            backend_execution::ScheduleError::Attempt(backend_execution::AttemptError::Expired)
        ))
    ));
    assert!(
        dispatcher
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    assert!(dispatcher.pending_authority.is_empty());
    assert!(
        dispatcher
            .replayed
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    assert!(
        !dispatcher
            .replayed
            .committed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&statement)
    );

    let replacement = dispatcher
        .plan(remote_request(identity))
        .unwrap_or_else(|error| panic!("failed attempt must not poison reuse: {error:?}"));
    assert!(matches!(replacement, DispatchPlan::Scheduled(_)));
    dispatcher.cancel(replacement);
}

#[test]
fn successful_remote_completion_commits_authority_and_replay() {
    let (dispatcher, plan, contract, wire) = remote_fixture(b"accepted output");
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let statement = statement_id(&wire.attestation_material());
    let admission = dispatcher
        .admit_remote(&plan, &wire, &contract)
        .unwrap_or_else(|_| unreachable!("signed result admission"));
    assert!(
        dispatcher
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    let completion = dispatcher
        .complete_remote(plan, admission, 1)
        .unwrap_or_else(|error| panic!("signed result completion: {error:?}"));
    assert!(matches!(completion, DispatchCompletion::Accepted(_)));
    assert!(
        dispatcher
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&identity.work_key())
    );
    assert!(
        dispatcher
            .replayed
            .committed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&statement)
    );
    assert!(dispatcher.pending_authority.is_empty());
}

#[test]
fn racing_conflict_cannot_publish_or_poison_reuse() {
    let (dispatcher, plan, contract, wire) = remote_fixture(b"racing output");
    let identity = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule.identity(),
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let scheduled = match &plan {
        DispatchPlan::Scheduled(schedule) => schedule,
        DispatchPlan::Reused(_) | DispatchPlan::Waiting(_) => unreachable!("fresh test identity"),
    };
    let expected = request_expectation(scheduled, &contract)
        .unwrap_or_else(|_| unreachable!("test request expectation"));
    let request = wire_request(scheduled, &contract, &expected);

    let mut disagreement = wire.clone();
    disagreement.output_bytes = Arc::new(b"racing disagreement".to_vec());
    disagreement.byte_coverage = SparseCoverage::complete(disagreement.output_bytes.len() as u64)
        .unwrap_or_else(|_| unreachable!("nonempty disagreement output"));
    let disagreement_output = OutputVersion::from_value(disagreement.output_bytes.as_slice());
    disagreement.output = WireIdentity::from_typed(&disagreement_output);
    disagreement.receipt = WireIdentity::from_typed(&worker_receipt_id(
        &request,
        disagreement_output,
        disagreement.output_bytes.as_slice(),
    ));
    let mut signer = Blake3AuthorityVerifier::new();
    signer.insert_key(identity.authority.to_bytes(), [0x42; 32]);
    disagreement.attestation = signer.sign(
        identity.authority.to_bytes(),
        &disagreement.attestation_material(),
    );

    let dispatcher = Arc::new(dispatcher);
    let plan = Arc::new(plan);
    let barrier = Arc::new(Barrier::new(2));
    let exact_thread = {
        let dispatcher = Arc::clone(&dispatcher);
        let plan = Arc::clone(&plan);
        let contract = contract.clone();
        let wire = wire.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            dispatcher.admit_remote(&plan, &wire, &contract)
        })
    };
    let disagreement_thread = {
        let dispatcher = Arc::clone(&dispatcher);
        let plan = Arc::clone(&plan);
        let contract = contract.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            dispatcher.admit_remote(&plan, &disagreement, &contract)
        })
    };
    let exact = exact_thread
        .join()
        .unwrap_or_else(|_| unreachable!("exact admission thread completed"));
    let disagreement = disagreement_thread
        .join()
        .unwrap_or_else(|_| unreachable!("disagreement admission thread completed"));

    assert!(matches!(
        (&exact, &disagreement),
        (Ok(_), Err(DispatchError::AuthorityConflict))
            | (Err(DispatchError::AuthorityConflict), Ok(_)),
    ));
    assert!(
        dispatcher
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
    assert!(dispatcher.pending_authority.is_empty());
    let evidence = dispatcher
        .authority_quarantine
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&identity.work_key())
        .copied()
        .unwrap_or_else(|| panic!("conflicting authority evidence missing"));
    assert_ne!(evidence.first_output, evidence.second_output);
    assert_ne!(evidence.first_statement, evidence.second_statement);

    let plan = Arc::try_unwrap(plan)
        .unwrap_or_else(|_| unreachable!("admission threads released their plan handles"));
    if let Ok(admission) = exact {
        assert!(matches!(
            dispatcher.complete_remote(plan, admission, 1),
            Err(DispatchError::AuthorityConflict | DispatchError::Schedule(_))
        ));
    } else if let Ok(admission) = disagreement {
        assert!(matches!(
            dispatcher.complete_remote(plan, admission, 1),
            Err(DispatchError::AuthorityConflict | DispatchError::Schedule(_))
        ));
    } else {
        panic!("one candidate must retain an admission for the fenced completion check");
    }
    assert!(matches!(
        dispatcher.plan(remote_request(identity)),
        Err(DispatchError::AuthorityConflict)
    ));
}

#[test]
fn route_cost_owner_is_cold_confident_bounded_and_tail_aware() {
    let (identity, _) = identity(90);
    let model = cost::RouteCostModel::new();
    let cold = model
        .snapshot(&identity, 1_024, LocalState::Ready, RemoteState::Warm, 0)
        .expect("cold owner snapshot");
    assert_eq!(cold.confidence_per_mille(), 0);
    assert!(!cold.usable_for(identity.work_key(), 0));

    model.record_local(&identity, 1_024, RemoteState::Warm, 0, 1);
    let first_exploration = model
        .snapshot(&identity, 1_024, LocalState::Ready, RemoteState::Warm, 1)
        .expect("first local completion opens one bounded exploration");
    assert_eq!(first_exploration.local(), 1);
    assert!(first_exploration.usable_for(identity.work_key(), 1));
    assert!(first_exploration.remote().total() < first_exploration.local());

    for tick in 2..=5 {
        model.record_local(&identity, 1_024, RemoteState::Warm, 100, tick);
    }
    let exploratory = model
        .snapshot(&identity, 1_024, LocalState::Ready, RemoteState::Warm, 5)
        .expect("exploratory owner snapshot");
    assert!(exploratory.usable_for(identity.work_key(), 5));
    assert!(exploratory.remote().total() < exploratory.local());

    for tick in 6..=10 {
        model.record_remote(
            &identity,
            1_024,
            LocalState::Ready,
            RemoteState::Warm,
            CompletionCost {
                execution: 20,
                validation: 3,
                ..CompletionCost::default()
            },
            tick,
        );
    }
    let learned = model
        .snapshot(&identity, 1_024, LocalState::Ready, RemoteState::Warm, 10)
        .expect("learned owner snapshot");
    assert!(learned.usable_for(identity.work_key(), 10));
    assert_eq!(learned.remote_p95(), 23);
    assert!(learned.remote().total() < learned.local());

    let expiry_tick = 10 + cost::OBSERVATION_TTL_TICKS;
    let expired = model
        .snapshot(
            &identity,
            1_024,
            LocalState::Ready,
            RemoteState::Warm,
            expiry_tick,
        )
        .expect("expired owner snapshot");
    assert!(!expired.usable_for(identity.work_key(), expiry_tick));

    for seed in 0..512_u64 {
        let mut distinct = identity;
        distinct.recipe = RecipeId::from_value(&seed.to_be_bytes());
        model.record_local(&distinct, 1_024, RemoteState::Warm, 10, seed + 2_000);
    }
    assert!(model.class_count() <= 256);
}

#[test]
fn stream_transport_treats_an_idle_read_as_pending_work() {
    let mut transport = StreamTransport::new(IdleStream, TransportLimits::default())
        .expect("construct bounded stream transport");
    assert_eq!(RemoteTransport::recv(&mut transport), Ok(None));
}

#[test]
fn warm_route_inherits_the_available_prior_until_it_has_exact_samples() {
    let (identity, _) = identity(91);
    let model = cost::RouteCostModel::new();
    model.record_local(&identity, 1_024, RemoteState::Available, 100, 1);
    model.record_remote(
        &identity,
        1_024,
        LocalState::Ready,
        RemoteState::Available,
        CompletionCost {
            execution: 40,
            validation: 5,
            ..CompletionCost::default()
        },
        2,
    );

    let inherited = model
        .snapshot(&identity, 1_024, LocalState::Ready, RemoteState::Warm, 3)
        .expect("warm route inherits available evidence");
    assert!(inherited.usable_for(identity.work_key(), 3));
    assert!(inherited.remote().total() < inherited.local());

    model.record_remote(
        &identity,
        1_024,
        LocalState::Ready,
        RemoteState::Warm,
        CompletionCost {
            execution: 30,
            ..CompletionCost::default()
        },
        4,
    );
    let exact = model
        .snapshot(&identity, 1_024, LocalState::Ready, RemoteState::Warm, 4)
        .expect("exact warm evidence");
    assert_eq!(exact.local(), 100);
    assert!(exact.usable_for(identity.work_key(), 4));

    model.record_local(&identity, 1_024, RemoteState::Warm, 7, 5);
    let blended = model
        .snapshot(&identity, 1_024, LocalState::Ready, RemoteState::Warm, 5)
        .expect("local evidence is shared across remote locality states");
    assert!(blended.local() < 100);
    assert!(blended.local() >= 7);
}

#[test]
fn remote_circuit_accumulates_failures_and_reopens_after_its_window() {
    let (identity, _) = identity(92);
    let model = cost::RouteCostModel::new();
    model.record_local(&identity, 1_024, RemoteState::Available, 100, 1);
    for now in 2..=4 {
        model.record_remote_failure(
            &identity,
            1_024,
            LocalState::Ready,
            RemoteState::Available,
            now,
        );
    }
    let open = model
        .snapshot(
            &identity,
            1_024,
            LocalState::Ready,
            RemoteState::Available,
            4,
        )
        .expect("open circuit snapshot");
    assert_eq!(open.confidence_per_mille(), 0);
    assert_eq!(open.remote().total(), u64::MAX);

    let recovered = model
        .snapshot(
            &identity,
            1_024,
            LocalState::Ready,
            RemoteState::Available,
            1_005,
        )
        .expect("recovered circuit snapshot");
    assert!(recovered.usable_for(identity.work_key(), 1_005));
    assert!(recovered.remote().total() < recovered.local());
}
