#![allow(
    clippy::expect_used,
    clippy::format_collect,
    clippy::suspicious_open_options
)]

use super::super::super::ObjectId;
use super::super::super::{ClosureManifest, LayoutId, OrderedMap, StoredValue};
use super::index::{MarkIndex, QueueIndex};
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
