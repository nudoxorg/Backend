//! Integration tests for nudox_ir::sync repo_glue.

use std::sync::Arc;

use crate::wire::{
    EntryPayloadFlags, FunctionWire, KindWire, OwnedEntryPayload, PayloadTable, SymbolWire,
};
use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

use heart::sync::{ContentIo, SyncError};

use crate::sync::{
    ChangeId, ChannelRef, MemChangeIo, MergeEvent, PackageName as SyncPackageName,
    repo_glue::{FsChangeIo, RepoApplyHook, merge_event},
    transport::{RemoteConfig, SyncService, Syncer},
};

use iroh::address_lookup::MemoryLookup;
use tokio::time::Duration;

use crate::repo::IrRepository;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("test-pkg"))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn function_payload(name: &str) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        SymbolWire {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: "src/lib.rs".to_owned(),
            span_start: 0,
            span_end: name.len() as u32,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
        },
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: Default::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn gen1() -> PayloadTable {
    let mut ir = PayloadTable::new();
    ir.insert_live(intro(1), function_payload("alpha"), None);
    ir
}

fn gen2() -> PayloadTable {
    let mut ir = PayloadTable::new();
    ir.insert_live(intro(1), function_payload("alpha"), None);
    ir.insert_live(intro(2), function_payload("beta"), None);
    ir
}

fn test_channel_ref() -> ChannelRef {
    ChannelRef {
        package: SyncPackageName::new("cargo/test-pkg").unwrap(),
        channel: "main".into(),
    }
}

// ---------------------------------------------------------------------------
// Test 1: FsChangeIo path-compat
// ---------------------------------------------------------------------------

#[test]
fn fs_change_io_path_compat() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let repo = IrRepository::open(&root, pkg(), "main").unwrap();
    let hash_hex = repo
        .record_generation(&gen1())
        .unwrap()
        .expect("must produce a change");

    let io = FsChangeIo::for_repo(&root);

    let change_id: ChangeId = hash_hex.0.parse().unwrap();

    let bytes = io.read(&change_id).expect("read must succeed");
    assert!(!bytes.is_empty(), "change bytes must not be empty");

    io.verify(&change_id, &bytes)
        .expect("verify must pass for genuine bytes");

    let mut bad = bytes.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0xff;
    let err = io.verify(&change_id, &bad);
    assert!(err.is_err(), "verify must fail for tampered bytes");
}

// ---------------------------------------------------------------------------
// Test 2: apply_external_changes cross-repo
// ---------------------------------------------------------------------------

#[test]
fn apply_external_changes_cross_repo() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let repo_a = IrRepository::open(dir_a.path(), pkg(), "main").unwrap();
    let hash1 = repo_a
        .record_generation(&gen1())
        .unwrap()
        .expect("gen1 change");
    let hash2 = repo_a
        .record_generation(&gen2())
        .unwrap()
        .expect("gen2 change");

    let repo_b = IrRepository::open(dir_b.path(), pkg(), "main").unwrap();

    let io_a = FsChangeIo::for_repo(dir_a.path());
    let io_b = FsChangeIo::for_repo(dir_b.path());

    for hex in [&hash1, &hash2] {
        let id: ChangeId = hex.0.parse().unwrap();
        let bytes = io_a.read(&id).unwrap();
        io_b.write(&id, &bytes).unwrap();
        assert!(io_b.has(&id).unwrap(), "change must be present after write");
    }

    let tip = repo_b
        .apply_external_changes(&[hash1.clone(), hash2.clone()])
        .expect("apply_external_changes must succeed");

    let mat_a = repo_a.materialize_index().unwrap();
    let mat_b = repo_b.materialize_index().unwrap();
    assert_eq!(
        mat_a.tip, mat_b.tip,
        "B's tip must equal A's after applying all changes"
    );
    assert_eq!(mat_b.symbols.len(), mat_a.symbols.len());
    for intro in mat_a.intros() {
        assert!(mat_b.get(intro).is_some(), "B must have intro {:?}", intro);
        assert_eq!(
            mat_a.get(intro).unwrap()[..],
            mat_b.get(intro).unwrap()[..],
            "symbol bytes must match"
        );
    }

    let tip2 = repo_b
        .apply_external_changes(&[hash1, hash2])
        .expect("re-apply must succeed (idempotent)");
    assert_eq!(
        tip.fingerprint(),
        tip2.fingerprint(),
        "re-apply must not change the tip"
    );
}

