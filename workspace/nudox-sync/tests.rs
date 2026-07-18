//! Integration tests for nudox-sync.
//!
//! Test plan:
//! 1. `fs_change_io_path_compat` — record a generation in a temp repo;
//!    `FsChangeIo::read_change` can read the bytes; `verify` passes;
//!    tampered bytes fail verify.
//! 2. `apply_external_changes_cross_repo` — two on-disk repos; copy change
//!    files via FsChangeIo read/write; apply on B; B materialize == A;
//!    re-apply is idempotent (no error, tip unchanged).
//! 3. `full_iroh_sync_loop` — full in-process iroh loop: repo A records 2
//!    generations → merge_event → Syncer::on_merge → SyncService with B's
//!    FsChangeIo + RepoApplyHook → B's materialized index == A's, tip matches
//!    ack. Plus tampered-change scenario → VerificationFailed, B unchanged.
//! 4. `channel_mismatch_error` — RepoApplyHook returns channel-mismatch error
//!    when ChannelRef.channel != repo's working channel.

use std::sync::Arc;

use nudox_change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use nudox_ir::apply::PristineIntroTable;
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::symbol::Visibility;
use nudox_ir::wire::{
    EntryPayloadFlags, FunctionWire, KindWire, OwnedEntryPayload, SymbolWire,
};

use ir_sync::io::{ApplyHook, ChangeIo, MemChangeIo};
use ir_sync::transport::{RemoteConfig, SyncService, Syncer};
use ir_sync::{ChangeId, ChannelRef, PackageName as SyncPackageName};
use iroh::address_lookup::MemoryLookup;
use tokio::time::Duration;

use nudox_ir_vcs::repo::IrRepository;

use crate::{merge_event, FsChangeIo, RepoApplyHook};

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

/// First IR generation: one function symbol.
fn gen1() -> PristineIntroTable {
    let mut ir = PristineIntroTable::new();
    ir.insert_live(intro(1), function_payload("alpha"), None);
    ir
}

/// Second IR generation: two function symbols.
fn gen2() -> PristineIntroTable {
    let mut ir = PristineIntroTable::new();
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

/// Record a generation in a temp on-disk repo, then verify that
/// `FsChangeIo::read_change` can read the change bytes, `verify` passes, and
/// tampered bytes fail verify.
#[test]
fn fs_change_io_path_compat() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let repo = IrRepository::open(&root, pkg(), "main").unwrap();
    let hash_hex = repo.record_generation(&gen1()).unwrap().expect("must produce a change");

    let io = FsChangeIo::for_repo(&root);

    let change_id: ChangeId = hash_hex.0.parse().unwrap();

    // FsChangeIo can read the bytes written by libpijul.
    let bytes = io.read_change(&change_id).expect("read_change must succeed");
    assert!(!bytes.is_empty(), "change bytes must not be empty");

    // Full pijul verify passes on genuine bytes.
    io.verify(&change_id, &bytes).expect("verify must pass for genuine bytes");

    // Tampered bytes fail verify.
    let mut bad = bytes.clone();
    // Flip a byte in the middle of the content (far past the header).
    let mid = bad.len() / 2;
    bad[mid] ^= 0xff;
    let err = io.verify(&change_id, &bad);
    assert!(err.is_err(), "verify must fail for tampered bytes");
}

// ---------------------------------------------------------------------------
// Test 2: apply_external_changes cross-repo
// ---------------------------------------------------------------------------

/// Copy change files from repo A to repo B via FsChangeIo read/write; apply on
/// B; B's materialized index equals A's; re-apply is idempotent.
#[test]
fn apply_external_changes_cross_repo() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    // Repo A: record 2 generations.
    let repo_a = IrRepository::open(dir_a.path(), pkg(), "main").unwrap();
    let hash1 = repo_a.record_generation(&gen1()).unwrap().expect("gen1 change");
    let hash2 = repo_a.record_generation(&gen2()).unwrap().expect("gen2 change");

    // Repo B: open (empty).
    let repo_b = IrRepository::open(dir_b.path(), pkg(), "main").unwrap();

    // Copy change files from A → B via FsChangeIo.
    let io_a = FsChangeIo::for_repo(dir_a.path());
    let io_b = FsChangeIo::for_repo(dir_b.path());

    for hex in [&hash1, &hash2] {
        let id: ChangeId = hex.0.parse().unwrap();
        let bytes = io_a.read_change(&id).unwrap();
        // write_change verifies then writes atomically.
        io_b.write_change(&id, &bytes).unwrap();
        assert!(io_b.has_change(&id).unwrap(), "change must be present after write");
    }

    // Apply both changes on B.
    let tip = repo_b
        .apply_external_changes(&[hash1.clone(), hash2.clone()])
        .expect("apply_external_changes must succeed");

    // B's materialized index must equal A's.
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

    // Re-apply is idempotent: no error, tip unchanged.
    let tip2 = repo_b
        .apply_external_changes(&[hash1, hash2])
        .expect("re-apply must succeed (idempotent)");
    assert_eq!(tip.fingerprint(), tip2.fingerprint(), "re-apply must not change the tip");
}

// ---------------------------------------------------------------------------
// Test 3: full iroh sync loop
// ---------------------------------------------------------------------------

