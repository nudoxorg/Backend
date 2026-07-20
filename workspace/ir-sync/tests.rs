//! End-to-end sync tests using in-process iroh endpoints.
//!
//! **In-process networking**: We use `MemoryLookup` (iroh's in-memory address
//! registry) to let endpoints discover each other without a relay server.
//! After binding, each endpoint registers its `EndpointAddr`; the other
//! endpoint picks it up on `connect()`.
//!
//! **Key pre-generation**: Both the sender and receiver generate `SecretKey`s
//! up-front so each side knows the other's `EndpointId` before binding, breaking
//! the circular dependency (sender needs receiver_id for RemoteConfig; receiver
//! needs sender_id for blob fetches).
//!
//! **Test ChangeIds**: A test ChangeId = `blake3(bytes)` → 64-hex chars.
//! `MemChangeIo::verify` uses BLAKE3, so verification is real (not mocked).

use std::sync::{Arc, Mutex};

use iroh::address_lookup::MemoryLookup;
use tokio::time::Duration;

use crate::{
    ChangeId, ChannelRef, MergeEvent, PackageName,
    io::{ApplyHook, ChangeIo, MemChangeIo, change_id_for_bytes},
    transport::{RemoteConfig, SyncService, Syncer},
    types::SyncError,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn test_channel() -> ChannelRef {
    ChannelRef {
        package: PackageName::new("test-org/test-pkg").unwrap(),
        channel: "main".into(),
    }
}

/// An `ApplyHook` that records calls and returns the last `ChangeId` as tip.
#[derive(Debug, Clone, Default)]
struct RecordingApplyHook {
    calls: Arc<Mutex<Vec<(ChannelRef, Vec<ChangeId>)>>>,
}

impl RecordingApplyHook {
    fn new() -> Self {
        Self::default()
    }

    fn calls(&self) -> Vec<(ChannelRef, Vec<ChangeId>)> {
        self.calls.lock().expect("poisoned").clone()
    }
}

impl ApplyHook for RecordingApplyHook {
    fn apply(&self, channel: &ChannelRef, changes: &[ChangeId]) -> Result<ChangeId, SyncError> {
        self.calls
            .lock()
            .expect("poisoned")
            .push((channel.clone(), changes.to_vec()));
        changes
            .last()
            .cloned()
            .ok_or_else(|| SyncError::Transport("apply called with empty list".into()))
    }
}

fn three_changes() -> Vec<(ChangeId, Vec<u8>)> {
    [
        b"change-alpha: symbol A added" as &[u8],
        b"change-beta:  symbol B modified",
        b"change-gamma: symbol C deleted",
    ]
    .iter()
    .map(|b| (change_id_for_bytes(b), b.to_vec()))
    .collect()
}

/// Build a (Syncer, SyncService) pair with pre-determined keys so each side
/// knows the other's EndpointId before binding.
async fn make_pair(
    sender_io: Arc<MemChangeIo>,
    receiver_io: Arc<MemChangeIo>,
    hook: Arc<RecordingApplyHook>,
) -> (Syncer<MemChangeIo>, SyncService<MemChangeIo, RecordingApplyHook>) {
    let lookup = MemoryLookup::new();

    let sender_key = iroh::SecretKey::generate();
    let receiver_key = iroh::SecretKey::generate();
    let sender_id = sender_key.public();
    let receiver_id = receiver_key.public();

    // Build service (receiver) first — it needs sender_id for blob fetches.
    let service = SyncService::new_with_key(
        receiver_io,
        hook,
        sender_id,
        Some(lookup.clone()),
        receiver_key,
    )
    .await
    .expect("service bind");
    lookup.add_endpoint_info(service.endpoint().addr());

    // Build syncer (sender) — it needs receiver_id as the remote.
    let syncer = Syncer::new_with_key(
        sender_io,
        RemoteConfig { node_id: receiver_id },
        Some(lookup.clone()),
        sender_key,
    )
    .await
    .expect("syncer bind");
    lookup.add_endpoint_info(syncer.endpoint().addr());

    (syncer, service)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// E2E happy path: 3 changes → all received, applied, ack matches tip.
#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_sync_happy_path() {
    let changes = three_changes();
    let sender_io = Arc::new(MemChangeIo::new());
    let receiver_io = Arc::new(MemChangeIo::new());
    let hook = Arc::new(RecordingApplyHook::new());

    for (id, bytes) in &changes {
        sender_io.insert(id.clone(), bytes.clone());
    }

    let change_ids: Vec<ChangeId> = changes.iter().map(|(id, _)| id.clone()).collect();
    let tip = change_ids.last().unwrap().clone();
    let channel = test_channel();
    let event = MergeEvent {
        channel: channel.clone(),
        tip: tip.clone(),
        new_changes: change_ids.clone(),
    };

    let (syncer, service) = make_pair(
        Arc::clone(&sender_io),
        Arc::clone(&receiver_io),
        Arc::clone(&hook),
    )
    .await;

    let accept_task = tokio::spawn(async move { service.accept_one().await });

    let ack = tokio::time::timeout(Duration::from_secs(30), syncer.on_merge(event))
        .await
        .expect("no on_merge timeout")
        .expect("on_merge ok");

    let recv_ack = tokio::time::timeout(Duration::from_secs(30), accept_task)
        .await
        .expect("no accept timeout")
        .expect("join ok")
        .expect("accept_one ok");

    assert_eq!(ack.tip, tip);
    assert_eq!(recv_ack.tip, tip);
    assert_eq!(recv_ack.applied, 3);

    for (id, _) in &changes {
        assert!(receiver_io.has_change(id).unwrap(), "change {id} missing");
    }

    let calls = hook.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, channel);
    assert_eq!(calls[0].1, change_ids);
}

