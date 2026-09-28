#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use backend_cluster_transport::{
    BlobHash, CapabilityIssuer, ChunkRange, ControlGrantPage, ControlOffer, ControlResultReceipt,
    ControlRole, ExecutionFailureReason, GrantDirection, MAX_CONTROL_GRANT_PAGES,
    MAX_RESPONSE_BYTES, MaterializedObject, ProbeInventoryDescriptor, ProbeInventoryHasher,
    ProbeObjectClaim, SecretKey, StoreBlobCatalog, StoreObjectMapping, bind_direct,
    connect_control, now_unix_ms,
};
use backend_engine::compiler_cluster_transport::{
    CaptureWorkspaceIdentityV2, CapturedFullWorkspaceV2, CompilationUnitKeyV2, CompilerInputEntry,
    CompilerInputFileRole, CompilerInputManifestV1, CompilerInputManifestV2,
    CompilerInputMerklePageSchema, CompilerInputTreeRecordV2, CompilerInvocationRecipeV2,
    CompilerPackageTargetV2, CompilerResultGrantPageReceiver, CompilerTransportBridgeError,
    CompilerWorkspaceEntryV2, CompilerWorkspaceFileRoleV2, CompilerWorkspaceFileV2Schema,
    InputCompleteness, MAX_COMPILER_RESULT_OBJECTS, WorkspaceSnapshotSourceV2,
    accept_compiler_control, accept_compiler_control_for_scope, accept_coordinator_control,
    admit_compiler_input_manifest_v2, admit_compiler_result, capture_full_workspace_v2,
    compiler_assignment_scope, compiler_control_offer_v2, connect_compiler_control,
    connect_stored_compiler_result_ack, connect_worker_control,
    fallback_after_worker_execution_failure, fallback_after_worker_input_reject,
    issue_compiler_object_range, issue_worker_result_object_range, receive_compiler_accept,
    receive_compiler_cancel, receive_compiler_execution_failure, receive_compiler_grant_pages,
    receive_compiler_input_reject, receive_compiler_result, receive_worker_result_ack,
    send_compiler_cancel, send_compiler_grant_pages, send_compiler_offer_v2,
    send_rejected_compiler_result_ack, send_stored_compiler_result_ack,
    send_superseded_compiler_result_ack, send_worker_accept, send_worker_execution_failure,
    send_worker_input_reject, send_worker_result_grant_pages, send_worker_result_receipt,
    store_compiler_result, verify_full_workspace_closure_v2,
};
use backend_execution::{
    CompilerAssignment, CompilerAssignmentOutcome, CompilerAttemptToken, CompilerClusterScheduler,
    CompilerDemand, CompilerInputIdentityClaim, CompilerInputScope, CompilerPeerId,
    CompilerPlacementPolicy, CompilerRemoteEvidenceError, CompilerRemoteEvidenceVerifier,
    CompilerRemoteProbeBinding, CompilerWorkIdentity, CompletionCost, FullWorkspaceInputClaim,
    FullWorkspaceInputError, FullWorkspaceInputVerifier, LocalCompilerAvailability,
    PackageLineageId, RemoteCompilerCapabilityClaim, RemoteCompilerCostClaim,
    RemoteCompilerEvidenceClaim, RemoteHaveClaim, VerifiedCompilerInput, VerifiedRemoteCompiler,
    VerifierAcceptedFullWorkspaceInput,
};
use backend_replication::{AttemptId, Fence, ReplicationError};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, RustEdition, Stage};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectClaim, ArtifactPlan, ClosureId,
    ClosureManifest, FileStore, StoredClosureReceipt, StreamingClosureBudget, TypedObject,
    UntrustedObjectId,
};
use backend_version::{
    CompileRecipeDomain, ContentId, GenerationId, ObjectKey, Schema, SourceFactDomain,
    ToolchainDomain,
};

struct PayloadSchema;

fn compiler_attempt_token(
    attempt: u64,
    fence: [u8; 32],
) -> Result<CompilerAttemptToken, ReplicationError> {
    Ok(CompilerAttemptToken::new(
        AttemptId::new(attempt)?,
        Fence::from_bytes(fence)?,
    ))
}

impl Schema for PayloadSchema {
    const DOMAIN: u8 = 201;
    const TYPE: u16 = 1;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

struct TestDirectory(PathBuf);

#[derive(Clone)]
struct V2WorkspaceFixture {
    entries: Vec<CompilerWorkspaceEntryV2>,
    files: BTreeMap<String, Vec<u8>>,
    fence: [u8; 32],
}

impl WorkspaceSnapshotSourceV2 for V2WorkspaceFixture {
    fn entries(&self) -> &[CompilerWorkspaceEntryV2] {
        &self.entries
    }

    fn policy_identity(&self) -> &str {
        "nudox.compiler-workspace.v2/test;symlink=reject"
    }

    fn fence_digest(&self) -> [u8; 32] {
        self.fence
    }

    fn revalidate(&self) -> Result<bool, String> {
        let mut file_count = 0;
        for entry in &self.entries {
            if let backend_engine::compiler_cluster_transport::CompilerWorkspaceEntryKindV2::File {
                byte_length,
            } = entry.kind()
            {
                file_count += 1;
                if self.files.get(entry.path()).map(Vec::len)
                    != usize::try_from(byte_length).ok()
                {
                    return Ok(false);
                }
            }
        }
        Ok(file_count == self.files.len())
    }

