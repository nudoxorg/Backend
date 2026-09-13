use super::*;
use crate::builtin::BuiltinIntent;
use backend_engine::{Cursor, CursorEvent};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn workspace_view_producer_rejects_forged_context_and_evidence() {
    let head = super::super::genesis().expect("checked builtin genesis");
    let snapshot = head.snapshot();
    let producer = backend_engine::WorkspaceViewProducerAdmission::from_snapshot(
        &snapshot,
        backend_engine::object_version(super::super::VIEW_SOURCE_VALUE),
    );
    let admitted = producer.admit().expect("admitted workspace producer");
    let forged = backend_engine::UntrustedProducerObservation::new(
        admitted.producer_identity(),
        admitted.scope_root(),
        [0; 32],
        vec![0],
    );
    assert!(backend_engine::admit_producer_observation(forged, &producer).is_err());
}

#[test]
fn roundtrip_retains_nonempty_view_and_exact_owner_binding() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-locald-view-{stamp}.journal"));
    let head = super::super::genesis().expect("checked builtin genesis");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (base, _) = super::super::initial_view().expect("checked initial view");
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("journal::roundtrip")),
        base.basis(),
        "journal::roundtrip",
    );
    let prepared = base
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("prepare journal row");
    let (view, _) = base.commit(prepared).expect("commit journal row");
    let cursor = Cursor::for_view_root(&view);
    let mut journal = ViewJournal::open(&path).expect("open view journal");
    journal
        .persist(head.root(), &view, cursor, None)
        .expect("persist selected view");
    let recovered = journal
        .load_for_workspace(head.root(), &capability)
        .expect("load selected view")
        .expect("view snapshot");
    assert_eq!(recovered.cursor, cursor);
    assert_eq!(recovered.view.root(), view.root());
    assert!(recovered.view.row_count() > 0);
    let _ = fs::remove_file(path);
}

#[test]
fn workspace_head_change_starts_with_a_snapshot_before_any_view_event() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-locald-view-rollover-{stamp}.journal"));
    let old_head = super::super::genesis().expect("checked builtin genesis");
    let label = "fixture:next-workspace";
    let intent = BuiltinIntent::add(backend_engine::package_key(label), label).expect("intent");
    let relation = super::super::workspace_relation(Some(&intent)).expect("source relation");
    let semantic = super::super::semantic_relation().expect("semantic relation");
    let new_workspace = super::super::workspace_manifest(&relation, &semantic)
        .expect("new manifest")
        .root();
    assert_ne!(old_head.root(), new_workspace);

    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (base, cursor) = super::super::initial_view().expect("initial view");
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("rollover::row")),
        base.basis(),
        "rollover::row",
    );
    let prepared = base
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("prepare rollover");
    let (target, delta) = base.clone().commit(prepared).expect("commit rollover");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let mut journal = ViewJournal::open(&path).expect("open view journal");
    journal
        .persist(old_head.root(), &base, cursor, None)
        .expect("persist old snapshot");
    journal
        .persist(
            new_workspace,
            &target,
            Cursor::for_view_root(&target),
            Some(&event),
        )
        .expect("persist new workspace view");
    let recovered = journal
        .load_for_workspace(new_workspace, &capability)
        .expect("load new workspace view")
        .expect("new workspace snapshot");
    assert_eq!(recovered.view, target);
    assert!(recovered.events.is_empty());

    // Reopening must preserve the rollover boundary. The old workspace head
    // is a cache miss, while the new head recovers the compact snapshot and
    // never attempts to replay the event that preceded it.
    drop(journal);
    let reopened = ViewJournal::open(&path).expect("reopen rollover journal");
    assert!(
        reopened
            .load_for_workspace(old_head.root(), &capability)
            .expect("load old workspace after reopen")
            .is_none()
    );
    let recovered = reopened
        .load_for_workspace(new_workspace, &capability)
        .expect("load new workspace after reopen")
        .expect("new workspace snapshot after reopen");
    assert_eq!(recovered.view, target);
    assert!(recovered.events.is_empty());
    let _ = fs::remove_file(path);
}

