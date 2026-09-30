use std::{
    io::{Seek, SeekFrom, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_cluster_transport::{
    AcceptedClusterConnection, AdmissionPolicy, AssignmentScope, BlobHash, Capability,
    CapabilityClaims, CapabilityIssuer, ChunkRange, ClusterListener, ControlAdmissionPolicy,
    ControlChannel, ControlMessage, ControlResultReceipt, ControlRole, MAX_RANGE_CHUNKS,
    MAX_RESPONSE_BYTES, RemoteIndexCapabilityClaims, RemoteIndexCapabilityIssuer,
    RemoteIndexChannel, RemoteIndexOutcome, RemoteIndexPermission, RemoteIndexProductScope,
    RemoteIndexQueryOperation, RemoteIndexResponse, ResumeState, ServerState, StoreBlobCatalog,
    StoreObjectMapping, TransferScope, TransportError, bind_direct, connect_control, fetch_range,
    now_unix_ms, remote_index_now, serve_one, serve_one_measured, verify_admission,
};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactPlan, ClosureManifest, FileStore, TypedObject,
    UntrustedObjectId,
};
use backend_version::{ObjectKey, Schema};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

struct PayloadSchema;

impl Schema for PayloadSchema {
    const DOMAIN: u8 = 201;
    const TYPE: u16 = 1;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

fn secret(seed: u8) -> SecretKey {
    SecretKey::from_bytes(&[seed; 32])
}

fn artifact_budget() -> ArtifactBudget {
    ArtifactBudget::new(8, 8, 2 * 1024 * 1024, 64 * 1024, 128)
}

fn large_artifact_budget() -> ArtifactBudget {
    ArtifactBudget::new(8, 8, 32 * 1024 * 1024, 64 * 1024, 128)
}

fn temp_dir(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "backend-iroh-bao-{label}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("temporary directory");
    path
}

#[tokio::test]
async fn remote_index_capability_echo_works_from_an_independent_client_process() {
    let owner_secret = secret(133);
    let client_secret = secret(134);
    let endpoint = bind_direct(
        owner_secret.clone(),
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("loopback address"),
    )
    .await
    .expect("bind remote-index test owner");
    let directory = temp_dir("remote-index-peer");
    let (artifact_store, _, artifact_closure) =
        prepare_store(&directory.join("artifact-store"), b"fixture");
    let artifact_scope = TransferScope::from_store_closure(
        AssignmentScope::new([137; 16], [138; 16], 1, [139; 32]).expect("artifact assignment"),
        artifact_closure,
    )
    .expect("artifact transfer scope");
    let artifact_issuer = CapabilityIssuer::new(secret(140));
    let artifact_state = ServerState::new(
        AdmissionPolicy::new(
            endpoint.id(),
            artifact_issuer.public_key(),
            [client_secret.public()],
            artifact_scope,
        ),
        std::sync::Arc::new(StoreBlobCatalog::new(
            artifact_store.artifact_sink(artifact_budget()),
            directory.join("outboard"),
        )),
    );
    let control_policy =
        ControlAdmissionPolicy::coordinator_ingress(endpoint.id(), [client_secret.public()]);
    let mut listener = ClusterListener::spawn(endpoint.clone(), control_policy, artifact_state, 2)
        .expect("spawn shared authenticated ALPN listener");
    let address = endpoint
        .bound_sockets()
        .into_iter()
        .next()
        .expect("owner direct socket address");
    let now = remote_index_now().expect("current remote-index time");
    let issuer = RemoteIndexCapabilityIssuer::new(owner_secret);
    let capability = issuer
        .issue(
            RemoteIndexCapabilityClaims {
                version: 1,
                server: endpoint.id(),
                client: client_secret.public(),
                grant_id: [135; 16],
                issued_at_unix_ms: now,
                expires_at_unix_ms: now + 60_000,
                request_budget: 4,
                byte_budget: 128 * 1024,
                permissions: vec![RemoteIndexPermission::ProductRead],
                product: Some(RemoteIndexProductScope {
                    view_root: [136; 32],
                    operations: vec![RemoteIndexQueryOperation::Search],
                }),
                semantic: None,
            },
            now,
        )
        .expect("owner-signed read capability");
    let body = (0..64 * 1024)
        .map(|index| (index.wrapping_mul(31) % 251) as u8)
        .collect::<Vec<_>>();
    let serving = tokio::spawn(async move {
        let event = timeout(Duration::from_secs(20), listener.recv())
            .await
            .expect("owner accept timeout")
            .expect("owner listener still open")
            .expect("owner listener event");
        let connection = match event {
            AcceptedClusterConnection::RemoteIndex(connection) => connection,
            _ => panic!("shared listener routed the wrong ALPN"),
        };
        let mut session = connection
            .accept()
            .await
            .expect("admit signed remote-index session");
        assert_eq!(session.channel(), RemoteIndexChannel::ProductQuery);
        let request = session
            .receive_request()
            .await
            .expect("receive query frame");
        session
            .send_response(&RemoteIndexResponse {
                request_id: request.request_id,
                outcome: RemoteIndexOutcome::Payload(request.body),
            })
            .await
            .expect("send bounded echo response");
        listener
            .shutdown()
            .await
            .expect("close owner ALPN listener");
    });
    let input = serde_json::json!({
        "owner": endpoint.id(),
        "address": address,
        "bind": "127.0.0.1:0",
        "secret": client_secret.to_bytes(),
        "capability": capability,
        "body": body,
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_remote-index-peer"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn independent remote-index client process");
    child
        .stdin
        .take()
        .expect("client stdin")
        .write_all(&serde_json::to_vec(&input).expect("encode child input"))
        .await
        .expect("write child input");
    let output = timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("remote-index client process timeout")
        .expect("wait for remote-index client process");
    assert!(
        output.status.success(),
        "remote-index client failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("remote-index-echo-bytes=65536"));
    serving.await.expect("remote-index owner task");
    endpoint.close().await;
    std::fs::remove_dir_all(directory).expect("remove remote-index test state");
}

async fn run_child(
    server_addr: EndpointAddr,
    issuer: &CapabilityIssuer,
    client_key: &SecretKey,
    capability: Capability,
    scope: TransferScope,
    checkpoint: &Path,
) -> std::process::Output {
    run_child_batch(
        server_addr,
        issuer,
        client_key,
        vec![capability],
        scope,
        checkpoint,
    )
    .await
}

async fn run_child_batch(
    server_addr: EndpointAddr,
    issuer: &CapabilityIssuer,
    client_key: &SecretKey,
    capabilities: Vec<Capability>,
    scope: TransferScope,
    checkpoint: &Path,
) -> std::process::Output {
    run_child_batch_mode(
        server_addr,
        issuer,
        client_key,
        capabilities,
        scope,
        checkpoint,
        false,
        None,
    )
    .await
}

async fn run_child_batch_mode(
    server_addr: EndpointAddr,
    issuer: &CapabilityIssuer,
    client_key: &SecretKey,
    capabilities: Vec<Capability>,
    scope: TransferScope,
    checkpoint: &Path,
    stateless: bool,
    fail_after_ranges: Option<usize>,
) -> std::process::Output {
    let input = serde_json::json!({
        "server_addr": server_addr,
        "bind_addr": "127.0.0.1:0",
        "client_secret": client_key.to_bytes(),
        "trusted_issuer": issuer.public_key(),
        "capabilities": capabilities,
        "expected_scope": scope,
        "checkpoint": checkpoint,
        "stateless": stateless,
        "fail_after_ranges": fail_after_ranges,
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_cluster-peer"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("BACKEND_CLUSTER_MEASURE_TIMINGS", "1")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn independent client process");
    let bytes = serde_json::to_vec(&input).expect("serialize client input");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(&bytes)
        .await
        .expect("write child input");
    timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("client process timeout")
        .expect("wait for client process")
}

fn child_range_samples(output: &std::process::Output) -> Vec<u128> {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("cluster-peer-range "))
        .filter_map(|fields| fields.split_whitespace().last())
        .filter_map(|field| field.strip_prefix("elapsed_us="))
        .filter_map(|value| value.parse().ok())
        .collect()
}

fn child_checkpoint_open_sample(
    output: &std::process::Output,
    expected_existing: bool,
) -> Option<u128> {
    let expected = format!("existing_checkpoint={expected_existing}");
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("cluster-peer-open "))
        .find(|fields| fields.split_whitespace().any(|field| field == expected))?
        .split_whitespace()
        .last()?
        .strip_prefix("elapsed_us=")?
        .parse()
        .ok()
}

fn child_validation_sample(output: &std::process::Output) -> Option<[u64; 6]> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let fields = stderr
        .lines()
        .find_map(|line| line.strip_prefix("cluster-peer-validation "))?;
    let parse = |name: &str| {
        fields
            .split_whitespace()
            .find_map(|field| field.strip_prefix(&format!("{name}=")))?
            .parse()
            .ok()
    };
    Some([
        parse("cold_opens")?,
        parse("ranges")?,
        parse("chunks")?,
        parse("payload_bytes")?,
        parse("bao_bytes")?,
        parse("whole_hash_bytes")?,
    ])
}

fn report_latency_distribution(size_mib: usize, mode: &str, samples: &[u128]) {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        eprintln!("cluster-transport range latency: size_mib={size_mib} mode={mode} n=0");
        return;
    }
    let median = if sorted.len() % 2 == 0 {
        let upper = sorted.len() / 2;
        sorted[upper - 1] / 2 + sorted[upper] / 2 + (sorted[upper - 1] % 2 + sorted[upper] % 2) / 2
    } else {
        sorted[sorted.len() / 2]
    };
    let p95_index = sorted
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    eprintln!(
        "cluster-transport range latency: size_mib={size_mib} mode={mode} samples={} median_us={median} p95_nearest_rank_us={} min_us={} max_us={} note=per-range_elapsed_includes_direct_QUIC_connection_and_checkpoint_commit",
        sorted.len(),
        sorted[p95_index],
        sorted[0],
        sorted[sorted.len() - 1],
    );
}

fn claims(
    client: EndpointId,
    server: EndpointId,
    scope: TransferScope,
    object: StoreObjectMapping,
    blob_hash: BlobHash,
    range: ChunkRange,
    nonce: u8,
) -> CapabilityClaims {
    CapabilityClaims {
        client,
        server,
        scope,
        blob_hash,
        object,
        range,
        byte_budget: MAX_RESPONSE_BYTES,
        expires_at_unix_ms: now_unix_ms().expect("clock") + 600_000,
        nonce: [nonce; 16],
    }
}

fn prepare_store(root: &Path, bytes: &[u8]) -> (FileStore, TypedObject, backend_store::ClosureId) {
    prepare_store_with_capacity(root, bytes, 1024 * 1024)
}

fn prepare_store_with_capacity(
    root: &Path,
    bytes: &[u8],
    capacity: usize,
) -> (FileStore, TypedObject, backend_store::ClosureId) {
    let store = FileStore::open(root, capacity).expect("open CAS");
    let key = ObjectKey::<PayloadSchema>::from_value(bytes);
    let object = TypedObject::from_value(&key, bytes);
    let closure = ClosureManifest::new(vec![object.clone()]).expect("canonical closure");
    let closure_id = store
        .write_closure(&closure)
        .expect("durable source closure");
    (store, object, closure_id)
}

async fn start_measured_server(
    server_key: SecretKey,
    issuer: EndpointId,
    client: EndpointId,
    scope: TransferScope,
    sink: backend_store::ArtifactSink,
    outboard_root: PathBuf,
    request_count: usize,
) -> (iroh::Endpoint, EndpointAddr, tokio::task::JoinHandle<u64>) {
    let endpoint = bind_direct(
        server_key,
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("loopback address"),
    )
    .await
    .expect("bind measured direct server");
    let address = endpoint.addr();
    assert!(address.addrs.iter().all(|address| !address.is_relay()));
    let state = ServerState::new(
        AdmissionPolicy::new(endpoint.id(), issuer, [client], scope),
        std::sync::Arc::new(StoreBlobCatalog::new(sink, outboard_root)),
    );
    let listener = endpoint.clone();
    let serving = tokio::spawn(async move {
        let mut bao_stream_bytes = 0_u64;
        for _ in 0..request_count {
            let metrics = timeout(
                Duration::from_secs(30),
                serve_one_measured(&listener, &state),
            )
            .await
            .expect("measured range server timeout")
            .expect("serve measured range");
            bao_stream_bytes = bao_stream_bytes.saturating_add(metrics.bao_stream_bytes);
        }
        bao_stream_bytes
    });
    (endpoint, address, serving)
}

async fn send_control_result_receipt(
    endpoint: iroh::Endpoint,
    peer_addr: EndpointAddr,
    peer: EndpointId,
    scope: AssignmentScope,
    receipt: ControlResultReceipt,
) -> Result<(), TransportError> {
    let mut channel =
        connect_control(&endpoint, peer_addr, peer, scope, ControlRole::Worker).await?;
    channel
        .send(&ControlMessage::ResultReceipt(receipt))
        .await?;
    channel.finish().await
}

async fn receive_control_result_receipt(
    mut channel: ControlChannel,
    expected: [(AssignmentScope, [u8; 32]); 2],
) -> [u8; 32] {
    assert_eq!(channel.scope(), None, "owner ingress begins multi-scope");
    let receipt = channel
        .receive()
        .await
        .expect("receive demuxed result receipt");
    let receipt = match receipt {
        ControlMessage::ResultReceipt(receipt) => receipt,
        _ => panic!("dispatcher delivered non-result control message"),
    };
    assert_eq!(receipt.pack_id, None);
    let (_, target) = expected
        .iter()
        .find(|(scope, target)| *scope == receipt.scope && *target == receipt.target_root)
        .copied()
        .expect("receipt must bind one of the two active assignments");
    assert_eq!(channel.scope(), Some(receipt.scope));
    channel
        .finish()
        .await
        .expect("finish demuxed control stream");
    target
}

fn directory_bytes(root: &Path) -> u64 {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(root).expect("list measured directory") {
        let path = entry.expect("directory entry").path();
        let metadata = std::fs::symlink_metadata(&path).expect("measure retained file");
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            total += directory_bytes(&path);
        } else if metadata.is_file() {
            total += metadata.len();
        }
    }
    total
}