    fn stream_file(
        &self,
        path: &str,
        max_bytes: u64,
        chunk_bytes: usize,
        consume: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, String> {
        if chunk_bytes == 0 {
            return Err("zero source stream chunk size".to_owned());
        }
        let bytes = self
            .files
            .get(path)
            .ok_or_else(|| format!("file is absent from snapshot: {path}"))?;
        let length = u64::try_from(bytes.len()).map_err(|error| error.to_string())?;
        if length != max_bytes {
            return Err(format!("file length changed for {path}"));
        }
        for chunk in bytes.chunks(chunk_bytes) {
            consume(chunk)?;
        }
        Ok(length)
    }
}

fn v2_workspace_fixture() -> V2WorkspaceFixture {
    let files = BTreeMap::from([
        ("src/empty.rs".to_owned(), Vec::new()),
        ("src/lib.rs".to_owned(), b"pub fn alpha() {}\n".to_vec()),
        (
            "src/nested/mod.rs".to_owned(),
            b"pub fn beta(x: u32) -> u32 { x + 1 }\n".to_vec(),
        ),
    ]);
    let entries = vec![
        CompilerWorkspaceEntryV2::directory(""),
        CompilerWorkspaceEntryV2::directory("src"),
        CompilerWorkspaceEntryV2::file("src/empty.rs", 0, CompilerWorkspaceFileRoleV2::Source),
        CompilerWorkspaceEntryV2::file(
            "src/lib.rs",
            u64::try_from(files["src/lib.rs"].len()).expect("bounded fixture file"),
            CompilerWorkspaceFileRoleV2::Source,
        ),
        CompilerWorkspaceEntryV2::directory("src/nested"),
        CompilerWorkspaceEntryV2::file(
            "src/nested/mod.rs",
            u64::try_from(files["src/nested/mod.rs"].len()).expect("bounded fixture file"),
            CompilerWorkspaceFileRoleV2::Source,
        ),
    ];
    V2WorkspaceFixture {
        entries,
        files,
        fence: [0x73; 32],
    }
}

fn independent_v2_offer_work_id(offer: &ControlOffer) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.compiler.full-workspace.v2\0");
    hasher.update(&offer.package_lineage);
    hasher.update(&offer.target);
    hasher.update(&offer.recipe);
    hasher.update(&offer.read_manifest);
    hasher.update(&offer.input_root);
    hasher.update(&offer.input_closure_id);
    hasher.update(&offer.input_manifest_object_id);
    hasher.update(&offer.max_output_bytes.to_be_bytes());
    let digest = hasher.finalize();
    let mut work_id = [0; 16];
    work_id.copy_from_slice(&digest.as_bytes()[..16]);
    work_id
}

fn independent_workspace_root(source: &V2WorkspaceFixture) -> [u8; 32] {
    let mut records = source
        .entries
        .iter()
        .map(|entry| match entry.kind() {
            backend_engine::compiler_cluster_transport::CompilerWorkspaceEntryKindV2::Directory => {
                CompilerInputTreeRecordV2::Directory {
                    path: entry.path().into(),
                }
            }
            backend_engine::compiler_cluster_transport::CompilerWorkspaceEntryKindV2::File {
                byte_length,
            } => {
                let bytes = &source.files[entry.path()];
                assert_eq!(
                    u64::try_from(bytes.len()).expect("test byte length"),
                    byte_length
                );
                let key = ObjectKey::<CompilerWorkspaceFileV2Schema>::from_value(bytes.as_slice());
                let object = TypedObject::from_value(&key, bytes.as_slice());
                CompilerInputTreeRecordV2::File {
                    path: entry.path().into(),
                    role: entry.role().expect("regular file role"),
                    object_id: *object.id().as_bytes(),
                    length: byte_length,
                }
            }
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.path()
            .as_bytes()
            .cmp(right.path().as_bytes())
            .then_with(|| reference_record_tag(left).cmp(&reference_record_tag(right)))
    });

    let priorities = records
        .iter()
        .map(|record| {
            let path = record.path().as_bytes();
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"backend.compiler.input.path-priority.v2\0");
            hasher.update(&[1]);
            hasher.update(
                &u32::try_from(path.len() + 1)
                    .expect("bounded test path")
                    .to_be_bytes(),
            );
            hasher.update(path);
            hasher.update(&[reference_record_tag(record)]);
            *hasher.finalize().as_bytes()
        })
        .collect::<Vec<_>>();
    let mut left = vec![None; records.len()];
    let mut right = vec![None; records.len()];
    let mut stack = Vec::<usize>::new();
    for index in 0..records.len() {
        let mut last = None;
        while stack
            .last()
            .is_some_and(|parent| priorities[*parent] >= priorities[index])
        {
            last = stack.pop();
        }
        if let Some(parent) = stack.last().copied() {
            right[parent] = Some(index);
        }
        left[index] = last;
        stack.push(index);
    }
    let root = *stack.first().expect("fixture has workspace records");
    let mut child_ids: Vec<Option<[u8; 32]>> = vec![None; records.len()];
    let mut traversal = vec![(root, false)];
    while let Some((index, visited)) = traversal.pop() {
        if !visited {
            traversal.push((index, true));
            if let Some(child) = right[index] {
                traversal.push((child, false));
            }
            if let Some(child) = left[index] {
                traversal.push((child, false));
            }
            continue;
        }

        let record = &records[index];
        let path = record.path().as_bytes();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BKCIPG02");
        bytes.push(1);
        bytes.push(reference_record_tag(record));
        bytes.extend_from_slice(
            &u32::try_from(path.len())
                .expect("bounded test path")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(path);
        if let CompilerInputTreeRecordV2::File {
            role,
            object_id,
            length,
            ..
        } = record
        {
            bytes.push(*role as u8);
            bytes.extend_from_slice(object_id);
            bytes.extend_from_slice(&length.to_be_bytes());
        }
        for child in [left[index], right[index]] {
            if let Some(child) = child {
                bytes.push(1);
                bytes.extend_from_slice(&child_ids[child].expect("postorder child identity"));
            } else {
                bytes.push(0);
            }
        }
        let key = ObjectKey::<CompilerInputMerklePageSchema>::from_value(bytes.as_slice());
        child_ids[index] = Some(
            *TypedObject::from_value(&key, bytes.as_slice())
                .id()
                .as_bytes(),
        );
    }
    child_ids[root].expect("captured workspace root")
}

fn reference_record_tag(record: &CompilerInputTreeRecordV2) -> u8 {
    match record {
        CompilerInputTreeRecordV2::Directory { .. } => 1,
        CompilerInputTreeRecordV2::File { .. } => 2,
        CompilerInputTreeRecordV2::PresentFile { .. } => 3,
        CompilerInputTreeRecordV2::AbsentPath { .. } => 4,
        CompilerInputTreeRecordV2::DirectoryListing { .. } => 5,
    }
}

fn control_offer_facts(
    offer: &ControlOffer,
) -> (
    backend_cluster_transport::AssignmentScope,
    [u8; 32],
    [u8; 32],
    [u8; 32],
    [u8; 32],
    [u8; 32],
    [u8; 32],
    [u8; 32],
    Option<[u8; 32]>,
    u64,
    u64,
    u32,
) {
    (
        offer.scope,
        offer.package_lineage,
        offer.target,
        offer.recipe,
        offer.input_root,
        offer.read_manifest,
        offer.input_closure_id,
        offer.input_manifest_object_id,
        offer.selected_base,
        offer.max_output_bytes,
        offer.deadline_unix_ms,
        offer.input_grant_pages,
    )
}

fn with_stage<T, E: std::fmt::Display>(
    stage: &'static str,
    result: Result<T, E>,
) -> Result<T, std::io::Error> {
    result.map_err(|error| std::io::Error::other(format!("{stage}: {error}")))
}

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "backend-engine-cluster-control-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn child(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct TestFullWorkspaceVerifier(FullWorkspaceInputClaim);

impl FullWorkspaceInputVerifier for TestFullWorkspaceVerifier {
    fn verify_full_workspace_capture(
        &self,
        claim: FullWorkspaceInputClaim,
    ) -> Result<(), FullWorkspaceInputError> {
        (claim == self.0)
            .then_some(())
            .ok_or(FullWorkspaceInputError::Rejected)
    }
}

struct TestRemoteEvidenceVerifier {
    expected: RemoteCompilerEvidenceClaim,
}

impl CompilerRemoteEvidenceVerifier for TestRemoteEvidenceVerifier {
    fn verify_remote_preflight(
        &self,
        expected: CompilerWorkIdentity,
        claim: RemoteCompilerEvidenceClaim,
    ) -> Result<(), CompilerRemoteEvidenceError> {
        if claim == self.expected
            && claim.work == expected
            && claim.binding.work_id() == expected.transfer_work_id()
            && claim.binding.attempt() == self.expected.binding.attempt()
            && claim.binding.namespace_id() == self.expected.binding.namespace_id()
            && claim.binding.fence() == self.expected.binding.fence()
            && claim.binding.nonce() != [0; 16]
            && claim.worker_incarnation == self.expected.worker_incarnation
            && claim.capability.supported
            && claim.capability.target == expected.target()
            && claim.capability.recipe == expected.recipe()
            && claim.capability.capability_digest == test_remote_capability_digest(expected)
            && claim.have.proof_digest == test_remote_preflight_proof_digest(&claim)
        {
            Ok(())
        } else {
            Err(CompilerRemoteEvidenceError::Rejected)
        }
    }
}

fn test_work_with_inventory()
-> Result<(CompilerWorkIdentity, ProbeInventoryDescriptor), Box<dyn std::error::Error>> {
    let capture_dir = TestDirectory::new()?;
    let (input_closure, _, manifest, inventory) =
        store_test_input_closure(&capture_dir.child("input-identity"))?;
    let identity = test_input_identity()?;
    let claim = FullWorkspaceInputClaim {
        identity,
        input_closure_id: *input_closure.as_bytes(),
        manifest_object_id: *manifest.object_id()?.as_bytes(),
    };
    let input =
        VerifierAcceptedFullWorkspaceInput::admit(claim, &TestFullWorkspaceVerifier(claim))?;
    Ok((
        CompilerWorkIdentity::new(
            identity.scope.package,
            identity.scope.target,
            identity.scope.recipe,
            VerifiedCompilerInput::FullWorkspaceFresh(input),
            None,
            8 * 1024 * 1024,
        )?,
        inventory,
    ))
}

fn capture_test_workspace_v2(
    root: &Path,
) -> Result<
    (
        FileStore,
        CapturedFullWorkspaceV2,
        backend_engine::compiler_cluster_transport::VerifiedFullWorkspaceClosureV2,
    ),
    Box<dyn std::error::Error>,
> {
    let package_text = "pkg:cargo/cluster-fixture@1.0.0";
    let package = PackageUrl::parse(package_text.to_owned())
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let package_target = CompilerPackageTargetV2::for_package(package);
    let invocation = CompilerInvocationRecipeV2::new(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        backend_semantic::vocabulary::NativeTool::Rustc,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"cluster-control-test-rustc"),
        [0x34; 32],
        [0x35; 32],
        [0x36; 32],
    )?;
    let lineage = PackageLineageId::from_canonical_parts(
        b"registry:local-test",
        package_text.as_bytes(),
        b"branch:main",
    )?;
    let identity = CaptureWorkspaceIdentityV2::new(
        package_target,
        lineage.as_bytes(),
        invocation,
        [0x37; 32],
        8 * 1024 * 1024,
    );

    let mut files = BTreeMap::new();
    let mut entries = vec![
        CompilerWorkspaceEntryV2::directory(""),
        CompilerWorkspaceEntryV2::directory("src"),
    ];
    for index in 0..10_u8 {
        let path = format!("src/unit_{index:02}.rs");
        let payload = vec![index.wrapping_add(1); 1024];
        entries.push(CompilerWorkspaceEntryV2::file(
            path.clone(),
            u64::try_from(payload.len())?,
            CompilerWorkspaceFileRoleV2::Source,
        ));
        files.insert(path, payload);
    }
    let source = V2WorkspaceFixture {
        entries,
        files,
        fence: [0x73; 32],
    };
    let store = FileStore::open(root, 32 * 1024 * 1024)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let budget = StreamingClosureBudget::new(
        128,
        16 * 1024 * 1024,
        1024 * 1024,
        64 * 1024,
        8192,
        16 * 1024 * 1024,
    );
    let capture = capture_full_workspace_v2(&source, identity, &store, budget)?;
    let verified = capture.verify_in_store(&store)?;
    Ok((store, capture, verified))
}

fn work_for_test_capture(
    capture: &CapturedFullWorkspaceV2,
) -> Result<CompilerWorkIdentity, Box<dyn std::error::Error>> {
    let manifest = capture.manifest();
    let identity = manifest.identity_claim()?;
    let claim = FullWorkspaceInputClaim {
        identity,
        input_closure_id: *capture.closure().as_bytes(),
        manifest_object_id: *capture.manifest_object_id().as_bytes(),
    };
    let accepted =
        VerifierAcceptedFullWorkspaceInput::admit(claim, &TestFullWorkspaceVerifier(claim))?;
    Ok(CompilerWorkIdentity::new(
        identity.scope.package,
        identity.scope.target,
        identity.scope.recipe,
        VerifiedCompilerInput::FullWorkspaceFresh(accepted),
        None,
        manifest.max_output_bytes(),
    )?)
}

fn make_assignment_for_test_capture(
    worker: backend_cluster_transport::EndpointId,
    namespace_id: [u8; 16],
    work: CompilerWorkIdentity,
    inventory: ProbeInventoryDescriptor,
    attempt_token: CompilerAttemptToken,
) -> Result<(CompilerClusterScheduler, CompilerAssignment), Box<dyn std::error::Error>> {
    let peer = CompilerPeerId::new(*worker.as_bytes())?;
    let (claim, verifier) =
        test_remote_preflight(work, peer, namespace_id, attempt_token, inventory, 10, 500)?;
    let remote = VerifiedRemoteCompiler::admit(work, claim, &verifier)?;
    let scheduler = CompilerClusterScheduler::new(NonZeroUsize::new(2).ok_or("zero capacity")?);
    let outcome = scheduler.place_and_assign(
        &CompilerPlacementPolicy,
        work,
        CompilerDemand::Background,
        LocalCompilerAvailability::Ready,
        100,
        10,
        &[remote],
        attempt_token,
    )?;
    let CompilerAssignmentOutcome::Assigned(assignment) = outcome else {
        return Err("V2 test assignment unexpectedly selected offline".into());
    };
    Ok((scheduler, assignment))
}

