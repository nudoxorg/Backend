#![allow(clippy::expect_used, clippy::unwrap_used)]

use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectClaim, ArtifactPlan, ClosureId,
    ClosureManifest, FileStore, GcLimits, GcRoots, RawRelation, StoredValue, TypedObject,
    UntrustedObjectId, admit_object_envelope, write_object_envelope,
};
use backend_version::{ObjectKey, Schema};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    sync::mpsc,
    thread,
    time::Duration,
};

const OBJECT_COUNT: usize = 64;
const OBJECT_BYTES: usize = 2 * 1024;
const CHUNK_BYTES: usize = 1024;
const OBJECT_HEADER_BYTES: usize = b"LUNA_OBJECT_V1\0".len() + 32 + 1 + 2 + 1 + 32 + 32 + 8;
static NEXT_STORE: AtomicUsize = AtomicUsize::new(0);

struct BytesSchema;

impl Schema for BytesSchema {
    const DOMAIN: u8 = 0xf3;
    const TYPE: u16 = 23;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

struct TestStore {
    path: PathBuf,
    store: FileStore,
}

impl TestStore {
    fn new() -> Self {
        let ordinal = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "backend-store-artifact-{}-{ordinal}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        let store = FileStore::open(&path, 64 * 1024).expect("open artifact test store");
        Self { path, store }
    }
}

impl Drop for TestStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn object(seed: u8, length: usize) -> TypedObject {
    let bytes = (0..length)
        .map(|index| seed.wrapping_add(u8::try_from(index % 251).expect("bounded test index")))
        .collect::<Vec<_>>();
    let key = ObjectKey::<BytesSchema>::from_value(bytes.as_slice());
    TypedObject::from_value(&key, bytes.as_slice())
}

fn keyed_object(key_bytes: &[u8], payload: &[u8]) -> TypedObject {
    let key = ObjectKey::<BytesSchema>::from_value(key_bytes);
    TypedObject::from_value(&key, payload)
}

fn relation_object() -> TypedObject {
    let state = backend_version::RelationState::<RawRelation>::from_entries(
        [(
            b"relation-key".to_vec(),
            StoredValue::new(b"relation-value".to_vec(), 1, Vec::new()),
        )],
        backend_version::CoverageWitness::closed_relation(
            backend_version::ClosedRelationScope::from_scope_root(
                backend_version::ScopeRoot::from_u64(1),
            ),
        ),
    )
    .expect("build checked test relation state");
    TypedObject::from_relation_state(&state).expect("materialize checked test relation root")
}

fn sorted_objects(mut objects: Vec<TypedObject>) -> Vec<TypedObject> {
    objects.sort_by_key(|item| (item.schema(), *item.key(), *item.version()));
    objects
}

fn closure_id(objects: &[TypedObject]) -> ClosureId {
    ClosureManifest::new(sorted_objects(objects.to_vec()))
        .expect("canonical test closure")
        .id()
}

fn object_claim(object: &TypedObject) -> ArtifactObjectClaim {
    ArtifactObjectClaim::new(
        object.schema(),
        *object.key(),
        *object.version(),
        u64::try_from(object.bytes().len()).expect("test object length fits u64"),
    )
    .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
}

fn budget() -> ArtifactBudget {
    ArtifactBudget::new(
        OBJECT_COUNT * 2,
        OBJECT_COUNT,
        1024 * 1024,
        CHUNK_BYTES,
        512,
    )
}

fn object_path(root: &Path, object: &TypedObject) -> PathBuf {
    root.join("objects")
        .join(format!("{}.object", hex(object.id().as_bytes())))
}

fn closure_path(root: &Path, id: ClosureId) -> PathBuf {
    root.join("closures")
        .join(format!("{}.closure", hex(id.as_bytes())))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn put_objects(
    session: &mut backend_store::ArtifactSession,
    objects: &[TypedObject],
    replay_first_frame: bool,
) {
    for (object_index, item) in objects.iter().enumerate() {
        for (chunk_index, chunk) in item.bytes().chunks(CHUNK_BYTES).enumerate() {
            let offset = u64::try_from(chunk_index * CHUNK_BYTES).expect("bounded test offset");
            let receipt = session
                .put(object_index, offset, chunk)
                .expect("accept contiguous object frame");
            assert!(!receipt.duplicate);
            if replay_first_frame && object_index == 0 && chunk_index == 0 {
                let replay = session
                    .put(object_index, offset, chunk)
                    .expect("accept exact in-flight replay");
                assert!(replay.duplicate);
                assert_eq!(replay.bytes_accepted, 0);
            }
        }
        if replay_first_frame && object_index == 0 {
            let replay = session
                .put(object_index, 0, &item.bytes()[..CHUNK_BYTES])
                .expect("accept exact completed-object replay");
            assert!(replay.duplicate);
            assert!(replay.object_complete);
        }
    }
}

fn stored_one_object(test_store: &TestStore, seed: u8) -> (TypedObject, ClosureId) {
    let item = object(seed, 32);
    let id = closure_id(std::slice::from_ref(&item));
    let mut session = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(id),
            vec![object_claim(&item)],
            Vec::new(),
        ))
        .expect("begin one-object artifact");
    put_objects(&mut session, std::slice::from_ref(&item), false);
    let receipt = session.finish().expect("store one-object closure");
    assert_eq!(receipt.closure(), id);
    (item, id)
}

