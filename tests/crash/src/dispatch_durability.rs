//! Independent crash-boundary checks for dispatch retention capabilities.
//!
//! The daemon currently keeps its pending remote table in memory.  The tests
//! here exercise the durable capability that is already available underneath
//! that table: a transfer checkpoint is a typed, fenced object, and its GC
//! roots remain live until the owner settles the transfer.  Keeping this in a
//! separate crate makes the check an independent consumer of the wire API.
#![allow(clippy::expect_used, clippy::too_many_lines)]

use backend_replication::{
    AdmittedAuthority, AuthorityClaim, AuthorityEpoch, AuthorityExpectation, ChunkChain,
    ChunkParts, Fence, Frame, GcRoot as TransferGcRoot, ObjectRequest, OwnerId, RevocationVersion,
    Transfer, TransferId, TransferLease, TransportLimits,
};
use backend_store::{FileStore, GcLimits, GcRoot as StoreGcRoot, GcRoots, TypedObject};
use backend_version::{ObjectKey, ObjectVersion, Schema};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
struct Bytes;

impl Schema for Bytes {
    const DOMAIN: u8 = 0x9d;
    const TYPE: u16 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

fn limits() -> TransportLimits {
    TransportLimits {
        max_chunk: 3,
        max_object: 64,
        ..TransportLimits::default()
    }
}

fn transfer_fixture() -> (
    ObjectRequest<Bytes>,
    AdmittedAuthority,
    ObjectKey<Bytes>,
    ObjectVersion<Bytes>,
    OwnerId<Bytes>,
) {
    let key = ObjectKey::<Bytes>::from_value(b"payload".as_slice());
    let version = ObjectVersion::<Bytes>::from_value(b"abcdef".as_slice());
    let authority_key = ObjectKey::<Bytes>::from_value(b"authority".as_slice());
    let authority = AuthorityClaim::from_typed(&authority_key, AuthorityEpoch(3));
    let admitted_authority =
        AuthorityExpectation::from_typed(&authority_key, AuthorityEpoch(3), RevocationVersion(9))
            .admit_capability(authority, RevocationVersion(9))
            .expect("authority admission");
    let owner = OwnerId::from_typed(&key);
    let request = ObjectRequest::whole(
        TransferId::new(41).expect("transfer id"),
        key,
        version,
        6,
        limits().max_ranges,
    )
    .expect("whole request");
    (request, admitted_authority, key, version, owner)
}

fn admitted_frame(
    request: &ObjectRequest<Bytes>,
    authority: AuthorityClaim,
    sequence: u64,
    offset: u64,
    previous_chain: ChunkChain,
    bytes: &[u8],
) -> backend_replication::AdmittedChunk<Bytes> {
    Frame::new(
        request.transfer,
        request.key,
        request.version,
        ChunkParts {
            object_len: request.len,
            offset,
            sequence,
            previous_chain,
            payload: bytes.to_vec(),
        },
        authority,
    )
    .expect("frame")
    .admit(limits())
    .expect("admit frame")
}

fn temporary_store(label: &str) -> (FileStore, PathBuf) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "backend-dispatch-durability-{label}-{}-{stamp}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    let store = FileStore::open(&path, 64 * 1024).expect("store");
    (store, path)
}

#[test]
fn leased_remote_checkpoint_survives_wire_restart_and_expires_at_fence() {
    let (request, admitted_authority, key, version, owner) = transfer_fixture();
    let authority = admitted_authority.claim();
    let mut transfer =
        Transfer::with_limits(request.clone(), authority, limits()).expect("transfer");
    let first = admitted_frame(&request, authority, 0, 0, ChunkChain([0; 32]), b"abc");
    let second = admitted_frame(&request, authority, 1, 3, first.chain(), b"def");
    transfer.stage(first).expect("first chunk");
    transfer.stage(second).expect("second chunk");

    let fence = Fence::new(17).expect("fence");
    let lease = TransferLease::new(
        request.transfer,
        owner,
        admitted_authority,
        RevocationVersion(9),
        fence,
        100,
        200,
    )
    .expect("lease");
    let root = TransferGcRoot::new(&lease, key, version);
    let leased = transfer
        .checkpoint_leased(lease.clone(), vec![root], limits())
        .expect("leased checkpoint");

    // The wire value is the durable restart boundary.  Re-admission requires
    // the caller-owned request, owner, root, authority, revocation, fence,
    // and monotonic clock; none of those are inferred from opaque bytes.
    let wire = leased.into_wire().expect("wire checkpoint");
    let revoked = wire
        .clone()
        .admit_against(
            &request,
            owner,
            &[root],
            admitted_authority,
            RevocationVersion(8),
            fence,
            150,
            limits(),
        )
        .expect_err("a restart below the lease revocation fence must fail");
    assert_eq!(
        revoked,
        backend_replication::ReplicationError::RevokedAuthority
    );
    let resumed = wire
        .admit_against(
            &request,
            owner,
            &[root],
            admitted_authority,
            RevocationVersion(9),
            fence,
            150,
            limits(),
        )
        .expect("restart admission");
    assert_eq!(resumed.lease().token(), lease.token());
    assert_eq!(resumed.roots(), &[root]);

    let expired = resumed
        .lease()
        .validate_at(
            owner,
            request.transfer,
            admitted_authority,
            fence,
            RevocationVersion(9),
            200,
        )
        .expect_err("expired remote lease must be fenced");
    assert_eq!(expired, backend_replication::ReplicationError::StaleFence);
}

#[test]
fn transfer_pin_retains_object_until_terminal_release_then_gc_reclaims_it() {
    let (store, path) = temporary_store("pin-release");
    let key = ObjectKey::<Bytes>::from_value(b"catalog-output".as_slice());
    let live = TypedObject::from_value(&key, b"live output".as_slice());
    let orphan = TypedObject::from_value(&key, b"orphan output".as_slice());
    let live_id = store.write_object(&live).expect("live object");
    let orphan_id = store.write_object(&orphan).expect("orphan object");

    // A pending transfer's durable lease is represented in the GC root set by
    // the daemon/replication owner.  The collector receives only the checked
    // root, so it cannot accidentally inspect product payloads and drop it.
    let mut transfer_roots = GcRoots::new();
    transfer_roots.add_transfer_lease(StoreGcRoot::Object(live_id));
    let report = store
        .collect_garbage(&transfer_roots, GcLimits::default())
        .expect("collect with live transfer pin");
    assert_eq!(report.swept_items, 1);
    assert!(store.contains_object(live_id).expect("live after pin"));
    assert!(
        !store
            .contains_object(orphan_id)
            .expect("orphan after sweep")
    );

    // Reopen the physical owner before settling the transfer. The checked
    // root remains sufficient to protect the object across an owner restart.
    drop(store);
    let store = FileStore::open(&path, 64 * 1024).expect("reopen store with pending pin");
    store
        .collect_garbage(&transfer_roots, GcLimits::default())
        .expect("collect after restart with live transfer pin");
    assert!(store.contains_object(live_id).expect("live after restart"));

    // Terminal completion/cancellation removes the transfer root.  A later
    // collection may now reclaim the object, proving the pin has a finite
    // lifetime rather than becoming a permanent retention leak.
    drop(transfer_roots);
    store
        .collect_garbage(&GcRoots::new(), GcLimits::default())
        .expect("collect after terminal release");
    assert!(!store.contains_object(live_id).expect("live released"));
    drop(store);
    fs::remove_dir_all(path).expect("cleanup");
}