fn test_remote_preflight(
    work: CompilerWorkIdentity,
    peer: CompilerPeerId,
    namespace_id: [u8; 16],
    attempt_token: backend_execution::CompilerAttemptToken,
    inventory: ProbeInventoryDescriptor,
    observed_at: u64,
    expires_at: u64,
) -> Result<(RemoteCompilerEvidenceClaim, TestRemoteEvidenceVerifier), Box<dyn std::error::Error>> {
    let full_workspace = work
        .input()
        .full_workspace()
        .ok_or_else(|| std::io::Error::other("test work must be a full-workspace capture"))?
        .claim();
    let attempt = attempt_token.attempt().get();
    let fence = attempt_token.fence().as_bytes();
    let mut nonce_hasher = blake3::Hasher::new();
    nonce_hasher.update(b"backend.engine.tests.compiler-preflight-nonce.v1\0");
    nonce_hasher.update(&work.transfer_work_id());
    nonce_hasher.update(&peer.as_bytes());
    nonce_hasher.update(&attempt.to_be_bytes());
    nonce_hasher.update(&fence);
    let nonce_digest = nonce_hasher.finalize();
    let mut nonce = [0; 16];
    nonce.copy_from_slice(&nonce_digest.as_bytes()[..16]);
    let binding = CompilerRemoteProbeBinding::new(
        namespace_id,
        work.transfer_work_id(),
        attempt,
        fence,
        nonce,
        observed_at,
        expires_at,
    )?;

    let mut incarnation_hasher = blake3::Hasher::new();
    incarnation_hasher.update(b"backend.engine.tests.compiler-worker-incarnation.v1\0");
    incarnation_hasher.update(&peer.as_bytes());
    incarnation_hasher.update(&attempt.to_be_bytes());
    let worker_incarnation = *incarnation_hasher.finalize().as_bytes();

    let capability = RemoteCompilerCapabilityClaim {
        target: work.target(),
        recipe: work.recipe(),
        supported: true,
        capability_digest: test_remote_capability_digest(work),
    };
    let cost = RemoteCompilerCostClaim {
        completion: CompletionCost {
            execution: 1,
            ..CompletionCost::default()
        },
        confidence_per_mille: 950,
        observed_at,
        expires_at,
    };
    let mut claim = RemoteCompilerEvidenceClaim {
        peer,
        work,
        binding,
        worker_incarnation,
        capability,
        have: RemoteHaveClaim {
            input_manifest: work.input_identity().manifest,
            input_closure_id: full_workspace.input_closure_id,
            manifest_object_id: full_workspace.manifest_object_id,
            inventory_digest: inventory.digest,
            required_objects: inventory.object_count,
            verified_have_objects: 0,
            owner_streamable_objects: inventory.object_count,
            missing_bytes: inventory.payload_bytes,
            proof_digest: [0; 32],
        },
        cost,
    };
    claim.have.proof_digest = test_remote_preflight_proof_digest(&claim);
    let verifier = TestRemoteEvidenceVerifier { expected: claim };
    Ok((claim, verifier))
}

fn test_remote_capability_digest(work: CompilerWorkIdentity) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.engine.tests.compiler-capability.v1\0");
    hasher.update(work.target().as_ref());
    hasher.update(work.recipe().as_ref());
    *hasher.finalize().as_bytes()
}

fn test_remote_preflight_proof_digest(claim: &RemoteCompilerEvidenceClaim) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.engine.tests.compiler-preflight-proof.v1\0");
    hasher.update(&claim.peer.as_bytes());
    hasher.update(&claim.work.transfer_work_id());
    hasher.update(&claim.binding.namespace_id());
    hasher.update(&claim.binding.work_id());
    hasher.update(&claim.binding.attempt().to_be_bytes());
    hasher.update(&claim.binding.fence());
    hasher.update(&claim.binding.nonce());
    hasher.update(&claim.binding.observed_at().to_be_bytes());
    hasher.update(&claim.binding.expires_at().to_be_bytes());
    hasher.update(&claim.worker_incarnation);
    hasher.update(claim.capability.target.as_ref());
    hasher.update(claim.capability.recipe.as_ref());
    hasher.update(&[u8::from(claim.capability.supported)]);
    hasher.update(&claim.capability.capability_digest);
    hasher.update(claim.have.input_manifest.as_bytes());
    hasher.update(&claim.have.input_closure_id);
    hasher.update(&claim.have.manifest_object_id);
    hasher.update(&claim.have.inventory_digest);
    hasher.update(&claim.have.required_objects.to_be_bytes());
    hasher.update(&claim.have.verified_have_objects.to_be_bytes());
    hasher.update(&claim.have.owner_streamable_objects.to_be_bytes());
    hasher.update(&claim.have.missing_bytes.to_be_bytes());
    hasher.update(&claim.cost.completion.client_queue.to_be_bytes());
    hasher.update(&claim.cost.completion.round_trip.to_be_bytes());
    hasher.update(&claim.cost.completion.input_transfer.to_be_bytes());
    hasher.update(&claim.cost.completion.worker_queue.to_be_bytes());
    hasher.update(&claim.cost.completion.warmup.to_be_bytes());
    hasher.update(&claim.cost.completion.execution.to_be_bytes());
    hasher.update(&claim.cost.completion.output_transfer.to_be_bytes());
    hasher.update(&claim.cost.completion.validation.to_be_bytes());
    hasher.update(&claim.cost.completion.contention.to_be_bytes());
    hasher.update(&claim.cost.confidence_per_mille.to_be_bytes());
    hasher.update(&claim.cost.observed_at.to_be_bytes());
    hasher.update(&claim.cost.expires_at.to_be_bytes());
    *hasher.finalize().as_bytes()
}

#[test]
fn remote_preflight_rejects_input_and_attempt_mismatches() -> Result<(), Box<dyn std::error::Error>>
{
    let (work, inventory) = test_work_with_inventory()?;
    let peer = CompilerPeerId::new([0x5a; 32])?;
    let attempt_token = compiler_attempt_token(23, [0x5b; 32])?;
    let (claim, verifier) =
        test_remote_preflight(work, peer, [0x5c; 16], attempt_token, inventory, 10, 500)?;
    VerifiedRemoteCompiler::admit(work, claim, &verifier)?;

    let mut wrong_closure = claim;
    wrong_closure.have.input_closure_id[0] ^= 1;
    wrong_closure.have.proof_digest = test_remote_preflight_proof_digest(&wrong_closure);
    assert!(matches!(
        VerifiedRemoteCompiler::admit(work, wrong_closure, &verifier),
        Err(CompilerRemoteEvidenceError::ScopeMismatch)
    ));

    let mut wrong_manifest = claim;
    wrong_manifest.have.manifest_object_id[0] ^= 1;
    wrong_manifest.have.proof_digest = test_remote_preflight_proof_digest(&wrong_manifest);
    assert!(matches!(
        VerifiedRemoteCompiler::admit(work, wrong_manifest, &verifier),
        Err(CompilerRemoteEvidenceError::ScopeMismatch)
    ));

    let mut wrong_attempt = claim;
    wrong_attempt.binding = CompilerRemoteProbeBinding::new(
        claim.binding.namespace_id(),
        claim.binding.work_id(),
        claim.binding.attempt() + 1,
        claim.binding.fence(),
        claim.binding.nonce(),
        claim.binding.observed_at(),
        claim.binding.expires_at(),
    )?;
    wrong_attempt.have.proof_digest = test_remote_preflight_proof_digest(&wrong_attempt);
    assert!(matches!(
        VerifiedRemoteCompiler::admit(work, wrong_attempt, &verifier),
        Err(CompilerRemoteEvidenceError::Rejected)
    ));
    Ok(())
}

fn test_input_identity()
-> Result<CompilerInputIdentityClaim, backend_execution::CompilerIdentityError> {
    // V1 manifests bind the target to the exact pinned package URL identity.
    // Keep this package spelling in sync with `store_test_input_closure`.
    let package = PackageUrl::parse("pkg:cargo/cluster-fixture@1.0.0".to_owned())
        .expect("test package URL is valid");
    Ok(CompilerInputIdentityClaim {
        scope: CompilerInputScope {
            package: PackageLineageId::from_canonical_parts(
                b"registry:local-test",
                b"pkg://rust/crates/cluster-fixture",
                b"branch:main",
            )?,
            target: package.identity,
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe:test"),
        },
        input_root: GenerationId::from_canonical_bytes(b"input-root"),
        manifest: backend_execution::ReadManifestId::from_value(b"verified-full-workspace"),
    })
}

fn store_test_closure(
    root: &Path,
    first_seed: u8,
) -> Result<(ClosureId, Vec<MaterializedObject>), Box<dyn std::error::Error>> {
    let store = FileStore::open(root, 16 * 1024 * 1024)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let mut members = Vec::new();
    let mut materialized = Vec::new();
    for index in 0..10_u8 {
        let seed = first_seed.wrapping_add(index);
        let payload = vec![seed; 1024];
        let key = ObjectKey::<PayloadSchema>::from_value(&payload);
        let object = TypedObject::from_value(&key, payload.as_slice());
        materialized.push(MaterializedObject {
            blob_hash: BlobHash(*blake3::hash(&payload).as_bytes()),
            object: StoreObjectMapping::from_typed_object(&object),
        });
        members.push(object);
    }
    members.sort_by(|left, right| {
        (left.schema(), left.key(), left.version()).cmp(&(
            right.schema(),
            right.key(),
            right.version(),
        ))
    });
    let closure = ClosureManifest::new(members)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let closure_id = store
        .write_closure(&closure)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    Ok((closure_id, materialized))
}

fn store_test_closure_with_receipt(
    root: &Path,
    first_seed: u8,
) -> Result<(StoredClosureReceipt, Vec<MaterializedObject>), Box<dyn std::error::Error>> {
    let store = FileStore::open(root, 16 * 1024 * 1024)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let mut members = Vec::new();
    let mut materialized = Vec::new();
    for index in 0..10_u8 {
        let seed = first_seed.wrapping_add(index);
        let payload = vec![seed; 1024];
        let key = ObjectKey::<PayloadSchema>::from_value(&payload);
        let object = TypedObject::from_value(&key, payload.as_slice());
        materialized.push(MaterializedObject {
            blob_hash: BlobHash(*blake3::hash(&payload).as_bytes()),
            object: StoreObjectMapping::from_typed_object(&object),
        });
        members.push(object);
    }
    members.sort_by(|left, right| {
        (left.schema(), left.key(), left.version()).cmp(&(
            right.schema(),
            right.key(),
            right.version(),
        ))
    });
    let closure = ClosureManifest::new(members.clone())
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let claims = members
        .iter()
        .map(|object| {
            ArtifactObjectClaim::new(
                object.schema(),
                *object.key(),
                *object.version(),
                u64::try_from(object.bytes().len()).expect("fixture payload length fits u64"),
            )
            .with_object_id(backend_store::UntrustedObjectId::from_bytes(
                *object.id().as_bytes(),
            ))
        })
        .collect();
    let mut session = store
        .artifact_sink(ArtifactBudget::new(16, 16, 16 * 1024 * 1024, 1024, 16))
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(closure.id()),
            claims,
            Vec::new(),
        ))
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    for (index, object) in members.iter().enumerate() {
        session
            .put(index, 0, object.bytes())
            .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    }
    let receipt = session
        .finish()
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    Ok((receipt, materialized))
}