#[test]
fn received_closure_keeps_its_existing_gc_pin_through_selection() {
    let test_store = TestStore::new();
    let item = object(42, 32);
    let id = closure_id(std::slice::from_ref(&item));
    let mut session = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(id),
            vec![object_claim(&item)],
            Vec::new(),
        ))
        .expect("begin received closure");
    put_objects(&mut session, std::slice::from_ref(&item), false);
    let pinned = session
        .finish_pinned()
        .expect("finish with original GC pin");
    assert_eq!(pinned.receipt().closure(), id);

    let store = test_store.store.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let collector = thread::spawn(move || {
        started_tx.send(()).expect("signal collector start");
        done_tx
            .send(store.collect_garbage(&GcRoots::new(), GcLimits::default()))
            .expect("report collector outcome");
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("collector started");
    assert!(
        done_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "GC must wait until the unselected result is published or abandoned"
    );
    assert!(
        test_store
            .store
            .contains_object(item.id())
            .expect("member still present")
    );
    drop(pinned);
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("collector resumed")
        .expect("collector completed");
    collector.join().expect("join collector");
}

#[test]
fn envelope_writer_and_reader_share_the_canonical_store_abi() {
    let item = keyed_object(b"independent-object-key", b"canonical payload bytes");
    let mut envelope = Vec::new();
    let written =
        write_object_envelope(&item, &mut envelope, 4096).expect("write canonical envelope");
    assert_eq!(
        usize::try_from(written).expect("test envelope length"),
        envelope.len()
    );

    let admitted = admit_object_envelope(
        &mut envelope.as_slice(),
        4096,
        &backend_store::RelationAdmissionRegistry::default(),
        UntrustedObjectId::from_bytes(*item.id().as_bytes()),
    )
    .expect("admit canonical envelope");
    assert_eq!(admitted.id(), item.id());
    assert_eq!(admitted.schema(), item.schema());
    assert_eq!(admitted.key(), item.key());
    assert_eq!(admitted.version(), item.version());
    assert_eq!(
        admitted.payload_len(),
        u64::try_from(item.bytes().len()).expect("length")
    );

    let mut corrupt = envelope.clone();
    corrupt[OBJECT_HEADER_BYTES] ^= 1;
    assert!(
        admit_object_envelope(
            &mut corrupt.as_slice(),
            4096,
            &backend_store::RelationAdmissionRegistry::default(),
            UntrustedObjectId::from_bytes(*item.id().as_bytes()),
        )
        .is_err()
    );
    envelope.push(0);
    assert!(
        admit_object_envelope(
            &mut envelope.as_slice(),
            4096,
            &backend_store::RelationAdmissionRegistry::default(),
            UntrustedObjectId::from_bytes(*item.id().as_bytes()),
        )
        .is_err()
    );
}

#[test]
fn relation_envelopes_keep_their_schema_key_and_pass_registered_grammar() {
    let item = relation_object();
    let mut envelope = Vec::new();
    write_object_envelope(&item, &mut envelope, 4096).expect("write relation envelope");
    let registry = backend_store::RelationAdmissionRegistry::default();
    let admitted = admit_object_envelope(
        &mut envelope.as_slice(),
        4096,
        &registry,
        UntrustedObjectId::from_bytes(*item.id().as_bytes()),
    )
    .expect("admit canonical relation envelope");
    assert_eq!(admitted.id(), item.id());
    assert_eq!(admitted.schema(), item.schema());
    assert_eq!(admitted.key(), item.key());
    assert_eq!(admitted.version(), item.version());
}

#[test]
fn keyed_object_stream_admission_commits_and_reopens() {
    let test_store = TestStore::new();
    let item = keyed_object(b"logical-record-key", b"canonical record payload");
    let id = closure_id(std::slice::from_ref(&item));
    let mut session = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(id),
            vec![object_claim(&item)],
            Vec::new(),
        ))
        .expect("begin keyed-object stream");
    put_objects(&mut session, std::slice::from_ref(&item), false);
    let receipt = session.finish().expect("commit keyed object closure");
    assert_eq!(receipt.closure(), id);
    test_store
        .store
        .reopen_stored_closure(ArtifactClosureClaim::from_id(id), budget())
        .expect("reopen keyed object closure");
}

