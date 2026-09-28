#![allow(
    clippy::expect_used,
    clippy::format_collect,
    clippy::suspicious_open_options
)]

use super::super::super::{ArtifactClosureClaim, ClosureId, ObjectId, PackId};
use super::super::super::{ClosureManifest, LayoutId, OrderedMap, StoredValue};
use super::index::{MarkIndex, QueueIndex};
use super::roots::QueueItem;
use super::*;
use backend_version::{ObjectKey, Schema};
use std::io::Write;
use std::path::{Path, PathBuf};

struct TestSchema;

impl Schema for TestSchema {
    const DOMAIN: u8 = 0xf1;
    const TYPE: u16 = 1;
    type Value = u64;

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }
}

fn temp_store(label: &str) -> (FileStore, PathBuf) {
    let path =
        std::env::temp_dir().join(format!("backend-store-gc-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    let store = FileStore::open(&path, 64 * 1024).expect("open test store");
    (store, path)
}

fn object(value: u64) -> super::super::super::TypedObject {
    let key = ObjectKey::<TestSchema>::from_value(&value);
    super::super::super::TypedObject::from_value(&key, &value)
}

fn map(value: u8) -> OrderedMap {
    OrderedMap::try_from_iter([(
        b"key".to_vec(),
        StoredValue::new(vec![value], 1, Vec::new()),
    )])
    .expect("map")
}

fn physical_pack_path(root: &Path, id: super::super::super::PackId) -> PathBuf {
    let stem = id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let tree = root.join("packs").join(format!("{stem}.tree"));
    if tree.is_file() {
        tree
    } else {
        root.join("packs").join(format!("{stem}.pack"))
    }
}

fn physical_closure_path(root: &Path, id: super::super::super::ClosureId) -> PathBuf {
    let stem = id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    root.join("closures").join(format!("{stem}.closure"))
}

#[test]
fn orphan_objects_are_reclaimed_and_roots_are_retained() {
    let (store, path) = temp_store("objects");
    let live = object(1);
    let orphan = object(2);
    store.write_object(&live).expect("write live");
    store.write_object(&orphan).expect("write orphan");
    let mut roots = GcRoots::new();
    roots.add(GcRoot::Object(live.id()));
    let report = store
        .collect_garbage(&roots, GcLimits::default())
        .expect("collect");
    assert_eq!(report.swept_items, 1);
    assert!(store.contains_object(live.id()).expect("live metadata"));
    assert!(!store.contains_object(orphan.id()).expect("orphan metadata"));
    let _ = fs::remove_dir_all(path);
}

#[test]
fn missing_reachable_object_fails_closed_before_sweep() {
    let (store, path) = temp_store("missing");
    let orphan = object(3);
    store.write_object(&orphan).expect("write orphan");
    let mut roots = GcRoots::new();
    roots.add(GcRoot::Object(ObjectId::from_bytes([9; 32])));
    assert!(matches!(
        store.collect_garbage(&roots, GcLimits::default()),
        Err(StoreError::Corrupt)
    ));
    assert!(store.contains_object(orphan.id()).expect("orphan retained"));
    let _ = fs::remove_dir_all(path);
}

#[test]
fn remote_residency_requires_scoped_verifier_and_keeps_unlisted_members() {
    let (store, path) = temp_store("remote-residency-allowlist");
    let remote = object(101);
    let local = object(102);
    let mut members = vec![remote.clone(), local.clone()];
    members.sort_by_key(|value| (value.schema(), *value.key(), *value.version()));
    let manifest = ClosureManifest::new(members).expect("closure manifest");
    let closure = store.write_closure(&manifest).expect("write closure");

    // A failed owner-side receipt/fallback proof never produces durable GC
    // roots, so no member can be reclaimed by a raw closure or ID claim.
    assert!(matches!(
        store.collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_members(closure, [remote.id()], |_, _| {
                    Err(StoreError::Corrupt)
                })?;
                Ok(())
            },
            GcLimits::default(),
        ),
        Err(StoreError::Corrupt)
    ));
    assert!(
        store
            .contains_object(remote.id())
            .expect("remote still local")
    );
    assert!(
        store
            .contains_object(local.id())
            .expect("local still local")
    );

    // Even a successful verifier can authorize only listed members. The
    // checked closure index and every unlisted object remain locally usable.
    store
        .collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_members(closure, [remote.id()], |checked, ids| {
                    assert_eq!(checked, closure);
                    assert_eq!(ids, &[remote.id()]);
                    Ok(())
                })?;
                Ok(())
            },
            GcLimits::default(),
        )
        .expect("collect with scoped verified remote member");
    assert!(!store.contains_object(remote.id()).expect("evicted member"));
    assert!(store.contains_object(local.id()).expect("retained member"));
    assert!(physical_closure_path(&path, closure).is_file());
    let index = store
        .open_closure_claim(ArtifactClosureClaim::from_id(closure))
        .expect("open metadata-only closure index");
    assert_eq!(index.object_count(), 2);
    assert!(
        index
            .contains_object_id(remote.id())
            .expect("remote member index")
    );
    assert!(
        index
            .contains_object_id(local.id())
            .expect("local member index")
    );
    let page = index.page_ids(None, 8).expect("read IDs without payloads");
    assert_eq!(page.object_ids().len(), 2);

    // Restart proves the physical payload may be absent while the durable
    // closure membership index remains sufficient for cold S3 hydration.
    drop(index);
    drop(store);
    let reopened = FileStore::open(&path, 64 * 1024).expect("cold reopen");
    assert!(
        !reopened
            .contains_object(remote.id())
            .expect("remote still absent")
    );
    assert!(
        reopened
            .contains_object(local.id())
            .expect("local still present")
    );
    let cold_index = reopened
        .open_closure_claim(ArtifactClosureClaim::from_id(closure))
        .expect("cold metadata-only index");
    assert!(
        cold_index
            .contains_object_id(remote.id())
            .expect("cold membership")
    );
    assert!(
        cold_index
            .contains_object_id(local.id())
            .expect("cold local membership")
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn remote_allowlist_rejects_nonmembers_and_empty_allowlist_keeps_every_payload() {
    let (store, path) = temp_store("remote-residency-empty");
    let member = object(103);
    let manifest = ClosureManifest::new(vec![member.clone()]).expect("manifest");
    let closure = store.write_closure(&manifest).expect("write closure");

    let invalid = ObjectId::from_bytes([0x99; 32]);
    assert!(matches!(
        store.collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_members(closure, [invalid], |_, _| {
                    panic!("receipt verifier must not see a nonmember")
                })?;
                Ok(())
            },
            GcLimits::default(),
        ),
        Err(StoreError::Corrupt)
    ));
    assert!(matches!(
        store.collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_member_claims(
                    closure,
                    [crate::UntrustedObjectId::from_bytes([0x99; 32])],
                    |_, _| panic!("receipt verifier must not see an untrusted nonmember"),
                )?;
                Ok(())
            },
            GcLimits::default(),
        ),
        Err(StoreError::Corrupt)
    ));

    store
        .collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_members(closure, [], |checked, ids| {
                    assert_eq!(checked, closure);
                    assert!(ids.is_empty());
                    Ok(())
                })?;
                Ok(())
            },
            GcLimits::default(),
        )
        .expect("metadata-only remote receipt roots no leaf for eviction");
    assert!(
        store
            .contains_object(member.id())
            .expect("empty allowlist retains payload")
    );
    store
        .collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_member_claims(
                    closure,
                    [crate::UntrustedObjectId::from_bytes(
                        *member.id().as_bytes(),
                    )],
                    |checked, ids| {
                        assert_eq!(checked, closure);
                        assert_eq!(ids, &[member.id()]);
                        Ok(())
                    },
                )?;
                Ok(())
            },
            GcLimits::default(),
        )
        .expect("promote only checked member claim");
    assert!(
        !store
            .contains_object(member.id())
            .expect("claimed member evicted")
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn verified_remote_member_reachable_from_selected_pack_stays_local() {
    let (store, path) = temp_store("remote-selected-head");
    store
        .publish(&map(3), LayoutId::derive(b"remote-selected-head"))
        .expect("publish selected map");
    let selected = store
        .head()
        .expect("selected head read")
        .expect("head exists");
    let pack = selected.descriptor().pack();
    let closure = selected.descriptor().closure();
    let manifest = store.open_closure(closure).expect("open selected index");
    let remote_member = manifest
        .page_ids(None, 8)
        .expect("read selected member IDs")
        .object_ids()[0];
    let selected_tree_root = store
        .read_relation_ref(
            backend_version::SchemaIdentity::of_relation::<super::super::super::RawRelation>(),
            &selected.descriptor().target(),
        )
        .expect("selected pack root relation reference");
    assert_eq!(remote_member, selected_tree_root);
    assert!(
        store
            .contains_object(remote_member)
            .expect("selected member initially local")
    );

    store
        .collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_members(
                    closure,
                    [remote_member],
                    |checked, members| {
                        assert_eq!(checked, closure);
                        assert_eq!(members, &[remote_member]);
                        Ok(())
                    },
                )?;
                Ok(())
            },
            GcLimits::default(),
        )
        .expect("collect selected remote-backed member");

    // This selected map's tree root is itself a closure member. The selected
    // pack reaches it through the root relation reference, so the ordinary
    // object walk must retain it despite its closure-page allowlist entry.
    assert!(
        store
            .contains_object(remote_member)
            .expect("reachable member retained")
    );
    assert!(physical_pack_path(&path, pack).is_file());
    assert!(physical_closure_path(&path, closure).is_file());
    let reopened = store
        .open_closure(closure)
        .expect("selected index retained");
    assert!(
        reopened
            .contains_object_id(remote_member)
            .expect("membership remains authenticated")
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn unreferenced_allowlisted_member_is_reclaimed_across_cold_gc_while_selected_pack_survives() {
    let (mut store, path) = temp_store("remote-unreferenced-selected-pack");
    store
        .publish(
            &map(4),
            LayoutId::derive(b"remote-unreferenced-selected-pack"),
        )
        .expect("publish unrelated selected map");
    let selected = store
        .head()
        .expect("selected head read")
        .expect("head exists");
    let selected_pack = selected.descriptor().pack();
    let selected_closure = selected.descriptor().closure();

    // This closure member has no edge from the selected pack or another local
    // object. The owner verifier is the test fixture's durable receipt and
    // complete fallback attestation for precisely this ID.
    let remote = object(141);
    let local = object(142);
    let manifest = ClosureManifest::new(vec![remote.clone(), local.clone()])
        .expect("independent remote closure");
    let remote_closure = store
        .write_closure(&manifest)
        .expect("write remote closure");

    for pass in 0..2 {
        store
            .collect_garbage_resolving_roots(
                |roots| {
                    roots.add_remote_closure_members(
                        remote_closure,
                        [remote.id()],
                        |checked, ids| {
                            assert_eq!(checked, remote_closure);
                            assert_eq!(ids, &[remote.id()]);
                            Ok(())
                        },
                    )?;
                    Ok(())
                },
                GcLimits::default(),
            )
            .expect("collect verified remote member");
        assert!(
            !store
                .contains_object(remote.id())
                .expect("remote object absent")
        );
        assert!(
            store
                .contains_object(local.id())
                .expect("local object retained")
        );
        assert!(physical_pack_path(&path, selected_pack).is_file());
        assert!(physical_closure_path(&path, selected_closure).is_file());
        assert!(physical_closure_path(&path, remote_closure).is_file());

        if pass == 0 {
            // Reopening between collections exercises the persisted index
            // rather than a warm manifest handle or previous in-memory queue.
            drop(store);
            store = FileStore::open(&path, 64 * 1024).expect("cold reopen store");
            assert!(
                store
                    .open_closure_claim(ArtifactClosureClaim::from_id(remote_closure))
                    .expect("metadata-only closure reopen")
                    .contains_object_id(remote.id())
                    .expect("remote identity remains a member")
            );
            assert!(
                !store
                    .contains_object(remote.id())
                    .expect("remote still absent after cold reopen")
            );
            assert!(
                store
                    .contains_object(local.id())
                    .expect("local retained after reopen")
            );
        }
    }
    let _ = fs::remove_dir_all(path);
}

#[test]
fn remote_allowlist_keeps_member_referenced_by_local_relation_after_cold_gc() {
    let (mut store, path) = temp_store("remote-transitive-local-edge");
    let remote = object(151);
    let remote_manifest = ClosureManifest::new(vec![remote.clone()]).expect("remote manifest");
    let remote_closure = store
        .write_closure(&remote_manifest)
        .expect("write remote closure");
    let local_relation = relation_with_value_reference(b"local-edge", remote.id());
    let local_manifest =
        ClosureManifest::new(vec![local_relation.clone()]).expect("local relation manifest");
    let local_closure = store
        .write_closure(&local_manifest)
        .expect("write relation closure with local cross-closure target");

    for pass in 0..2 {
        store
            .collect_garbage_resolving_roots(
                |roots| {
                    roots.add(GcRoot::Closure(local_closure));
                    roots.add_remote_closure_members(
                        remote_closure,
                        [remote.id()],
                        |checked, ids| {
                            assert_eq!(checked, remote_closure);
                            assert_eq!(ids, &[remote.id()]);
                            Ok(())
                        },
                    )?;
                    Ok(())
                },
                GcLimits::default(),
            )
            .expect("relation edge keeps remotely listed target local");
        assert!(
            store
                .contains_object(remote.id())
                .expect("remote member retained through cross-closure relation edge")
        );
        assert!(
            store
                .contains_object(local_relation.id())
                .expect("relation retained")
        );
        assert!(store.read_closure_index(local_closure).is_ok());
        assert!(store.read_closure_index(remote_closure).is_ok());
        if pass == 0 {
            drop(store);
            store = FileStore::open(&path, 64 * 1024).expect("cold reopen relation closures");
        }
    }
    let _ = fs::remove_dir_all(path);
}

#[test]
fn cold_gc_fails_closed_for_missing_remote_relation_target_and_nonmember_claim() {
    let (mut store, path) = temp_store("remote-transitive-missing-target");
    let remote = object(161);
    let remote_manifest = ClosureManifest::new(vec![remote.clone()]).expect("remote manifest");
    let remote_closure = store
        .write_closure(&remote_manifest)
        .expect("write remote closure");

    store
        .collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_members(
                    remote_closure,
                    [remote.id()],
                    |checked, ids| {
                        assert_eq!(checked, remote_closure);
                        assert_eq!(ids, &[remote.id()]);
                        Ok(())
                    },
                )?;
                Ok(())
            },
            GcLimits::default(),
        )
        .expect("offload an unreferenced member");
    assert!(
        !store
            .contains_object(remote.id())
            .expect("remote was evicted")
    );
    drop(store);
    store = FileStore::open(&path, 64 * 1024).expect("cold reopen after remote eviction");

    // A new relation object may exist independently, but a closure cannot be
    // durably sealed over a missing external target. This prevents future
    // selected roots from creating a local-to-remote edge that generic store
    // reads and GC cannot hydrate.
    // Use the exact member that the prior collection evicted. This models an
    // older/hostile local relation A -> B after B has become remote-only.
    let dangling = relation_with_value_reference(b"dangling", remote.id());
    let dangling_manifest =
        ClosureManifest::new(vec![dangling.clone()]).expect("well-formed relation object manifest");
    assert!(matches!(
        store.write_closure(&dangling_manifest),
        Err(StoreError::Corrupt)
    ));
    assert!(matches!(
        store.write_object(&dangling),
        Err(StoreError::Corrupt)
    ));

    // Model a legacy/hostile CAS that already contains A even though B was
    // evicted. Its valid physical envelope does not make the dangling graph
    // edge safe: after cold reopen, GC must reject the local edge and leave
    // both A and all unrelated files untouched.
    let mut encoded = Vec::new();
    crate::write_object_envelope(&dangling, &mut encoded, 64 * 1024)
        .expect("encode canonical relation envelope");
    fs::write(store.object_path(dangling.id()), encoded).expect("inject legacy relation object");
    drop(store);
    store = FileStore::open(&path, 64 * 1024).expect("cold reopen injected relation object");
    let orphan = object(162);
    store.write_object(&orphan).expect("store unrelated orphan");
    let mut tampered_remote_id = *remote.id().as_bytes();
    tampered_remote_id[0] ^= 0x80;
    let wrong_remote_claim = crate::UntrustedObjectId::from_bytes(tampered_remote_id);
    assert!(matches!(
        store.collect_garbage_resolving_roots(
            |roots| {
                roots.add_remote_closure_member_claims(
                    remote_closure,
                    [wrong_remote_claim],
                    |_, _| panic!("a nonmember claim must not reach receipt verification"),
                )?;
                Ok(())
            },
            GcLimits::default(),
        ),
        Err(StoreError::Corrupt)
    ));
    assert!(
        store
            .contains_object(orphan.id())
            .expect("no sweep on rejection")
    );

    assert!(matches!(
        store.collect_garbage_resolving_roots(
            |roots| {
                roots.add(GcRoot::Object(dangling.id()));
                roots.add_remote_closure_members(
                    remote_closure,
                    [remote.id()],
                    |checked, ids| {
                        assert_eq!(checked, remote_closure);
                        assert_eq!(ids, &[remote.id()]);
                        Ok(())
                    },
                )?;
                Ok(())
            },
            GcLimits::default(),
        ),
        Err(StoreError::Corrupt)
    ));
    assert!(
        store
            .contains_object(dangling.id())
            .expect("A survives failed mark")
    );
    assert!(
        store
            .contains_object(orphan.id())
            .expect("no sweep after missing edge")
    );
    assert!(store.read_closure_index(remote_closure).is_ok());
    let _ = fs::remove_dir_all(path);
}