fn store_test_input_closure(
    root: &Path,
) -> Result<
    (
        ClosureId,
        Vec<MaterializedObject>,
        CompilerInputManifestV1,
        ProbeInventoryDescriptor,
    ),
    Box<dyn std::error::Error>,
> {
    let store = FileStore::open(root, 16 * 1024 * 1024)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let identity = test_input_identity()?;
    let mut members = Vec::new();
    let mut materialized = Vec::new();
    let mut entries = vec![CompilerInputEntry::Directory { path: "src".into() }];
    for index in 0..10_u8 {
        let payload = vec![index.wrapping_add(1); 1024];
        let key = ObjectKey::<PayloadSchema>::from_value(&payload);
        let object = TypedObject::from_value(&key, payload.as_slice());
        entries.push(CompilerInputEntry::File {
            path: format!("src/unit_{index:02}.rs").into_boxed_str(),
            role: CompilerInputFileRole::Source,
            object_id: backend_store::UntrustedObjectId::from_bytes(*object.id().as_bytes()),
            length: 1024,
        });
        materialized.push(MaterializedObject {
            blob_hash: BlobHash(*blake3::hash(&payload).as_bytes()),
            object: StoreObjectMapping::from_typed_object(&object),
        });
        members.push(object);
    }
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    let input_manifest = CompilerInputManifestV1::new(
        PackageUrl::parse("pkg:cargo/cluster-fixture@1.0.0".to_owned())
            .map_err(|error| std::io::Error::other(format!("{error:?}")))?,
        identity.scope.package.as_bytes(),
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        *identity.scope.target.as_ref(),
        *identity.scope.recipe.as_ref(),
        [0x31; 32],
        [0x32; 32],
        [0x33; 32],
        *identity.input_root.as_ref(),
        *identity.manifest.as_bytes(),
        None,
        8 * 1024 * 1024,
        InputCompleteness::FullWorkspace,
        entries,
        Vec::new(),
    )?;
    let manifest_object = input_manifest.typed_object()?;
    materialized.insert(
        0,
        MaterializedObject {
            blob_hash: BlobHash(*blake3::hash(manifest_object.bytes()).as_bytes()),
            object: StoreObjectMapping::from_typed_object(&manifest_object),
        },
    );
    members.push(manifest_object);
    members.sort_by(|left, right| {
        (left.schema(), left.key(), left.version()).cmp(&(
            right.schema(),
            right.key(),
            right.version(),
        ))
    });
    let closure = ClosureManifest::new(members)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let closure_id = store
        .write_closure(&closure)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let inventory = probe_inventory_from_store(&store, closure_id)?;
    Ok((closure_id, materialized, input_manifest, inventory))
}

fn probe_inventory_from_store(
    store: &FileStore,
    closure: ClosureId,
) -> Result<ProbeInventoryDescriptor, Box<dyn std::error::Error>> {
    let index = store
        .read_closure_index(closure)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let mut inventory = ProbeInventoryHasher::new();
    let mut after = None;
    loop {
        let page = index
            .page_ids(after, 256)
            .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
        for object_id in page.object_ids() {
            let envelope = store
                .verify_object_claim(UntrustedObjectId::from_bytes(*object_id.as_bytes()))
                .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
            inventory.push(ProbeObjectClaim {
                object_id: *object_id.as_bytes(),
                payload_bytes: u32::try_from(envelope.payload_len())?,
            })?;
        }
        after = page.next();
        if after.is_none() {
            break;
        }
    }
    Ok(inventory.finish()?)
}

fn materialized_v2_closure_members(
    store: &FileStore,
    verified: &backend_engine::compiler_cluster_transport::VerifiedFullWorkspaceClosureV2,
) -> Result<Vec<MaterializedObject>, Box<dyn std::error::Error>> {
    verified
        .member_ids()
        .map(|object_id| {
            let object = store
                .read_object_claim(UntrustedObjectId::from_bytes(*object_id.as_bytes()))
                .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
            Ok(MaterializedObject {
                blob_hash: BlobHash(*blake3::hash(object.bytes()).as_bytes()),
                object: StoreObjectMapping::from_typed_object(&object),
            })
        })
        .collect()
}

fn page_series(
    scope: backend_cluster_transport::AssignmentScope,
    direction: GrantDirection,
    grants: Vec<backend_cluster_transport::Capability>,
) -> Vec<ControlGrantPage> {
    let page_count = u32::try_from(grants.len()).expect("bounded grant page count");
    grants
        .into_iter()
        .enumerate()
        .map(|(page_index, grant)| ControlGrantPage {
            scope,
            direction,
            page_index: u32::try_from(page_index).expect("bounded page index"),
            page_count,
            grants: vec![grant],
        })
        .collect()
}

fn result_claims(
    owner: backend_cluster_transport::EndpointId,
    worker: backend_cluster_transport::EndpointId,
    scope: backend_cluster_transport::AssignmentScope,
    closure: ClosureId,
    object: MaterializedObject,
    nonce: u8,
) -> Result<backend_cluster_transport::Capability, Box<dyn std::error::Error>> {
    let worker_key = SecretKey::from_bytes(&[2; 32]);
    let issuer = CapabilityIssuer::new(worker_key);
    Ok(issue_worker_result_object_range(
        &issuer,
        scope,
        owner,
        worker,
        closure,
        object,
        ChunkRange { start: 0, end: 1 },
        MAX_RESPONSE_BYTES,
        now_unix_ms()? + 60_000,
        [nonce; 16],
    )?)
}

fn make_assignment(
    worker: backend_cluster_transport::EndpointId,
    namespace_id: [u8; 16],
) -> Result<(CompilerClusterScheduler, CompilerAssignment), Box<dyn std::error::Error>> {
    let (work, inventory) = test_work_with_inventory()?;
    let peer = CompilerPeerId::new(*worker.as_bytes())?;
    let attempt_token = compiler_attempt_token(7, [0x51; 32])?;
    let (claim, verifier) =
        test_remote_preflight(work, peer, namespace_id, attempt_token, inventory, 10, 500)?;
    let verified = VerifiedRemoteCompiler::admit(work, claim, &verifier)?;
    let scheduler = CompilerClusterScheduler::new(NonZeroUsize::new(2).ok_or("zero capacity")?);
    let outcome = scheduler.place_and_assign(
        &CompilerPlacementPolicy,
        work,
        CompilerDemand::Background,
        LocalCompilerAvailability::Ready,
        100,
        10,
        &[verified],
        attempt_token,
    )?;
    let CompilerAssignmentOutcome::Assigned(assignment) = outcome else {
        return Err("assignment unexpectedly selected offline".into());
    };
    Ok((scheduler, assignment))
}

#[test]
fn v2_offer_hash_matcher_binds_manifest_only_fixture_without_capture_admission()
-> Result<(), Box<dyn std::error::Error>> {
    let package = PackageUrl::parse("pkg:cargo/cluster-v2-fixture@1.0.0".to_owned())
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let package_target = CompilerPackageTargetV2::new(
        package,
        CompilationUnitKeyV2::RustCrate {
            name: "lib".into(),
            root: "src/lib.rs".into(),
        },
    )?;
    let invocation = CompilerInvocationRecipeV2::new(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        backend_semantic::vocabulary::NativeTool::Rustc,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"v2-test-rustc"),
        [0x74; 32],
        [0x75; 32],
        [0x76; 32],
    )?;
    let manifest = CompilerInputManifestV2::new(
        package_target,
        PackageLineageId::from_canonical_parts(
            b"registry:local-test",
            b"pkg:cargo/cluster-v2-fixture@1.0.0",
            b"branch:main",
        )?
        .as_bytes(),
        invocation,
        [0x77; 32],
        "nudox.compiler-workspace.v2/test;symlink=reject",
        [0x78; 32],
        [0x79; 32],
        8 * 1024 * 1024,
    )?;
    let manifest_object = manifest.typed_object()?;
    let temp = TestDirectory::new()?;
    let store = FileStore::open(temp.child("manifest-only-input"), 16 * 1024 * 1024)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let input_closure = store
        .write_closure(
            &ClosureManifest::new(vec![manifest_object.clone()])
                .map_err(|error| std::io::Error::other(format!("{error:?}")))?,
        )
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let inventory = probe_inventory_from_store(&store, input_closure)?;
    let identity = manifest.identity_claim()?;
    let claim = FullWorkspaceInputClaim {
        identity,
        input_closure_id: *input_closure.as_bytes(),
        manifest_object_id: *manifest_object.id().as_bytes(),
    };
    let captured =
        VerifierAcceptedFullWorkspaceInput::admit(claim, &TestFullWorkspaceVerifier(claim))?;
    let work = CompilerWorkIdentity::new(
        identity.scope.package,
        identity.scope.target,
        identity.scope.recipe,
        VerifiedCompilerInput::FullWorkspaceFresh(captured),
        None,
        manifest.max_output_bytes(),
    )?;
    let peer = CompilerPeerId::new([0x7a; 32])?;
    let namespace_id = [0x7e; 16];
    let attempt_token = compiler_attempt_token(9, [0x7d; 32])?;
    let (preflight, verifier) =
        test_remote_preflight(work, peer, namespace_id, attempt_token, inventory, 5, 500)?;
    let remote = VerifiedRemoteCompiler::admit(work, preflight, &verifier)?;
    let scheduler = CompilerClusterScheduler::new(NonZeroUsize::new(1).ok_or("zero")?);
    let CompilerAssignmentOutcome::Assigned(assignment) = scheduler.place_and_assign(
        &CompilerPlacementPolicy,
        work,
        CompilerDemand::Background,
        LocalCompilerAvailability::Ready,
        10,
        100,
        &[remote],
        attempt_token,
    )?
    else {
        return Err("V2 test assignment unexpectedly selected offline".into());
    };

    let offer = compiler_control_offer_v2(
        assignment,
        namespace_id,
        input_closure,
        &manifest,
        1,
        now_unix_ms()? + 60_000,
    )?;
    assert_eq!(offer.scope.work_id, work.transfer_work_id());
    assert_eq!(offer.input_closure_id, *input_closure.as_bytes());
    assert_eq!(
        offer.input_manifest_object_id,
        *manifest.object_id()?.as_bytes()
    );
    assert_eq!(manifest.matches_offer(&offer, input_closure), Ok(true));

    let mut changed_closure = offer.clone();
    changed_closure.input_closure_id[0] ^= 1;
    assert_eq!(
        manifest.matches_offer(&changed_closure, input_closure),
        Ok(false)
    );
    let mut changed_manifest = offer;
    changed_manifest.input_manifest_object_id[0] ^= 1;
    assert_eq!(
        manifest.matches_offer(&changed_manifest, input_closure),
        Ok(false)
    );
    Ok(())
}