#[test]
fn cold_pinned_result_receipt_uses_verified_payload_not_worker_claim() {
    let test_store = TestStore::new();
    let item = keyed_object(b"recoverable-result", b"asymmetric checked payload");
    let id = closure_id(std::slice::from_ref(&item));
    let mut session = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(id),
            vec![object_claim(&item)],
            Vec::new(),
        ))
        .expect("begin result stream");
    put_objects(&mut session, std::slice::from_ref(&item), false);
    session.finish().expect("commit result closure");
    let cold = FileStore::open(&test_store.path, 64 * 1024).expect("cold reopen store");
    let reopened = cold
        .reopen_pinned_stored_closure(ArtifactClosureClaim::from_id(id), budget())
        .expect("reopen with GC pin");
    let exact_bytes = u64::try_from(item.bytes().len()).expect("bounded payload");
    assert_eq!(reopened.receipt().payload_bytes(), 0);
    assert_eq!(reopened.receipt().bytes_verified(), exact_bytes);
    assert!(reopened.admit_recovered_payload(exact_bytes + 1).is_err());
    assert!(reopened.admit_recovered_payload(0).is_err());
    let admitted = reopened
        .admit_recovered_payload(exact_bytes)
        .expect("only independently verified count is publishable");
    assert_eq!(admitted.closure(), id);
    assert_eq!(admitted.object_count(), 1);
    assert_eq!(admitted.payload_bytes(), exact_bytes);
}

#[test]
fn relation_object_stream_admission_commits_and_reopens() {
    let test_store = TestStore::new();
    let item = relation_object();
    let id = closure_id(std::slice::from_ref(&item));
    let mut session = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(id),
            vec![object_claim(&item)],
            Vec::new(),
        ))
        .expect("begin relation-object stream");
    put_objects(&mut session, std::slice::from_ref(&item), false);
    let receipt = session.finish().expect("commit relation object closure");
    assert_eq!(receipt.closure(), id);
    test_store
        .store
        .reopen_stored_closure(ArtifactClosureClaim::from_id(id), budget())
        .expect("reopen relation object closure");
}