#[test]
fn compact_event_record_does_not_grow_with_the_visible_view() {
    let (base, base_cursor) = super::super::initial_view().expect("initial view");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let first_row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("compact::first")),
        base.basis(),
        "compact::first",
    )
    .with_document(Vec::<backend_engine::Fragment>::new());
    let prepared = base
        .prepare(
            backend_engine::ViewDelta::Upsert { row: first_row },
            capability.clone(),
        )
        .expect("first delta");
    let (mut large, first_delta) = base.commit(prepared).expect("first commit");
    let first_event = CursorEvent::View {
        delta: Box::new(first_delta),
    };
    let first_payload = encode_envelope(
        super::super::genesis().expect("head").root(),
        Cursor::for_view_root(&large),
        &large,
        Some(&first_event),
    )
    .expect("first compact envelope");

    for index in 0..256 {
        let row = backend_engine::Row::new(
            backend_engine::RowId::Symbol(backend_engine::symbol_key(&format!(
                "compact::{index:04}"
            ))),
            large.basis(),
            format!("compact::{index:04}"),
        )
        .with_document(Vec::<backend_engine::Fragment>::new());
        let prepared = large
            .prepare(
                backend_engine::ViewDelta::Upsert { row },
                capability.clone(),
            )
            .expect("growth delta");
        let (next, _) = large.commit(prepared).expect("growth commit");
        large = next;
    }
    let final_row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("compact::final")),
        large.basis(),
        "compact::final",
    )
    .with_document(Vec::<backend_engine::Fragment>::new());
    let prepared = large
        .prepare(
            backend_engine::ViewDelta::Upsert { row: final_row },
            capability.clone(),
        )
        .expect("final delta");
    let (target, final_delta) = large.commit(prepared).expect("final commit");
    let final_event = CursorEvent::View {
        delta: Box::new(final_delta),
    };
    let final_payload = encode_envelope(
        super::super::genesis().expect("head").root(),
        Cursor::for_view_root(&target),
        &target,
        Some(&final_event),
    )
    .expect("final compact envelope");
    assert!(final_payload.len() < first_payload.len().saturating_mul(2));
    assert!(final_payload.len() < 64 * 1024);
    assert_eq!(base_cursor.sequence(), 0);
}

#[test]
fn compacts_before_an_event_can_cross_the_scan_file_bound() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-locald-view-bound-{stamp}.journal"));
    let head = super::super::genesis().expect("checked builtin genesis");
    let (base, _) = super::super::initial_view().expect("initial view");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("bound::row")),
        base.basis(),
        "bound::row",
    )
    .with_document(Vec::<backend_engine::Fragment>::new());
    let prepared = base
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("prepare");
    let (target, delta) = base.commit(prepared).expect("commit");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let journal = ViewJournal::open(&path).expect("open view journal");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .expect("preallocate journal");
    file.set_len(MAX_FILE_BYTES - 1).expect("preallocate bound");
    drop(file);
    let mut journal = journal;
    journal
        .persist(
            head.root(),
            &target,
            Cursor::for_view_root(&target),
            Some(&event),
        )
        .expect("pre-crossing compaction");
    assert!(fs::metadata(&path).expect("journal metadata").len() < MAX_FILE_BYTES);
    assert!(
        journal
            .load_for_workspace(head.root(), &capability)
            .expect("recover compacted journal")
            .is_some()
    );
    let _ = fs::remove_file(path);
}

#[test]
fn torn_tail_repair_is_durable_across_a_second_open() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-locald-view-tail-{stamp}.journal"));
    let head = super::super::genesis().expect("checked builtin genesis");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (view, cursor) = super::super::initial_view().expect("checked initial view");
    let mut journal = ViewJournal::open(&path).expect("open view journal");
    journal
        .persist(head.root(), &view, cursor, None)
        .expect("persist view snapshot");
    let clean_length = fs::metadata(&path).expect("snapshot metadata").len();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open torn journal")
        .write_all(&MAGIC[..3])
        .expect("write torn header");

    let recovered = journal
        .load_for_workspace(head.root(), &capability)
        .expect("repair torn suffix")
        .expect("recover snapshot");
    assert_eq!(recovered.view.root(), view.root());
    assert_eq!(
        fs::metadata(&path).expect("repaired metadata").len(),
        clean_length
    );

    let reopened = ViewJournal::open(&path).expect("reopen view journal");
    assert!(
        reopened
            .load_for_workspace(head.root(), &capability)
            .expect("load repaired journal")
            .is_some()
    );
    let _ = fs::remove_file(path);
}

