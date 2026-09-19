use super::*;
use backend_version::{
    AdmittedProducerObservation, AuthorityScopeClaim, CoverageWitness, IdAdmissionError, IdContext,
    ObjectClosure, ObjectKey as VersionObjectKey, ObjectVersion as VersionObjectVersion,
    ProducerObservationVerifier, RelationBinding, RelationState, Schema, UntrustedId,
    UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
    partial_coverage,
};
use std::io::{self, Cursor, Read, Write};
use std::{cell::Cell, collections::BTreeMap, sync::Arc};

struct TestAuthoritySchema;
impl Schema for TestAuthoritySchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 1;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

struct TestProducerVerifier;
impl ProducerObservationVerifier for TestProducerVerifier {
    type Error = &'static str;

    fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        (observation.producer_identity() == [0x51; 32]
            && observation.context() == [0x52; 32]
            && observation.evidence() == b"test-producer")
            .then_some(())
            .ok_or("test producer evidence")
    }
}
struct TestRecipeSchema;
impl Schema for TestRecipeSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 2;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}
struct TestWorkSchema;
impl Schema for TestWorkSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 3;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}
struct TestReadSchema;
impl Schema for TestReadSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 4;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}
struct TestOutputSchema;
impl Schema for TestOutputSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 5;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}
struct TestReceiptSchema;
impl Schema for TestReceiptSchema {
    const DOMAIN: u8 = 0x61;
    const TYPE: u16 = 6;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

struct AlternateObjectSchema;
impl Schema for AlternateObjectSchema {
    const DOMAIN: u8 = 0x62;
    const TYPE: u16 = 1;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

#[allow(clippy::panic)]
fn must<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|error| panic!("test helper received an error: {error:?}"))
}
fn model_missing(object_len: u64, covered: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut covered = covered.to_vec();
    covered.sort_unstable();
    let mut missing = Vec::new();
    let mut cursor = 0;
    for (start, end) in covered {
        if cursor < start {
            missing.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < object_len {
        missing.push((cursor, object_len));
    }
    missing
}
fn limits() -> TransportLimits {
    TransportLimits {
        max_frame: 1024,
        max_chunk: 3,
        max_object: 6,
        max_objects: 16,
        max_ranges: 16,
        max_capabilities: 16,
        max_key_bytes: 32,
        max_inputs: 16,
    }
}
fn object_ids() -> (ObjectKey, ObjectVersion) {
    (
        VersionObjectKey::from_value(b"object-key".as_slice()),
        VersionObjectVersion::from_value(b"abcdef".as_slice()),
    )
}
fn authority() -> (VersionObjectKey<TestAuthoritySchema>, AuthorityClaim) {
    let id = VersionObjectKey::from_value(b"authority".as_slice());
    let claim = AuthorityClaim::from_typed(&id, AuthorityEpoch(2));
    (id, claim)
}

fn complete_coverage() -> CoverageWitness {
    let authority =
        VersionObjectVersion::<TestAuthoritySchema>::from_value(b"authority".as_slice());
    let declared = AuthorityScopeClaim::from_object_version(authority);
    let observation = UntrustedProducerObservation::new(
        [0x51; 32],
        declared.scope_root(),
        [0x52; 32],
        b"test-producer".to_vec(),
    );
    let admitted: AdmittedProducerObservation = must(admit_producer_observation(
        observation,
        &TestProducerVerifier,
    ));
    CoverageWitness::Complete(must(admit_complete_scope(declared, admitted)))
}

fn authority_version() -> VersionObjectVersion<TestAuthoritySchema> {
    VersionObjectVersion::from_value(b"authority".as_slice())
}

fn workspace() -> WorkspaceRoot {
    must(backend_version::WorkspaceManifest::new_checked(
        1,
        Vec::new(),
        Vec::new(),
        ObjectClosure::from_version(authority_version()),
        complete_coverage(),
    ))
    .root()
}
fn relation_root() -> StateRoot {
    let state = must(RelationState::<ReplicationRelation>::from_entries(
        [([1; 32], [2; 32])],
        complete_coverage(),
    ));
    state.root()
}
fn workspace_with_relation(_root: StateRoot) -> WorkspaceRoot {
    let state = must(RelationState::<ReplicationRelation>::from_entries(
        [([1; 32], [2; 32])],
        complete_coverage(),
    ));
    must(backend_version::WorkspaceManifest::new_checked(
        1,
        vec![RelationBinding::from_state(&state)],
        Vec::new(),
        ObjectClosure::from_version(authority_version()),
        complete_coverage(),
    ))
    .root()
}
fn frame(sequence: u64, offset: u64, bytes: &[u8], previous: ChunkChain) -> Frame {
    let (key, version) = object_ids();
    let (_, authority_claim) = authority();
    must(Frame::new(
        must(TransferId::new(7)),
        key,
        version,
        ChunkParts {
            object_len: 6,
            offset,
            sequence,
            previous_chain: previous,
            payload: bytes.to_vec(),
        },
        authority_claim,
    ))
}
fn transfer() -> Transfer {
    let (key, version) = object_ids();
    let (_, authority) = authority();
    let request = must(ObjectRequest::whole(
        must(TransferId::new(7)),
        key,
        version,
        6,
        limits().max_ranges,
    ));
    must(Transfer::with_limits(request, authority, limits()))
}
fn capabilities() -> CapabilityManifest {
    let recipe = VersionObjectVersion::<TestRecipeSchema>::from_value(b"recipe".as_slice());
    CapabilityManifest {
        protocol: must(VersionRange::new(1, 2)),
        schemas: vec![SchemaDescriptor::of::<ImmutableObjectSchema>()],
        recipes: vec![RecipeCapability {
            recipe: claim_typed_identity(&recipe),
            versions: must(VersionRange::new(1, 1)),
        }],
        max_object: 6,
        max_chunk: 3,
        max_frame: 1024,
        max_ranges: 16,
        max_resources: ResourceEnvelope::UNBOUNDED,
    }
}
fn store_wire_pack() -> backend_store::WirePack {
    let map = must(backend_store::OrderedMap::try_from_iter([(
        b"object".to_vec(),
        backend_store::StoredValue::new(b"bytes".to_vec(), 1, Vec::new()),
    )]));
    must(backend_store::encode_pack(
        &map,
        backend_store::LayoutId::derive(b"replication-test"),
        limits().max_frame,
    ))
    .to_wire()
}
fn resource_envelope() -> ResourceEnvelope {
    ResourceEnvelope {
        cpu_millis: 1,
        memory_bytes: 2,
        network_bytes: 3,
        storage_bytes: 4,
        output_bytes: 6,
        processes: 1,
        wall_millis: 5,
    }
}
fn execution_material() -> (
    ExecutionRequestExpectation,
    WireRecipeRequest,
    ExecutionResultExpectation,
    WireRecipeResult,
) {
    let (_, authority_claim) = authority();
    let cancellation = must(CancellationId::new([7; 32]));
    let resources = resource_envelope();
    let authority_id = VersionObjectKey::<TestAuthoritySchema>::from_value(b"authority".as_slice());
    let authority_expected =
        AuthorityExpectation::from_typed(&authority_id, AuthorityEpoch(2), RevocationVersion(1));
    let recipe = VersionObjectVersion::<TestRecipeSchema>::from_value(b"recipe".as_slice());
    let work = VersionObjectVersion::<TestWorkSchema>::from_value(b"work".as_slice());
    let reads = VersionObjectVersion::<TestReadSchema>::from_value(b"reads".as_slice());
    let output = VersionObjectVersion::<TestOutputSchema>::from_value(b"output".as_slice());
    let receipt = VersionObjectVersion::<TestReceiptSchema>::from_value(b"receipt".as_slice());
    let (_, input) = object_ids();
    let expected = ExecutionRequestExpectation {
        attempt: must(AttemptId::new(1)),
        recipe: ExpectedIdentity::from_typed(&recipe),
        work_key: ExpectedIdentity::from_typed(&work),
        inputs: vec![ExpectedIdentity::from_typed(&input)],
        read_manifest: ExpectedIdentity::from_typed(&reads),
        scope: 1,
        authority: authority_expected,
        resources,
        fence: must(Fence::new([1; 32])),
        input_basis: workspace(),
        cancellation,
    };
    let wire_request = WireRecipeRequest {
        attempt: expected.attempt,
        recipe: claim_typed_identity(&recipe),
        work_key: claim_typed_identity(&work),
        inputs: vec![claim_typed_identity(&input)],
        read_manifest: claim_typed_identity(&reads),
        scope: 1,
        authority: WireAuthorityPolicy {
            id: authority_claim.id,
            minimum_epoch: AuthorityEpoch(2),
            revocation_version: RevocationVersion(1),
        },
        resources,
        fence: expected.fence,
        cancellation,
        input_basis: WorkspaceRootClaim::from_bytes(*expected.input_basis.as_bytes()),
    };
    let semantic_identity =
        VersionObjectVersion::<TestOutputSchema>::from_value(b"semantic-coverage".as_slice());
    let semantic_coverage = SemanticCoverageExpectation {
        identity: ExpectedIdentity::from_typed(&semantic_identity),
        scope: expected.scope,
        read_manifest: expected.read_manifest,
        authority: authority_expected,
    };
    let result_expected = ExecutionResultExpectation {
        output: ExpectedIdentity::from_typed(&output),
        receipt: ExpectedIdentity::from_typed(&receipt),
        output_len: 6,
        byte_coverage: must(SparseCoverage::complete(6)),
        semantic_coverage,
    };
    let wire_result = WireRecipeResult {
        attempt: expected.attempt,
        recipe: wire_request.recipe,
        work_key: wire_request.work_key,
        input_basis: wire_request.input_basis,
        inputs: wire_request.inputs.clone(),
        read_manifest: wire_request.read_manifest,
        output: claim_typed_identity(&output),
        output_bytes: Arc::new(b"output".to_vec()),
        scope: expected.scope,
        resources,
        byte_coverage: must(SparseCoverage::complete(6)),
        semantic_coverage: WireSemanticCoverage {
            identity: claim_typed_identity(&semantic_identity),
            scope: expected.scope,
            read_manifest: wire_request.read_manifest,
            authority: authority_claim,
        },
        authority: authority_claim,
        authority_policy: wire_request.authority,
        revocation_version: RevocationVersion(1),
        fence: expected.fence,
        cancellation,
        attestation: None,
        receipt: claim_typed_identity(&receipt),
    };
    (expected, wire_request, result_expected, wire_result)
}

struct FixtureAttestationVerifier {
    class: AttestationClass,
}

struct AcceptingAttestationVerifier(Cell<usize>);
impl AttestationVerifier for AcceptingAttestationVerifier {
    fn verify(
        &self,
        attestation: Option<Attestation>,
        _material: &AttestationMaterial,
    ) -> Result<AttestationClass, ReplicationError> {
        attestation
            .map(|_| AttestationClass::TrustedSigned)
            .ok_or(ReplicationError::InvalidAttestation)
    }

    fn verify_view(
        &self,
        attestation: Option<Attestation>,
        material: &AttestationMaterialView<'_>,
    ) -> Result<AttestationClass, ReplicationError> {
        self.0.set(material.output_bytes.as_ptr() as usize);
        attestation
            .map(|_| AttestationClass::TrustedSigned)
            .ok_or(ReplicationError::InvalidAttestation)
    }
}
impl AttestationVerifier for FixtureAttestationVerifier {
    fn verify(
        &self,
        attestation: Option<Attestation>,
        material: &AttestationMaterial,
    ) -> Result<AttestationClass, ReplicationError> {
        self.verify_view(attestation, &material.as_view())
    }

    fn verify_view(
        &self,
        attestation: Option<Attestation>,
        material: &AttestationMaterialView<'_>,
    ) -> Result<AttestationClass, ReplicationError> {
        let Some(attestation) = attestation else {
            return Err(ReplicationError::InvalidAttestation);
        };
        let first = blake3::hash(&material.canonical_bytes());
        let mut second_input = Vec::from(first.as_bytes());
        second_input.extend_from_slice(b"fixture-attestation");
        let second = blake3::hash(&second_input);
        let mut expected = [0; 64];
        expected[..32].copy_from_slice(first.as_bytes());
        expected[32..].copy_from_slice(second.as_bytes());
        if attestation.0 != expected {
            return Err(ReplicationError::InvalidAttestation);
        }
        Ok(self.class)
    }
}

fn fixture_attestation(material: &AttestationMaterial) -> Attestation {
    let first = blake3::hash(&material.canonical_bytes());
    let mut second_input = Vec::from(first.as_bytes());
    second_input.extend_from_slice(b"fixture-attestation");
    let second = blake3::hash(&second_input);
    let mut value = [0; 64];
    value[..32].copy_from_slice(first.as_bytes());
    value[32..].copy_from_slice(second.as_bytes());
    Attestation(value)
}

#[test]
fn typed_wire_claim_requires_the_expected_context() {
    let (_, version) = object_ids();
    let claim: UntrustedId<ImmutableObjectSchema> = must(UntrustedId::from_wire(
        version.as_bytes(),
        IdContext::object_key::<ImmutableObjectSchema>(),
    ));
    assert_eq!(
        VersionObjectVersion::admit(claim),
        Err(IdAdmissionError::ContextMismatch {
            expected: IdContext::schema::<ImmutableObjectSchema>(),
            actual: IdContext::object_key::<ImmutableObjectSchema>(),
        })
    );

    let mut wrong_frame = frame(0, 0, b"abc", ChunkChain([0; 32]));
    wrong_frame.key = must(WireObjectKey::from_wire(
        version.as_bytes(),
        IdContext::schema::<ImmutableObjectSchema>(),
    ));
    assert_eq!(
        wrong_frame.admit(limits()),
        Err(ReplicationError::IdentityContext)
    );
}

#[test]
fn execution_claim_cannot_relabel_a_schema_or_identity_class() {
    let (_, version) = object_ids();
    let claim = claim_typed_identity(&version);
    assert_eq!(
        claim.admit_against(version, IdContext::object_key::<ImmutableObjectSchema>()),
        Err(ReplicationError::IdentityContext)
    );
    assert_eq!(claim.admit_typed(version), Ok(version));

    let decoded = must(execution_identity_claim(version.as_bytes(), &version));
    assert_eq!(
        decoded.context(),
        IdContext::schema::<ImmutableObjectSchema>()
    );
    assert_eq!(
        must(untrusted_execution_identity_claim(
            version.as_bytes(),
            IdContext::object_key::<ImmutableObjectSchema>(),
        ))
        .admit_typed(version),
        Err(ReplicationError::IdentityContext)
    );
}

#[test]
fn tamper_replay_and_reconnect_are_bounded() {
    let mut tampered = frame(0, 0, b"abc", ChunkChain([0; 32]));
    tampered.payload[0] = b'X';
    assert_eq!(
        tampered.admit(limits()),
        Err(ReplicationError::CorruptFrame)
    );
    let wrong_range = frame(0, 4, b"abc", ChunkChain([0; 32]));
    assert_eq!(wrong_range.admit(limits()), Err(ReplicationError::Range));
    let mut transfer = transfer();
    let first = must(frame(0, 0, b"abc", ChunkChain([0; 32])).admit(limits()));
    let duplicate = first.clone();
    must(transfer.stage(first));
    assert_eq!(must(transfer.stage(duplicate)).chunks, 1);
    let checkpoint = transfer.checkpoint();
    let expected_request = must(ObjectRequest::whole(
        must(TransferId::new(7)),
        object_ids().0,
        object_ids().1,
        6,
        limits().max_ranges,
    ));
    let expected_authority = authority().1;
    assert_eq!(
        must(checkpoint.to_wire()).admit_against(&expected_request, expected_authority, limits(),),
        Ok(checkpoint.clone())
    );
    let admitted_checkpoint = must(must(checkpoint.to_wire()).admit_against(
        &expected_request,
        expected_authority,
        limits(),
    ));
    let mut resumed = must(Transfer::resume(
        expected_request.clone(),
        expected_authority,
        limits(),
        admitted_checkpoint,
    ));
    let second = must(frame(1, 3, b"def", checkpoint.chunks[0].chain).admit(limits()));
    must(resumed.stage(second));
    assert_eq!(must(resumed.accept_canonical()).bytes, b"abcdef");
    let mut tampered_wire_checkpoint = must(checkpoint.to_wire());
    tampered_wire_checkpoint.chunks[0].bytes[0] = b'X';
    assert_eq!(
        tampered_wire_checkpoint.admit_against(&expected_request, expected_authority, limits(),),
        Err(ReplicationError::CorruptFrame)
    );
    let mut stale_wire_checkpoint = must(checkpoint.to_wire());
    stale_wire_checkpoint.version = must(claim_object_version(VersionObjectVersion::from_value(
        b"different-version".as_slice(),
    )));
    assert_eq!(
        stale_wire_checkpoint.admit_against(&expected_request, expected_authority, limits(),),
        Err(ReplicationError::StaleFence)
    );
    let mut bad = checkpoint.clone();
    bad.coverage = must(SparseCoverage::complete(6));
    let (_, authority) = authority();
    assert!(matches!(
        Transfer::resume(
            must(ObjectRequest::whole(
                must(TransferId::new(7)),
                object_ids().0,
                object_ids().1,
                6,
                limits().max_ranges,
            )),
            authority,
            limits(),
            bad,
        ),
        Err(ReplicationError::IdentityMismatch)
    ));
    let wrong_request = must(ObjectRequest::whole(
        must(TransferId::new(7)),
        object_ids().0,
        VersionObjectVersion::from_value(b"different".as_slice()),
        6,
        limits().max_ranges,
    ));
    assert!(matches!(
        Transfer::resume(wrong_request, authority, limits(), checkpoint),
        Err(ReplicationError::StaleFence)
    ));
}

#[test]
fn sparse_partial_ranges_report_exact_missing_extents() {
    let coverage = must(SparseCoverage::from_ranges(
        [must(ByteRange::new(0, 2)), must(ByteRange::new(4, 1))],
        limits().max_ranges,
    ));
    let expected = model_missing(6, &[(0, 2), (4, 5)])
        .into_iter()
        .map(|(start, end)| must(ByteRange::new(start, end - start)))
        .collect::<Vec<_>>();
    assert_eq!(coverage.missing(6, limits().max_ranges), Ok(expected));
}

#[test]
fn stale_authority_and_fence_are_rejected_by_execution_boundary() {
    let (mut expected, request, _, _) = execution_material();
    expected.authority.minimum_epoch = AuthorityEpoch(3);
    assert_eq!(
        request.admit_against(&expected, limits()),
        Err(ReplicationError::StaleAuthority)
    );
    let (expected, _, result_expected, mut result) = execution_material();
    result.fence = must(Fence::new(2));
    assert_eq!(
        result.admit_against(&expected, &result_expected, limits(), RevocationVersion(1)),
        Err(ReplicationError::StaleFence)
    );
    let (expected, _, result_expected, mut stale_result) = execution_material();
    stale_result.revocation_version = RevocationVersion(0);
    assert_eq!(
        stale_result.admit_against(&expected, &result_expected, limits(), RevocationVersion(1),),
        Err(ReplicationError::RevokedAuthority)
    );
}

#[test]
fn execution_wire_envelopes_compare_only_against_typed_expectations() {
    let (expected, request, result_expected, result) = execution_material();
    assert_eq!(request.admit_against(&expected, limits()), Ok(()));
    assert!(
        result
            .admit_against(&expected, &result_expected, limits(), RevocationVersion(1))
            .is_ok()
    );
    let (_, _, result_expected, mut tampered_output) = execution_material();
    let mut wrong_output = tampered_output.output.as_bytes();
    wrong_output[0] ^= 1;
    tampered_output.output = must(WireIdentity::from_wire(
        &wrong_output,
        tampered_output.output.context(),
    ));
    assert_eq!(
        tampered_output.admit_against(
            &execution_material().0,
            &result_expected,
            limits(),
            RevocationVersion(1),
        ),
        Err(ReplicationError::IdentityMismatch)
    );
    let (_, _, result_expected, mut partial_output) = execution_material();
    partial_output.byte_coverage = must(SparseCoverage::new(limits().max_ranges));
    assert_eq!(
        partial_output.admit_against(
            &execution_material().0,
            &result_expected,
            limits(),
            RevocationVersion(1),
        ),
        Err(ReplicationError::IdentityMismatch)
    );
}

#[test]
fn result_publication_requires_full_attestation_and_rejects_tampering() {
    let (expected, _, result_expected, mut result) = execution_material();
    assert_eq!(
        result.attestation_material().canonical_bytes(),
        result.attestation_material_view().canonical_bytes()
    );
    result.attestation = Some(fixture_attestation(&result.attestation_material()));
    let verifier = FixtureAttestationVerifier {
        class: AttestationClass::LocallyVerifiable,
    };
    let publishable = result.admit_publishable(
        &expected,
        &result_expected,
        limits(),
        RevocationVersion(1),
        &verifier,
    );
    assert!(matches!(
        publishable,
        Ok(value) if value.class() == AttestationClass::LocallyVerifiable
    ));

    let (_, _, result_expected, absent) = execution_material();
    assert_eq!(
        absent.admit_publishable(
            &execution_material().0,
            &result_expected,
            limits(),
            RevocationVersion(1),
            &verifier,
        ),
        Err(ReplicationError::AttestationRequired)
    );

    let (_, _, result_expected, mut wrong_key) = execution_material();
    wrong_key.attestation = Some(Attestation([9; 64]));
    assert_eq!(
        wrong_key.admit_publishable(
            &execution_material().0,
            &result_expected,
            limits(),
            RevocationVersion(1),
            &verifier,
        ),
        Err(ReplicationError::InvalidAttestation)
    );

    let (_, _, result_expected, mut memo) = execution_material();
    memo.attestation = Some(fixture_attestation(&memo.attestation_material()));
    let memo_verifier = FixtureAttestationVerifier {
        class: AttestationClass::UntrustedMemo,
    };
    assert_eq!(
        memo.admit_publishable(
            &execution_material().0,
            &result_expected,
            limits(),
            RevocationVersion(1),
            &memo_verifier,
        ),
        Err(ReplicationError::UntrustedAttestation)
    );

    let (_, _, result_expected, mut tampered) = execution_material();
    tampered.attestation = Some(fixture_attestation(&tampered.attestation_material()));
    Arc::make_mut(&mut tampered.output_bytes)[0] ^= 1;
    assert_eq!(
        tampered.admit_publishable(
            &execution_material().0,
            &result_expected,
            limits(),
            RevocationVersion(1),
            &verifier,
        ),
        Err(ReplicationError::IdentityMismatch)
    );
}

#[test]
fn object_transfer_retains_schema_marker_for_same_bytes() {
    let key = VersionObjectKey::<AlternateObjectSchema>::from_value(b"same-key".as_slice());
    let version = VersionObjectVersion::<AlternateObjectSchema>::from_value(b"abcdef".as_slice());
    let request = must(ObjectRequest::<AlternateObjectSchema>::whole(
        must(TransferId::new(17)),
        key,
        version,
        6,
        limits().max_ranges,
    ));
    let (_, authority) = authority();
    let mut transfer = must(Transfer::<Receiving, AlternateObjectSchema>::with_limits(
        request,
        authority,
        limits(),
    ));
    let mut frame = must(Frame::<AlternateObjectSchema>::new(
        must(TransferId::new(17)),
        key,
        version,
        ChunkParts {
            object_len: 6,
            offset: 0,
            sequence: 0,
            previous_chain: ChunkChain([0; 32]),
            payload: b"abc".to_vec(),
        },
        authority,
    ));
    let admitted = must(frame.clone().admit(limits()));
    must(transfer.stage(admitted));
    let second = must(Frame::<AlternateObjectSchema>::new(
        must(TransferId::new(17)),
        key,
        version,
        ChunkParts {
            object_len: 6,
            offset: 3,
            sequence: 1,
            previous_chain: frame.chain,
            payload: b"def".to_vec(),
        },
        authority,
    ));
    must(transfer.stage(must(second.admit(limits()))));
    assert_eq!(must(transfer.accept_canonical()).bytes, b"abcdef");

    frame.key = must(SchemaWireObjectKey::<AlternateObjectSchema>::from_wire(
        key.as_bytes(),
        IdContext::object_key::<ImmutableObjectSchema>(),
    ));
    assert_eq!(
        frame.admit(limits()),
        Err(ReplicationError::IdentityContext)
    );
}

#[test]
fn codec_is_versioned_bounded_and_local_streams_share_bytes() {
    let message = TransportMessage::Chunk(frame(0, 0, b"abc", ChunkChain([0; 32])));
    let encoded = must(message.encode(limits()));
    assert_eq!(must(TransportMessage::decode(&encoded, limits())), message);

    let mut bad_version = encoded.clone();
    bad_version[4] = u8::MAX;
    assert_eq!(
        TransportMessage::decode(&bad_version, limits()),
        Err(ReplicationError::UnsupportedWireVersion)
    );
    let mut bad_tag = encoded.clone();
    bad_tag[5] = u8::MAX;
    assert_eq!(
        TransportMessage::decode(&bad_tag, limits()),
        Err(ReplicationError::UnknownMessage)
    );
    assert_eq!(
        TransportMessage::decode(&encoded[..encoded.len() - 1], limits()),
        Err(ReplicationError::TruncatedFrame)
    );
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(
        TransportMessage::decode(&trailing, limits()),
        Err(ReplicationError::TrailingFrame)
    );
    let mut malicious_length = encoded.clone();
    malicious_length[6..10].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        TransportMessage::decode(&malicious_length, limits()),
        Err(ReplicationError::TruncatedFrame)
    );

    let local = LocalTransport::new();
    must(local.send_message(message.clone(), 1, limits()));
    assert_eq!(must(local.recv()), Some(message));
    let pack = TransportMessage::WirePack(WirePackClaim::from_wire(&store_wire_pack()));
    must(local.send_message(pack.clone(), 1, limits()));
    assert_eq!(must(local.recv()), Some(pack));
    let (_, wire_request, _, _) = execution_material();
    let cancel = TransportMessage::CancelAttempt(CancelAttempt {
        attempt: wire_request.attempt,
        work_key: wire_request.work_key,
        cancellation: wire_request.cancellation,
        fence: wire_request.fence,
    });
    must(local.send_message(cancel.clone(), 1, limits()));
    assert_eq!(must(local.recv()), Some(cancel));

    let mut partial = must(FramedStream::new(Cursor::new(vec![0, 0, 8, 0]), limits()));
    assert_eq!(
        partial.recv_message(),
        Err(ReplicationError::MessageTooLarge)
    );
}

struct InterruptedFrame {
    bytes: Cursor<Vec<u8>>,
    reads: usize,
}

impl Read for InterruptedFrame {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let call = self.reads;
        self.reads = self.reads.saturating_add(1);
        if matches!(call, 1 | 3) {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let bounded = output.len().min(2);
        self.bytes.read(&mut output[..bounded])
    }
}

impl Write for InterruptedFrame {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn framed_stream_retains_header_and_payload_cursors_across_timeouts() {
    let message = TransportMessage::Chunk(frame(0, 0, b"abc", ChunkChain([0; 32])));
    let stream = InterruptedFrame {
        bytes: Cursor::new(must(encode_stream_frame(&message, limits()))),
        reads: 0,
    };
    let mut framed = must(FramedStream::new(stream, limits()));

    assert_eq!(
        framed.recv_message_or_eof_with_io(),
        Err(FramedStreamError::Io(io::ErrorKind::TimedOut))
    );
    assert_eq!(
        framed.recv_message_or_eof_with_io(),
        Err(FramedStreamError::Io(io::ErrorKind::TimedOut))
    );
    assert_eq!(must(framed.recv_message_or_eof_with_io()), Some(message));
}

#[cfg(unix)]
#[test]
fn unix_stream_roundtrip_uses_the_same_encoded_message() {
    use std::os::unix::net::UnixStream;

    let (left, right) = must(UnixStream::pair());
    let mut sender = must(FramedStream::new(left, limits()));
    let mut receiver = must(FramedStream::new(right, limits()));
    let message = TransportMessage::Chunk(frame(0, 0, b"abc", ChunkChain([0; 32])));
    let encoded = must(message.encode(limits()));
    must(sender.send_message(&message));
    assert_eq!(must(receiver.recv_message()), message);
    assert_eq!(encoded, must(message.encode(limits())));

    let pack = TransportMessage::WirePack(WirePackClaim::from_wire(&store_wire_pack()));
    let pack_encoded = must(pack.encode(limits()));
    must(sender.send_message(&pack));
    assert_eq!(must(receiver.recv_message()), pack);
    assert_eq!(pack_encoded, must(pack.encode(limits())));

    let (_, wire_request, _, _) = execution_material();
    let cancel = TransportMessage::CancelAttempt(CancelAttempt {
        attempt: wire_request.attempt,
        work_key: wire_request.work_key,
        cancellation: wire_request.cancellation,
        fence: wire_request.fence,
    });
    let cancel_encoded = must(cancel.encode(limits()));
    must(sender.send_message(&cancel));
    assert_eq!(must(receiver.recv_message()), cancel);
    assert_eq!(cancel_encoded, must(cancel.encode(limits())));
}

#[test]
fn capability_negotiation_and_local_transport_share_bounds() {
    let capabilities = capabilities();
    let message = TransportMessage::Capabilities(capabilities.clone());
    let encoded = must(message.encode(limits()));
    assert_eq!(message.estimated_size(), encoded.len());
    assert!(
        capabilities.validate(limits()).is_ok(),
        "capabilities: {:?}",
        capabilities.validate(limits())
    );
    let negotiated = capabilities.negotiate(&capabilities, limits());
    assert!(negotiated.is_ok(), "negotiation: {negotiated:?}");
    let negotiated = must(negotiated);
    assert_eq!(negotiated.protocol, 2);
    assert_eq!(negotiated.max_resources.output_bytes, 6);
    assert_eq!(
        negotiated.max_resources.processes,
        u32::try_from(limits().max_inputs).unwrap_or(u32::MAX)
    );
    let mut bounded_peer = capabilities.clone();
    bounded_peer.max_resources = resource_envelope();
    let bounded = must(capabilities.negotiate(&bounded_peer, limits()));
    assert_eq!(bounded.max_resources, resource_envelope());
    assert!(resource_envelope().fits_within(bounded.max_resources));
    let mut excessive = resource_envelope();
    excessive.cpu_millis = excessive.cpu_millis.saturating_add(1);
    assert!(!excessive.fits_within(bounded.max_resources));
    let local = LocalTransport::new();
    must(local.send(frame(0, 0, b"abc", ChunkChain([0; 32])), 1));
    assert!(matches!(
        must(local.recv()),
        Some(TransportMessage::Chunk(_))
    ));
}

#[test]
fn root_summary_reconciliation_repairs_version_mismatch() {
    let (key, version) = object_ids();
    let (_, authority) = authority();
    let summary = RootSummary::from_parts_for_test(
        1,
        workspace(),
        vec![],
        vec![ObjectSummary {
            key,
            version,
            len: 6,
        }],
        authority,
        must(SparseCoverage::complete(1)),
    );
    let local = BTreeMap::from([(key, VersionObjectVersion::from_value(b"old".as_slice()))]);
    let result = must(reconcile(
        &summary,
        Some(summary.workspace()),
        &local,
        must(TransferId::new(3)),
        limits(),
    ));
    assert!(result.root_present);
    assert_eq!(result.missing.len(), 1);
}

#[test]
fn relation_root_is_schema_marked() {
    let state = must(RelationState::<ReplicationRelation>::from_entries(
        [([1; 32], [2; 32])],
        CoverageWitness::Partial(partial_coverage(1)),
    ));
    let typed = state.root();
    let wire = must(state_root_claim(typed.as_bytes()));
    let request = WireRangeRequest {
        relation: 7,
        root: wire,
        start: vec![1],
        end: Some(vec![2]),
        limit: 1,
        resume: must(SparseCoverage::new(limits().max_ranges)),
    };
    assert_eq!(must(request.admit_against(7, typed, limits())).root, typed);
}

#[test]
fn transfer_accepts_reordered_chunks_and_rejects_a_gap() {
    let first_wire = frame(0, 0, b"abc", ChunkChain([0; 32]));
    let first = must(first_wire.clone().admit(limits()));
    let second = must(frame(1, 3, b"def", first.chain()).admit(limits()));

    let mut reordered = transfer();
    must(reordered.stage(second.clone()));
    assert_eq!(reordered.checkpoint().coverage.covered_bytes(), 3);
    must(reordered.stage(first.clone()));
    let object = must(reordered.accept_canonical());
    assert_eq!(object.bytes, b"abcdef");

    let mut gap = transfer();
    must(gap.stage(second));
    assert!(matches!(gap.finish(), Err(ReplicationError::Incomplete)));
}

#[test]
fn transfer_rejects_admitted_overlapping_ranges() {
    let first = must(frame(0, 0, b"abc", ChunkChain([0; 32])).admit(limits()));
    let overlap = must(frame(1, 2, b"cde", first.chain()).admit(limits()));
    let mut transfer = transfer();
    must(transfer.stage(first));
    assert_eq!(
        transfer.stage(overlap),
        Err(ReplicationError::ReplayConflict)
    );
}

#[test]
fn sparse_coverage_checks_boundaries_and_budget() {
    assert_eq!(SparseCoverage::complete(0), Err(ReplicationError::Range));
    assert_eq!(ByteRange::new(u64::MAX, 1), Err(ReplicationError::Overflow));
    assert_eq!(
        must(SparseCoverage::from_ranges(
            [must(ByteRange::new(0, 6))],
            limits().max_ranges,
        )),
        must(SparseCoverage::complete(6)),
        "local mutation capacity is not part of the coverage observation",
    );
    let coverage = must(SparseCoverage::from_ranges(
        [must(ByteRange::new(0, 2))],
        limits().max_ranges,
    ));
    assert_eq!(
        coverage.missing(1, limits().max_ranges),
        Err(ReplicationError::Range)
    );
    assert_eq!(coverage.missing(6, 0), Err(ReplicationError::CoverageLimit));
    let mut bounded = must(SparseCoverage::from_ranges([must(ByteRange::new(0, 1))], 1));
    let before = bounded.clone();
    assert_eq!(
        bounded.insert(must(ByteRange::new(2, 1))),
        Err(ReplicationError::CoverageLimit)
    );
    assert_eq!(bounded, before);
}

#[test]
fn negotiation_rejects_missing_schema_and_recipe_intersections() {
    let local = capabilities();
    let mut no_schema = local.clone();
    no_schema.schemas = vec![SchemaDescriptor {
        domain: 0xee,
        type_id: 99,
        versions: must(VersionRange::new(1, 1)),
    }];
    assert_eq!(
        local.negotiate(&no_schema, limits()),
        Err(ReplicationError::NoCommonSchema)
    );

    let mut no_recipe = local.clone();
    let other = VersionObjectVersion::<TestRecipeSchema>::from_value(b"other-recipe".as_slice());
    no_recipe.recipes = vec![RecipeCapability {
        recipe: claim_typed_identity(&other),
        versions: must(VersionRange::new(1, 1)),
    }];
    assert_eq!(
        local.negotiate(&no_recipe, limits()),
        Err(ReplicationError::NoCommonRecipe)
    );

    let mut duplicate_schema = local;
    duplicate_schema.schemas = vec![
        SchemaDescriptor {
            domain: 0x72,
            type_id: 1,
            versions: must(VersionRange::new(1, 1)),
        },
        SchemaDescriptor {
            domain: 0x72,
            type_id: 1,
            versions: must(VersionRange::new(2, 2)),
        },
    ];
    assert_eq!(
        duplicate_schema.validate(limits()),
        Err(ReplicationError::InvalidCapabilities)
    );
}

#[test]
fn authority_epoch_context_and_revocation_matrix_is_explicit() {
    let (authority_id, claim) = authority();
    let expected =
        AuthorityExpectation::from_typed(&authority_id, AuthorityEpoch(2), RevocationVersion(2));
    assert_eq!(expected.admit(claim, RevocationVersion(2)), Ok(()));
    assert_eq!(
        expected.admit(
            AuthorityClaim::new(claim.id, AuthorityEpoch(1)),
            RevocationVersion(2),
        ),
        Err(ReplicationError::StaleAuthority)
    );
    assert_eq!(
        expected.admit(claim, RevocationVersion(1)),
        Err(ReplicationError::RevokedAuthority)
    );

    let other = VersionObjectKey::<TestAuthoritySchema>::from_value(b"other-authority");
    assert_eq!(
        expected.admit(
            AuthorityClaim::from_typed(&other, AuthorityEpoch(2),),
            RevocationVersion(2),
        ),
        Err(ReplicationError::IdentityMismatch)
    );
    let recipe = VersionObjectVersion::<TestRecipeSchema>::from_value(b"authority-context");
    assert_eq!(
        expected.admit(
            AuthorityClaim::new(claim_typed_identity(&recipe), AuthorityEpoch(2),),
            RevocationVersion(2),
        ),
        Err(ReplicationError::IdentityContext)
    );
}

#[test]
fn root_wire_admission_rejects_disagreement_and_duplicate_order() {
    let (key, version) = object_ids();
    let (_, authority_claim) = authority();
    let relation = relation_root();
    let summary = RootSummary::from_parts_for_test(
        1,
        workspace_with_relation(relation),
        vec![RelationSummary {
            relation: 7,
            root: relation,
            coverage: must(SparseCoverage::complete(1)),
        }],
        vec![ObjectSummary {
            key,
            version,
            len: 6,
        }],
        authority_claim,
        must(SparseCoverage::complete(1)),
    );
    let expected_closure = RootSummaryExpectation {
        schema: 1,
        workspace: summary.workspace(),
        relations: vec![RelationRootExpectation {
            relation: 7,
            root: relation,
        }],
        objects: vec![ObjectSummaryExpectation {
            key,
            version,
            len: 6,
        }],
    };
    let mut wrong_workspace = must(summary.to_wire());
    wrong_workspace.workspace = WorkspaceRootClaim::from_bytes([9; 32]);
    assert_eq!(
        wrong_workspace.admit(&expected_closure, limits()),
        Err(ReplicationError::IdentityMismatch)
    );
    assert_eq!(
        must(summary.to_wire()).admit_against(&expected_closure, limits()),
        Ok(summary.clone())
    );
    let mut wrong_schema = must(summary.to_wire());
    wrong_schema.schema = 2;
    assert_eq!(
        wrong_schema.admit_against(&expected_closure, limits()),
        Err(ReplicationError::IdentityMismatch)
    );
    let other_relation = must(RelationState::<ReplicationRelation>::from_entries(
        [([3; 32], [4; 32])],
        CoverageWitness::Partial(partial_coverage(1)),
    ))
    .root();
    let mut wrong_relation = must(summary.to_wire());
    wrong_relation.relations[0].root = must(claim_state_root(other_relation));
    assert_eq!(
        wrong_relation.admit_against(&expected_closure, limits()),
        Err(ReplicationError::IdentityMismatch)
    );

    let (authority_id, _) = authority();
    let authority_policy =
        AuthorityExpectation::from_typed(&authority_id, AuthorityEpoch(2), RevocationVersion(1));
    assert_eq!(
        must(summary.to_wire()).admit_with_authority(
            &expected_closure,
            authority_policy,
            RevocationVersion(1),
            limits(),
        ),
        Ok(summary.clone())
    );
    assert_eq!(
        must(summary.to_wire()).admit_with_authority(
            &expected_closure,
            authority_policy,
            RevocationVersion(0),
            limits(),
        ),
        Err(ReplicationError::RevokedAuthority)
    );

    let mut duplicate = must(summary.to_wire());
    duplicate.objects.push(duplicate.objects[0]);
    assert_eq!(
        duplicate.admit(&expected_closure, limits()),
        Err(ReplicationError::Unsorted)
    );
}

#[test]
fn every_wire_message_variant_obeys_the_same_local_bounds() {
    let (key, version) = object_ids();
    let (_, authority) = authority();
    let summary = RootSummary::from_parts_for_test(
        1,
        workspace(),
        vec![],
        vec![ObjectSummary {
            key,
            version,
            len: 6,
        }],
        authority,
        must(SparseCoverage::complete(1)),
    );
    let node = NodeRequest {
        transfer: must(TransferId::new(9)),
        key,
        version,
        len: 6,
        resume: must(SparseCoverage::new(limits().max_ranges)),
    };
    let range = RangeRequest {
        relation: 7,
        root: relation_root(),
        start: vec![1],
        end: Some(vec![2]),
        limit: 1,
        resume: must(SparseCoverage::new(limits().max_ranges)),
    };
    let resume = ResumeRequest {
        transfer: must(TransferId::new(9)),
        key,
        version,
        len: 6,
        coverage: must(SparseCoverage::new(limits().max_ranges)),
    };
    let wire_summary = must(summary.to_wire());
    let wire_node = must(node.to_wire());
    let wire_range = must(range.to_wire());
    let wire_resume = must(resume.to_wire());
    let (_, wire_request, _, wire_result) = execution_material();
    let cancellation = CancelAttempt {
        attempt: wire_request.attempt,
        work_key: wire_request.work_key,
        cancellation: wire_request.cancellation,
        fence: wire_request.fence,
    };
    let pack = WirePackClaim::from_wire(&store_wire_pack());
    let messages = vec![
        TransportMessage::WirePack(pack),
        TransportMessage::Capabilities(capabilities()),
        TransportMessage::RootSummary(summary),
        TransportMessage::WireRootSummary(wire_summary),
        TransportMessage::NodeRequest(node.clone()),
        TransportMessage::WireNodeRequest(wire_node),
        TransportMessage::RangeRequest(range.clone()),
        TransportMessage::WireRangeRequest(wire_range),
        TransportMessage::Chunk(frame(0, 0, b"abc", ChunkChain([0; 32]))),
        TransportMessage::Resume(resume.clone()),
        TransportMessage::WireResumeRequest(wire_resume),
        TransportMessage::WireRecipeRequest(wire_request),
        TransportMessage::WireRecipeResult(Box::new(wire_result)),
        TransportMessage::CancelAttempt(cancellation),
    ];
    for message in messages {
        assert_eq!(message.validate(limits()), Ok(()), "{message:?}");
        let bytes = must(message.encode(limits()));
        let decoded = must(TransportMessage::decode(&bytes, limits()));
        assert_eq!(decoded.validate(limits()), Ok(()), "{decoded:?}");
    }

    assert_eq!(
        must(node.to_wire()).admit_against(&node, limits()),
        Ok(node.clone())
    );
    assert_eq!(
        must(range.to_wire()).admit_against(7, range.root, limits()),
        Ok(range.clone())
    );
    let mut stale_resume = must(resume.to_wire());
    stale_resume.version = must(claim_object_version(VersionObjectVersion::from_value(
        b"different-resume".as_slice(),
    )));
    assert_eq!(
        stale_resume.admit_against(&resume, limits()),
        Err(ReplicationError::StaleFence)
    );
    assert_eq!(
        must(resume.to_wire()).admit_against(&resume, limits()),
        Ok(resume)
    );
}

#[test]
fn cancellation_command_requires_the_active_attempt_and_fence() {
    let (expected, request, _, _) = execution_material();
    let expected = CancelAttemptExpectation {
        attempt: expected.attempt,
        work_key: expected.work_key,
        cancellation: expected.cancellation,
        fence: expected.fence,
    };
    let cancel = CancelAttempt {
        attempt: request.attempt,
        work_key: request.work_key,
        cancellation: request.cancellation,
        fence: request.fence,
    };
    assert_eq!(cancel.validate(limits()), Ok(()));
    assert_eq!(cancel.admit_against(expected, limits()), Ok(()));

    let mut stale = cancel;
    stale.fence = must(Fence::new([9; 32]));
    assert_eq!(
        stale.admit_against(expected, limits()),
        Err(ReplicationError::StaleFence)
    );

    let mut wrong_attempt = cancel;
    wrong_attempt.attempt = must(AttemptId::new(99));
    assert_eq!(
        wrong_attempt.admit_against(expected, limits()),
        Err(ReplicationError::StaleFence)
    );

    let mut wrong_work = cancel;
    wrong_work.work_key = claim_typed_identity(
        &VersionObjectVersion::<TestWorkSchema>::from_value(b"other-work".as_slice()),
    );
    assert_eq!(
        wrong_work.admit_against(expected, limits()),
        Err(ReplicationError::IdentityMismatch)
    );

    let encoded = must(TransportMessage::CancelAttempt(cancel).encode(limits()));
    assert_eq!(
        must(TransportMessage::decode(&encoded, limits())),
        TransportMessage::CancelAttempt(cancel)
    );
}

#[test]
fn shared_transfer_checkpoint_and_output_clone_without_payload_copies() {
    let limits = TransportLimits {
        max_frame: 600 * 1024,
        max_chunk: 512 * 1024,
        max_object: 2 * 1024 * 1024,
        max_objects: 16,
        max_ranges: 16,
        max_capabilities: 16,
        max_key_bytes: 32,
        max_inputs: 16,
    };
    let object_len = 2 * 1024 * 1024usize;
    let object_bytes = vec![0x5a; object_len];
    let key = VersionObjectKey::<ImmutableObjectSchema>::from_value(b"large-object".as_slice());
    let version = VersionObjectVersion::<ImmutableObjectSchema>::from_value(&object_bytes);
    let authority = authority().1;
    let request = must(ObjectRequest::whole(
        must(TransferId::new(77)),
        key,
        version,
        object_len as u64,
        limits.max_ranges,
    ));
    let mut transfer = must(Transfer::with_limits(request.clone(), authority, limits));
    let chunk_len = limits.max_chunk;
    let mut previous = ChunkChain([0; 32]);
    for (sequence, start) in (0..object_len).step_by(chunk_len).enumerate() {
        let end = (start + chunk_len).min(object_len);
        let frame = must(Frame::new(
            must(TransferId::new(77)),
            key,
            version,
            ChunkParts {
                object_len: object_len as u64,
                offset: start as u64,
                sequence: sequence as u64,
                previous_chain: previous,
                payload: object_bytes[start..end].to_vec(),
            },
            authority,
        ));
        let admitted = must(frame.admit(limits));
        if sequence == 0 {
            let payload = admitted.payload_shared();
            let replay = admitted.clone();
            assert!(Arc::ptr_eq(&payload, &replay.payload_shared()));
        }
        previous = admitted.chain();
        must(transfer.stage(admitted));
    }

    let checkpoint = transfer.checkpoint_shared();
    let checkpoint_clone = checkpoint.clone();
    assert_eq!(checkpoint.chunks.len(), 4);
    assert!(Arc::ptr_eq(
        &checkpoint.chunks[0].bytes,
        &checkpoint_clone.chunks[0].bytes
    ));
    let original_chunk = checkpoint.chunks[0].bytes_shared();
    let resumed = must(Transfer::resume_shared(
        request.clone(),
        authority,
        limits,
        checkpoint,
    ));
    let resumed_checkpoint = resumed.checkpoint_shared();
    assert!(Arc::ptr_eq(
        &original_chunk,
        &resumed_checkpoint.chunks[0].bytes
    ));

    let shared_object = must(must(resumed.finish()).accept_shared(|expected, bytes| {
        VersionObjectVersion::<ImmutableObjectSchema>::from_value(bytes) == expected
    }));
    let shared_clone = shared_object.clone();
    assert!(Arc::ptr_eq(&shared_object.bytes, &shared_clone.bytes));
    assert_eq!(shared_object.bytes().len(), object_len);
}

#[test]
fn owned_execution_admission_retains_a_multi_megabyte_output_buffer() {
    let (expected_request, request, mut expected_result, mut wire) = execution_material();
    let output_len = 2 * 1024 * 1024usize;
    let output = vec![0x42; output_len];
    let limits = TransportLimits {
        max_frame: output_len + 1024,
        max_chunk: 64 * 1024,
        max_object: output_len as u64,
        ..limits()
    };
    wire.output_bytes = Arc::new(output);
    let output_identity =
        VersionObjectVersion::<TestOutputSchema>::from_value(wire.output_bytes.as_slice());
    wire.output = claim_typed_identity(&output_identity);
    expected_result.output = ExpectedIdentity::from_typed(&output_identity);
    wire.byte_coverage = must(SparseCoverage::complete(output_len as u64));
    wire.attestation = Some(Attestation([7; 64]));
    expected_result.output_len = output_len as u64;
    expected_result.byte_coverage = wire.byte_coverage.clone();
    let source = wire.output_bytes.as_ptr();
    let verifier = AcceptingAttestationVerifier(Cell::new(0));
    let publishable = must(wire.admit_publishable_owned(
        &expected_request,
        &expected_result,
        limits,
        RevocationVersion(1),
        &verifier,
    ));
    assert_eq!(verifier.0.get(), source as usize);
    assert_eq!(publishable.wire().output_bytes.as_ptr(), source);
    let clone = publishable.clone();
    assert_eq!(clone.wire().output_bytes.as_ptr(), source);
    assert!(clone.into_admitted().shared_owner_count() >= 1);
    assert_eq!(request.attempt, expected_request.attempt);
}

#[test]
#[allow(clippy::panic)]
fn store_pack_admission_is_bounded_and_tamper_evident() {
    let wire = store_wire_pack();
    assert!(admit_pack(wire.clone(), limits()).is_ok());
    let expected_layout = wire.layout;
    let claim = WirePackClaim::from_wire(&wire);
    let pack_message = TransportMessage::WirePack(claim.clone());
    let pack_bytes = must(pack_message.encode(limits()));
    assert_eq!(pack_message.estimated_size(), pack_bytes.len());
    let decoded_pack = must(TransportMessage::decode(&pack_bytes, limits()));
    assert_eq!(decoded_pack, pack_message);
    let TransportMessage::WirePack(decoded_claim) = decoded_pack else {
        panic!("decoded pack changed transport variant");
    };
    assert!(
        decoded_claim
            .clone()
            .admit_against(expected_layout, limits())
            .is_ok()
    );
    assert_eq!(
        decoded_claim
            .clone()
            .admit_against(backend_store::LayoutId::derive(b"wrong-layout"), limits()),
        Err(ReplicationError::IdentityMismatch)
    );

    let mut tampered_claim = decoded_claim.clone();
    tampered_claim.bytes[0] ^= 1;
    assert_eq!(
        tampered_claim.admit_against(expected_layout, limits()),
        Err(ReplicationError::CorruptFrame)
    );

    let mut tampered = wire.clone();
    tampered.bytes[0] ^= 1;
    assert_eq!(
        admit_pack(tampered, limits()),
        Err(ReplicationError::CorruptFrame)
    );

    let mut oversized_key = wire;
    oversized_key
        .locations
        .insert(vec![0; limits().max_key_bytes + 1], (0, 0));
    assert_eq!(
        admit_pack(oversized_key, limits()),
        Err(ReplicationError::MessageTooLarge)
    );

    let mut oversized_claim = claim;
    oversized_claim
        .locations
        .insert(vec![0; limits().max_key_bytes + 1], (0, 0));
    assert_eq!(
        TransportMessage::WirePack(oversized_claim).encode(limits()),
        Err(ReplicationError::MessageTooLarge)
    );

    let mut malformed = WirePackClaim::from_wire(&store_wire_pack());
    malformed.locations.insert(b"bad".to_vec(), (u32::MAX, 1));
    assert_eq!(
        malformed.validate(limits()),
        Err(ReplicationError::CorruptFrame)
    );
}

#[test]
fn local_transport_bounds_backpressure_disconnect_and_reconnect() {
    let local = LocalTransport::new();
    let chunk = frame(0, 0, b"abc", ChunkChain([0; 32]));
    must(local.send(chunk.clone(), 1));
    assert_eq!(local.send(chunk, 1), Err(ReplicationError::Backpressure));
    assert!(must(local.recv_frame()).is_some());

    must(local.send_message(TransportMessage::Capabilities(capabilities()), 1, limits()));
    assert_eq!(local.recv_frame(), Err(ReplicationError::WrongMessage));

    local.disconnect();
    assert_eq!(local.len(), Err(ReplicationError::Disconnected));
    assert_eq!(
        local.send_message(
            TransportMessage::Chunk(frame(0, 0, b"abc", ChunkChain([0; 32]))),
            1,
            limits(),
        ),
        Err(ReplicationError::Disconnected)
    );
    assert_eq!(local.recv(), Err(ReplicationError::Disconnected));
    local.reconnect();
    must(local.send(frame(0, 0, b"abc", ChunkChain([0; 32])), 1));
    assert!(must(local.recv_frame()).is_some());
}