#[test]
fn streamed_delta_reopens_cold_and_accounts_for_one_object_edit() {
    let test_store = TestStore::new();
    let initial = sorted_objects(
        (0..OBJECT_COUNT)
            .map(|seed| {
                object(
                    u8::try_from(seed).expect("bounded object seed"),
                    OBJECT_BYTES,
                )
            })
            .collect(),
    );
    let initial_id = closure_id(&initial);
    let initial_payload_bytes = initial
        .iter()
        .map(|item| u64::try_from(item.bytes().len()).expect("test object length"))
        .sum::<u64>();
    let initial_envelope_bytes = initial
        .iter()
        .map(|item| {
            u64::try_from(OBJECT_HEADER_BYTES + item.bytes().len()).expect("test envelope length")
        })
        .sum::<u64>();
    let initial_claims = initial.iter().map(object_claim).collect::<Vec<_>>();
    let sink = test_store.store.artifact_sink(budget());
    let before = sink
        .have(
            &initial
                .iter()
                .map(|item| UntrustedObjectId::from_bytes(*item.id().as_bytes()))
                .collect::<Vec<_>>(),
        )
        .expect("negotiate empty CAS");
    assert_eq!(before.present_count(), 0);

    let mut session = sink
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(initial_id),
            initial_claims,
            Vec::new(),
        ))
        .expect("begin initial closure transfer");
    assert_eq!(session.have_bitmap().present_count(), 0);
    put_objects(&mut session, &initial, true);
    let initial_receipt = session.finish().expect("finish initial closure");
    assert_eq!(initial_receipt.closure(), initial_id);
    assert_eq!(
        initial_receipt.object_count(),
        u64::try_from(OBJECT_COUNT).expect("test count fits u64")
    );
    assert_eq!(initial_receipt.payload_bytes(), initial_payload_bytes);
    assert!(initial_receipt.bytes_written() >= initial_envelope_bytes);

    let mut object_reader = test_store
        .store
        .artifact_sink(budget())
        .open_object(UntrustedObjectId::from_bytes(*initial[0].id().as_bytes()))
        .expect("open checked local object")
        .expect("object is present");
    assert_eq!(object_reader.id(), initial[0].id());
    assert_eq!(object_reader.schema(), initial[0].schema());
    assert_eq!(object_reader.key(), initial[0].key());
    assert_eq!(object_reader.version(), initial[0].version());
    assert_eq!(
        object_reader.payload_len(),
        u64::try_from(initial[0].bytes().len()).expect("test length fits u64")
    );
    let mut range = [0; 37];
    let range_len = object_reader
        .read_payload_range(211, &mut range)
        .expect("read verified payload range");
    assert_eq!(range_len, range.len());
    assert_eq!(&range, &initial[0].bytes()[211..248]);

    let present = test_store
        .store
        .artifact_sink(budget())
        .have(
            &initial
                .iter()
                .map(|item| UntrustedObjectId::from_bytes(*item.id().as_bytes()))
                .collect::<Vec<_>>(),
        )
        .expect("negotiate stored CAS objects");
    assert_eq!(present.present_count(), OBJECT_COUNT);
    let cold = test_store
        .store
        .reopen_stored_closure(ArtifactClosureClaim::from_id(initial_id), budget())
        .expect("cold-reopen stored closure");
    assert_eq!(
        cold.object_count(),
        u64::try_from(OBJECT_COUNT).expect("test count fits u64")
    );
    assert_eq!(cold.bytes_verified(), initial_payload_bytes);

    let deleted = initial[7].clone();
    let replacement = object(250, OBJECT_BYTES);
    let mut target_objects = initial
        .iter()
        .filter(|item| item.id() != deleted.id())
        .cloned()
        .collect::<Vec<_>>();
    target_objects.push(replacement.clone());
    let target_id = closure_id(&target_objects);
    let edit = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            Some(ArtifactClosureClaim::from_id(initial_id)),
            ArtifactClosureClaim::from_id(target_id),
            vec![object_claim(&replacement)],
            vec![UntrustedObjectId::from_bytes(*deleted.id().as_bytes())],
        ))
        .expect("begin one-object closure delta");
    let mut edit = edit;
    assert_eq!(edit.have_bitmap().present_count(), 0);
    put_objects(&mut edit, std::slice::from_ref(&replacement), true);
    let edit_receipt = edit.finish().expect("finish one-object delta");
    let target_payload_bytes = target_objects
        .iter()
        .map(|item| u64::try_from(item.bytes().len()).expect("test object length"))
        .sum::<u64>();
    let replacement_payload_bytes =
        u64::try_from(replacement.bytes().len()).expect("replacement length");
    assert_eq!(edit_receipt.closure(), target_id);
    assert_eq!(
        edit_receipt.object_count(),
        u64::try_from(OBJECT_COUNT).expect("test count fits u64")
    );
    assert_eq!(edit_receipt.payload_bytes(), replacement_payload_bytes);
    assert!(edit_receipt.bytes_written() >= replacement_payload_bytes);
    assert!(
        edit_receipt.bytes_written() < target_payload_bytes,
        "one-object edit wrote {} bytes for a {}-byte resulting payload closure (full object-envelope baseline: {} bytes)",
        edit_receipt.bytes_written(),
        target_payload_bytes,
        initial_envelope_bytes
    );
    let reopened = test_store
        .store
        .reopen_stored_closure(ArtifactClosureClaim::from_id(target_id), budget())
        .expect("cold-reopen edited closure");
    assert_eq!(
        reopened.object_count(),
        u64::try_from(OBJECT_COUNT).expect("test count fits u64")
    );
    let model_ids = target_objects
        .iter()
        .map(|item| *item.id().as_bytes())
        .collect::<BTreeSet<_>>();
    let persisted_ids = test_store
        .store
        .read_closure(target_id)
        .expect("read checked closure model")
        .objects()
        .iter()
        .map(|item| *item.id().as_bytes())
        .collect::<BTreeSet<_>>();
    assert_eq!(persisted_ids, model_ids);

    fs::remove_file(object_path(&test_store.path, &replacement))
        .expect("remove one reachable CAS member");
    assert!(
        test_store
            .store
            .reopen_stored_closure(ArtifactClosureClaim::from_id(target_id), budget())
            .is_err()
    );
}