// ---------------------------------------------------------------------------
// Test 3: full iroh sync loop
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn full_iroh_sync_loop() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let repo_a = IrRepository::open(dir_a.path(), pkg(), "main").unwrap();
    let _h1 = repo_a.record_generation(&gen1()).unwrap().expect("gen1");
    let _h2 = repo_a.record_generation(&gen2()).unwrap().expect("gen2");

    let dir_b_path = dir_b.path().to_path_buf();
    let repo_b_for_hook = IrRepository::open(&dir_b_path, pkg(), "main").unwrap();
    let hook_b = Arc::new(RepoApplyHook::new(repo_b_for_hook));
    let io_b = Arc::new(FsChangeIo::for_repo(&dir_b_path));

    let io_a = Arc::new(FsChangeIo::for_repo(dir_a.path()));

    let event = merge_event(&repo_a, test_channel_ref(), None).unwrap();
    let tip_announced = event.tip.clone();

    let lookup = MemoryLookup::new();
    let sender_key = iroh::SecretKey::generate();
    let receiver_key = iroh::SecretKey::generate();
    let sender_id = sender_key.public();
    let receiver_id = receiver_key.public();

    let service = SyncService::new_with_key(
        Arc::clone(&io_b),
        Arc::clone(&hook_b),
        sender_id,
        Some(lookup.clone()),
        receiver_key,
    )
    .await
    .expect("service bind");
    lookup.add_endpoint_info(service.endpoint().addr());

    let syncer = Syncer::new_with_key(
        Arc::clone(&io_a),
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

    let ack = tokio::time::timeout(Duration::from_secs(30), syncer.on_merge(event))
        .await
        .expect("no timeout")
        .expect("on_merge ok");

    let recv_ack = tokio::time::timeout(Duration::from_secs(30), accept_task)
        .await
        .expect("no accept timeout")
        .expect("join ok")
        .expect("accept_one ok");

    assert_eq!(ack.tip, tip_announced);
    assert_eq!(recv_ack.tip, tip_announced);
    assert!(
        recv_ack.applied > 0,
        "must have applied at least one change"
    );

    let mat_a = repo_a.materialize_index().unwrap();
    let repo_b_verify = IrRepository::open(&dir_b_path, pkg(), "main").unwrap();
    let mat_b = repo_b_verify.materialize_index().unwrap();
    assert_eq!(mat_a.tip, mat_b.tip, "B's tip must match A's after sync");
    assert_eq!(mat_b.symbols.len(), mat_a.symbols.len());

    // ---------------------------------------------------------------------------
    // Tampered-change sub-test
    // ---------------------------------------------------------------------------
    let dir_b2 = tempfile::tempdir().unwrap();
    let dir_b2_path = dir_b2.path().to_path_buf();
    let repo_b2_for_hook = IrRepository::open(&dir_b2_path, pkg(), "main").unwrap();
    let hook_b2 = Arc::new(RepoApplyHook::new(repo_b2_for_hook));
    let io_b2 = Arc::new(FsChangeIo::for_repo(&dir_b2_path));

    let real_log = repo_a.log().unwrap();
    let first_hash = real_log.first().unwrap();
    let real_bytes = io_a.read(&first_hash.0.parse().unwrap()).unwrap();

    let mut tampered = real_bytes.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0xff;

    let tampered_io = Arc::new(MemChangeIo::new());
    let real_id: ChangeId = first_hash.0.parse().unwrap();
    tampered_io.insert(real_id.clone(), tampered);

    let tampered_event = MergeEvent {
        channel: test_channel_ref(),
        tip: real_id.clone(),
        new_changes: vec![real_id.clone()],
    };

    let lookup2 = MemoryLookup::new();
    let sk2 = iroh::SecretKey::generate();
    let rk2 = iroh::SecretKey::generate();
    let sid2 = sk2.public();
    let rid2 = rk2.public();

    let service2 = SyncService::new_with_key(
        Arc::clone(&io_b2),
        Arc::clone(&hook_b2),
        sid2,
        Some(lookup2.clone()),
        rk2,
    )
    .await
    .unwrap();
    lookup2.add_endpoint_info(service2.endpoint().addr());

    let syncer2 = Syncer::new_with_key(
        tampered_io,
        RemoteConfig { node_id: rid2 },
        Some(lookup2.clone()),
        sk2,
    )
    .await
    .unwrap();
    lookup2.add_endpoint_info(syncer2.endpoint().addr());

    let accept2 = tokio::spawn(async move { service2.accept_one().await });

    // Same invariant as `sync::tests::test_tampered_blob_rejected`, over a real
    // pijul repository: the receiver's rejection has to travel back as a frame.
    // Dropping this result concealed that it did not — the sender waited out
    // QUIC's ~30 s idle timer, which is the whole reason this test read as a
    // hang under a 30 s gate.
    let started2 = std::time::Instant::now();
    let send2 = tokio::time::timeout(Duration::from_secs(30), syncer2.on_merge(tampered_event))
        .await
        .expect("sender must not have to wait out a timeout to learn it was refused");
    let elapsed2 = started2.elapsed();

    assert!(
        matches!(send2, Err(SyncError::RemoteRefused(_))),
        "sender must be told the tampered push was refused, got {send2:?}"
    );
    assert!(
        elapsed2 < Duration::from_secs(10),
        "the refusal must be delivered, not discovered via QUIC's ~30s idle \
         timeout; on_merge took {elapsed2:?}"
    );

    let recv2 = tokio::time::timeout(Duration::from_secs(30), accept2)
        .await
        .expect("no timeout")
        .expect("join ok");

    assert!(
        matches!(recv2, Err(SyncError::VerificationFailed(_))),
        "tampered change must fail verification on the receiver, got {recv2:?}"
    );
    let repo_b2_verify = IrRepository::open(&dir_b2_path, pkg(), "main").unwrap();
    let mat_b2 = repo_b2_verify.materialize_index().unwrap();
    assert_eq!(
        mat_b2.symbols.len(),
        0,
        "B2 must be empty after tampered sync"
    );
}

// ---------------------------------------------------------------------------
// Test 4: channel-mismatch typed error
// ---------------------------------------------------------------------------

#[test]
fn channel_mismatch_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = IrRepository::open(dir.path(), pkg(), "main").unwrap();
    let hook = RepoApplyHook::new(repo);

    let wrong_channel = ChannelRef {
        package: SyncPackageName::new("cargo/test-pkg").unwrap(),
        channel: "other-branch".into(),
    };

    use heart::sync::ApplyHook;
    let dummy_id: ChangeId = "a".repeat(64).parse().unwrap();
    let result = hook.apply(&wrong_channel, &[dummy_id]);

    match result {
        Err(SyncError::RemoteRefused(msg)) => {
            assert!(
                msg.contains("channel mismatch"),
                "error message must mention channel mismatch, got: {msg}"
            );
        }
        other => panic!("expected RemoteRefused(channel mismatch), got: {other:?}"),
    }
}