/// Full in-process iroh loop: A records 2 generations → merge_event →
/// Syncer::on_merge → SyncService on B's FsChangeIo + RepoApplyHook → B's
/// materialized index equals A's, tip matches ack.
///
/// Also tests the tampered-change scenario: a corrupted change served by A
/// causes VerificationFailed on B, which leaves B unchanged.
#[tokio::test(flavor = "multi_thread")]
async fn full_iroh_sync_loop() {

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    // Repo A: record 2 generations.
    let repo_a = IrRepository::open(dir_a.path(), pkg(), "main").unwrap();
    let _h1 = repo_a.record_generation(&gen1()).unwrap().expect("gen1");
    let _h2 = repo_a.record_generation(&gen2()).unwrap().expect("gen2");

    // Repo B: open (empty). RepoApplyHook takes ownership; we keep the root
    // path to open a second handle for post-sync verification.
    let dir_b_path = dir_b.path().to_path_buf();
    let repo_b_for_hook = IrRepository::open(&dir_b_path, pkg(), "main").unwrap();
    let hook_b = Arc::new(RepoApplyHook::new(repo_b_for_hook));
    let io_b = Arc::new(FsChangeIo::for_repo(&dir_b_path));

    // Sender (A) uses FsChangeIo backed by A's changestore.
    let io_a = Arc::new(FsChangeIo::for_repo(dir_a.path()));

    // Build MergeEvent from A's full log.
    let event = merge_event(&repo_a, test_channel_ref(), None).unwrap();
    let tip_announced = event.tip.clone();

    // Set up in-process iroh pair.
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
        RemoteConfig { node_id: receiver_id },
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

    // Ack tip must equal the announced tip.
    assert_eq!(ack.tip, tip_announced);
    assert_eq!(recv_ack.tip, tip_announced);
    assert!(recv_ack.applied > 0, "must have applied at least one change");

    // B's materialized index must equal A's. Open a fresh handle to B's on-disk
    // repo (the hook owns the other handle inside its Mutex).
    let mat_a = repo_a.materialize_index().unwrap();
    let repo_b_verify = IrRepository::open(&dir_b_path, pkg(), "main").unwrap();
    let mat_b = repo_b_verify.materialize_index().unwrap();
    assert_eq!(mat_a.tip, mat_b.tip, "B's tip must match A's after sync");
    assert_eq!(mat_b.symbols.len(), mat_a.symbols.len());

    // ---------------------------------------------------------------------------
    // Tampered-change sub-test: a MemChangeIo wrapper that corrupts one blob.
    //
    // We use MemChangeIo (from ir-sync) as the sender's change store for this
    // sub-test: pre-populate it with the real bytes for a real change id, but
    // tamper one of the bytes before inserting. Because FsChangeIo::verify calls
    // Change::check_from_buffer (full pijul verification), the receiver must
    // reject the tampered bytes with VerificationFailed.
    // ---------------------------------------------------------------------------
    let dir_b2 = tempfile::tempdir().unwrap();
    let dir_b2_path = dir_b2.path().to_path_buf();
    let repo_b2_for_hook = IrRepository::open(&dir_b2_path, pkg(), "main").unwrap();
    let hook_b2 = Arc::new(RepoApplyHook::new(repo_b2_for_hook));
    let io_b2 = Arc::new(FsChangeIo::for_repo(&dir_b2_path));

    // Get a real change from A.
    let real_log = repo_a.log().unwrap();
    let first_hash = real_log.first().unwrap();
    let real_bytes = io_a.read_change(&first_hash.0.parse().unwrap()).unwrap();

    // Tamper the bytes (flip a byte in the middle).
    let mut tampered = real_bytes.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0xff;

    // Build a MemChangeIo that serves tampered bytes under the real change id.
    let tampered_io = Arc::new(MemChangeIo::new());
    let real_id: ChangeId = first_hash.0.parse().unwrap();
    tampered_io.insert(real_id.clone(), tampered);

    // Wrap in an event with just the first (tampered) change.
    let tampered_event = ir_sync::MergeEvent {
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
    let _send2 =
        tokio::time::timeout(Duration::from_secs(30), syncer2.on_merge(tampered_event)).await;

    let recv2 = tokio::time::timeout(Duration::from_secs(30), accept2)
        .await
        .expect("no timeout")
        .expect("join ok");

    assert!(
        recv2.is_err(),
        "tampered change must cause an error on the receiver"
    );
    // B2 must be unchanged: no changes applied, materialize returns empty.
    let repo_b2_verify = IrRepository::open(&dir_b2_path, pkg(), "main").unwrap();
    let mat_b2 = repo_b2_verify.materialize_index().unwrap();
    assert_eq!(mat_b2.symbols.len(), 0, "B2 must be empty after tampered sync");
}

// ---------------------------------------------------------------------------
// Test 4: channel-mismatch typed error
// ---------------------------------------------------------------------------

/// `RepoApplyHook::apply` returns a channel-mismatch error when the
/// `ChannelRef.channel` does not match the repo's working channel.
#[test]
fn channel_mismatch_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = IrRepository::open(dir.path(), pkg(), "main").unwrap();
    let hook = RepoApplyHook::new(repo);

    // Repo is on "main"; sync event targets "other-branch".
    let wrong_channel = ChannelRef {
        package: SyncPackageName::new("cargo/test-pkg").unwrap(),
        channel: "other-branch".into(),
    };

    // Use a dummy ChangeId (valid hex format).
    let dummy_id: ChangeId = "a".repeat(64).parse().unwrap();
    let result = hook.apply(&wrong_channel, &[dummy_id]);

    match result {
        Err(ir_sync::SyncError::RemoteRefused(msg)) => {
            assert!(
                msg.contains("channel mismatch"),
                "error message must mention channel mismatch, got: {msg}"
            );
        }
        other => panic!("expected RemoteRefused(channel mismatch), got: {other:?}"),
    }
}