fn extension_bytes(root: &Path, extension: &str) -> u64 {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(root).expect("list catalog directory") {
        let path = entry.expect("catalog entry").path();
        let metadata = std::fs::symlink_metadata(&path).expect("measure catalog entry");
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            total += extension_bytes(&path, extension);
        } else if metadata.is_file()
            && path
                .extension()
                .is_some_and(|candidate| candidate == std::ffi::OsStr::new(extension))
        {
            total += metadata.len();
        }
    }
    total
}

fn checkpoint_logical_bytes(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        ResumeState::data_path(path),
        ResumeState::outboard_path(path),
    ]
    .iter()
    .map(|path| std::fs::metadata(path).expect("checkpoint sidecar").len())
    .sum()
}

fn segment_ranges(start: u64, end: u64) -> Vec<ChunkRange> {
    let mut ranges = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let next = cursor.saturating_add(MAX_RANGE_CHUNKS).min(end);
        ranges.push(ChunkRange {
            start: cursor,
            end: next,
        });
        cursor = next;
    }
    ranges
}

fn measured_capabilities(
    issuer: &CapabilityIssuer,
    client: EndpointId,
    server: EndpointId,
    scope: TransferScope,
    mapping: StoreObjectMapping,
    blob_hash: BlobHash,
    ranges: &[ChunkRange],
    nonce_seed: u8,
) -> Vec<Capability> {
    ranges
        .iter()
        .enumerate()
        .map(|(index, range)| {
            let nonce = nonce_seed.wrapping_add(index as u8);
            issuer
                .issue(claims(
                    client, server, scope, mapping, blob_hash, *range, nonce,
                ))
                .expect("sign bounded measurement range")
        })
        .collect()
}