fn relation_with_value_reference(key: &[u8], target: ObjectId) -> super::super::super::TypedObject {
    let state = backend_version::RelationState::<super::super::super::RawRelation>::from_entries(
        [(
            key.to_vec(),
            super::super::super::StoredValue::new(Vec::new(), 1, vec![*target.as_bytes()]),
        )],
        backend_version::CoverageWitness::Partial(backend_version::partial_coverage(1)),
    )
    .expect("build checked relation with external edge");
    super::super::super::TypedObject::from_relation_state(&state)
        .expect("materialize checked relation")
}

#[test]
fn remote_gc_queue_records_roundtrip_and_reject_malformed_tags() {
    let closure = ClosureId::from_bytes([0x51; 32]);
    let object = ObjectId::from_bytes([0x62; 32]);
    let index = QueueItem::RemoteClosureIndex(closure);
    assert_eq!(QueueItem::decode(&index.encode()), Ok(index));
    let remote_member = QueueItem::RemoteMember { closure, object };
    assert_eq!(
        QueueItem::decode(&remote_member.encode()),
        Ok(remote_member)
    );
    let continuation = QueueItem::RemoteManifestPage {
        closure,
        after: object,
    };
    assert_eq!(QueueItem::decode(&continuation.encode()), Ok(continuation));
    let selected = QueueItem::RemoteHead {
        pack: PackId::from_wire([0x73; 32]),
        closure,
    };
    assert_eq!(QueueItem::decode(&selected.encode()), Ok(selected));
    let mut malformed_index = index.encode();
    malformed_index[33] = 1;
    assert_eq!(
        QueueItem::decode(&malformed_index),
        Err(StoreError::Corrupt)
    );
    let mut malformed = remote_member.encode();
    malformed[0] = u8::MAX;
    assert_eq!(QueueItem::decode(&malformed), Err(StoreError::Corrupt));
}