#[test]
fn retried_synced_event_does_not_duplicate_after_ack_fault() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("backend-locald-view-event-retry-{stamp}.journal"));
    let head = super::super::genesis().expect("checked builtin genesis");
    let (base, cursor) = super::super::initial_view().expect("initial view");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("retry::row")),
        base.basis(),
        "retry::row",
    )
    .with_document(Vec::<backend_engine::Fragment>::new());
    let prepared = base
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("prepare");
    let (view, delta) = base.clone().commit(prepared).expect("commit");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let faults = Arc::new(Faults::default());
    let mut journal = ViewJournal::open_with_faults(&path, Arc::clone(&faults))
        .expect("open faulted view journal");
    journal
        .persist(head.root(), &base, cursor, None)
        .expect("persist base");
    faults.arm(Boundary::JournalFlush);
    assert!(
        journal
            .persist(
                head.root(),
                &view,
                Cursor::for_view_root(&view),
                Some(&event),
            )
            .is_err()
    );
    journal
        .persist(
            head.root(),
            &view,
            Cursor::for_view_root(&view),
            Some(&event),
        )
        .expect("retry event publication");
    let recovered = journal
        .load_for_workspace(head.root(), &capability)
        .expect("recover retried event")
        .expect("recovered view");
    assert_eq!(recovered.cursor, Cursor::for_view_root(&view));
    assert_eq!(recovered.events.len(), 1);
    let _ = fs::remove_file(path);
}

#[test]
fn snapshot_compaction_retries_are_old_or_new_at_each_publish_boundary() {
    let boundaries = [
        Boundary::TempCreate,
        Boundary::TempWrite,
        Boundary::FileSync,
        Boundary::Rename,
        Boundary::DirSync,
    ];
    for (index, boundary) in boundaries.into_iter().enumerate() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "backend-locald-view-compact-retry-{stamp}-{index}.journal"
        ));
        let head = super::super::genesis().expect("checked builtin genesis");
        let (base, _) = super::super::initial_view().expect("initial view");
        let capability = super::super::test_builtin_view_capability().expect("coverage");
        let row = backend_engine::Row::new(
            backend_engine::RowId::Symbol(backend_engine::symbol_key(&format!(
                "compact-retry::{index}"
            ))),
            base.basis(),
            format!("compact-retry::{index}"),
        )
        .with_document(Vec::<backend_engine::Fragment>::new());
        let prepared = base
            .prepare(
                backend_engine::ViewDelta::Upsert { row },
                capability.clone(),
            )
            .expect("prepare");
        let (view, delta) = base.commit(prepared).expect("commit");
        let event = CursorEvent::View {
            delta: Box::new(delta),
        };
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .expect("preallocate journal");
        file.set_len(MAX_FILE_BYTES - 1).expect("preallocate bound");
        drop(file);
        let faults = Arc::new(Faults::default());
        let mut journal = ViewJournal::open_with_faults(&path, Arc::clone(&faults))
            .expect("open faulted journal");
        faults.arm(boundary);
        assert!(
            journal
                .persist(
                    head.root(),
                    &view,
                    Cursor::for_view_root(&view),
                    Some(&event),
                )
                .is_err()
        );
        journal
            .persist(
                head.root(),
                &view,
                Cursor::for_view_root(&view),
                Some(&event),
            )
            .expect("retry compacted publication");
        let recovered = journal
            .load_for_workspace(head.root(), &capability)
            .expect("recover compacted publication")
            .expect("recovered compacted view");
        assert_eq!(recovered.view.root(), view.root());
        assert!(recovered.events.is_empty());
        let _ = fs::remove_file(path);
    }
}