#[test]
fn out_of_order_and_incomplete_streams_never_create_a_closure_receipt() {
    let test_store = TestStore::new();
    let first = object(1, 64);
    let second = object(2, 64);
    let objects = sorted_objects(vec![first.clone(), second.clone()]);
    let target = closure_id(&objects);
    let claims = objects.iter().map(object_claim).collect::<Vec<_>>();
    let mut session = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(target),
            claims,
            Vec::new(),
        ))
        .expect("begin malformed-stream test session");

    assert!(session.put(1, 0, objects[1].bytes()).is_err());
    assert!(session.finish().is_err());
    assert!(
        test_store
            .store
            .reopen_stored_closure(ArtifactClosureClaim::from_id(target), budget())
            .is_err()
    );

    let target = closure_id(std::slice::from_ref(&first));
    let mut incomplete = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(target),
            vec![object_claim(&first)],
            Vec::new(),
        ))
        .expect("begin incomplete-stream test session");
    let partial_len = first.bytes().len() / 2;
    let partial = incomplete
        .put(0, 0, &first.bytes()[..partial_len])
        .expect("accept incomplete object prefix");
    assert!(!partial.object_complete);
    assert!(incomplete.finish().is_err());
    assert!(
        test_store
            .store
            .reopen_stored_closure(ArtifactClosureClaim::from_id(target), budget())
            .is_err()
    );
}

#[test]
fn missing_member_before_finish_never_commits_a_closure_descriptor() {
    let test_store = TestStore::new();
    let item = object(71, 64);
    let id = closure_id(std::slice::from_ref(&item));
    let mut session = test_store
        .store
        .artifact_sink(budget())
        .begin(ArtifactPlan::new(
            None,
            ArtifactClosureClaim::from_id(id),
            vec![object_claim(&item)],
            Vec::new(),
        ))
        .expect("begin missing-member test session");
    put_objects(&mut session, std::slice::from_ref(&item), false);
    fs::remove_file(object_path(&test_store.path, &item)).expect("remove streamed CAS member");
    assert!(session.finish().is_err());
    assert!(!closure_path(&test_store.path, id).exists());
}

