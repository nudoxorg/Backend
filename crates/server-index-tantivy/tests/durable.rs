//! Adversarial durable segment projection checks.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::large_stack_arrays,
    reason = "fixed adversarial fixtures fail fast and use bounded scratch arrays"
)]

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use compiler_ir::EntityId;
use heart_identity::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};
use server_index_core::{
    EntityArtifactIdentity, EntityDocumentId, IndexSnapshot, LexicalManifest, LexicalOperation,
    LexicalRow, LexicalScore, LexicalSegment, LexicalSnapshotHit, LexicalTopK,
};
use server_index_tantivy::{TantivySegmentStore, TantivySegmentStoreError};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "nudox-tantivy-durable-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).expect("unique test root");
    path
}

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(b"durable-test"),
        ),
        entity: EntityId::new(entity),
    }
}

fn snapshot(ids: &[server_index_core::LexicalSegmentId]) -> IndexSnapshot<'_> {
    IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"durable-generation"),
        &[],
        ids,
    )
    .expect("snapshot")
}

#[test]
fn project_reopen_and_reuse_roundtrip() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let rows = [LexicalRow::new(
        b"alpha",
        document(1),
        LexicalScore::from(7),
    )];
    let segment = LexicalSegment::new(&rows).expect("segment");
    let opened = store.project(segment).expect("project");
    let reopened = store.reopen(segment.id).expect("reopen");
    assert_eq!(opened.id(), reopened.id());
    let selection = [segment.id];
    let opened_segments = [&reopened];
    let pinned = store
        .compose(snapshot(&selection), &opened_segments)
        .expect("compose");
    let mut output = [None; 1];
    let mut candidates = [None; 256];
    assert_eq!(
        pinned
            .search(
                LexicalOperation::new(b"alpha"),
                1,
                &mut output,
                &mut candidates
            )
            .expect("search"),
        1
    );
    let hit = output[0].expect("hit");
    assert_eq!(hit.provenance().snapshot(), pinned.snapshot());
    assert_eq!(hit.provenance().segment(), segment.id);
    assert_eq!(hit.provenance().document(), document(1));
    assert_eq!(hit.term(), b"alpha");
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn temporary_selection_slice_can_yield_a_longer_lived_segment_hit() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let rows = [LexicalRow::new(
        b"alpha",
        document(13),
        LexicalScore::from(1),
    )];
    let segment = LexicalSegment::new(&rows).expect("segment");
    let opened = store.project(segment).expect("project");
    let hit = {
        let selection = [segment.id];
        let opened_segments = [&opened];
        let pinned = store
            .compose(snapshot(&selection), &opened_segments)
            .expect("compose");
        let mut output = [None];
        let mut candidates = [None];
        assert_eq!(
            pinned
                .search(
                    LexicalOperation::new(b"alpha"),
                    1,
                    &mut output,
                    &mut candidates,
                )
                .expect("search"),
            1
        );
        output[0]
    };
    assert_eq!(hit.expect("hit").term(), b"alpha");
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn corrupt_final_projection_is_rebuilt_and_incomplete_is_not_authority() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let rows = [LexicalRow::new(
        b"alpha",
        document(2),
        LexicalScore::from(1),
    )];
    let segment = LexicalSegment::new(&rows).expect("segment");
    let _ = store.project(segment).expect("project");
    let directory = fs::read_dir(path.join("ntvx-v3"))
        .expect("root")
        .filter_map(Result::ok)
        .find(|entry| entry.path().is_dir())
        .expect("segment directory")
        .path();
    fs::write(directory.join("rows.sidecar"), b"incomplete").expect("corrupt sidecar");
    let rebuilt = store.project(segment).expect("rebuild");
    assert_eq!(rebuilt.id(), segment.id);
    drop(rebuilt);
    fs::remove_dir_all(directory.join("tantivy")).expect("backend corruption");
    assert_eq!(
        store.project(segment).expect("backend rebuild").id(),
        segment.id
    );
    fs::remove_file(directory.join("recipe")).expect("recipe file");
    fs::create_dir(directory.join("recipe")).expect("recipe directory");
    assert_eq!(
        store.project(segment).expect("recipe rebuild").id(),
        segment.id
    );
    let final_path = directory;
    fs::remove_dir_all(&final_path).expect("remove rebuilt projection");
    fs::write(&final_path, b"invalid final identity").expect("invalid final file");
    assert_eq!(
        store.project(segment).expect("file identity rebuild").id(),
        segment.id
    );
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn newest_tombstone_shadows_old_key_and_same_document_terms_survive() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let old_rows = [
        LexicalRow::new(b"alpha", document(3), LexicalScore::from(9)),
        LexicalRow::new(b"beta", document(3), LexicalScore::from(8)),
    ];
    let new_rows = [LexicalRow::tombstone(b"alpha", document(3))];
    let old = LexicalSegment::new(&old_rows).expect("old");
    let newest = LexicalSegment::new(&new_rows).expect("newest");
    let old_open = store.project(old).expect("old projection");
    let newest_open = store.project(newest).expect("new projection");
    let ids = [newest.id, old.id];
    let opened_segments = [&newest_open, &old_open];
    let pinned = store
        .compose(snapshot(&ids), &opened_segments)
        .expect("compose");
    let mut output = [None; 2];
    let operations = [
        LexicalOperation::new(b"alpha"),
        LexicalOperation::new(b"beta"),
    ];
    let mut scratch = [None; 4];
    let mut candidates = [None; 512];
    assert_eq!(
        pinned
            .search_terms(&operations, 2, &mut output, &mut scratch, &mut candidates)
            .expect("search"),
        1
    );
    assert_eq!(output[0].expect("beta").term(), b"beta");
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn durable_union_matches_core_manifest_for_overlapping_terms_and_tombstones() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let old_rows = [
        LexicalRow::new(b"alpha", document(14), LexicalScore::from(9)),
        LexicalRow::new(b"alpine", document(14), LexicalScore::from(8)),
        LexicalRow::new(b"beta", document(15), LexicalScore::from(7)),
    ];
    let newest_rows = [LexicalRow::tombstone(b"alpha", document(14))];
    let old = LexicalSegment::new(&old_rows).expect("old");
    let newest = LexicalSegment::new(&newest_rows).expect("newest");
    let ids = [newest.id, old.id];
    let core_segments = [newest, old];
    let core_manifest =
        LexicalManifest::new(snapshot(&ids), &core_segments, &[]).expect("manifest");
    let old_open = store.project(old).expect("old projection");
    let newest_open = store.project(newest).expect("newest projection");
    let opened_segments = [&newest_open, &old_open];
    let pinned = store
        .compose(snapshot(&ids), &opened_segments)
        .expect("compose");

    let top_k = LexicalTopK::new(2).expect("top-k");
    let mut core_seen = [None; 4];
    let mut core_output =
        [LexicalSnapshotHit::new(ids[0], b"", document(0), LexicalScore::from(0)); 2];
    {
        let prefix_terminal = core_manifest
            .execute(
                LexicalOperation::prefix(b"al"),
                top_k,
                &mut core_seen,
                &mut core_output,
            )
            .expect("core prefix");
        let prefix_hits = match prefix_terminal {
            server_index_core::LexicalTerminal::Complete { hits, .. } => hits,
            _ => &[],
        };
        assert!(
            prefix_hits.is_empty(),
            "newest document tombstone wins by document"
        );
    }

    let exact_terminal = core_manifest
        .execute(
            LexicalOperation::new(b"beta"),
            top_k,
            &mut core_seen,
            &mut core_output,
        )
        .expect("core exact");
    let exact_hits = match exact_terminal {
        server_index_core::LexicalTerminal::Complete { hits, .. } => hits,
        _ => &[],
    };
    assert_eq!(exact_hits.len(), 1);
    let mut output = [None; 2];
    let mut scratch = [None; 4];
    let mut candidates = [None; 256];
    assert_eq!(
        pinned
            .search_terms(
                &[
                    LexicalOperation::prefix(b"al"),
                    LexicalOperation::new(b"beta")
                ],
                2,
                &mut output,
                &mut scratch,
                &mut candidates,
            )
            .expect("durable union"),
        1
    );
    let durable_hit = output[0].expect("beta hit");
    assert_eq!(durable_hit.provenance().document(), exact_hits[0].document);
    assert_eq!(durable_hit.term(), exact_hits[0].term);
    assert_eq!(durable_hit.score(), exact_hits[0].score);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn prefix_selector_returns_multiple_backend_ordinals_and_byte_grammar() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let rows = [
        LexicalRow::new(b"alpha", document(7), LexicalScore::from(3)),
        LexicalRow::new(b"alpine", document(8), LexicalScore::from(2)),
        LexicalRow::new(b"beta", document(9), LexicalScore::from(1)),
    ];
    let segment = LexicalSegment::new(&rows).expect("segment");
    let opened = store.project(segment).expect("project");
    let ids = [segment.id];
    let opened_segments = [&opened];
    let pinned = store
        .compose(snapshot(&ids), &opened_segments)
        .expect("compose");
    let mut output = [None; 2];
    let mut scratch = [None; 4];
    let mut candidates = [None; 256];
    let written = pinned
        .search_terms(
            &[LexicalOperation::prefix(b"al")],
            2,
            &mut output,
            &mut scratch,
            &mut candidates,
        )
        .expect("prefix");
    assert_eq!(written, 2);
    assert_eq!(output[0].expect("alpha").term(), b"alpha");
    assert_eq!(output[1].expect("alpine").term(), b"alpine");
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn byte_exact_and_empty_prefix_preserve_core_terms() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let rows = [
        LexicalRow::new(&[0x00], document(9), LexicalScore::from(3)),
        LexicalRow::new(&[0xff], document(10), LexicalScore::from(2)),
    ];
    let segment = LexicalSegment::new(&rows).expect("segment");
    let opened = store.project(segment).expect("project");
    let ids = [segment.id];
    let opened_segments = [&opened];
    let pinned = store
        .compose(snapshot(&ids), &opened_segments)
        .expect("compose");
    let mut output = [None; 2];
    let mut candidates = [None; 256];
    assert_eq!(
        pinned
            .search(
                LexicalOperation::new(&[0xff]),
                1,
                &mut output,
                &mut candidates
            )
            .expect("exact"),
        1
    );
    assert_eq!(output[0].expect("byte").term(), &[0xff]);
    assert_eq!(
        pinned
            .search(LexicalOperation::new(&[]), 1, &mut output, &mut candidates)
            .expect("empty exact"),
        0
    );
    let mut scratch = [None; 4];
    assert_eq!(
        pinned
            .search_terms(
                &[LexicalOperation::prefix(&[])],
                2,
                &mut output,
                &mut scratch,
                &mut candidates
            )
            .expect("empty prefix"),
        2
    );
    assert_eq!(
        pinned
            .search(
                LexicalOperation::prefix(b"a?*[]"),
                1,
                &mut output,
                &mut candidates
            )
            .expect("punctuation prefix"),
        0
    );
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn encoded_term_bound_is_checked_before_tantivy_writer() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let admitted_term = Box::leak(vec![0x11; 32_764].into_boxed_slice());
    let admitted_rows = [LexicalRow::new(
        admitted_term,
        document(11),
        LexicalScore::from(1),
    )];
    let admitted = LexicalSegment::new(&admitted_rows).expect("admitted segment");
    assert!(store.project(admitted).is_ok());
    let rejected_term = Box::leak(vec![0x11; 32_765].into_boxed_slice());
    let rejected_rows = [LexicalRow::new(
        rejected_term,
        document(12),
        LexicalScore::from(1),
    )];
    let rejected = LexicalSegment::new(&rejected_rows).expect("rejected segment");
    assert!(matches!(
        store.project(rejected),
        Err(TantivySegmentStoreError::TermTooLong {
            raw_bytes: 32_765,
            ..
        })
    ));
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn wrong_snapshot_selection_is_typed() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let rows = [LexicalRow::new(
        b"alpha",
        document(4),
        LexicalScore::from(1),
    )];
    let segment = LexicalSegment::new(&rows).expect("segment");
    let opened = store.project(segment).expect("project");
    let other_rows = [LexicalRow::new(
        b"other",
        document(4),
        LexicalScore::from(1),
    )];
    let other = LexicalSegment::new(&other_rows).expect("other");
    let other_ids = [other.id];
    let opened_segments = [&opened];
    let result = store.compose(snapshot(&other_ids), &opened_segments);
    assert!(matches!(
        result,
        Err(TantivySegmentStoreError::SnapshotSegmentOrder { .. })
    ));
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn concurrent_same_identity_projects_reopen_without_clobbering() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let rows = Box::leak(Box::new([LexicalRow::new(
        b"shared",
        document(5),
        LexicalScore::from(3),
    )]));
    let segment = LexicalSegment::new(rows).expect("segment");
    let _ = store.project(segment).expect("seed projection");
    let recipe_root = path.join("ntvx-v3");
    let final_path = fs::read_dir(&recipe_root)
        .expect("recipe root")
        .filter_map(Result::ok)
        .find(|entry| entry.path().is_dir())
        .expect("seed projection directory")
        .path();
    fs::write(final_path.join("rows.sidecar"), b"racing corruption")
        .expect("corrupt seed projection");
    let mut workers = Vec::new();
    for _ in 0..4 {
        let store = store.clone();
        workers.push(std::thread::spawn(move || {
            store.project(segment).map(|opened| opened.id())
        }));
    }
    for worker in workers {
        assert_eq!(
            worker.join().expect("worker").expect("projection"),
            segment.id
        );
    }
    assert!(store.reopen(segment.id).is_ok());
    assert!(
        fs::read_dir(recipe_root)
            .expect("recipe root after race")
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
    );
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn abandoned_temporary_directory_is_not_reopenable_authority() {
    let path = root();
    let store = TantivySegmentStore::open(&path).expect("store");
    let rows = [LexicalRow::new(
        b"abandoned",
        document(6),
        LexicalScore::from(3),
    )];
    let segment = LexicalSegment::new(&rows).expect("segment");
    let temporary = path.join("ntvx-v3").join(".prepublish.tmp");
    fs::create_dir(&temporary).expect("temporary");
    fs::write(temporary.join("canonical.rows"), b"partial").expect("partial");
    assert!(matches!(
        store.reopen(segment.id),
        Err(TantivySegmentStoreError::Missing { .. })
    ));
    fs::remove_dir_all(path).expect("cleanup");
}