/// Tampered blob: ChangeId vs bytes mismatch → VerificationFailed, nothing written.
#[tokio::test(flavor = "multi_thread")]
async fn test_tampered_blob_rejected() {
    let real_bytes = b"legitimate change bytes";
    let tampered_bytes = b"TAMPERED! evil bytes";

    // The announced ChangeId is blake3(real_bytes), but the sender stores tampered_bytes.
    let real_id = change_id_for_bytes(real_bytes);

    let sender_io = Arc::new(MemChangeIo::new());
    sender_io.insert(real_id.clone(), tampered_bytes.to_vec());

    let receiver_io = Arc::new(MemChangeIo::new());
    let hook = Arc::new(RecordingApplyHook::new());

    let (syncer, service) = make_pair(
        Arc::clone(&sender_io),
        Arc::clone(&receiver_io),
        Arc::clone(&hook),
    )
    .await;

    let accept_task = tokio::spawn(async move { service.accept_one().await });

    let event = MergeEvent {
        channel: test_channel(),
        tip: real_id.clone(),
        new_changes: vec![real_id.clone()],
    };

    // Sender: tampered_bytes get added as iroh blob (with their own BLAKE3 hash).
    // The announcement says the pijul hash is `real_id` (= blake3(real_bytes)).
    // Receiver: fetches blob by iroh hash, gets tampered_bytes, then verify(real_id,
    // tampered_bytes) computes blake3(tampered_bytes) ≠ real_id → VerificationFailed.
    let _sender_result =
        tokio::time::timeout(Duration::from_secs(30), syncer.on_merge(event)).await;

    let recv_result = tokio::time::timeout(Duration::from_secs(30), accept_task)
        .await
        .expect("no accept timeout")
        .expect("join ok");

    match recv_result {
        Err(SyncError::VerificationFailed(_)) => {}
        other => panic!("expected VerificationFailed, got: {other:?}"),
    }

    assert!(
        !receiver_io.has_change(&real_id).unwrap(),
        "tampered change must NOT be stored"
    );
    assert!(hook.calls().is_empty(), "apply hook must not fire on failure");
}

/// Sender doesn't have the announced change → MissingChange error at sender.
#[tokio::test(flavor = "multi_thread")]
async fn test_missing_change_on_sender() {
    let bytes = b"a real change";
    let id = change_id_for_bytes(bytes);

    let sender_io = Arc::new(MemChangeIo::new()); // empty — change is missing
    let receiver_io = Arc::new(MemChangeIo::new());
    let hook = Arc::new(RecordingApplyHook::new());

    let (syncer, service) = make_pair(
        Arc::clone(&sender_io),
        Arc::clone(&receiver_io),
        Arc::clone(&hook),
    )
    .await;

    // Receiver accept in background (may never get a connection if sender fails early).
    let _accept_task = tokio::spawn(async move {
        let _ = service.accept_one().await;
    });

    let event = MergeEvent {
        channel: test_channel(),
        tip: id.clone(),
        new_changes: vec![id.clone()],
    };

    let result = tokio::time::timeout(Duration::from_secs(10), syncer.on_merge(event))
        .await
        .expect("no timeout");

    match result {
        Err(SyncError::MissingChange(missing)) => assert_eq!(missing, id),
        other => panic!("expected MissingChange, got {other:?}"),
    }
}