#[cfg(unix)]
#[test]
fn symlinks_and_hardlinks_in_staging_cas_and_closure_paths_fail_closed() {
    use std::os::unix::fs::symlink;

    let staging_link = TestStore::new();
    symlink(&staging_link.path, staging_link.path.join("staging"))
        .expect("plant staging-root symlink");
    assert!(
        staging_link
            .store
            .artifact_sink(budget())
            .begin(ArtifactPlan::new(
                None,
                ArtifactClosureClaim::from_bytes([7; 32]),
                Vec::new(),
                Vec::new(),
            ))
            .is_err()
    );

    let stale_stage = TestStore::new();
    let session_root = stale_stage.path.join("staging/artifacts/session-stale");
    fs::create_dir_all(&session_root).expect("create stale session fixture");
    fs::write(session_root.join("ACTIVE.lock"), b"lease").expect("create stale lease fixture");
    symlink(&stale_stage.path, session_root.join("object-0.tmp"))
        .expect("plant stale chunk symlink");
    assert!(
        stale_stage
            .store
            .artifact_sink(budget())
            .begin(ArtifactPlan::new(
                None,
                ArtifactClosureClaim::from_bytes([8; 32]),
                Vec::new(),
                Vec::new(),
            ))
            .is_err()
    );

    let stale_hardlink = TestStore::new();
    let session_root = stale_hardlink
        .path
        .join("staging/artifacts/session-hardlink");
    fs::create_dir_all(&session_root).expect("create stale hardlink session fixture");
    fs::write(session_root.join("ACTIVE.lock"), b"lease").expect("create stale lease fixture");
    fs::write(session_root.join("object-0.tmp"), b"stage").expect("create stale chunk fixture");
    fs::hard_link(
        session_root.join("object-0.tmp"),
        stale_hardlink.path.join("outside-stage-link"),
    )
    .expect("plant staging hardlink");
    assert!(
        stale_hardlink
            .store
            .artifact_sink(budget())
            .begin(ArtifactPlan::new(
                None,
                ArtifactClosureClaim::from_bytes([9; 32]),
                Vec::new(),
                Vec::new(),
            ))
            .is_err()
    );

    let object_symlink = TestStore::new();
    let symlinked_object = object(31, 32);
    symlink(
        &object_symlink.path,
        object_path(&object_symlink.path, &symlinked_object),
    )
    .expect("plant CAS object symlink");
    assert!(
        object_symlink
            .store
            .artifact_sink(budget())
            .have(&[UntrustedObjectId::from_bytes(
                *symlinked_object.id().as_bytes()
            )])
            .is_err()
    );

    let object_hardlink = TestStore::new();
    let linked_object = object(32, 32);
    object_hardlink
        .store
        .write_object(&linked_object)
        .expect("write CAS object fixture");
    fs::hard_link(
        object_path(&object_hardlink.path, &linked_object),
        object_hardlink.path.join("outside-object-link"),
    )
    .expect("plant CAS object hardlink");
    assert!(
        object_hardlink
            .store
            .artifact_sink(budget())
            .have(&[UntrustedObjectId::from_bytes(
                *linked_object.id().as_bytes()
            )])
            .is_err()
    );

    let closure_symlink = TestStore::new();
    let (_, closure_id) = stored_one_object(&closure_symlink, 41);
    let path = closure_path(&closure_symlink.path, closure_id);
    fs::remove_file(&path).expect("remove closure before symlink fixture");
    symlink(&closure_symlink.path, &path).expect("plant closure descriptor symlink");
    assert!(
        closure_symlink
            .store
            .reopen_stored_closure(ArtifactClosureClaim::from_id(closure_id), budget())
            .is_err()
    );

    let closure_hardlink = TestStore::new();
    let (_, closure_id) = stored_one_object(&closure_hardlink, 42);
    fs::hard_link(
        closure_path(&closure_hardlink.path, closure_id),
        closure_hardlink.path.join("outside-closure-link"),
    )
    .expect("plant closure descriptor hardlink");
    assert!(
        closure_hardlink
            .store
            .reopen_stored_closure(ArtifactClosureClaim::from_id(closure_id), budget())
            .is_err()
    );
}