#[tokio::test]
async fn two_process_bao_transfer_reopens_after_mid_batch_failure_and_enters_artifact_sink() {
    let directory = temp_dir("two-process");
    let payload = (0..40_000_usize)
        .map(|index| (index.wrapping_mul(17) % 251) as u8)
        .collect::<Vec<_>>();
    let source_root = directory.join("source-store");
    let outboard_root = directory.join("bao-catalog");
    let checkpoint = directory.join("sparse-transfer.chk");
    let (source_store, object, closure_id) = prepare_store(&source_root, &payload);
    let source_sink = source_store.artifact_sink(artifact_budget());
    let object_claim = UntrustedObjectId::from_bytes(*object.id().as_bytes());
    let source_catalog = StoreBlobCatalog::new(source_sink.clone(), &outboard_root);
    let materialized = source_catalog
        .register_store_object(object_claim)
        .await
        .expect("build durable Bao source")
        .expect("object in source CAS");
    #[cfg(unix)]
    let outboard_inode = {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(source_catalog.outboard_path(materialized.blob_hash))
            .expect("first Bao outboard")
            .ino()
    };
    let reused = source_catalog
        .register_store_object(object_claim)
        .await
        .expect("reuse durable Bao source")
        .expect("object remains in source CAS");
    assert_eq!(reused, materialized);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(
            std::fs::metadata(source_catalog.outboard_path(materialized.blob_hash))
                .expect("reused Bao outboard")
                .ino(),
            outboard_inode,
            "a warm registration must not rewrite the immutable Bao layout"
        );
    }
    let outboard_path = source_catalog.outboard_path(materialized.blob_hash);
    let pristine_outboard = std::fs::read(&outboard_path).expect("read Bao layout");
    assert!(
        !pristine_outboard.is_empty(),
        "fixture must cover Bao metadata"
    );
    let mut corrupt_outboard = pristine_outboard.clone();
    corrupt_outboard[0] ^= 0x80;
    std::fs::write(&outboard_path, &corrupt_outboard).expect("damage cached Bao layout");
    assert_eq!(
        source_catalog
            .register_store_object(object_claim)
            .await
            .expect("repair corrupt Bao layout"),
        Some(materialized)
    );
    assert_eq!(
        std::fs::read(&outboard_path).expect("reopen repaired Bao layout"),
        pristine_outboard,
        "a same-length corrupt cache entry must be rebuilt from authenticated CAS bytes"
    );
    let catalog_files = std::fs::read_dir(&outboard_root)
        .expect("read CAS Bao catalog")
        .map(|entry| entry.expect("catalog entry").path())
        .collect::<Vec<_>>();
    assert!(
        catalog_files
            .iter()
            .all(|path| path.extension().is_none_or(|ext| ext != "blob"))
    );
    assert_eq!(
        catalog_files
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "map"))
            .count(),
        1
    );
    assert_eq!(
        catalog_files
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "bao"))
            .count(),
        1
    );
    let scope = TransferScope::from_store_closure(
        AssignmentScope::new([21; 16], [22; 16], 3, [23; 32]).expect("authority scope"),
        closure_id,
    )
    .expect("exact transfer scope");
    let issuer = CapabilityIssuer::new(secret(1));
    let server_key = secret(2);
    let client_key = secret(3);
    let mapping = StoreObjectMapping::from_typed_object(&object);
    assert_eq!(materialized.object, mapping);
    assert_ne!(materialized.blob_hash.0, mapping.object_id);

    let first_claims = claims(
        client_key.public(),
        server_key.public(),
        scope,
        mapping,
        materialized.blob_hash,
        ChunkRange { start: 0, end: 16 },
        31,
    );
    let second_claims = claims(
        client_key.public(),
        server_key.public(),
        scope,
        mapping,
        materialized.blob_hash,
        ChunkRange { start: 16, end: 40 },
        32,
    );

    let first_endpoint = bind_direct(
        server_key.clone(),
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("loopback address"),
    )
    .await
    .expect("private direct server endpoint");
    let first_addr = first_endpoint.addr();
    assert!(first_addr.addrs.iter().all(|address| !address.is_relay()));
    let first_state = ServerState::new(
        AdmissionPolicy::new(
            server_key.public(),
            issuer.public_key(),
            [client_key.public()],
            scope,
        ),
        std::sync::Arc::new(StoreBlobCatalog::new(source_sink.clone(), &outboard_root)),
    );
    let first_serving_endpoint = first_endpoint.clone();
    let first_serve = tokio::spawn(async move {
        timeout(
            Duration::from_secs(30),
            serve_one(&first_serving_endpoint, &first_state),
        )
        .await
        .expect("first server request timeout")
        .expect("serve first Bao range");
    });
    let first = run_child_batch_mode(
        first_addr,
        &issuer,
        &client_key,
        vec![
            issuer
                .issue(first_claims.clone())
                .expect("first range grant"),
            issuer
                .issue(second_claims.clone())
                .expect("second range grant"),
        ],
        scope,
        &checkpoint,
        false,
        Some(1),
    )
    .await;
    assert!(
        !first.status.success(),
        "the injected process failure should stop this transfer after one durable range"
    );
    assert!(
        String::from_utf8_lossy(&first.stderr)
            .contains("injected failure after durable range checkpoint"),
        "first client failure should occur after the range checkpoint is durable: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    first_serve.await.expect("first server task");
    let interrupted = ResumeState::load(&checkpoint, &first_claims)
        .expect("first range checkpoint survives client failure");
    assert!(!interrupted.is_complete());
    assert_eq!(interrupted.covered, vec![first_claims.range]);
    timeout(Duration::from_secs(10), first_endpoint.close())
        .await
        .expect("first server shutdown timeout");

    // Reopen the same durable CAS and outboard through new handles after a cold server restart.
    let restarted_source = FileStore::open(&source_root, 1024 * 1024).expect("cold reopen CAS");
    let restarted_sink = restarted_source.artifact_sink(artifact_budget());
    assert!(
        restarted_sink
            .open_object(object_claim)
            .expect("reverify CAS object")
            .is_some()
    );
    let second_endpoint = bind_direct(
        server_key.clone(),
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("loopback address"),
    )
    .await
    .expect("restarted private direct server endpoint");
    let second_addr = second_endpoint.addr();
    assert!(second_addr.addrs.iter().all(|address| !address.is_relay()));
    let second_state = ServerState::new(
        AdmissionPolicy::new(
            server_key.public(),
            issuer.public_key(),
            [client_key.public()],
            scope,
        ),
        std::sync::Arc::new(StoreBlobCatalog::new(
            restarted_sink.clone(),
            &outboard_root,
        )),
    );
    let second_serving_endpoint = second_endpoint.clone();
    let second_serve = tokio::spawn(async move {
        timeout(
            Duration::from_secs(30),
            serve_one(&second_serving_endpoint, &second_state),
        )
        .await
        .expect("resumed server request timeout")
        .expect("serve resumed Bao range");
    });
    let second = run_child(
        second_addr,
        &issuer,
        &client_key,
        issuer
            .issue(second_claims.clone())
            .expect("resume range grant"),
        scope,
        &checkpoint,
    )
    .await;
    assert!(
        second.status.success(),
        "resumed client process failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("complete=true"));
    second_serve.await.expect("resumed server task");

    let resumed = ResumeState::load(&checkpoint, &second_claims).expect("cold-resumed checkpoint");
    assert!(resumed.is_complete());
    resumed
        .verify_complete_payload(&checkpoint)
        .await
        .expect("whole raw payload hash");

    let destination = FileStore::open(directory.join("destination-store"), 1024 * 1024)
        .expect("open destination CAS");
    let destination_sink = destination.artifact_sink(artifact_budget());
    let plan = ArtifactPlan::new(
        None,
        ArtifactClosureClaim::from_id(closure_id),
        vec![mapping.artifact_claim()],
        vec![],
    );
    let mut session = destination_sink
        .begin(plan)
        .expect("begin typed receive session");
    resumed
        .feed_to_artifact_session(&checkpoint, 0, &mut session)
        .await
        .expect("stream verified bytes into ArtifactSink");
    let receipt = session.finish().expect("admit exact closure into CAS");
    assert_eq!(receipt.closure(), closure_id);
    let reopened = destination
        .reopen_stored_closure(ArtifactClosureClaim::from_id(closure_id), artifact_budget())
        .expect("cold reopen admitted closure");
    assert_eq!(reopened.closure(), closure_id);
    assert_eq!(reopened.object_count(), 1);
    timeout(Duration::from_secs(10), second_endpoint.close())
        .await
        .expect("restarted server shutdown timeout");
    std::fs::remove_dir_all(directory).expect("remove test state");
}

#[tokio::test]
async fn zero_byte_cas_member_uses_authenticated_empty_range_and_cold_resumes() {
    let directory = temp_dir("empty-object");
    let empty_payload = Vec::new();
    let source_root = directory.join("source-store");
    let outboard_root = directory.join("bao-catalog");
    let checkpoint = directory.join("empty-transfer.chk");
    let (source_store, object, closure_id) = prepare_store(&source_root, &empty_payload);
    let source_sink = source_store.artifact_sink(artifact_budget());
    let mapping = StoreObjectMapping::from_typed_object(&object);
    assert_eq!(mapping.payload_length, 0);
    assert_eq!(mapping.object_id, *object.id().as_bytes());
    assert_ne!(mapping.object_id, [0; 32]);

    let source_catalog = StoreBlobCatalog::new(source_sink.clone(), &outboard_root);
    let materialized = source_catalog
        .register_store_object(UntrustedObjectId::from_bytes(mapping.object_id))
        .await
        .expect("register empty CAS object")
        .expect("empty object in source closure");
    let empty_blob = BlobHash(*blake3::hash(&[]).as_bytes());
    assert_eq!(materialized.blob_hash, empty_blob);
    assert_eq!(materialized.object, mapping);
    assert_eq!(extension_bytes(&outboard_root, "bao"), 0);
    assert_eq!(extension_bytes(&outboard_root, "blob"), 0);

    let scope = TransferScope::from_store_closure(
        AssignmentScope::new([61; 16], [62; 16], 5, [63; 32]).expect("empty work fence"),
        closure_id,
    )
    .expect("exact empty closure scope");
    let issuer = CapabilityIssuer::new(secret(64));
    let server_key = secret(65);
    let client_key = secret(66);
    let empty_range = ChunkRange { start: 0, end: 0 };
    let empty_capability = |nonce| {
        let mut claims = claims(
            client_key.public(),
            server_key.public(),
            scope,
            mapping,
            empty_blob,
            empty_range,
            nonce,
        );
        claims.byte_budget = 0;
        issuer.issue(claims).expect("sign exact empty-object grant")
    };
    let first_capability = empty_capability(67);
    let first_endpoint = bind_direct(
        server_key.clone(),
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("loopback socket"),
    )
    .await
    .expect("bind first empty-object server");
    let first_addr = first_endpoint.addr();
    assert!(first_addr.addrs.iter().all(|address| !address.is_relay()));
    let first_state = ServerState::new(
        AdmissionPolicy::new(
            first_endpoint.id(),
            issuer.public_key(),
            [client_key.public()],
            scope,
        ),
        std::sync::Arc::new(StoreBlobCatalog::new(source_sink.clone(), &outboard_root)),
    );
    let first_listener = first_endpoint.clone();
    let first_server = tokio::spawn(async move {
        timeout(
            Duration::from_secs(10),
            serve_one_measured(&first_listener, &first_state),
        )
        .await
        .expect("first empty range timeout")
        .expect("serve authenticated empty range")
    });
    let first = run_child(
        first_addr,
        &issuer,
        &client_key,
        first_capability,
        scope,
        &checkpoint,
    )
    .await;
    assert!(
        first.status.success(),
        "empty-object client failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("complete=true"));
    assert_eq!(
        first_server
            .await
            .expect("first empty server task")
            .bao_stream_bytes,
        0
    );
    timeout(Duration::from_secs(10), first_endpoint.close())
        .await
        .expect("first empty server close timeout");

    let restarted_source = FileStore::open(&source_root, 1024 * 1024).expect("reopen empty CAS");
    let restarted_sink = restarted_source.artifact_sink(artifact_budget());
    let second_capability = empty_capability(68);
    let second_claims = second_capability.claims.clone();
    let second_endpoint = bind_direct(
        server_key,
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("restart loopback socket"),
    )
    .await
    .expect("restart empty-object server");
    let second_addr = second_endpoint.addr();
    let second_state = ServerState::new(
        AdmissionPolicy::new(
            second_endpoint.id(),
            issuer.public_key(),
            [client_key.public()],
            scope,
        ),
        std::sync::Arc::new(StoreBlobCatalog::new(
            restarted_sink.clone(),
            &outboard_root,
        )),
    );
    let second_listener = second_endpoint.clone();
    let second_server = tokio::spawn(async move {
        timeout(
            Duration::from_secs(10),
            serve_one_measured(&second_listener, &second_state),
        )
        .await
        .expect("cold-resume empty range timeout")
        .expect("serve cold-resumed empty range")
    });
    let second = run_child(
        second_addr,
        &issuer,
        &client_key,
        second_capability,
        scope,
        &checkpoint,
    )
    .await;
    assert!(
        second.status.success(),
        "cold-resume empty client failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("complete=true"));
    assert_eq!(
        second_server
            .await
            .expect("cold-resume empty server task")
            .bao_stream_bytes,
        0
    );
    let resumed =
        ResumeState::load(&checkpoint, &second_claims).expect("load cold-resumed empty checkpoint");
    assert!(resumed.is_complete());
    resumed
        .verify_complete_payload(&checkpoint)
        .await
        .expect("verify BLAKE3(empty) after restart");

    let destination = FileStore::open(directory.join("destination-store"), 1024 * 1024)
        .expect("open empty destination CAS");
    let destination_sink = destination.artifact_sink(artifact_budget());
    let mut session = destination_sink
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(closure_id),
            vec![mapping.artifact_claim()],
            vec![],
        ))
        .expect("begin empty typed receive session");
    resumed
        .feed_to_artifact_session(&checkpoint, 0, &mut session)
        .await
        .expect("admit verified empty bytes through ArtifactSink");
    let receipt = session.finish().expect("store empty result closure");
    assert_eq!(receipt.closure(), closure_id);
    assert_eq!(receipt.object_count(), 1);
    assert_eq!(receipt.payload_bytes(), 0);
    let reopened = destination
        .reopen_stored_closure(ArtifactClosureClaim::from_id(closure_id), artifact_budget())
        .expect("cold reopen empty closure");
    assert_eq!(reopened.closure(), closure_id);
    let admitted = destination
        .artifact_sink(artifact_budget())
        .open_object(UntrustedObjectId::from_bytes(mapping.object_id))
        .expect("open admitted empty object")
        .expect("empty object present in receiver CAS");
    assert_eq!(admitted.id().as_bytes(), &mapping.object_id);
    assert_eq!(admitted.payload_len(), 0);

    let data_path = ResumeState::data_path(&checkpoint);
    let mut mutation = std::fs::OpenOptions::new()
        .write(true)
        .open(&data_path)
        .expect("open sparse empty data for hostile mutation");
    mutation
        .set_len(1)
        .expect("make empty payload sidecar nonempty");
    mutation
        .seek(SeekFrom::Start(0))
        .expect("seek in mutated empty payload");
    mutation
        .write_all(b"x")
        .expect("write mistaken payload byte");
    assert!(matches!(
        resumed.verify_complete_payload(&checkpoint).await,
        Err(TransportError::CheckpointInvalid)
    ));
    timeout(Duration::from_secs(10), second_endpoint.close())
        .await
        .expect("second empty server close timeout");
    std::fs::remove_dir_all(directory).expect("remove empty-object test state");
}

#[test]
fn admission_rejects_wrong_peer_work_and_exact_fence_before_catalog_access() {
    let issuer = CapabilityIssuer::new(secret(11));
    let server_key = secret(12);
    let allowed_client = secret(13);
    let wrong_client = secret(14);
    let payload = b"admission proof fixture";
    let key = ObjectKey::<PayloadSchema>::from_value(&payload[..]);
    let object = TypedObject::from_value(&key, &payload[..]);
    let mapping = StoreObjectMapping::from_typed_object(&object);
    let scope = TransferScope::new([15; 16], [16; 16], 1, [17; 32], [18; 32]).expect("scope");
    let policy = AdmissionPolicy::new(
        server_key.public(),
        issuer.public_key(),
        [allowed_client.public()],
        scope,
    );
    let capability = |client, scope, nonce| {
        issuer
            .issue(claims(
                client,
                server_key.public(),
                scope,
                mapping,
                BlobHash(*blake3::hash(payload).as_bytes()),
                ChunkRange { start: 0, end: 1 },
                nonce,
            ))
            .expect("signed test capability")
    };
    let valid = capability(allowed_client.public(), scope, 19);
    assert_eq!(
        verify_admission(
            &policy,
            wrong_client.public(),
            &valid,
            valid.claims.range,
            now_unix_ms().unwrap()
        ),
        Err(backend_cluster_transport::RejectCode::PeerNotAllowed)
    );
    for wrong_scope in [
        TransferScope::new([20; 16], [16; 16], 1, [17; 32], [18; 32]).expect("wrong namespace"),
        TransferScope::new([15; 16], [21; 16], 1, [17; 32], [18; 32]).expect("wrong work"),
        TransferScope::new([15; 16], [16; 16], 1, [22; 32], [18; 32]).expect("wrong fence"),
    ] {
        let capability = capability(allowed_client.public(), wrong_scope, 20);
        assert_eq!(
            verify_admission(
                &policy,
                allowed_client.public(),
                &capability,
                capability.claims.range,
                now_unix_ms().unwrap(),
            ),
            Err(backend_cluster_transport::RejectCode::ScopeMismatch)
        );
    }
    assert_eq!(
        verify_admission(
            &policy,
            allowed_client.public(),
            &valid,
            valid.claims.range,
            now_unix_ms().unwrap(),
        ),
        Ok(())
    );
}

#[tokio::test]
async fn one_cluster_listener_demultiplexes_alternating_control_and_artifact_connections() {
    let directory = temp_dir("cluster-demux");
    let source_root = directory.join("source-store");
    let outboard_root = directory.join("bao-catalog");
    let payload_a = (0..32_768_usize)
        .map(|index| ((index.wrapping_mul(37) + 11) % 251) as u8)
        .collect::<Vec<_>>();
    let payload_b = (0..19_337_usize)
        .map(|index| ((index.wrapping_mul(13) + (index >> 5) + 7) % 253) as u8)
        .collect::<Vec<_>>();
    let store = FileStore::open(&source_root, 1024 * 1024).expect("open demux CAS");
    let key_a = ObjectKey::<PayloadSchema>::from_value(&payload_a[..]);
    let object_a = TypedObject::from_value(&key_a, &payload_a[..]);
    let key_b = ObjectKey::<PayloadSchema>::from_value(&payload_b[..]);
    let object_b = TypedObject::from_value(&key_b, &payload_b[..]);
    let mut source_objects = vec![object_a.clone(), object_b.clone()];
    source_objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let closure = ClosureManifest::new(source_objects).expect("two-object source closure");
    let closure_id = store.write_closure(&closure).expect("write source closure");
    let closure_a = store
        .write_closure(
            &ClosureManifest::new(vec![object_a.clone()]).expect("single-object closure"),
        )
        .expect("write single-object closure");
    let mapping_a = StoreObjectMapping::from_typed_object(&object_a);
    let mapping_b = StoreObjectMapping::from_typed_object(&object_b);
    assert_ne!(mapping_a.object_id, mapping_b.object_id);
    let source_sink = store.artifact_sink(artifact_budget());
    let source_catalog = StoreBlobCatalog::new(source_sink.clone(), &outboard_root);
    assert!(
        source_catalog
            .register_closure_member(
                closure_a,
                UntrustedObjectId::from_bytes(mapping_b.object_id),
            )
            .await
            .expect("check exact closure membership")
            .is_none(),
        "a checked CAS object outside the claimed closure must not obtain a grant token"
    );
    let object_a_ready = source_catalog
        .register_store_object(UntrustedObjectId::from_bytes(mapping_a.object_id))
        .await
        .expect("register first Bao object")
        .expect("first object in closure");
    let object_b_ready = source_catalog
        .register_store_object(UntrustedObjectId::from_bytes(mapping_b.object_id))
        .await
        .expect("register second Bao object")
        .expect("second object in closure");

    let server_key = secret(121);
    let client_key = secret(122);
    let issuer = CapabilityIssuer::new(secret(123));
    let server_endpoint = bind_direct(
        server_key,
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("server loopback socket"),
    )
    .await
    .expect("bind shared cluster endpoint");
    let server_address = server_endpoint.addr();
    assert!(
        server_address
            .addrs
            .iter()
            .all(|address| !address.is_relay())
    );
    let client_endpoint = bind_direct(
        client_key.clone(),
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("client loopback socket"),
    )
    .await
    .expect("bind demux client endpoint");
    let assignment_a = AssignmentScope::new([124; 16], [125; 16], 1, [126; 32])
        .expect("first exact control assignment");
    let assignment_b = AssignmentScope::new([124; 16], [128; 16], 2, [129; 32])
        .expect("second exact control assignment");
    let transfer_scope = TransferScope::from_store_closure(assignment_a, closure_id)
        .expect("shared artifact closure scope");
    let server_state = ServerState::new(
        AdmissionPolicy::new(
            server_endpoint.id(),
            issuer.public_key(),
            [client_endpoint.id()],
            transfer_scope,
        ),
        std::sync::Arc::new(source_catalog),
    );
    let control_policy =
        ControlAdmissionPolicy::coordinator_ingress(server_endpoint.id(), [client_endpoint.id()]);
    assert!(
        ClusterListener::spawn(
            server_endpoint.clone(),
            control_policy.clone(),
            server_state.clone(),
            0,
        )
        .is_err()
    );
    let mut listener = ClusterListener::spawn(
        server_endpoint.clone(),
        control_policy,
        server_state.clone(),
        2,
    )
    .expect("one bounded ALPN dispatcher");

    let capability_a = issuer
        .issue(claims(
            client_endpoint.id(),
            server_endpoint.id(),
            transfer_scope,
            mapping_a,
            object_a_ready.blob_hash,
            ChunkRange {
                start: 0,
                end: (payload_a.len() as u64).div_ceil(1_024),
            },
            131,
        ))
        .expect("sign first full object range");
    let capability_b = issuer
        .issue(claims(
            client_endpoint.id(),
            server_endpoint.id(),
            transfer_scope,
            mapping_b,
            object_b_ready.blob_hash,
            ChunkRange {
                start: 0,
                end: (payload_b.len() as u64).div_ceil(1_024),
            },
            132,
        ))
        .expect("sign second full object range");
    let checkpoint_a = directory.join("first-object.chk");
    let checkpoint_b = directory.join("second-object.chk");
    let client_a_endpoint = client_endpoint.clone();
    let client_a_addr = server_address.clone();
    let client_a_issuer = issuer.public_key();
    let client_a = tokio::spawn(async move {
        fetch_range(
            &client_a_endpoint,
            client_a_addr,
            client_a_issuer,
            capability_a,
            transfer_scope,
            checkpoint_a,
        )
        .await
    });
    let client_b_endpoint = client_endpoint.clone();
    let client_b_addr = server_address.clone();
    let client_b_issuer = issuer.public_key();
    let client_b = tokio::spawn(async move {
        fetch_range(
            &client_b_endpoint,
            client_b_addr,
            client_b_issuer,
            capability_b,
            transfer_scope,
            checkpoint_b,
        )
        .await
    });

    let target_a = [142; 32];
    let target_b = [152; 32];
    let expected_receipts = [(assignment_a, target_a), (assignment_b, target_b)];
    let receipt_a = ControlResultReceipt {
        scope: assignment_a,
        target_root: target_a,
        pack_id: None,
        closure_id: [146; 32],
        object_count: 2,
        payload_bytes: (payload_a.len() + payload_b.len()) as u64,
        result_grant_pages: 1,
    };
    let client_control_a_endpoint = client_endpoint.clone();
    let client_control_a_addr = server_address.clone();
    let client_control_a = tokio::spawn(send_control_result_receipt(
        client_control_a_endpoint,
        client_control_a_addr,
        server_endpoint.id(),
        assignment_a,
        receipt_a,
    ));
    let receipt_b = ControlResultReceipt {
        scope: assignment_b,
        target_root: target_b,
        pack_id: None,
        closure_id: [156; 32],
        object_count: 1,
        payload_bytes: payload_b.len() as u64,
        result_grant_pages: 1,
    };
    let client_control_b_endpoint = client_endpoint.clone();
    let client_control_b_addr = server_address.clone();
    let client_control_b = tokio::spawn(send_control_result_receipt(
        client_control_b_endpoint,
        client_control_b_addr,
        server_endpoint.id(),
        assignment_b,
        receipt_b,
    ));

    let mut served_artifacts = Vec::with_capacity(2);
    let mut served_controls = Vec::with_capacity(2);
    for _ in 0..4 {
        let accepted = timeout(Duration::from_secs(10), listener.recv())
            .await
            .expect("mixed demux event timeout")
            .expect("cluster listener closed")
            .expect("accepted mixed protocol connection");
        match accepted {
            AcceptedClusterConnection::Artifact(artifact) => {
                let state = server_state.clone();
                served_artifacts.push(tokio::spawn(async move {
                    artifact
                        .serve(&state)
                        .await
                        .expect("serve demuxed artifact")
                }));
            }
            AcceptedClusterConnection::Control(control) => {
                served_controls.push(tokio::spawn(receive_control_result_receipt(
                    control,
                    expected_receipts,
                )));
            }
            AcceptedClusterConnection::Probe(_) => {
                panic!("unexpected probe connection in artifact/control test");
            }
            AcceptedClusterConnection::RemoteIndex(_) => {
                panic!("unexpected remote index connection in artifact/control test");
            }
        }
    }
    assert_eq!(served_artifacts.len(), 2);
    assert_eq!(served_controls.len(), 2);

    let (client_a, client_b, client_control_a, client_control_b) =
        tokio::join!(client_a, client_b, client_control_a, client_control_b);
    let resumed_a = client_a
        .expect("first artifact client task")
        .expect("first artifact fetch");
    let resumed_b = client_b
        .expect("second artifact client task")
        .expect("second artifact fetch");
    client_control_a
        .expect("first control client task")
        .expect("first control roundtrip");
    client_control_b
        .expect("second control client task")
        .expect("second control roundtrip");
    let mut total_bao_stream_bytes = 0_u64;
    for served in served_artifacts {
        total_bao_stream_bytes = total_bao_stream_bytes
            .saturating_add(served.await.expect("artifact server task").bao_stream_bytes);
    }
    assert!(total_bao_stream_bytes >= (payload_a.len() + payload_b.len()) as u64);
    let mut received_targets = Vec::with_capacity(2);
    for served in served_controls {
        received_targets.push(served.await.expect("control server task"));
    }
    received_targets.sort_unstable();
    let mut expected_targets = vec![target_a, target_b];
    expected_targets.sort_unstable();
    assert_eq!(received_targets, expected_targets);
    assert!(resumed_a.is_complete());
    assert!(resumed_b.is_complete());
    resumed_a
        .verify_complete_payload(directory.join("first-object.chk"))
        .await
        .expect("first independently verified object");
    resumed_b
        .verify_complete_payload(directory.join("second-object.chk"))
        .await
        .expect("second independently verified object");
    assert_eq!(
        std::fs::read(ResumeState::data_path(directory.join("first-object.chk")))
            .expect("first transferred payload"),
        payload_a
    );
    assert_eq!(
        std::fs::read(ResumeState::data_path(directory.join("second-object.chk")))
            .expect("second transferred payload"),
        payload_b
    );
    listener.shutdown().await.expect("bounded demux shutdown");
    timeout(Duration::from_secs(10), server_endpoint.close())
        .await
        .expect("shared server endpoint close timeout");
    timeout(Duration::from_secs(10), client_endpoint.close())
        .await
        .expect("shared client endpoint close timeout");
    std::fs::remove_dir_all(directory).expect("remove demux state");
}

#[tokio::test]
async fn reports_two_process_cold_and_interrupted_resume_costs_for_one_and_sixteen_mib() {
    let directory = temp_dir("measurement");
    eprintln!(
        "cluster-transport measurement environment: os={} arch={} parallelism={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism()
            .map(|count| count.get().to_string())
            .unwrap_or_else(|_| "unknown".into())
    );

    for (case, payload_length) in [1_usize << 20, 16_usize << 20].into_iter().enumerate() {
        let case_root = directory.join(format!("case-{case}"));
        std::fs::create_dir_all(&case_root).expect("create measurement case");
        let source_root = case_root.join("source-store");
        let outboard_root = case_root.join("bao-catalog");
        let payload = (0..payload_length)
            .map(|index| ((index.wrapping_mul(31) + (index >> 8)) % 251) as u8)
            .collect::<Vec<_>>();
        let (source_store, object, closure_id) =
            prepare_store_with_capacity(&source_root, &payload, 64 * 1024 * 1024);
        let object_mapping = StoreObjectMapping::from_typed_object(&object);
        drop(object);
        drop(payload);

        let source_sink = source_store.artifact_sink(large_artifact_budget());
        let source_catalog = StoreBlobCatalog::new(source_sink.clone(), &outboard_root);
        let outboard_started = Instant::now();
        let materialized = source_catalog
            .register_store_object(UntrustedObjectId::from_bytes(object_mapping.object_id))
            .await
            .expect("register measured CAS object")
            .expect("measured CAS object exists");
        let outboard_build = outboard_started.elapsed();
        assert_eq!(materialized.object, object_mapping);
        assert_eq!(extension_bytes(&outboard_root, "blob"), 0);

        let namespace = [0x31 + case as u8; 16];
        let work = [0x41 + case as u8; 16];
        let fence = [0x51 + case as u8; 32];
        let assignment =
            AssignmentScope::new(namespace, work, 1, fence).expect("measurement assignment fence");
        let scope = TransferScope::from_store_closure(assignment, closure_id)
            .expect("measurement output closure scope");
        let issuer = CapabilityIssuer::new(secret(91 + case as u8));
        let server_key = secret(101 + case as u8);
        let client_key = secret(111 + case as u8);
        let server_id = server_key.public();
        let total_chunks = (payload_length as u64).div_ceil(1_024);
        let cold_ranges = segment_ranges(0, total_chunks);
        let cold_capabilities = measured_capabilities(
            &issuer,
            client_key.public(),
            server_id,
            scope,
            object_mapping,
            materialized.blob_hash,
            &cold_ranges,
            1,
        );
        let cold_checkpoint = case_root.join("cold.chk");

        let cold_started = Instant::now();
        let (cold_endpoint, cold_address, cold_server) = start_measured_server(
            server_key.clone(),
            issuer.public_key(),
            client_key.public(),
            scope,
            source_sink.clone(),
            outboard_root.clone(),
            cold_capabilities.len(),
        )
        .await;
        let cold_output = run_child_batch(
            cold_address,
            &issuer,
            &client_key,
            cold_capabilities.clone(),
            scope,
            &cold_checkpoint,
        )
        .await;
        assert!(
            cold_output.status.success(),
            "cold client failed: {}",
            String::from_utf8_lossy(&cold_output.stderr)
        );
        assert!(String::from_utf8_lossy(&cold_output.stdout).contains("complete=true"));
        let cold_range_samples = child_range_samples(&cold_output);
        assert_eq!(cold_range_samples.len(), cold_capabilities.len());
        report_latency_distribution(
            payload_length / (1024 * 1024),
            "cold-full",
            &cold_range_samples,
        );
        let cold_open_us = child_checkpoint_open_sample(&cold_output, false)
            .expect("cold client reports fresh checkpoint open");
        eprintln!(
            "cluster-transport checkpoint open: size_mib={} mode=cold-new elapsed_us={cold_open_us} samples=1",
            payload_length / (1024 * 1024),
        );
        let cold_bao_bytes = cold_server.await.expect("cold measured server");
        let cold_elapsed = cold_started.elapsed();
        let cold_state = ResumeState::load(
            &cold_checkpoint,
            &cold_capabilities.last().expect("cold capabilities").claims,
        )
        .expect("cold checkpoint");
        cold_state
            .verify_complete_payload(&cold_checkpoint)
            .await
            .expect("cold complete payload hash");
        let cold_validation = child_validation_sample(&cold_output)
            .expect("cold transfer reports validation counters");
        assert_eq!(
            cold_validation[0], 1,
            "one live grant batch has one cold open"
        );
        assert_eq!(cold_validation[1..5], [0, 0, 0, 0]);
        assert_eq!(cold_validation[5], payload_length as u64);
        timeout(Duration::from_secs(10), cold_endpoint.close())
            .await
            .expect("cold server close timeout");

        if cold_capabilities.len() > 1 {
            let stateless_checkpoint = case_root.join("stateless.chk");
            let stateless_started = Instant::now();
            let (stateless_endpoint, stateless_address, stateless_server) = start_measured_server(
                server_key.clone(),
                issuer.public_key(),
                client_key.public(),
                scope,
                source_sink.clone(),
                outboard_root.clone(),
                cold_capabilities.len(),
            )
            .await;
            let stateless_output = run_child_batch_mode(
                stateless_address,
                &issuer,
                &client_key,
                cold_capabilities.clone(),
                scope,
                &stateless_checkpoint,
                true,
                None,
            )
            .await;
            assert!(
                stateless_output.status.success(),
                "stateless client failed: {}",
                String::from_utf8_lossy(&stateless_output.stderr)
            );
            let stateless_server_bytes = stateless_server.await.expect("stateless server");
            let stateless_elapsed = stateless_started.elapsed();
            let stateless_state = ResumeState::load(
                &stateless_checkpoint,
                &cold_capabilities
                    .last()
                    .expect("stateless capabilities")
                    .claims,
            )
            .expect("stateless checkpoint");
            assert!(stateless_state.is_complete());
            stateless_state
                .verify_complete_payload(&stateless_checkpoint)
                .await
                .expect("stateless complete payload hash");
            timeout(Duration::from_secs(10), stateless_endpoint.close())
                .await
                .expect("stateless server close timeout");
            let stateless_validation = child_validation_sample(&stateless_output)
                .expect("stateless transfer reports validation counters");
            let expected_chunks_revalidated = cold_ranges
                .iter()
                .scan(0_u64, |covered_chunks, range| {
                    let current = *covered_chunks;
                    *covered_chunks = covered_chunks.saturating_add(range.end - range.start);
                    Some(current)
                })
                .sum::<u64>();
            let expected_payload_bytes_revalidated = cold_ranges
                .iter()
                .scan(0_u64, |covered_chunks, range| {
                    let current = *covered_chunks;
                    *covered_chunks = covered_chunks.saturating_add(range.end - range.start);
                    Some(current.saturating_mul(1_024).min(payload_length as u64))
                })
                .sum::<u64>();
            assert_eq!(stateless_validation[0], cold_capabilities.len() as u64);
            assert_eq!(stateless_validation[1], cold_capabilities.len() as u64 - 1);
            assert_eq!(stateless_validation[2], expected_chunks_revalidated);
            assert_eq!(stateless_validation[3], expected_payload_bytes_revalidated);
            assert!(stateless_validation[4] > payload_length as u64);
            assert_eq!(stateless_validation[5], payload_length as u64);
            report_latency_distribution(
                payload_length / (1024 * 1024),
                "stateless-cold-full",
                &child_range_samples(&stateless_output),
            );
            eprintln!(
                "cluster-transport validation comparison: size_mib={} batch_cold_opens={} batch_prior_chunks={} batch_prior_bao_bytes={} stateless_cold_opens={} stateless_prior_chunks={} stateless_prior_payload_bytes={} stateless_prior_bao_bytes={} stateless_whole_hash_bytes={} elapsed_ms={} bao_stream_bytes={}",
                payload_length / (1024 * 1024),
                cold_validation[0],
                cold_validation[2],
                cold_validation[4],
                stateless_validation[0],
                stateless_validation[2],
                stateless_validation[3],
                stateless_validation[4],
                stateless_validation[5],
                stateless_elapsed.as_millis(),
                stateless_server_bytes,
            );
        }

        let resume_checkpoint = case_root.join("resume.chk");
        let interrupted_at = total_chunks / 2;
        let first_ranges = segment_ranges(0, interrupted_at);
        let remaining_ranges = segment_ranges(interrupted_at, total_chunks);
        let first_capabilities = measured_capabilities(
            &issuer,
            client_key.public(),
            server_id,
            scope,
            object_mapping,
            materialized.blob_hash,
            &first_ranges,
            61,
        );
        let remaining_capabilities = measured_capabilities(
            &issuer,
            client_key.public(),
            server_id,
            scope,
            object_mapping,
            materialized.blob_hash,
            &remaining_ranges,
            101,
        );
        let resumed_started = Instant::now();
        let (first_endpoint, first_address, first_server) = start_measured_server(
            server_key.clone(),
            issuer.public_key(),
            client_key.public(),
            scope,
            source_sink.clone(),
            outboard_root.clone(),
            first_capabilities.len(),
        )
        .await;
        let first_output = run_child_batch(
            first_address,
            &issuer,
            &client_key,
            first_capabilities,
            scope,
            &resume_checkpoint,
        )
        .await;
        assert!(
            first_output.status.success(),
            "interrupted client failed: {}",
            String::from_utf8_lossy(&first_output.stderr)
        );
        assert!(String::from_utf8_lossy(&first_output.stdout).contains("complete=false"));
        let first_bao_bytes = first_server.await.expect("first resumed server");
        timeout(Duration::from_secs(10), first_endpoint.close())
            .await
            .expect("first resumed server close timeout");

        let (second_endpoint, second_address, second_server) = start_measured_server(
            server_key,
            issuer.public_key(),
            client_key.public(),
            scope,
            source_sink,
            outboard_root.clone(),
            remaining_capabilities.len(),
        )
        .await;
        let second_output = run_child_batch(
            second_address,
            &issuer,
            &client_key,
            remaining_capabilities.clone(),
            scope,
            &resume_checkpoint,
        )
        .await;
        assert!(
            second_output.status.success(),
            "resumed client failed: {}",
            String::from_utf8_lossy(&second_output.stderr)
        );
        assert!(String::from_utf8_lossy(&second_output.stdout).contains("complete=true"));
        let first_range_samples = child_range_samples(&first_output);
        let second_range_samples = child_range_samples(&second_output);
        assert_eq!(
            first_range_samples.len(),
            first_ranges.len(),
            "the interrupted process should fetch each planned range once"
        );
        assert_eq!(
            second_range_samples.len(),
            remaining_ranges.len(),
            "the resumed process should fetch each remaining planned range once"
        );
        let mut resumed_range_samples = first_range_samples;
        resumed_range_samples.extend(second_range_samples);
        assert_eq!(
            resumed_range_samples.len(),
            first_ranges.len() + remaining_ranges.len()
        );
        report_latency_distribution(
            payload_length / (1024 * 1024),
            "interrupted-resume",
            &resumed_range_samples,
        );
        let revalidation_us = child_checkpoint_open_sample(&second_output, true)
            .expect("resumed client reports revalidated checkpoint open");
        eprintln!(
            "cluster-transport checkpoint open: size_mib={} mode=resume-existing elapsed_us={revalidation_us} samples=1 note=includes_one_cold_validation_of_prior_coverage",
            payload_length / (1024 * 1024),
        );
        let second_bao_bytes = second_server.await.expect("second resumed server");
        let resumed_elapsed = resumed_started.elapsed();
        let resume_state = ResumeState::load(
            &resume_checkpoint,
            &remaining_capabilities
                .last()
                .expect("remaining capabilities")
                .claims,
        )
        .expect("resumed checkpoint");
        resume_state
            .verify_complete_payload(&resume_checkpoint)
            .await
            .expect("resumed complete payload hash");
        timeout(Duration::from_secs(10), second_endpoint.close())
            .await
            .expect("second resumed server close timeout");

        let cas_file_bytes = directory_bytes(&source_root);
        let outboard_bytes = extension_bytes(&outboard_root, "bao");
        let mapping_bytes = extension_bytes(&outboard_root, "map");
        let checkpoint_bytes = checkpoint_logical_bytes(&resume_checkpoint);
        let cold_mib_per_second =
            payload_length as f64 / (1024.0 * 1024.0) / cold_elapsed.as_secs_f64();
        let resumed_mib_per_second =
            payload_length as f64 / (1024.0 * 1024.0) / resumed_elapsed.as_secs_f64();
        eprintln!(
            "cluster-transport measurement: size_mib={} mode=cold-full app_bytes={} bao_stream_bytes={} quic_wire_bytes=unavailable elapsed_ms={} app_mib_per_second={:.2} outboard_build_ms={} cas_file_bytes={} retained_bao_bytes={} mapping_bytes={} checkpoint_logical_bytes={} client_processes=1 server_endpoint_restarts=0",
            payload_length / (1024 * 1024),
            payload_length,
            cold_bao_bytes,
            cold_elapsed.as_millis(),
            cold_mib_per_second,
            outboard_build.as_millis(),
            cas_file_bytes,
            outboard_bytes,
            mapping_bytes,
            checkpoint_bytes,
        );
        eprintln!(
            "cluster-transport measurement: size_mib={} mode=interrupted-resume app_bytes={} interrupted_after_app_bytes={} bao_stream_bytes={} quic_wire_bytes=unavailable elapsed_ms={} app_mib_per_second={:.2} outboard_build_ms={} cas_file_bytes={} retained_bao_bytes={} mapping_bytes={} checkpoint_logical_bytes={} client_processes=2 server_endpoint_restarts=1",
            payload_length / (1024 * 1024),
            payload_length,
            payload_length / 2,
            first_bao_bytes.saturating_add(second_bao_bytes),
            resumed_elapsed.as_millis(),
            resumed_mib_per_second,
            outboard_build.as_millis(),
            cas_file_bytes,
            outboard_bytes,
            mapping_bytes,
            checkpoint_bytes,
        );
    }

    std::fs::remove_dir_all(directory).expect("remove measurement state");
}