#[tokio::test]
async fn v2_offer_binds_independently_captured_workspace_pages_and_files()
-> Result<(), Box<dyn std::error::Error>> {
    let package = PackageUrl::parse("pkg:cargo/cluster-v2-captured@1.0.0".to_owned())
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let package_target = CompilerPackageTargetV2::new(
        package,
        CompilationUnitKeyV2::RustCrate {
            name: "lib".into(),
            root: "src/lib.rs".into(),
        },
    )?;
    let invocation = CompilerInvocationRecipeV2::new(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        backend_semantic::vocabulary::NativeTool::Rustc,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"captured-v2-test-rustc"),
        [0x84; 32],
        [0x85; 32],
        [0x86; 32],
    )?;
    let package_lineage = PackageLineageId::from_canonical_parts(
        b"registry:local-test",
        b"pkg:cargo/cluster-v2-captured@1.0.0",
        b"branch:main",
    )?;
    let capture_identity = CaptureWorkspaceIdentityV2::new(
        package_target,
        package_lineage.as_bytes(),
        invocation,
        [0x87; 32],
        8 * 1024 * 1024,
    );
    let temp = TestDirectory::new()?;
    let store = FileStore::open(temp.child("v2-capture-store"), 16 * 1024 * 1024)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let budget = StreamingClosureBudget::new(
        64,
        8 * 1024 * 1024,
        1024 * 1024,
        64 * 1024,
        4096,
        8 * 1024 * 1024,
    );
    let source = v2_workspace_fixture();
    let capture = capture_full_workspace_v2(&source, capture_identity.clone(), &store, budget)?;
    assert_eq!(
        capture.pinned_receipt().receipt().closure(),
        capture.closure(),
        "the capture value retains the GC pin for its sealed input closure"
    );
    let verified = capture.verify_in_store(&store)?;
    assert_eq!(verified.closure(), capture.closure());
    assert_eq!(
        verified.source_identity("src/lib.rs"),
        Some(ContentId::<SourceFactDomain>::from_canonical_bytes(
            &source.files["src/lib.rs"]
        ))
    );
    assert_eq!(
        verified.source_identity("src/empty.rs"),
        Some(ContentId::<SourceFactDomain>::from_canonical_bytes(b""))
    );
    assert_eq!(verified.source_identity("Cargo.toml"), None);
    assert!(
        capture.object_count() >= 6,
        "capture must include actual files, page(s), and manifest"
    );
    assert!(verified
        .workspace_records()
        .any(|record| matches!(record, backend_engine::compiler_cluster_transport::CompilerInputTreeRecordV2::Directory { path } if path.as_ref() == "src/nested")));
    assert!(verified.workspace_records().any(|record| matches!(
        record,
        backend_engine::compiler_cluster_transport::CompilerInputTreeRecordV2::File {
            path,
            length: 0,
            ..
        } if path.as_ref() == "src/empty.rs"
    )));

    let manifest = capture.manifest();
    assert_eq!(
        manifest.workspace_root(),
        independent_workspace_root(&source),
        "the captured Merkle root must match an independently encoded asymmetric fixture"
    );
    let manifest_object = manifest.typed_object()?;
    let identity = manifest.identity_claim()?;
    let claim = FullWorkspaceInputClaim {
        identity,
        input_closure_id: *capture.closure().as_bytes(),
        manifest_object_id: *capture.manifest_object_id().as_bytes(),
    };
    assert_eq!(manifest_object.id(), capture.manifest_object_id());
    let accepted =
        VerifierAcceptedFullWorkspaceInput::admit(claim, &TestFullWorkspaceVerifier(claim))?;
    let work = CompilerWorkIdentity::new(
        identity.scope.package,
        identity.scope.target,
        identity.scope.recipe,
        VerifiedCompilerInput::FullWorkspaceFresh(accepted),
        None,
        manifest.max_output_bytes(),
    )?;
    let namespace_id = [0x8c; 16];
    let owner = bind_direct(
        SecretKey::from_bytes(&[31; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let worker = bind_direct(
        SecretKey::from_bytes(&[32; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let peer = CompilerPeerId::new(*worker.id().as_bytes())?;
    let inventory = probe_inventory_from_store(&store, capture.closure())?;
    let attempt_token = compiler_attempt_token(10, [0x8b; 32])?;
    let (preflight, verifier) =
        test_remote_preflight(work, peer, namespace_id, attempt_token, inventory, 5, 500)?;
    let remote = VerifiedRemoteCompiler::admit(work, preflight, &verifier)?;
    let scheduler = CompilerClusterScheduler::new(NonZeroUsize::new(1).ok_or("zero")?);
    let CompilerAssignmentOutcome::Assigned(assignment) = scheduler.place_and_assign(
        &CompilerPlacementPolicy,
        work,
        CompilerDemand::Background,
        LocalCompilerAvailability::Ready,
        10,
        100,
        &[remote],
        attempt_token,
    )?
    else {
        return Err("V2 captured-workspace test assignment unexpectedly selected offline".into());
    };

    let offer_deadline = now_unix_ms()? + 60_000;
    let offer = compiler_control_offer_v2(
        assignment,
        namespace_id,
        capture.closure(),
        manifest,
        1,
        offer_deadline,
    )?;
    assert_eq!(offer.scope.work_id, independent_v2_offer_work_id(&offer));
    assert_eq!(offer.scope.work_id, work.transfer_work_id());
    assert_eq!(
        admit_compiler_input_manifest_v2(&offer, capture.closure(), &manifest_object)?,
        manifest.clone()
    );

    // Exercise the V2 builder through an authenticated Iroh control stream. The worker admits
    // the received manifest only after it binds the Offer to the captured input closure.
    let worker_listener = worker.clone();
    let owner_id = owner.id();
    let expected_offer = offer.clone();
    let input_closure = capture.closure();
    let expected_manifest = manifest.clone();
    let worker_manifest_object = manifest_object.clone();
    let worker_task = tokio::spawn(async move {
        let allowed_owner = CompilerPeerId::new(*owner_id.as_bytes())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let mut channel = with_stage(
            "V2 worker accept Offer stream",
            accept_compiler_control(&worker_listener, [allowed_owner]).await,
        )?;
        let received = with_stage(
            "V2 worker receive Offer",
            backend_engine::compiler_cluster_transport::receive_compiler_offer(&mut channel).await,
        )?;
        assert_eq!(
            control_offer_facts(&received),
            control_offer_facts(&expected_offer)
        );
        let admitted = with_stage(
            "V2 worker admit full-workspace manifest",
            admit_compiler_input_manifest_v2(&received, input_closure, &worker_manifest_object),
        )?;
        assert_eq!(admitted, expected_manifest);
        with_stage("V2 worker finish Offer stream", channel.finish().await)?;
        Ok::<(), std::io::Error>(())
    });
    let mut owner_channel =
        connect_compiler_control(&scheduler, &owner, worker.addr(), assignment, namespace_id)
            .await?;
    send_compiler_offer_v2(
        &mut owner_channel,
        &scheduler,
        assignment,
        namespace_id,
        input_closure,
        manifest,
        1,
        offer_deadline,
    )
    .await?;
    owner_channel.finish().await?;
    worker_task.await??;

    let mut wrong_closure = offer.clone();
    wrong_closure.input_closure_id[0] ^= 1;
    assert!(
        manifest
            .matches_offer(&wrong_closure, capture.closure())
            .is_ok_and(|matches| !matches)
    );
    let mut wrong_manifest = offer.clone();
    wrong_manifest.input_manifest_object_id[0] ^= 1;
    assert!(
        manifest
            .matches_offer(&wrong_manifest, capture.closure())
            .is_ok_and(|matches| !matches)
    );

    // Build a new closure from a separately recaptured source whose file bytes and page IDs
    // differ, then splice those pages under the old manifest. Independent closure admission must
    // reject this page-root substitution before any worker materializes the snapshot.
    let mut changed_source = source;
    changed_source
        .files
        .get_mut("src/lib.rs")
        .expect("fixture file")[0] ^= 1;
    changed_source.fence = [0x8d; 32];
    let changed_capture =
        capture_full_workspace_v2(&changed_source, capture_identity, &store, budget)?;
    assert_ne!(
        changed_capture.manifest().workspace_root(),
        manifest.workspace_root()
    );
    assert_ne!(changed_capture.closure(), capture.closure());
    let changed_verified = changed_capture.verify_in_store(&store)?;
    let mut mixed_members = vec![manifest_object.clone()];
    for object_id in changed_verified.member_ids() {
        if object_id != changed_capture.manifest_object_id() {
            mixed_members.push(
                store
                    .read_object_claim(UntrustedObjectId::from_bytes(*object_id.as_bytes()))
                    .map_err(|error| std::io::Error::other(format!("{error:?}")))?,
            );
        }
    }
    mixed_members.sort_by(|left, right| {
        (left.schema(), left.key(), left.version()).cmp(&(
            right.schema(),
            right.key(),
            right.version(),
        ))
    });
    assert!(
        mixed_members.windows(2).all(|pair| {
            (pair[0].schema(), pair[0].key(), pair[0].version())
                < (pair[1].schema(), pair[1].key(), pair[1].version())
        }),
        "mixed closure fixture contains duplicate or noncanonical members"
    );
    let mixed_closure = ClosureManifest::new(mixed_members)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let mixed_closure_id = store
        .write_closure(&mixed_closure)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    assert!(
        verify_full_workspace_closure_v2(
            &store,
            mixed_closure_id,
            capture.manifest_object_id(),
            Some(capture.object_count()),
        )
        .is_err()
    );
    let mut wrong_page_closure = offer;
    wrong_page_closure.input_closure_id = *mixed_closure_id.as_bytes();
    assert!(
        admit_compiler_input_manifest_v2(&wrong_page_closure, mixed_closure_id, &manifest_object,)
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn result_receipt_bounds_output_and_object_pages_before_grant_allocation()
-> Result<(), Box<dyn std::error::Error>> {
    let worker = bind_direct(
        SecretKey::from_bytes(&[44; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let namespace_id = [0x74; 16];
    let (scheduler, assignment) = make_assignment(worker.id(), namespace_id)?;
    let receipt = ControlResultReceipt {
        scope: compiler_assignment_scope(assignment, namespace_id)?,
        target_root: [0x75; 32],
        pack_id: Some([0x76; 32]),
        closure_id: [0x77; 32],
        object_count: MAX_COMPILER_RESULT_OBJECTS - 15,
        payload_bytes: assignment.work().max_output_bytes(),
        result_grant_pages: MAX_CONTROL_GRANT_PAGES,
    };
    let admitted = admit_compiler_result(&scheduler, assignment, namespace_id, receipt)?;
    assert_receipt_fields_equal(admitted.receipt(), receipt);

    // A single large object may use several signed Bao ranges/pages. Grant pages are bounded
    // by the object's nonempty 1 KiB chunk count, not by the closure's object count.
    let mut one_large_object = receipt;
    one_large_object.object_count = 1;
    one_large_object.payload_bytes = 2 * 1024 * 1024 + 17;
    one_large_object.result_grant_pages = 3;
    let admitted_large_object =
        admit_compiler_result(&scheduler, assignment, namespace_id, one_large_object)?;
    assert_receipt_fields_equal(admitted_large_object.receipt(), one_large_object);

    // Each nonempty grant page needs at least one range; for one object, the extra terminal
    // partial chunk is the tight upper-bound allowance beyond aggregate payload chunks.
    let mut too_many_ranges = one_large_object;
    too_many_ranges.result_grant_pages = 2_050;
    assert!(matches!(
        admit_compiler_result(&scheduler, assignment, namespace_id, too_many_ranges),
        Err(CompilerTransportBridgeError::ResultBudgetExceeded)
    ));

    let mut over_bytes = receipt;
    over_bytes.payload_bytes += 1;
    assert!(matches!(
        admit_compiler_result(&scheduler, assignment, namespace_id, over_bytes),
        Err(CompilerTransportBridgeError::ResultBudgetExceeded)
    ));

    let mut over_objects = receipt;
    over_objects.object_count = MAX_COMPILER_RESULT_OBJECTS + 1;
    assert!(matches!(
        admit_compiler_result(&scheduler, assignment, namespace_id, over_objects),
        Err(CompilerTransportBridgeError::ResultBudgetExceeded)
    ));

    let mut over_pages = receipt;
    over_pages.result_grant_pages = MAX_CONTROL_GRANT_PAGES + 1;
    assert!(matches!(
        admit_compiler_result(&scheduler, assignment, namespace_id, over_pages),
        Err(CompilerTransportBridgeError::ResultBudgetExceeded)
    ));

    let mut under_paged = receipt;
    under_paged.object_count = 65;
    under_paged.result_grant_pages = 4;
    assert!(matches!(
        admit_compiler_result(&scheduler, assignment, namespace_id, under_paged),
        Err(CompilerTransportBridgeError::ResultBudgetExceeded)
    ));

    Ok(())
}

fn assert_receipt_fields_equal(observed: ControlResultReceipt, expected: ControlResultReceipt) {
    assert_eq!(observed.scope, expected.scope);
    assert_eq!(observed.target_root, expected.target_root);
    assert_eq!(observed.pack_id, expected.pack_id);
    assert_eq!(observed.closure_id, expected.closure_id);
    assert_eq!(observed.object_count, expected.object_count);
    assert_eq!(observed.payload_bytes, expected.payload_bytes);
    assert_eq!(observed.result_grant_pages, expected.result_grant_pages);
}

#[tokio::test]
async fn encrypted_control_rolls_over_after_eight_pages_in_both_directions()
-> Result<(), Box<dyn std::error::Error>> {
    let owner_key = SecretKey::from_bytes(&[1; 32]);
    let worker_key = SecretKey::from_bytes(&[2; 32]);
    let owner = bind_direct(owner_key.clone(), "127.0.0.1:0".parse::<SocketAddr>()?).await?;
    let worker = bind_direct(worker_key.clone(), "127.0.0.1:0".parse::<SocketAddr>()?).await?;
    let namespace_id = [0x61; 16];
    let temp = TestDirectory::new()?;
    let (input_store, input_capture, input_verified) =
        capture_test_workspace_v2(&temp.child("input"))?;
    let input_closure = input_capture.closure();
    let input_manifest = input_capture.manifest().clone();
    let input_inventory = probe_inventory_from_store(&input_store, input_closure)?;
    let input_work = work_for_test_capture(&input_capture)?;
    let (scheduler, assignment) = make_assignment_for_test_capture(
        worker.id(),
        namespace_id,
        input_work,
        input_inventory,
        compiler_attempt_token(7, [0x51; 32])?,
    )?;
    let input_objects = materialized_v2_closure_members(&input_store, &input_verified)?;
    let (result_closure, result_objects) = store_test_closure(&temp.child("result"), 101)?;
    let input_manifest_object = input_manifest.typed_object()?;
    let input_manifest_object_id = *input_manifest_object.id().as_bytes();
    let assignment_scope = compiler_assignment_scope(assignment, namespace_id)?;
    let owner_issuer = CapabilityIssuer::new(owner_key);
    let input_catalog = StoreBlobCatalog::new(
        input_store.artifact_sink(ArtifactBudget::new(32, 32, 32 * 1024 * 1024, 1024, 32)),
        temp.child("input-outboards"),
    );
    let mut input_grants = Vec::with_capacity(input_objects.len());
    for (index, object) in input_objects.into_iter().enumerate() {
        let member = input_catalog
            .register_closure_member(
                input_closure,
                UntrustedObjectId::from_bytes(object.object.object_id),
            )
            .await?
            .ok_or_else(|| std::io::Error::other("input object is absent from its closure"))?;
        input_grants.push(issue_compiler_object_range(
            &scheduler,
            &owner_issuer,
            assignment,
            namespace_id,
            owner.id(),
            input_closure,
            member,
            ChunkRange { start: 0, end: 1 },
            MAX_RESPONSE_BYTES,
            now_unix_ms()? + 60_000,
            [0x81_u8.wrapping_add(u8::try_from(index)?); 16],
        )?);
    }
    let input_pages = page_series(
        assignment_scope,
        GrantDirection::InputsToWorker,
        input_grants,
    );
    let input_page_count = u32::try_from(input_pages.len())?;
    assert_eq!(u64::from(input_page_count), input_capture.object_count());
    let worker_result_grants = result_objects
        .into_iter()
        .enumerate()
        .map(|(index, object)| {
            result_claims(
                owner.id(),
                worker.id(),
                assignment_scope,
                result_closure,
                object,
                0x91_u8.wrapping_add(u8::try_from(index).expect("bounded grant index")),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let result_pages = page_series(
        assignment_scope,
        GrantDirection::ResultsToCoordinator,
        worker_result_grants,
    );
    let result_receipt = ControlResultReceipt {
        scope: assignment_scope,
        target_root: [0xa1; 32],
        pack_id: Some([0xa2; 32]),
        closure_id: *result_closure.as_bytes(),
        object_count: 10,
        payload_bytes: 10 * 1024,
        result_grant_pages: 10,
    };

    let owner_id = owner.id();
    let worker_id = worker.id();
    let owner_addr = owner.addr();
    let worker_addr = worker.addr();
    let worker_listener = worker.clone();
    let worker_manifest = input_manifest.clone();
    let worker_manifest_object = input_manifest_object.clone();
    let worker_task = tokio::spawn(async move {
        let allowed_owner =
            CompilerPeerId::new(*owner_id.as_bytes()).expect("valid owner endpoint ID");
        let mut channel = with_stage(
            "worker accept Offer stream",
            accept_compiler_control(&worker_listener, [allowed_owner]).await,
        )?;
        let offer = with_stage(
            "worker receive Offer",
            backend_engine::compiler_cluster_transport::receive_compiler_offer(&mut channel).await,
        )?;
        assert_eq!(offer.scope, assignment_scope);
        assert_eq!(offer.input_grant_pages, input_page_count);
        assert_eq!(offer.input_closure_id, *input_closure.as_bytes());
        assert_eq!(offer.input_manifest_object_id, input_manifest_object_id);
        let admitted_manifest = with_stage(
            "worker admit input manifest",
            admit_compiler_input_manifest_v2(&offer, input_closure, &worker_manifest_object),
        )?;
        assert_eq!(admitted_manifest, worker_manifest);
        with_stage(
            "worker send Accept",
            send_worker_accept(&mut channel, offer.scope, true).await,
        )?;
        with_stage("worker finish Offer stream", channel.finish().await)?;
        let input_pages = with_stage(
            "worker receive captured input grant pages",
            receive_compiler_grant_pages(
                &worker_listener,
                allowed_owner,
                offer.scope,
                offer.input_closure_id,
                input_page_count,
            )
            .await,
        )?;
        assert_eq!(
            input_pages.len(),
            usize::try_from(input_page_count).expect("bounded pages")
        );
        for page in input_pages {
            assert_eq!(
                page.grants[0].claims.scope.closure_id,
                *input_closure.as_bytes()
            );
        }
        let mut result_channel = with_stage(
            "worker connect result receipt stream",
            connect_worker_control(&worker_listener, owner_addr.clone(), owner_id, offer.scope)
                .await,
        )?;
        with_stage(
            "worker send result receipt",
            send_worker_result_receipt(&mut result_channel, result_receipt).await,
        )?;
        with_stage(
            "worker finish result receipt stream",
            result_channel.finish().await,
        )?;
        with_stage(
            "worker send 10 result grant pages",
            send_worker_result_grant_pages(
                &worker_listener,
                owner_addr,
                owner_id,
                offer.scope,
                result_closure,
                &result_pages,
            )
            .await,
        )?;
        let mut ack_channel = with_stage(
            "worker accept result ACK stream",
            accept_compiler_control_for_scope(&worker_listener, [allowed_owner], offer.scope).await,
        )?;
        let ack = with_stage(
            "worker receive result ACK",
            receive_worker_result_ack(&mut ack_channel, owner_id, offer.scope, result_closure)
                .await,
        )?;
        assert_eq!(
            ack,
            backend_cluster_transport::ResultAckDisposition::Rejected(
                backend_cluster_transport::ResultRejectReason::Admission
            )
        );
        with_stage(
            "worker finish result ACK stream",
            ack_channel.finish().await,
        )?;
        let mut cancel_channel = with_stage(
            "worker accept Cancel stream",
            accept_compiler_control_for_scope(&worker_listener, [allowed_owner], offer.scope).await,
        )?;
        let reason = with_stage(
            "worker receive Cancel",
            receive_compiler_cancel(&mut cancel_channel, offer.scope).await,
        )?;
        assert_eq!(reason, backend_cluster_transport::CancelReason::Requested);
        with_stage("worker finish Cancel stream", cancel_channel.finish().await)?;
        Ok::<(), std::io::Error>(())
    });

    let mut owner_channel = with_stage(
        "owner connect Offer stream",
        connect_compiler_control(
            &scheduler,
            &owner,
            worker_addr.clone(),
            assignment,
            namespace_id,
        )
        .await,
    )?;
    with_stage(
        "owner send Offer",
        send_compiler_offer_v2(
            &mut owner_channel,
            &scheduler,
            assignment,
            namespace_id,
            input_closure,
            &input_manifest,
            input_page_count,
            now_unix_ms()? + 60_000,
        )
        .await,
    )?;
    assert!(with_stage(
        "owner receive Accept",
        receive_compiler_accept(&mut owner_channel, &scheduler, assignment, namespace_id,).await,
    )?);
    with_stage("owner finish Offer stream", owner_channel.finish().await)?;
    with_stage(
        "owner send captured input grant pages",
        send_compiler_grant_pages(
            &scheduler,
            &owner,
            worker_addr.clone(),
            assignment,
            namespace_id,
            input_closure,
            &input_pages,
        )
        .await,
    )?;
    let mut result_channel = with_stage(
        "owner accept result receipt stream",
        accept_coordinator_control(&scheduler, &owner, assignment, namespace_id).await,
    )?;
    let fenced = with_stage(
        "owner receive result receipt",
        receive_compiler_result(&mut result_channel, &scheduler, assignment, namespace_id).await,
    )?;
    assert_eq!(fenced.receipt().closure_id, *result_closure.as_bytes());
    let admitted = admit_compiler_result(&scheduler, assignment, namespace_id, fenced.receipt())?;
    assert_eq!(admitted.receipt().pack_id, Some([0xa2; 32]));
    with_stage(
        "owner finish result receipt stream",
        result_channel.finish().await,
    )?;
    let mut page_receiver =
        CompilerResultGrantPageReceiver::new(&scheduler, assignment, namespace_id, admitted)?;
    while !page_receiver.is_complete() {
        let mut page_channel = with_stage(
            "owner accept result grant page stream",
            accept_coordinator_control(&scheduler, &owner, assignment, namespace_id).await,
        )?;
        with_stage(
            "owner consume result grant page stream",
            page_receiver
                .receive_channel(&scheduler, page_channel)
                .await,
        )?;
    }
    let result_pages = page_receiver.into_pages()?;
    assert_eq!(result_pages.len(), 10);
    for page in result_pages {
        assert_eq!(page.page_count, 10);
        assert_eq!(page.grants[0].claims.server, worker_id);
        assert_eq!(page.grants[0].claims.client, owner_id);
    }
    let mut ack_channel = with_stage(
        "owner connect result ACK stream",
        connect_compiler_control(
            &scheduler,
            &owner,
            worker_addr.clone(),
            assignment,
            namespace_id,
        )
        .await,
    )?;
    with_stage(
        "owner send rejected result ACK",
        send_rejected_compiler_result_ack(
            &mut ack_channel,
            &scheduler,
            assignment,
            namespace_id,
            *result_closure.as_bytes(),
            backend_cluster_transport::ResultRejectReason::Admission,
        )
        .await,
    )?;
    with_stage("owner finish result ACK stream", ack_channel.finish().await)?;
    let mut cancel_channel = with_stage(
        "owner connect Cancel stream",
        connect_compiler_control(&scheduler, &owner, worker_addr, assignment, namespace_id).await,
    )?;
    with_stage(
        "owner send Cancel",
        send_compiler_cancel(
            &mut cancel_channel,
            assignment,
            namespace_id,
            backend_cluster_transport::CancelReason::Requested,
        )
        .await,
    )?;
    scheduler.cancel(assignment)?;
    with_stage("owner finish Cancel stream", cancel_channel.finish().await)?;
    with_stage("join worker control journey", worker_task.await?)?;
    Ok(())
}

#[tokio::test]
async fn worker_input_rejection_is_closure_fenced_and_requires_a_new_local_attempt()
-> Result<(), Box<dyn std::error::Error>> {
    let owner_key = SecretKey::from_bytes(&[11; 32]);
    let worker_key = SecretKey::from_bytes(&[12; 32]);
    let owner = bind_direct(owner_key, "127.0.0.1:0".parse::<SocketAddr>()?).await?;
    let worker = bind_direct(worker_key, "127.0.0.1:0".parse::<SocketAddr>()?).await?;
    let namespace_id = [0x63; 16];
    let temp = TestDirectory::new()?;
    let (input_store, input_capture, _input_verified) =
        capture_test_workspace_v2(&temp.child("rejected-input"))?;
    let input_closure = input_capture.closure();
    let input_manifest = input_capture.manifest().clone();
    let inventory = probe_inventory_from_store(&input_store, input_closure)?;
    let input_work = work_for_test_capture(&input_capture)?;
    let (scheduler, assignment) = make_assignment_for_test_capture(
        worker.id(),
        namespace_id,
        input_work,
        inventory,
        compiler_attempt_token(7, [0x51; 32])?,
    )?;
    let owner_id = owner.id();
    let owner_addr = owner.addr();
    let worker_addr = worker.addr();
    let worker_listener = worker.clone();
    let worker_task = tokio::spawn(async move {
        let allowed_owner = CompilerPeerId::new(*owner_id.as_bytes()).expect("owner ID");
        let mut offer_channel = with_stage(
            "reject test worker accept Offer stream",
            accept_compiler_control(&worker_listener, [allowed_owner]).await,
        )?;
        let offer = with_stage(
            "reject test worker receive Offer",
            backend_engine::compiler_cluster_transport::receive_compiler_offer(&mut offer_channel)
                .await,
        )?;
        with_stage(
            "reject test worker send Accept",
            send_worker_accept(&mut offer_channel, offer.scope, true).await,
        )?;
        with_stage(
            "reject test worker finish Offer stream",
            offer_channel.finish().await,
        )?;

        let mut mismatched_channel = with_stage(
            "reject test worker connect mismatched reject stream",
            connect_worker_control(&worker_listener, owner_addr.clone(), owner_id, offer.scope)
                .await,
        )?;
        with_stage(
            "reject test worker send mismatched closure reject",
            send_worker_input_reject(
                &mut mismatched_channel,
                offer.scope,
                [0xff; 32],
                backend_cluster_transport::WorkerRejectReason::ManifestMismatch,
            )
            .await,
        )?;
        with_stage(
            "reject test worker finish mismatched reject stream",
            mismatched_channel.finish().await,
        )?;

        let mut rejected_channel = with_stage(
            "reject test worker connect exact reject stream",
            connect_worker_control(&worker_listener, owner_addr, owner_id, offer.scope).await,
        )?;
        with_stage(
            "reject test worker send exact input reject",
            send_worker_input_reject(
                &mut rejected_channel,
                offer.scope,
                offer.input_closure_id,
                backend_cluster_transport::WorkerRejectReason::UnsupportedToolchain,
            )
            .await,
        )?;
        with_stage(
            "reject test worker finish exact reject stream",
            rejected_channel.finish().await,
        )?;
        Ok::<(), std::io::Error>(())
    });

    let mut offer_channel = with_stage(
        "reject test owner connect Offer stream",
        connect_compiler_control(&scheduler, &owner, worker_addr, assignment, namespace_id).await,
    )?;
    with_stage(
        "reject test owner send Offer",
        send_compiler_offer_v2(
            &mut offer_channel,
            &scheduler,
            assignment,
            namespace_id,
            input_closure,
            &input_manifest,
            u32::try_from(input_capture.object_count())?,
            now_unix_ms()? + 60_000,
        )
        .await,
    )?;
    assert!(with_stage(
        "reject test owner receive Accept",
        receive_compiler_accept(&mut offer_channel, &scheduler, assignment, namespace_id,).await,
    )?);
    with_stage(
        "reject test owner finish Offer stream",
        offer_channel.finish().await,
    )?;

    let mut mismatched = with_stage(
        "reject test owner accept mismatched stream",
        accept_coordinator_control(&scheduler, &owner, assignment, namespace_id).await,
    )?;
    match receive_compiler_input_reject(
        &mut mismatched,
        &scheduler,
        assignment,
        namespace_id,
        *input_closure.as_bytes(),
    )
    .await
    {
        Err(CompilerTransportBridgeError::ScopeMismatch) => {}
        Err(error) => {
            return Err(std::io::Error::other(format!(
                "reject test owner receive forged-closure rejection: {error}"
            ))
            .into());
        }
        Ok(_) => {
            return Err(std::io::Error::other(
                "reject test accepted rejection for a different input closure",
            )
            .into());
        }
    }
    with_stage(
        "reject test owner finish mismatched stream",
        mismatched.finish().await,
    )?;

    let mut rejected = with_stage(
        "reject test owner accept exact rejection stream",
        accept_coordinator_control(&scheduler, &owner, assignment, namespace_id).await,
    )?;
    let rejection = with_stage(
        "reject test owner receive exact input rejection",
        receive_compiler_input_reject(
            &mut rejected,
            &scheduler,
            assignment,
            namespace_id,
            *input_closure.as_bytes(),
        )
        .await,
    )?;
    assert_eq!(
        rejection.reason(),
        backend_cluster_transport::WorkerRejectReason::UnsupportedToolchain
    );
    with_stage(
        "reject test owner finish rejection stream",
        rejected.finish().await,
    )?;
    let local = with_stage(
        "reject test local fallback after fresh attempt",
        fallback_after_worker_input_reject(
            &scheduler,
            rejection,
            compiler_attempt_token(8, [0x53; 32])?,
        ),
    )?;
    assert!(matches!(
        local.route(),
        backend_execution::CompilerAssignmentRoute::Local
    ));
    assert!(scheduler.validate_assignment(assignment).is_err());
    scheduler.validate_assignment(local)?;
    with_stage("join worker input rejection journey", worker_task.await?)?;
    Ok(())
}

#[tokio::test]
async fn worker_execution_failure_is_closure_fenced_and_requires_a_new_local_attempt()
-> Result<(), Box<dyn std::error::Error>> {
    let owner = bind_direct(
        SecretKey::from_bytes(&[21; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let worker = bind_direct(
        SecretKey::from_bytes(&[22; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let namespace_id = [0x64; 16];
    let temp = TestDirectory::new()?;
    let (input_store, input_capture, _input_verified) =
        capture_test_workspace_v2(&temp.child("failed-input"))?;
    let input_closure = input_capture.closure();
    let input_manifest = input_capture.manifest().clone();
    let inventory = probe_inventory_from_store(&input_store, input_closure)?;
    let input_work = work_for_test_capture(&input_capture)?;
    let (scheduler, assignment) = make_assignment_for_test_capture(
        worker.id(),
        namespace_id,
        input_work,
        inventory,
        compiler_attempt_token(7, [0x51; 32])?,
    )?;
    let owner_id = owner.id();
    let owner_addr = owner.addr();
    let worker_addr = worker.addr();
    let worker_listener = worker.clone();
    let worker_task = tokio::spawn(async move {
        let allowed_owner = CompilerPeerId::new(*owner_id.as_bytes()).expect("owner ID");
        let mut offer_channel = with_stage(
            "execution-failure worker accept Offer stream",
            accept_compiler_control(&worker_listener, [allowed_owner]).await,
        )?;
        let offer = with_stage(
            "execution-failure worker receive Offer",
            backend_engine::compiler_cluster_transport::receive_compiler_offer(&mut offer_channel)
                .await,
        )?;
        with_stage(
            "execution-failure worker send Accept",
            send_worker_accept(&mut offer_channel, offer.scope, true).await,
        )?;
        with_stage(
            "execution-failure worker finish Offer stream",
            offer_channel.finish().await,
        )?;

        with_stage(
            "worker send wrong-input execution failure",
            send_worker_execution_failure(
                &worker_listener,
                owner_addr.clone(),
                owner_id,
                offer.scope,
                [0xfe; 32],
                ExecutionFailureReason::CompilerFailed,
            )
            .await,
        )?;
        with_stage(
            "worker send exact execution failure",
            send_worker_execution_failure(
                &worker_listener,
                owner_addr,
                owner_id,
                offer.scope,
                offer.input_closure_id,
                ExecutionFailureReason::OutputBudget,
            )
            .await,
        )?;
        Ok::<(), std::io::Error>(())
    });

    let mut offer_channel = with_stage(
        "execution-failure owner connect Offer stream",
        connect_compiler_control(&scheduler, &owner, worker_addr, assignment, namespace_id).await,
    )?;
    with_stage(
        "execution-failure owner send Offer",
        send_compiler_offer_v2(
            &mut offer_channel,
            &scheduler,
            assignment,
            namespace_id,
            input_closure,
            &input_manifest,
            u32::try_from(input_capture.object_count())?,
            now_unix_ms()? + 60_000,
        )
        .await,
    )?;
    assert!(with_stage(
        "execution-failure owner receive Accept",
        receive_compiler_accept(&mut offer_channel, &scheduler, assignment, namespace_id,).await,
    )?);
    with_stage(
        "execution-failure owner finish Offer stream",
        offer_channel.finish().await,
    )?;

    let mismatch = receive_compiler_execution_failure(
        &scheduler,
        &owner,
        assignment,
        namespace_id,
        *input_closure.as_bytes(),
    )
    .await;
    if !matches!(mismatch, Err(CompilerTransportBridgeError::ScopeMismatch)) {
        return Err(std::io::Error::other(format!(
            "execution-failure closure mismatch was not rejected: {mismatch:?}"
        ))
        .into());
    }
    let failure = with_stage(
        "owner receive exact execution failure on fresh stream",
        receive_compiler_execution_failure(
            &scheduler,
            &owner,
            assignment,
            namespace_id,
            *input_closure.as_bytes(),
        )
        .await,
    )?;
    assert_eq!(failure.reason(), ExecutionFailureReason::OutputBudget);
    assert_eq!(failure.assignment(), assignment);
    let local = with_stage(
        "fallback after worker execution failure",
        fallback_after_worker_execution_failure(
            &scheduler,
            failure,
            compiler_attempt_token(8, [0x54; 32])?,
        ),
    )?;
    assert!(matches!(
        local.route(),
        backend_execution::CompilerAssignmentRoute::Local
    ));
    assert!(scheduler.validate_assignment(assignment).is_err());
    scheduler.validate_assignment(local)?;
    with_stage("join execution-failure worker", worker_task.await?)?;
    Ok(())
}

#[tokio::test]
async fn stored_result_ack_can_be_retried_after_scheduler_completion()
-> Result<(), Box<dyn std::error::Error>> {
    let owner = bind_direct(
        SecretKey::from_bytes(&[31; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let worker = bind_direct(
        SecretKey::from_bytes(&[32; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let namespace_id = [0x65; 16];
    let (scheduler, assignment) = make_assignment(worker.id(), namespace_id)?;
    let temp = TestDirectory::new()?;
    let (stored, _objects) = store_test_closure_with_receipt(&temp.child("stored-result"), 151)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    let receipt = ControlResultReceipt {
        scope,
        target_root: [0xb1; 32],
        pack_id: Some([0xb2; 32]),
        closure_id: *stored.closure().as_bytes(),
        object_count: u32::try_from(stored.object_count())?,
        payload_bytes: stored.payload_bytes(),
        result_grant_pages: 1,
    };
    let fenced = admit_compiler_result(&scheduler, assignment, namespace_id, receipt)?;
    let stored_result = store_compiler_result(&scheduler, fenced, stored)?;
    assert!(scheduler.validate_assignment(assignment).is_err());

    let owner_id = owner.id();
    let worker_addr = worker.addr();
    let worker_id = worker.id();
    let worker_listener = worker.clone();
    let result_closure = stored.closure();
    let worker_task = tokio::spawn(async move {
        let allowed_owner = CompilerPeerId::new(*owner_id.as_bytes()).expect("owner ID");
        // The worker may retain its durable output when delivery is uncertain across a crash.
        // Accept the exact immutable ACK twice to model a retry after an ambiguous first delivery.
        for _ in 0..2 {
            let mut channel = with_stage(
                "worker accept stored-result ACK retry stream",
                accept_compiler_control_for_scope(&worker_listener, [allowed_owner], scope).await,
            )?;
            let ack = with_stage(
                "worker receive stored-result ACK retry",
                receive_worker_result_ack(&mut channel, owner_id, scope, result_closure).await,
            )?;
            assert_eq!(ack, backend_cluster_transport::ResultAckDisposition::Stored);
            with_stage(
                "worker finish stored-result ACK retry stream",
                channel.finish().await,
            )?;
        }
        Ok::<(), std::io::Error>(())
    });

    for _ in 0..2 {
        let mut channel = with_stage(
            "owner connect stored-result ACK retry stream",
            connect_stored_compiler_result_ack(
                &owner,
                worker_addr.clone(),
                assignment,
                namespace_id,
                stored_result,
            )
            .await,
        )?;
        with_stage(
            "owner send stored-result ACK retry",
            send_stored_compiler_result_ack(&mut channel, assignment, namespace_id, stored_result)
                .await,
        )?;
        with_stage(
            "owner finish stored-result ACK retry stream",
            channel.finish().await,
        )?;
    }
    assert_eq!(
        stored_result.candidate().peer(),
        CompilerPeerId::new(*worker_id.as_bytes())?
    );
    with_stage("join stored-result ACK worker", worker_task.await?)?;
    Ok(())
}

#[tokio::test]
async fn superseded_result_ack_is_scoped_after_scheduler_retirement()
-> Result<(), Box<dyn std::error::Error>> {
    let owner = bind_direct(
        SecretKey::from_bytes(&[41; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let worker = bind_direct(
        SecretKey::from_bytes(&[42; 32]),
        "127.0.0.1:0".parse::<SocketAddr>()?,
    )
    .await?;
    let namespace_id = [0x75; 16];
    let (scheduler, assignment) = make_assignment(worker.id(), namespace_id)?;
    let temp = TestDirectory::new()?;
    let (stored, _objects) = store_test_closure_with_receipt(&temp.child("stale-result"), 173)?;
    let closure = stored.closure();
    let scope = compiler_assignment_scope(assignment, namespace_id)?;

    // The durable authority adapter proves supersession before invoking the bridge primitive.
    // This transport-level test only verifies that an already-retired scheduler slot does not
    // prevent an exact authenticated terminal ACK from reaching the worker's retained result.
    scheduler.cancel(assignment)?;

    let owner_id = owner.id();
    let worker_listener = worker.clone();
    let worker_task = tokio::spawn(async move {
        let allowed_owner = CompilerPeerId::new(*owner_id.as_bytes()).expect("owner ID");
        let mut channel = with_stage(
            "worker accept superseded-result ACK",
            accept_compiler_control_for_scope(&worker_listener, [allowed_owner], scope).await,
        )?;
        let ack = with_stage(
            "worker receive superseded-result ACK",
            receive_worker_result_ack(&mut channel, owner_id, scope, closure).await,
        )?;
        assert_eq!(
            ack,
            backend_cluster_transport::ResultAckDisposition::Rejected(
                backend_cluster_transport::ResultRejectReason::Scope
            )
        );
        with_stage(
            "worker finish superseded-result ACK",
            channel.finish().await,
        )?;
        Ok::<(), std::io::Error>(())
    });

    let mut channel = with_stage(
        "owner connect superseded-result ACK",
        connect_control(
            &owner,
            worker.addr(),
            worker.id(),
            scope,
            ControlRole::Coordinator,
        )
        .await,
    )?;
    with_stage(
        "owner send superseded-result ACK",
        send_superseded_compiler_result_ack(
            &mut channel,
            assignment,
            namespace_id,
            *closure.as_bytes(),
        )
        .await,
    )?;
    with_stage("owner finish superseded-result ACK", channel.finish().await)?;
    with_stage("join superseded-result worker", worker_task.await?)?;
    Ok(())
}