#[test]
fn root_snapshot_rejects_overflow_without_retaining_unbounded_inputs() {
    let mut roots = GcRoots::new();
    for value in 0..=MAX_ROOTS {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&(value as u64).to_be_bytes());
        roots.add(GcRoot::Object(ObjectId::from_bytes(id)));
    }
    assert!(matches!(
        roots.normalized_items(GcLimits::default().max_roots),
        Err(StoreError::Bounds)
    ));
}

#[test]
fn interrupted_quarantine_is_restored_on_reopen() {
    let (store, path) = temp_store("crash");
    let orphan = object(4);
    store.write_object(&orphan).expect("write orphan");
    set_test_fault(1);
    let result = store.collect_garbage(&GcRoots::new(), GcLimits::default());
    assert!(result.is_err());
    drop(store);
    let reopened = FileStore::open(&path, 64 * 1024).expect("reopen");
    assert!(
        reopened
            .contains_object(orphan.id())
            .expect("restored orphan")
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn malformed_mark_metadata_is_discarded_without_touching_objects() {
    let (store, path) = temp_store("metadata");
    let orphan = object(7);
    store.write_object(&orphan).expect("write orphan");
    let gc = path.join("gc");
    fs::create_dir_all(&gc).expect("create gc");
    fs::write(gc.join("state"), b"partial-state").expect("write malformed state");
    fs::write(gc.join("mark"), [1u8, 2, 3]).expect("write malformed mark");
    drop(store);
    let reopened = FileStore::open(&path, 64 * 1024).expect("reopen");
    assert!(!gc.join("state").exists());
    assert!(
        reopened
            .contains_object(orphan.id())
            .expect("orphan metadata")
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn unknown_files_are_retained() {
    let (store, path) = temp_store("unknown");
    let path = path.join("objects").join("future-format.object.tmp");
    fs::write(&path, b"opaque").expect("write unknown");
    store
        .collect_garbage(&GcRoots::new(), GcLimits::default())
        .expect("collect unknown");
    assert!(path.is_file());
    let _ = fs::remove_dir_all(
        path.parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("/")),
    );
}

#[test]
fn selected_head_collection_reclaims_old_generation() {
    let (store, path) = temp_store("generations");
    let _first_root = store
        .publish(&map(1), LayoutId::derive(b"first"))
        .expect("publish first");
    let first = store.head().expect("first head").expect("first selected");
    store
        .publish(&map(2), LayoutId::derive(b"second"))
        .expect("publish second");
    let second = store.head().expect("second head").expect("second selected");
    let report = store
        .collect_garbage_from_head(GcRoots::new(), GcLimits::default())
        .expect("collect generations");
    assert!(report.swept_items >= 1);
    assert!(!physical_pack_path(&path, first.descriptor().pack()).exists());
    assert!(physical_pack_path(&path, second.descriptor().pack()).exists());
    assert!(!physical_closure_path(&path, first.descriptor().closure()).exists());
    let _ = fs::remove_dir_all(path);
}

#[test]
fn live_reader_pin_retains_object_until_released() {
    let (store, path) = temp_store("reader-pin");
    let pinned = object(5);
    let orphan = object(6);
    store.write_object(&pinned).expect("write pinned");
    store.write_object(&orphan).expect("write orphan");
    let mut roots = GcRoots::new();
    roots.add_reader(GcRoot::Object(pinned.id()));
    store
        .collect_garbage(&roots, GcLimits::default())
        .expect("collect pinned");
    assert!(store.contains_object(pinned.id()).expect("pinned metadata"));
    assert!(!store.contains_object(orphan.id()).expect("orphan metadata"));
    let _ = fs::remove_dir_all(path);
}

#[test]
fn bucketed_sweep_reclaims_all_candidates_with_tiny_pages() {
    let (store, path) = temp_store("bucketed-pages");
    let live = object(10_000);
    store.write_object(&live).expect("write live");
    let mut orphan_ids = Vec::new();
    for value in 10_001..10_065 {
        let orphan = object(value);
        orphan_ids.push(orphan.id());
        store.write_object(&orphan).expect("write orphan");
    }
    let mut roots = GcRoots::new();
    roots.add(GcRoot::Object(live.id()));
    let limits = GcLimits {
        sweep_page_items: 1,
        ..GcLimits::default()
    };
    let report = store
        .collect_garbage(&roots, limits)
        .expect("collect all buckets");
    assert_eq!(report.swept_items, orphan_ids.len() as u64);
    assert_eq!(report.examined_items, (orphan_ids.len() + 1) as u64);
    assert!(store.contains_object(live.id()).expect("live metadata"));
    for id in orphan_ids {
        assert!(!store.contains_object(id).expect("orphan metadata"));
    }
    let _ = fs::remove_dir_all(path);
}

#[test]
fn all_live_candidates_are_charged_and_page_bounded() {
    let (store, path) = temp_store("all-live-pages");
    let mut roots = GcRoots::new();
    let count = 73u64;
    for value in 20_000..20_000 + count {
        let live = object(value);
        store.write_object(&live).expect("write live");
        roots.add_reader(GcRoot::Object(live.id()));
    }
    let report = store
        .collect_garbage(
            &roots,
            GcLimits {
                sweep_page_items: 1,
                ..GcLimits::default()
            },
        )
        .expect("collect all-live candidates");
    assert_eq!(report.swept_items, 0);
    assert_eq!(report.examined_items, count);
    let _ = fs::remove_dir_all(path);
}

#[test]
fn mark_freeze_external_merges_duplicates_with_bounded_records() {
    let (_store, path) = temp_store("mark-freeze");
    let directory = path.join("gc");
    fs::create_dir_all(&directory).expect("create gc directory");
    let mark = directory.join("mark");
    let unique = 5_003u64;
    let total = 12_345u64;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&mark)
        .expect("open mark log");
    for value in 0..total {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&(value % unique).to_be_bytes());
        file.write_all(&state::MarkKey::Object(hash).encode())
            .expect("write mark");
    }
    file.sync_all().expect("sync mark");
    let digest =
        state_io::freeze_mark_index(&mark, GcLimits::default(), &directory).expect("freeze mark");
    assert_ne!(digest, [0; 32]);
    assert_eq!(
        fs::metadata(&mark).expect("mark metadata").len(),
        unique * u64::try_from(GC_MARK_RECORD_BYTES).expect("record width")
    );
    let present = state::MarkKey::Object({
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&42u64.to_be_bytes());
        hash
    });
    let absent = state::MarkKey::Object({
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&unique.to_be_bytes());
        hash
    });
    assert!(state_io::mark_contains_sorted(&mark, present).expect("present mark"));
    assert!(!state_io::mark_contains_sorted(&mark, absent).expect("absent mark"));
    let mut index = MarkIndex::load(&mark, GcLimits::default()).expect("index");
    assert!(index.contains(present).expect("index present"));
    assert!(!index.contains(absent).expect("index absent"));
    let _ = fs::remove_dir_all(path);
}