/// A push from an endpoint that is not the enrolled sender is refused with a
/// typed `RemoteRefused`, and nothing is applied (INDEX-PLAN ID-18 trust gate).
#[tokio::test(flavor = "multi_thread")]
async fn test_push_from_non_enrolled_endpoint_rejected() {
    let bytes = b"a change from an untrusted peer";
    let id = change_id_for_bytes(bytes);

    let sender_io = Arc::new(MemChangeIo::new());
    sender_io.insert(id.clone(), bytes.to_vec());
    let receiver_io = Arc::new(MemChangeIo::new());
    let hook = Arc::new(RecordingApplyHook::new());

    let lookup = MemoryLookup::new();
    let sender_key = iroh::SecretKey::generate();
    let receiver_key = iroh::SecretKey::generate();
    let sender_id = sender_key.public();
    let receiver_id = receiver_key.public();

    // The receiver is told to trust some OTHER endpoint, not this sender.
    let unrelated_trusted = iroh::SecretKey::generate().public();

    let service = SyncService::new_with_key(
        receiver_io.clone(),
        hook.clone(),
        unrelated_trusted,
        Some(lookup.clone()),
        receiver_key,
    )
    .await
    .expect("service bind");
    lookup.add_endpoint_info(service.endpoint().addr());

    let syncer = Syncer::new_with_key(
        sender_io,
        RemoteConfig { node_id: receiver_id },
        Some(lookup.clone()),
        sender_key,
    )
    .await
    .expect("syncer bind");
    lookup.add_endpoint_info(syncer.endpoint().addr());

    let accept_task = tokio::spawn(async move { service.accept_one().await });

    let event = MergeEvent {
        channel: test_channel(),
        tip: id.clone(),
        new_changes: vec![id.clone()],
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), syncer.on_merge(event)).await;

    let recv_result = tokio::time::timeout(Duration::from_secs(10), accept_task)
        .await
        .expect("no accept timeout")
        .expect("join ok");

    match recv_result {
        Err(SyncError::RemoteRefused(_)) => {}
        other => panic!("expected RemoteRefused for non-enrolled push, got {other:?}"),
    }
    assert!(
        !receiver_io.has_change(&id).unwrap(),
        "no change from a non-enrolled peer should be stored"
    );
    assert!(hook.calls().is_empty(), "apply must not fire for non-enrolled peer");
    let _ = sender_id; // documented for symmetry with make_pair
}

/// Re-announcing an already-synced tip is a no-op: applied == 0.
#[tokio::test(flavor = "multi_thread")]
async fn test_idempotent_resync() {
    let bytes = b"the one and only change";
    let id = change_id_for_bytes(bytes);

    let sender_io = Arc::new(MemChangeIo::new());
    sender_io.insert(id.clone(), bytes.to_vec());

    let receiver_io = Arc::new(MemChangeIo::new());
    // Pre-populate receiver — simulating a prior successful sync.
    receiver_io.write_change(&id, bytes).unwrap();

    let hook = Arc::new(RecordingApplyHook::new());

    let (syncer, service) = make_pair(
        Arc::clone(&sender_io),
        Arc::clone(&receiver_io),
        Arc::clone(&hook),
    )
    .await;

    let accept_task = tokio::spawn(async move { service.accept_one().await });

    let event = MergeEvent {
        channel: test_channel(),
        tip: id.clone(),
        new_changes: vec![id.clone()],
    };

    let ack = tokio::time::timeout(Duration::from_secs(30), syncer.on_merge(event))
        .await
        .expect("no timeout")
        .expect("on_merge ok");

    let recv_ack = tokio::time::timeout(Duration::from_secs(30), accept_task)
        .await
        .expect("no accept timeout")
        .expect("join ok")
        .expect("accept_one ok");

    assert_eq!(recv_ack.applied, 0, "idempotent: nothing new fetched");
    assert_eq!(ack.tip, id);

    // Apply hook is still called to confirm the tip.
    assert_eq!(hook.calls().len(), 1);
    assert_eq!(hook.calls()[0].1, vec![id.clone()]);
}
