//! End-to-end sync tests using in-process iroh endpoints.

use std::sync::{Arc, Mutex};

use iroh::address_lookup::MemoryLookup;
use tokio::time::Duration;

use heart::sync::{ApplyHook, SyncError};

use crate::sync::{
    ChangeId, ChannelRef, MemChangeIo, MergeEvent, PackageName, change_id_for_bytes,
    transport::{RemoteConfig, SyncService, Syncer},
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
    type Id = ChangeId;
    type Target = ChannelRef;
    type Tip = ChangeId;

    fn apply(&self, channel: &ChannelRef, changes: &[ChangeId]) -> Result<ChangeId, SyncError> {
        self.calls
            .lock()
            .expect("poisoned")
            .push((channel.clone(), changes.to_vec()));
        changes
            .last()
            .cloned()
            .ok_or_else(|| SyncError::Other("apply called with empty list".into()))
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

async fn make_pair(
    sender_io: Arc<MemChangeIo>,
    receiver_io: Arc<MemChangeIo>,
    hook: Arc<RecordingApplyHook>,
) -> (
    Syncer<MemChangeIo>,
    SyncService<MemChangeIo, RecordingApplyHook>,
) {
    let lookup = MemoryLookup::new();

    let sender_key = iroh::SecretKey::generate();
    let receiver_key = iroh::SecretKey::generate();
    let sender_id = sender_key.public();
    let receiver_id = receiver_key.public();

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

    let syncer = Syncer::new_with_key(
        sender_io,
        RemoteConfig {
            node_id: receiver_id,
        },
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

    use heart::sync::ContentIo;
    for (id, _) in &changes {
        assert!(receiver_io.has(id).unwrap(), "change {id} missing");
    }

    let calls = hook.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, channel);
    assert_eq!(calls[0].1, change_ids);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_tampered_blob_rejected() {
    let real_bytes = b"legitimate change bytes";
    let tampered_bytes = b"TAMPERED! evil bytes";

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

    // The sender must be TOLD, by the receiver, that its push was rejected.
    // Discarding this result hid the fact that the receiver simply hung up: the
    // sender stayed parked on its ack read for QUIC's ~30 s idle timeout and
    // then surfaced a bare transport error. Assert the typed refusal *and* that
    // it arrived by being sent rather than by a timer expiring.
    let started = std::time::Instant::now();
    let sender_result = tokio::time::timeout(Duration::from_secs(30), syncer.on_merge(event))
        .await
        .expect("sender must not have to wait out a timeout to learn it was refused");
    let elapsed = started.elapsed();

    match sender_result {
        Err(SyncError::RemoteRefused(reason)) => {
            assert!(
                reason.contains("hash mismatch") || reason.contains("verif"),
                "the refusal must name the verification failure, got {reason:?}"
            );
        }
        other => panic!("expected the sender to be refused, got: {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(10),
        "the refusal must be delivered, not discovered via QUIC's ~30s idle \
         timeout; on_merge took {elapsed:?}"
    );

    let recv_result = tokio::time::timeout(Duration::from_secs(30), accept_task)
        .await
        .expect("no accept timeout")
        .expect("join ok");

    match recv_result {
        Err(SyncError::VerificationFailed(_)) => {}
        other => panic!("expected VerificationFailed, got: {other:?}"),
    }

    use heart::sync::ContentIo;
    assert!(
        !receiver_io.has(&real_id).unwrap(),
        "tampered change must NOT be stored"
    );
    assert!(
        hook.calls().is_empty(),
        "apply hook must not fire on failure"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_missing_change_on_sender() {
    let bytes = b"a real change";
    let id = change_id_for_bytes(bytes);

    let sender_io = Arc::new(MemChangeIo::new());
    let receiver_io = Arc::new(MemChangeIo::new());
    let hook = Arc::new(RecordingApplyHook::new());

    let (syncer, service) = make_pair(
        Arc::clone(&sender_io),
        Arc::clone(&receiver_io),
        Arc::clone(&hook),
    )
    .await;

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
        Err(SyncError::MissingItem(missing)) => assert_eq!(missing, id.to_string()),
        other => panic!("expected MissingItem, got {other:?}"),
    }
}

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
        RemoteConfig {
            node_id: receiver_id,
        },
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
    use heart::sync::ContentIo;
    assert!(
        !receiver_io.has(&id).unwrap(),
        "no change from a non-enrolled peer should be stored"
    );
    assert!(
        hook.calls().is_empty(),
        "apply must not fire for non-enrolled peer"
    );
    let _ = sender_id;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_idempotent_resync() {
    let bytes = b"the one and only change";
    let id = change_id_for_bytes(bytes);

    let sender_io = Arc::new(MemChangeIo::new());
    sender_io.insert(id.clone(), bytes.to_vec());

    let receiver_io = Arc::new(MemChangeIo::new());
    use heart::sync::ContentIo;
    receiver_io.write(&id, bytes).unwrap();

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

    assert_eq!(hook.calls().len(), 1);
    assert_eq!(hook.calls()[0].1, vec![id.clone()]);
}