#[test]
fn mark_freeze_rename_faults_restart_without_losing_objects() {
    for fault in [5, 6] {
        let (store, path) = temp_store(&format!("mark-freeze-fault-{fault}"));
        let live = object(80 + u64::from(fault));
        let orphan = object(90 + u64::from(fault));
        store.write_object(&live).expect("write live");
        store.write_object(&orphan).expect("write orphan");
        let mut roots = GcRoots::new();
        roots.add(GcRoot::Object(live.id()));
        set_test_fault(fault);
        assert!(store.collect_garbage(&roots, GcLimits::default()).is_err());
        assert!(store.contains_object(live.id()).expect("live after fault"));
        let report = store
            .collect_garbage(&roots, GcLimits::default())
            .expect("restart collection");
        assert!(report.marked_items >= 1);
        assert!(
            store
                .contains_object(live.id())
                .expect("live after restart")
        );
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn manifest_mark_continuation_bounds_one_page_per_work_item() {
    let (store, path) = temp_store("manifest-continuation");
    let mut objects = (0..600u64).map(object).collect::<Vec<_>>();
    objects.sort_by_key(|value| (value.schema(), *value.key(), *value.version()));
    let manifest = ClosureManifest::new(objects).expect("manifest");
    let closure = store.write_closure(&manifest).expect("write closure");
    let mut roots = GcRoots::new();
    roots.add(GcRoot::Closure(closure));
    let limits = GcLimits {
        mark_page_items: 1,
        mark_page_bytes: 64 * 1024,
        ..GcLimits::default()
    };
    let items = roots
        .normalized_items(limits.max_roots)
        .expect("normalized roots");
    let mut state = store.open_or_start_gc(&items, limits).expect("start gc");
    let paths = GcPaths::new(store.root());
    let mut mark_index = MarkIndex::load(&paths.mark, limits).expect("mark index");
    let mut queue_index = QueueIndex::load(&paths.queue, limits).expect("queue index");
    store
        .mark_page(&mut state, limits, &mut mark_index, &mut queue_index)
        .expect("one mark page");
    assert_eq!(state.phase, Phase::Mark);
    assert_eq!(
        state.queue_offset,
        u64::try_from(GC_QUEUE_RECORD_BYTES).expect("queue width")
    );
    let queue = fs::read(&paths.queue).expect("queue log");
    assert!(
        queue
            .chunks_exact(GC_QUEUE_RECORD_BYTES)
            .any(|record| record[0] == 5)
    );
    let _ = fs::remove_dir_all(path);
}

#[test]
fn relation_reference_continuation_is_paged_with_mark_credits() {
    let (store, path) = temp_store("relation-reference-continuation");
    let mut objects = (0..600u64).map(object).collect::<Vec<_>>();
    objects.sort_by_key(|value| (value.schema(), *value.key(), *value.version()));
    let manifest = ClosureManifest::new(objects).expect("manifest");
    let closure = store.write_closure(&manifest).expect("write closure");
    let root_id = store
        .open_closure(closure)
        .expect("open closure")
        .page(None, 1)
        .expect("root page")
        .node_ids()[0];
    let limits = GcLimits {
        mark_page_items: 1,
        mark_page_bytes: 64 * 1024,
        ..GcLimits::default()
    };
    let mut roots = GcRoots::new();
    roots.add(GcRoot::Object(root_id));
    let items = roots
        .normalized_items(limits.max_roots)
        .expect("normalized roots");
    let mut state = store.open_or_start_gc(&items, limits).expect("start gc");
    let paths = GcPaths::new(store.root());
    let mut mark_index = MarkIndex::load(&paths.mark, limits).expect("mark index");
    let mut queue_index = QueueIndex::load(&paths.queue, limits).expect("queue index");

    let mut found = false;
    for _ in 0..8 {
        store
            .mark_page(&mut state, limits, &mut mark_index, &mut queue_index)
            .expect("bounded mark page");
        let queue = fs::read(&paths.queue).expect("queue log");
        if queue
            .chunks_exact(GC_QUEUE_RECORD_BYTES)
            .any(|record| record[0] == 6)
        {
            found = true;
            break;
        }
    }
    let _ = fs::remove_dir_all(path);
    assert!(
        found,
        "dense relation node did not emit a bounded continuation"
    );
}

#[test]
fn flat_compatibility_closure_is_rejected_by_production_mark_walk() {
    let (store, path) = temp_store("flat-closure");
    let retained = object(60_000);
    let orphan = object(60_001);
    let manifest = ClosureManifest::new(vec![retained.clone()]).expect("manifest");
    let closure = store
        .write_closure(&manifest)
        .expect("write compact closure");
    store.write_object(&orphan).expect("write orphan");
    let flat = manifest.encode(64 * 1024).expect("flat export");
    fs::write(store.closure_path(closure), flat).expect("replace with flat compatibility file");

    let mut roots = GcRoots::new();
    roots.add(GcRoot::Closure(closure));
    assert!(matches!(
        store.collect_garbage(&roots, GcLimits::default()),
        Err(StoreError::Corrupt)
    ));
    assert!(store.contains_object(orphan.id()).expect("orphan retained"));
    let _ = fs::remove_dir_all(path);
}

#[test]
fn adversarial_filename_order_is_frozen_once_and_counted() {
    let (store, path) = temp_store("adversarial-order");
    let count = 41u8;
    for value in (0..count).rev() {
        let mut id = [0; 32];
        id[0] = 0xa7;
        id[1] = value;
        let stem = id
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        fs::write(
            path.join("objects").join(format!("{stem}.object")),
            b"opaque orphan candidate",
        )
        .expect("write adversarial candidate");
    }
    let report = store
        .collect_garbage(
            &GcRoots::new(),
            GcLimits {
                sweep_page_items: 1,
                ..GcLimits::default()
            },
        )
        .expect("collect adversarial candidates");
    assert_eq!(report.swept_items, u64::from(count));
    assert_eq!(report.examined_items, u64::from(count));
    let _ = fs::remove_dir_all(path);
}

#[test]
fn candidate_index_write_crash_before_or_after_rename_recovers() {
    for point in [2, 3] {
        let label = if point == 2 {
            "candidate-index-before"
        } else {
            "candidate-index-after"
        };
        let (store, path) = temp_store(label);
        let live = object(30_000 + u64::from(point));
        store.write_object(&live).expect("write live");
        let mut roots = GcRoots::new();
        roots.add(GcRoot::Object(live.id()));
        let limits = GcLimits {
            mark_page_items: 1,
            sweep_page_items: 1,
            ..GcLimits::default()
        };
        set_test_fault(point);
        assert!(store.collect_garbage(&roots, limits).is_err());
        drop(store);
        let reopened = FileStore::open(&path, 64 * 1024).expect("reopen after index fault");
        let report = reopened
            .collect_garbage(&roots, limits)
            .expect("resume after index fault");
        assert_eq!(report.swept_items, 0);
        assert_eq!(report.examined_items, 1);
        assert!(reopened.contains_object(live.id()).expect("live metadata"));
        let _ = fs::remove_dir_all(path);
    }
}

#[test]
fn committed_sweep_cursor_resumes_without_revisiting_live_candidates() {
    let (store, path) = temp_store("candidate-cursor-restart");
    let count = 9u64;
    let mut roots = GcRoots::new();
    for value in 31_000..31_000 + count {
        let live = object(value);
        store.write_object(&live).expect("write live");
        roots.add(GcRoot::Object(live.id()));
    }
    let limits = GcLimits {
        mark_page_items: 1,
        sweep_page_items: 1,
        ..GcLimits::default()
    };
    set_test_fault(4);
    assert!(store.collect_garbage(&roots, limits).is_err());
    drop(store);
    let reopened = FileStore::open(&path, 64 * 1024).expect("reopen after cursor commit");
    let report = reopened
        .collect_garbage(&roots, limits)
        .expect("resume committed cursor");
    assert_eq!(report.swept_items, 0);
    assert_eq!(report.examined_items, count);
    let _ = fs::remove_dir_all(path);
}
