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
fn v14_view_snapshot_is_refused_by_v15_and_left_unchanged() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-locald-view-v14-{stamp}.journal"));
    let head = super::super::genesis().expect("checked builtin genesis");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (view, cursor) = super::super::initial_view().expect("checked initial view");
    let payload = encode_envelope(head.root(), cursor, &view, None).expect("view envelope");
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&payload).expect("current envelope json");
    envelope["view"]["version"] = serde_json::json!(14);
    let legacy_payload = serde_json::to_vec(&envelope).expect("v14 envelope json");
    let journal = ViewJournal::open(&path).expect("open view journal");
    journal
        .append(SNAPSHOT, &legacy_payload)
        .expect("seed a complete v14 snapshot frame");
    let before = fs::read(&path).expect("read seeded v14 journal");

    assert!(
        journal
            .load_for_workspace(head.root(), &capability)
            .is_err()
    );
    let refusal = journal
        .written_by_another_build()
        .expect("identify old wire version");
    assert!(refusal.contains("wire version 14"));
    assert_eq!(fs::read(&path).expect("read refused v14 journal"), before);
    let _ = fs::remove_file(path);
}

#[test]
fn roundtrip_re_admits_occurrence_disambiguated_row_identity() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-locald-view-occurrence-{stamp}.journal"));
    let head = super::super::genesis().expect("checked builtin genesis");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (base, _) = super::super::initial_view().expect("checked initial view");
    let preimage = "fixture::src/lib.rs:1::run\0method\0fn run()\01";
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key(preimage)),
        base.basis(),
        "fixture::src/lib.rs:1::run",
    )
    .with_identity_preimage(
        backend_engine::RowIdentityPreimage::try_new(preimage).expect("bounded preimage"),
    );
    let prepared = base
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("prepare occurrence row");
    let (view, _) = base.commit(prepared).expect("commit occurrence row");
    let cursor = Cursor::for_view_root(&view);
    let mut journal = ViewJournal::open(&path).expect("open view journal");
    journal
        .persist(head.root(), &view, cursor, None)
        .expect("persist occurrence row");
    let recovered = journal
        .load_for_workspace(head.root(), &capability)
        .expect("load occurrence row")
        .expect("occurrence snapshot");
    assert_eq!(recovered.view.root(), view.root());
    assert_eq!(
        recovered.view.rows()[0]
            .identity_preimage()
            .map(|value| value.as_str()),
        Some(preimage)
    );
    for restart in 0..3 {
        let reopened = ViewJournal::open(&path).expect("reopen occurrence journal");
        let cold = reopened
            .load_for_workspace(head.root(), &capability)
            .expect("load occurrence row after restart")
            .expect("occurrence snapshot after restart");
        assert_eq!(
            cold.cursor, cursor,
            "cold restart {restart} changed revision"
        );
        assert_eq!(
            cold.view.version(),
            view.version(),
            "cold restart {restart} changed version"
        );
        assert_eq!(
            cold.view.root(),
            view.root(),
            "cold restart {restart} changed root"
        );
        assert_eq!(
            cold.view.rows(),
            view.rows(),
            "cold restart {restart} changed content"
        );
    }
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
fn stale_selected_head_rebuild_keeps_only_the_lineage_checked_cursor_sequence() {
    let temp = tempfile::tempdir().expect("private owner workspace");
    let profile =
        super::super::profile_descriptor(super::super::BuiltinProfile::Product).expect("profile");
    let dispatcher = super::super::builtin_dispatcher(
        Some(super::super::ECHO_AUTHORITY_SECRET),
        Arc::clone(&profile),
        60_000,
    )
    .expect("dispatcher");
    let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
        temp.path(),
        super::super::BuiltinModel,
        super::super::genesis().expect("genesis"),
        dispatcher,
        backend_engine::DaemonConfig::default(),
        super::super::product_relation_registry().expect("registry"),
    )
    .expect("owner");

    let prior = daemon.engine().daemon().owner().snapshot();
    let (prior_view, _) = super::super::initial_view_for_workspace(&prior).expect("prior view");
    let prior_cursor = Cursor::for_view_root_at(&prior_view, 5);
    let path = temp.path().join("n14-view.journal");
    let mut journal = ViewJournal::open(&path).expect("journal");
    journal
        .persist(prior.root(), &prior_view, prior_cursor, None)
        .expect("prior selected snapshot");
    let prior_row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("n14::prior")),
        prior_view.basis(),
        "n14::prior",
    );
    let prepared = prior_view
        .prepare(
            backend_engine::ViewDelta::Upsert { row: prior_row },
            super::super::builtin_view_capability_for_workspace(&prior).expect("prior capability"),
        )
        .expect("prepare prior event");
    let (prior_next_view, prior_delta) = prior_view.commit(prepared).expect("prior event");
    let prior_event = CursorEvent::View {
        delta: Box::new(prior_delta),
    };
    let prior_next_cursor = prior_cursor
        .advance_event(&prior_event)
        .expect("prior event sequence");
    assert_eq!(prior_next_cursor.sequence(), 6);
    journal
        .persist(
            prior.root(),
            &prior_next_view,
            prior_next_cursor,
            Some(&prior_event),
        )
        .expect("prior compact event");

    // The owner durably advances to a new checked root while the view journal
    // still names its previous selected base, matching the startup crash seam.
    let label = "fixture:n14-selected-head";
    let intent = BuiltinIntent::add(backend_engine::package_key(label), label).expect("intent");
    crate::builtin::commands::commit_builtin_intent(&mut daemon, 1, &intent)
        .expect("durable owner transition");
    let selected = daemon.engine().daemon().owner().snapshot();
    assert_ne!(selected.root(), prior.root());
    assert_eq!(selected.transition_base(), prior.root());
    let capability = super::super::builtin_view_capability_for_workspace(&selected)
        .expect("selected capability");

    // A partial final header is repaired, and the complete checked snapshot
    // still supplies its sequence without carrying its old root or view.
    let mut tail = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open journal tail");
    tail.write_all(&MAGIC[..7]).expect("append torn header");
    drop(tail);
    let recovery = journal
        .recover_for_workspace(selected.root(), Some(selected.transition_base()), &capability)
        .expect("recover prior lineage");
    assert!(recovery.recovered.is_none(), "old view is never admitted");
    assert_eq!(
        recovery.cursor_sequence_floor,
        CursorSequenceFloor::Proven(6)
    );
    drop(journal);

    // Simulate a crash before publishing the rebuilt snapshot: another cold
    // start must still get the same scalar floor from the unchanged old frame.
    let journal = ViewJournal::open(&path).expect("reopen before rebuilt publish");
    let recovery = journal
        .recover_for_workspace(selected.root(), Some(selected.transition_base()), &capability)
        .expect("repeat recovery after crash");
    assert!(recovery.recovered.is_none());
    assert_eq!(
        recovery.cursor_sequence_floor,
        CursorSequenceFloor::Proven(6)
    );
    drop(journal);

    let (rebuilt, _) = super::super::initial_view_for_workspace(&selected).expect("rebuilt view");
    assert_eq!(rebuilt.capability(), Some(capability.clone()));
    let view_sequence = rebuilt.frontier().sequence;
    let cursor = Cursor::for_view_root_at(
        &rebuilt,
        match recovery.cursor_sequence_floor {
            CursorSequenceFloor::Proven(sequence) => sequence.max(view_sequence),
            CursorSequenceFloor::Empty | CursorSequenceFloor::Unproven => unreachable!(),
        },
    );
    assert_eq!(cursor.sequence(), 6);

    // The rebuilt snapshot is a new generation. A second crash after this
    // durable write must recover the new checked view at the retained floor.
    let mut journal = ViewJournal::open(&path).expect("journal before rebuilt publish");
    journal
        .persist(selected.root(), &rebuilt, cursor, None)
        .expect("publish rebuilt snapshot");
    drop(journal);
    let mut journal = ViewJournal::open(&path).expect("reopen rebuilt snapshot");
    let recovery = journal
        .recover_for_workspace(selected.root(), Some(selected.transition_base()), &capability)
        .expect("recover rebuilt snapshot");
    let recovered = recovery.recovered.expect("new selected view");
    assert_eq!(recovered.cursor, cursor);
    assert_eq!(recovered.view, rebuilt);
    assert_eq!(recovered.view.capability(), Some(capability.clone()));
    assert!(recovered.events.is_empty());

    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("n14::next")),
        rebuilt.basis(),
        "n14::next",
    );
    let prepared = rebuilt
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("prepare next checked event");
    let (next, delta) = rebuilt.commit(prepared).expect("commit next checked event");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let next_cursor = cursor.advance_event(&event).expect("advance sequence");
    assert_eq!(next_cursor.sequence(), 7);
    journal
        .persist(selected.root(), &next, next_cursor, Some(&event))
        .expect("persist next event");
    drop(journal);
    let reopened = ViewJournal::open(&path).expect("cold reopen after next event");
    let recovered = reopened
        .load_for_workspace(selected.root(), &capability)
        .expect("load next event")
        .expect("current selected view");
    assert_eq!(recovered.cursor, next_cursor);
    assert_eq!(recovered.cursor.sequence(), 7);
}

#[test]
fn stale_sequence_floor_refuses_foreign_lineage_gaps_and_exhaustion() {
    let temp = tempfile::tempdir().expect("private owner workspace");
    let selected = super::super::genesis().expect("selected head");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (view, _) = super::super::initial_view().expect("view");
    let label = "fixture:foreign-workspace";
    let intent = BuiltinIntent::add(backend_engine::package_key(label), label).expect("intent");
    let relation = super::super::workspace_relation(Some(&intent)).expect("relation");
    let semantic = super::super::semantic_relation().expect("semantic relation");
    let foreign = super::super::workspace_manifest(&relation, &semantic)
        .expect("foreign checked manifest")
        .root();
    assert_ne!(foreign, selected.root());

    let foreign_path = temp.path().join("foreign.journal");
    let mut foreign_journal = ViewJournal::open(&foreign_path).expect("foreign journal");
    foreign_journal
        .persist(
            foreign,
            &view,
            Cursor::for_view_root_at(&view, 6),
            None,
        )
        .expect("foreign frame");
    let foreign_recovery = foreign_journal
        .recover_for_workspace(selected.root(), Some(selected.root()), &capability)
        .expect("foreign cache miss");
    assert!(foreign_recovery.recovered.is_none());
    assert_eq!(
        foreign_recovery.cursor_sequence_floor,
        CursorSequenceFloor::Unproven
    );

    let gap_path = temp.path().join("gap.journal");
    let mut gap_journal = ViewJournal::open(&gap_path).expect("gap journal");
    let base_cursor = Cursor::for_view_root_at(&view, 5);
    gap_journal
        .persist(selected.root(), &view, base_cursor, None)
        .expect("base snapshot");
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("n14::gap")),
        view.basis(),
        "n14::gap",
    );
    let prepared = view
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("prepare checked event");
    let (next, delta) = view.clone().commit(prepared).expect("commit checked event");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let next_cursor = base_cursor.advance_event(&event).expect("event cursor");
    let payload = encode_envelope(selected.root(), next_cursor, &next, Some(&event))
        .expect("valid compact event");
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&payload).expect("event envelope");
    envelope["cursor"][171] = serde_json::json!(7);
    gap_journal
        .append(
            EVENT,
            &serde_json::to_vec(&envelope).expect("gapped event envelope"),
        )
        .expect("checksummed gapped event");
    assert!(gap_journal
        .recover_for_workspace(selected.root(), None, &capability)
        .expect_err("event sequence gap must refuse recovery")
        .contains("sequence does not advance"));

    let exhausted_path = temp.path().join("exhausted.journal");
    let mut exhausted = ViewJournal::open(&exhausted_path).expect("exhausted journal");
    exhausted
        .persist(
            selected.root(),
            &view,
            Cursor::for_view_root_at(&view, u64::MAX),
            None,
        )
        .expect("max sequence snapshot");
    assert!(exhausted
        .recover_for_workspace(selected.root(), None, &capability)
        .expect_err("exhausted cursor cannot advance")
        .contains("sequence is exhausted"));

    let corrupt_path = temp.path().join("corrupt.journal");
    let corrupt = ViewJournal::open(&corrupt_path).expect("corrupt journal");
    corrupt.append(SNAPSHOT, b"{}").expect("checksummed malformed frame");
    assert!(corrupt
        .recover_for_workspace(selected.root(), None, &capability)
        .is_err());
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

/// Stale-generation recovery laws.
///
/// The workspace root is a content hash of the selected relations, so it
/// repeats: removing the last project returns the workspace to the root it
/// had before that project was added.  The capability each snapshot is
/// certified against additionally binds the current commit
/// (`crates/engine/src/builtin/authority.rs:29-48`), so the older frame with
/// the same root cannot be admitted under the newer commit's capability and
/// the check in `crates/library/wire/claims.rs:452-458` rejects it.
///
/// Recovery used to decode that older frame and propagate its rejection, so
/// the service refused to start — "view coverage: certificate producer
/// observation does not match the admitted capability" — although a valid
/// newer frame sat later in the same file.  A superseded frame is a stale
/// cache entry and is skipped; only malformed bytes fail closed.
///
/// The assertions are on the recovered row labels, not on a row count: both
/// generations are coherent views over the same basis, so a recovery that
/// returned the wrong one would keep every count intact.
mod stale_generation {
    use super::*;

    /// The workspace generations a project being added and removed produces.
    struct Generations {
        root: WorkspaceRoot,
        other_root: WorkspaceRoot,
        first: (ViewRoot, CoverageCapability),
        newest: (ViewRoot, CoverageCapability),
    }

    /// Builds two capability generations that share one workspace root.
    ///
    /// The newest generation carries a distinctive row so a recovery that
    /// returns the wrong frame is visible in content, not only in identity.
    fn generations() -> Generations {
        let genesis = super::super::super::genesis().expect("checked builtin genesis");
        let label = "fixture:intervening-workspace";
        let intent = BuiltinIntent::add(backend_engine::package_key(label), label).expect("intent");
        let other = super::super::super::test_head_for_intent(&intent).expect("second head");
        assert_ne!(
            genesis.root(),
            other.root(),
            "the two heads must really select different workspace roots"
        );

        let (first_view, _, first_capability) =
            super::super::super::test_view_generation(&genesis).expect("first generation");
        let (base, _, newest_capability) =
            super::super::super::test_view_generation(&other).expect("newest generation");
        assert_ne!(
            first_capability, newest_capability,
            "a new commit must mint a distinguishable capability"
        );
        let row = backend_engine::Row::new(
            backend_engine::RowId::Symbol(backend_engine::symbol_key("removal::survivor")),
            base.basis(),
            "removal::survivor",
        );
        let prepared = base
            .prepare(
                backend_engine::ViewDelta::Upsert { row },
                newest_capability.clone(),
            )
            .expect("prepare the newest row");
        let (newest_view, _) = base.commit(prepared).expect("commit the newest row");

        Generations {
            root: genesis.root(),
            other_root: other.root(),
            first: (first_view, first_capability),
            newest: (newest_view, newest_capability),
        }
    }

    fn journal_path(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock")
            .as_nanos();
        std::env::temp_dir().join(format!("backend-locald-stale-{label}-{stamp}.journal"))
    }

    /// Writes snapshot(root, first) then snapshot(other root) then
    /// snapshot(root, newest): the file a removal leaves behind.
    fn written(label: &str, generations: &Generations) -> (PathBuf, ViewJournal) {
        let path = journal_path(label);
        let mut journal = ViewJournal::open(&path).expect("open view journal");
        let (first_view, _) = &generations.first;
        let (newest_view, _) = &generations.newest;
        journal
            .persist(
                generations.root,
                first_view,
                Cursor::for_view_root(first_view),
                None,
            )
            .expect("persist the first generation for the returning root");
        journal
            .persist(
                generations.other_root,
                newest_view,
                Cursor::for_view_root(newest_view),
                None,
            )
            .expect("persist the intervening root");
        journal
            .persist(
                generations.root,
                newest_view,
                Cursor::for_view_root(newest_view),
                None,
            )
            .expect("persist the newest generation for the returning root");
        (path, journal)
    }

    fn labels(view: &ViewRoot) -> Vec<String> {
        view.rows().iter().map(|row| row.label.clone()).collect()
    }

    /// A returning workspace root recovers the frame certified against the
    /// live capability instead of refusing to start on the superseded one.
    #[test]
    fn a_returning_root_recovers_its_newest_certified_frame() {
        let generations = generations();
        let (path, journal) = written("returning", &generations);
        let (newest_view, newest_capability) = &generations.newest;

        let recovered = journal
            .load_for_workspace(generations.root, newest_capability)
            .expect("a superseded same-root frame must not refuse recovery")
            .expect("the newest frame for the returning root must be recovered");

        assert_eq!(
            labels(&recovered.view),
            labels(newest_view),
            "recovery must return the frame certified against the live \
             capability, not the superseded frame that shares its root"
        );
        assert!(
            labels(&recovered.view)
                .iter()
                .any(|label| label == "removal::survivor"),
            "the recovered view must carry the newest generation's row, got {:?}",
            labels(&recovered.view)
        );
        assert_eq!(
            recovered.cursor,
            Cursor::for_view_root(newest_view),
            "the recovered cursor must be the newest frame's cursor"
        );
        let _ = fs::remove_file(path);
    }

    /// Loading under the superseded capability is a cache miss, not a fault.
    #[test]
    fn the_superseded_capability_reports_a_cache_miss_and_not_a_fault() {
        let generations = generations();
        let (path, journal) = written("superseded", &generations);
        let (_, first_capability) = &generations.first;

        let recovered = journal
            .load_for_workspace(generations.root, first_capability)
            .expect("a frame certified against another capability is a cache miss");

        assert!(
            recovered.is_none(),
            "no frame is admissible under the superseded capability, so \
             recovery must report the miss the caller rebuilds from"
        );
        let _ = fs::remove_file(path);
    }

    /// Only the live generation is decoded. A superseded frame is read and
    /// checksummed but never parsed, so a start does not pay for every
    /// workspace root the journal ever held (the desktop fixture's journal
    /// held 20 generations, 113 MB of them dead).
    #[test]
    fn a_superseded_generation_is_checksummed_but_never_decoded() {
        let generations = generations();
        let (newest_view, newest_capability) = &generations.newest;
        let path = journal_path("superseded-undecoded");
        let mut journal = ViewJournal::open(&path).expect("open view journal");
        journal
            .append(
                SNAPSHOT,
                b"a superseded generation: checksummed, never decoded",
            )
            .expect("append a superseded frame");
        journal
            .persist(
                generations.root,
                newest_view,
                Cursor::for_view_root(newest_view),
                None,
            )
            .expect("persist the live generation");

        let recovered = journal
            .load_for_workspace(generations.root, newest_capability)
            .expect("a superseded frame's bytes are not recovery input")
            .expect("the live generation recovers");
        assert_eq!(labels(&recovered.view), labels(newest_view));
        assert_eq!(recovered.cursor, Cursor::for_view_root(newest_view));
        let _ = fs::remove_file(path);
    }

    /// A journal whose only frame is an event still fails closed.
    ///
    /// Skipping the events of a superseded snapshot must not also swallow an
    /// event that never had a snapshot: those bytes are malformed.
    #[test]
    fn an_event_without_any_snapshot_still_fails_closed() {
        let head = super::super::super::genesis().expect("checked builtin genesis");
        let capability = super::super::super::test_builtin_view_capability().expect("coverage");
        let (base, cursor) = super::super::super::initial_view().expect("initial view");
        let row = backend_engine::Row::new(
            backend_engine::RowId::Symbol(backend_engine::symbol_key("orphan::row")),
            base.basis(),
            "orphan::row",
        );
        let prepared = base
            .prepare(
                backend_engine::ViewDelta::Upsert { row },
                capability.clone(),
            )
            .expect("prepare orphan row");
        let (target, delta) = base.clone().commit(prepared).expect("commit orphan row");
        let event = CursorEvent::View {
            delta: Box::new(delta),
        };
        assert_ne!(cursor, Cursor::for_view_root(&target));

        let path = journal_path("orphan");
        let journal = ViewJournal::open(&path).expect("open view journal");
        let payload = encode_envelope(
            head.root(),
            Cursor::for_view_root(&target),
            &target,
            Some(&event),
        )
        .expect("encode an event envelope");
        journal.append(EVENT, &payload).expect("append the event");

        let error = journal
            .load_for_workspace(head.root(), &capability)
            .expect_err("an event with no snapshot is malformed and must fail closed");
        assert!(
            error.contains("event precedes its snapshot"),
            "malformed bytes must still fail closed, got {error}"
        );
        let _ = fs::remove_file(path);
    }
}

#[test]
fn a_reopened_journal_keeps_every_row_fact_with_its_text() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-locald-view-facts-{stamp}.journal"));
    let head = super::super::genesis().expect("checked builtin genesis");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (base, _) = super::super::initial_view().expect("checked initial view");
    let facts = backend_compile::DeclarationFacts {
        deprecation: backend_compile::Fact::Present(backend_compile::Deprecation::new(
            Some("1.2.0"),
            Some("use `fresh` instead"),
        )),
        obligation: backend_compile::Fact::Present(backend_compile::Obligation::Required),
    };
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("journal::stale")),
        base.basis(),
        "journal::stale",
    )
    .with_facts(facts.clone());
    let prepared = base
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("prepare facts row");
    let (view, _) = base.commit(prepared).expect("commit facts row");
    let cursor = Cursor::for_view_root(&view);
    let mut journal = ViewJournal::open(&path).expect("open view journal");
    journal
        .persist(head.root(), &view, cursor, None)
        .expect("persist facts row");
    let reopened = ViewJournal::open(&path).expect("reopen view journal");
    let recovered = reopened
        .load_for_workspace(head.root(), &capability)
        .expect("load facts row")
        .expect("facts snapshot");
    assert_eq!(recovered.view.root(), view.root());
    let row = &recovered.view.rows()[0];
    let notice = row
        .facts
        .deprecation
        .present()
        .expect("deprecation survived");
    assert_eq!(notice.since(), Some("1.2.0"));
    assert_eq!(notice.note(), Some("use `fresh` instead"));
    assert_eq!(row.facts, facts);
    let _ = fs::remove_file(path);
}

#[test]
fn capture_only_generation_rebind_cold_recovers_then_retains_compact_events() {
    use backend_engine::builtin::SemanticSourceCapture;
    use backend_engine::builtin::{ProductSemanticCaptureOutcome, ProductSemanticPublicationKey};
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, TypeScriptSource};
    let temp = tempfile::tempdir().expect("private owner workspace");
    let profile =
        super::super::profile_descriptor(super::super::BuiltinProfile::Product).expect("profile");
    let dispatcher = super::super::builtin_dispatcher(
        Some(super::super::ECHO_AUTHORITY_SECRET),
        Arc::clone(&profile),
        60_000,
    )
    .expect("dispatcher");
    let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
        temp.path(),
        super::super::BuiltinModel,
        super::super::genesis().expect("genesis"),
        dispatcher,
        backend_engine::DaemonConfig::default(),
        super::super::product_relation_registry().expect("registry"),
    )
    .expect("owner");
    let first_snapshot = daemon.engine().daemon().owner().snapshot();
    let first_cap = super::super::builtin_view_capability_for_workspace(&first_snapshot)
        .expect("first admission");
    let (base, _) = super::super::initial_view_for_workspace(&first_snapshot).expect("base view");
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("generation::retained")),
        base.basis(),
        "generation::retained",
    );
    let (base, _) = base
        .clone()
        .commit(
            base.prepare(backend_engine::ViewDelta::Upsert { row }, first_cap.clone())
                .expect("row patch"),
        )
        .expect("row commit");
    let path = temp.path().join("generation.journal");
    let mut journal = ViewJournal::open(&path).expect("journal");
    journal
        .persist(
            first_snapshot.root(),
            &base,
            Cursor::for_view_root(&base),
            None,
        )
        .expect("first snapshot");
    let before_bytes = fs::metadata(&path).expect("first bytes").len();
    let stale_path = temp.path().join("stale-generation.journal");
    fs::copy(&path, &stale_path).expect("preserve stale base");

    let label = "pkg:npm/capture-only-generation@1.0.0";
    let key = ProductSemanticPublicationKey::new(
        backend_engine::PackageReference::parse(label.to_owned()).expect("reference"),
        PackageUrl::parse(label.to_owned()).expect("coordinate"),
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
    )
    .expect("capture key");
    let intent = BuiltinIntent::index_with_capture(
        backend_engine::PackageKey::from_value(label),
        label,
        Vec::new(),
        Vec::new(),
        vec![super::super::BuiltinCaptureChange {
            key,
            expected: None,
            capture: SemanticSourceCapture::new(None, [0xB3; 32], [0xB4; 32], 1, 1)
                .expect("capture"),
            outcome: ProductSemanticCaptureOutcome::Pending { prior: None },
            compiler_failure: None,
        }],
    )
    .expect("capture-only intent");
    crate::builtin::commands::commit_builtin_intent(&mut daemon, 1, &intent)
        .expect("real capture-only owner commit");
    let selected = daemon.engine().daemon().owner().snapshot();
    assert_eq!(selected.root(), first_snapshot.root(), "same manifest root");
    assert_ne!(
        selected.commit().id(),
        first_snapshot.commit().id(),
        "new producer context"
    );
    let cap = super::super::builtin_view_capability_for_workspace(&selected)
        .expect("new exact admission");
    assert_ne!(
        capability_fingerprint(&cap),
        capability_fingerprint(&first_cap)
    );
    let prepared = base
        .prepare(
            backend_engine::ViewDelta::Patch {
                changes: Arc::from([]),
            },
            cap.clone(),
        )
        .expect("checked empty rebind");
    let (rebound, delta) = base.clone().commit(prepared).expect("commit rebind");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let cursor = Cursor::for_view_root(&rebound);
    // A cold miss (empty or stale) must not claim an accepted compact base.
    for recovery_path in [stale_path, temp.path().join("empty-generation.journal")] {
        let mut missed = ViewJournal::open(&recovery_path).expect("cold miss journal");
        assert!(
            missed
                .load_for_workspace(selected.root(), &cap)
                .expect("cache miss")
                .is_none()
        );
        missed
            .persist(selected.root(), &rebound, cursor, Some(&event))
            .expect("snapshot after miss");
        drop(missed);
        let cold = ViewJournal::open(&recovery_path).expect("cold missed-generation journal");
        let recovered = cold
            .load_for_workspace(selected.root(), &cap)
            .expect("recover snapshot after miss")
            .expect("accepted generation");
        assert_eq!(recovered.view.descriptor(), rebound.descriptor());
        assert_eq!(recovered.view.capability(), rebound.capability());
        assert_eq!(recovered.view.rows(), rebound.rows());
        assert_eq!(
            recovered.view, rebound,
            "cold hydration preserves the current relation authority"
        );
        assert!(recovered.events.is_empty());
    }
    journal
        .persist(selected.root(), &rebound, cursor, Some(&event))
        .expect("persist rebind");
    drop(journal);
    let mut journal = ViewJournal::open(&path).expect("cold journal");
    let recovered = journal
        .load_for_workspace(selected.root(), &cap)
        .expect("cold read")
        .expect("new generation must survive restart");
    assert_eq!(recovered.view.descriptor(), rebound.descriptor());
    assert_eq!(recovered.view.capability(), rebound.capability());
    assert_eq!(recovered.view.rows(), rebound.rows());
    assert_eq!(
        recovered.view, rebound,
        "the published root equals its cold reconstruction"
    );
    assert_eq!(recovered.cursor, cursor);
    assert_eq!(recovered.view.capability(), Some(cap.clone()));
    assert_eq!(recovered.view.row_count(), base.row_count());
    assert!(
        recovered.events.is_empty(),
        "authority change is a snapshot boundary"
    );
    let stale_reader = ViewJournal::open(&path).expect("independent stale-capability reader");
    assert!(
        stale_reader
            .load_for_workspace(selected.root(), &first_cap)
            .expect("stale capability is a cache miss")
            .is_none(),
        "retained rows cannot bypass the selected producer admission"
    );
    assert_eq!(
        recovered.view.capability().as_ref(),
        Some(&cap),
        "retained canonical rows are recovered only under the new workspace capability"
    );
    let snapshot_bytes = fs::metadata(&path).expect("rebound bytes").len() - before_bytes;
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key("generation::subsequent")),
        rebound.basis(),
        "generation::subsequent",
    );
    let (next, delta) = rebound
        .clone()
        .commit(
            rebound
                .prepare(backend_engine::ViewDelta::Upsert { row }, cap.clone())
                .expect("stable-cap patch"),
        )
        .expect("stable-cap commit");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let before_event = fs::metadata(&path).expect("before compact event").len();
    journal
        .persist(
            selected.root(),
            &next,
            Cursor::for_view_root(&next),
            Some(&event),
        )
        .expect("compact event");
    let event_bytes = fs::metadata(&path).expect("after compact event").len() - before_event;
    let mut kinds = Vec::new();
    journal
        .scan_frames(|kind, _| {
            kinds.push(kind);
            Ok(())
        })
        .expect("frames");
    assert_eq!(kinds, vec![SNAPSHOT, SNAPSHOT, EVENT]);
    drop(journal);
    let cold = ViewJournal::open(&path).expect("second cold journal");
    let recovered = cold
        .load_for_workspace(selected.root(), &cap)
        .expect("second recovery")
        .expect("stable generation");
    assert_eq!(recovered.view.descriptor(), next.descriptor());
    assert_eq!(recovered.view.capability(), next.capability());
    assert_eq!(recovered.view.rows(), next.rows());
    assert_eq!(recovered.events.len(), 1);
    println!(
        "generation snapshot bytes={snapshot_bytes}; subsequent compact event bytes={event_bytes}; retained rows={}",
        base.row_count()
    );
}

fn journal_v3_test_advance(
    view: ViewRoot,
    cursor: Cursor,
    capability: &CoverageCapability,
    label: &str,
) -> (ViewRoot, Cursor, CursorEvent) {
    let row = backend_engine::Row::new(
        backend_engine::RowId::Symbol(backend_engine::symbol_key(label)),
        view.basis(),
        label,
    );
    let prepared = view
        .prepare(
            backend_engine::ViewDelta::Upsert { row },
            capability.clone(),
        )
        .expect("checked journal transition");
    let (target, delta) = view.commit(prepared).expect("checked journal commit");
    let event = CursorEvent::View {
        delta: Box::new(delta),
    };
    let cursor = cursor
        .advance_event(&event)
        .expect("checked cursor transition");
    (target, cursor, event)
}

fn journal_v3_payload(bytes: &[u8], field: &str, version: u16) -> Vec<u8> {
    // These regression operands differ only in the declared version of the
    // unchanged payload grammar. Authentic old-build journals are exercised
    // separately through the public owner; this helper supplies no authority.
    let mut value: serde_json::Value = serde_json::from_slice(bytes).expect("envelope");
    value[field]["version"] = serde_json::json!(version);
    serde_json::to_vec(&value).expect("known unchanged envelope")
}

#[test]
fn journal_v3_known_snapshots_events_chain_to_current_without_replacing_history() {
    for version in [21, 22, 23, 24, 25, backend_library::DTO_VERSION] {
        journal_v3_known_version_chain(version);
    }
}

fn journal_v3_known_version_chain(version: u16) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-journal-v{version}-to-current-{stamp}"));
    let head = super::super::genesis().expect("head");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (base, cursor) = super::super::initial_view().expect("base");
    let (first, first_cursor, _) =
        journal_v3_test_advance(base, cursor, &capability, "legacy::first");
    let snapshot = journal_v3_payload(
        &encode_envelope(head.root(), first_cursor, &first, None).expect("snapshot"),
        "view",
        version,
    );
    let (second, second_cursor, legacy_event) =
        journal_v3_test_advance(first.clone(), first_cursor, &capability, "legacy::second");
    let event = journal_v3_payload(
        &encode_envelope(head.root(), second_cursor, &second, Some(&legacy_event)).expect("event"),
        "event",
        version,
    );
    let mut journal = ViewJournal::open(&path).expect("journal");
    journal
        .append(SNAPSHOT, &snapshot)
        .expect("checksummed v21 snapshot");
    journal
        .append(EVENT, &event)
        .expect("checksummed v21 compact event");
    let legacy_bytes = fs::read(&path).expect("legacy bytes");
    let recovered = journal
        .load_for_workspace(head.root(), &capability)
        .expect("known grammar")
        .expect("retained view");
    assert_eq!(recovered.view, second);
    assert_eq!(recovered.cursor, second_cursor);
    assert_eq!(recovered.events.len(), 1);
    assert_eq!(fs::read(&path).expect("unchanged recovery"), legacy_bytes);
    assert!(journal.written_by_another_build().is_none());
    let old_view: serde_json::Value = serde_json::from_slice(&snapshot).expect("snapshot JSON");
    let old_event: serde_json::Value = serde_json::from_slice(&event).expect("event JSON");
    if version != backend_library::DTO_VERSION {
        assert!(
            ViewDto::decode_with_certificate(
                &serde_json::to_vec(&old_view["view"]).expect("view"),
                Some(capability.clone())
            )
            .is_err(),
            "live view peers stay strict-current"
        );
        assert!(
            backend_engine::decode_compact_view_event(
                &serde_json::to_vec(&old_event["event"]).expect("event"),
                first_cursor,
                &first
            )
            .is_err(),
            "live compact codec stays strict-current"
        );
    }
    let (third, third_cursor, checked_current_event) = journal_v3_test_advance(
        recovered.view,
        recovered.cursor,
        &capability,
        "current::third",
    );
    journal
        .persist(
            head.root(),
            &third,
            third_cursor,
            Some(&checked_current_event),
        )
        .expect("append checked current event");
    let updated_bytes = fs::read(&path).expect("mixed journal");
    assert!(
        updated_bytes.starts_with(&legacy_bytes),
        "old history is never rewritten or reset"
    );
    let mut versions = Vec::new();
    journal
        .scan_frames(|_, bytes| {
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(json_error)?;
            versions.push(
                (value
                    .get("view")
                    .filter(|v| !v.is_null())
                    .or_else(|| value.get("event")))
                .expect("record")["version"]
                    .as_u64()
                    .expect("wire version"),
            );
            Ok(())
        })
        .expect("scan mixed history");
    assert_eq!(
        versions,
        [
            u64::from(version),
            u64::from(version),
            u64::from(backend_library::DTO_VERSION)
        ]
    );
    for _ in 0..2 {
        let cold = ViewJournal::open(&path).expect("cold open");
        let recovered = cold
            .load_for_workspace(head.root(), &capability)
            .expect("cold exact grammar")
            .expect("cold current view");
        assert_eq!(recovered.view, third);
        assert_eq!(recovered.cursor, third_cursor);
        assert_eq!(recovered.events.len(), 2);
        assert_eq!(recovered.view.row_count(), third.row_count());
        assert_eq!(
            fs::read(&path).expect("cold unchanged bytes"),
            updated_bytes
        );
    }
    fs::remove_file(path).expect("remove test journal");
}

#[test]
fn journal_v3_refuses_unknown_versions_fields_proof_basis_and_descriptor_changes() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let head = super::super::genesis().expect("head");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (base, cursor) = super::super::initial_view().expect("base");
    let (first, first_cursor, _) = journal_v3_test_advance(base, cursor, &capability, "old::first");
    let snapshot = journal_v3_payload(
        &encode_envelope(head.root(), first_cursor, &first, None).expect("snapshot"),
        "view",
        21,
    );
    let (second, second_cursor, event) =
        journal_v3_test_advance(first, first_cursor, &capability, "old::second");
    let event = journal_v3_payload(
        &encode_envelope(head.root(), second_cursor, &second, Some(&event)).expect("event"),
        "event",
        21,
    );
    assert!(backend_library::JournalViewGrammarV3::from_checked_container(VERSION - 1).is_err());
    for (kind, bytes, inner, version) in [
        (SNAPSHOT, &snapshot, "view", 21),
        (EVENT, &event, "event", 21),
        (SNAPSHOT, &snapshot, "view", backend_library::DTO_VERSION),
        (EVENT, &event, "event", backend_library::DTO_VERSION),
    ] {
        for (at, tamper) in [
            "old-version",
            "future-version",
            "unknown-inner",
            "unknown-outer",
            "proof",
            "basis",
            "descriptor",
            "both-records",
        ]
        .into_iter()
        .enumerate()
        {
            let path = std::env::temp_dir()
                .join(format!("backend-journal-v3-refusal-{stamp}-{kind}-{version}-{at}"));
            let journal = ViewJournal::open(&path).expect("journal");
            if kind == EVENT {
                journal.append(SNAPSHOT, &snapshot).expect("valid base");
            }
            let mut value: serde_json::Value = serde_json::from_slice(bytes).expect("JSON");
            value[inner]["version"] = serde_json::json!(version);
            match tamper {
                "old-version" => value[inner]["version"] = serde_json::json!(20),
                "future-version" => {
                    value[inner]["version"] = serde_json::json!(backend_library::DTO_VERSION + 1)
                }
                "unknown-inner" => {
                    value[inner]["future_locator"] = serde_json::json!("not-supported")
                }
                "unknown-outer" => value["future_locator"] = serde_json::json!("not-supported"),
                "proof" => {
                    value[inner]["certificate"]["claims"][0]["data"]["id"] =
                        serde_json::json!("00".repeat(32))
                }
                "basis" if kind == SNAPSHOT => {
                    value[inner]["snapshot"]["root"]["basis"]["schema"] = serde_json::json!(99)
                }
                "basis" => {
                    value[inner]["event"]["data"]["source"]["schema"] = serde_json::json!(99)
                }
                "descriptor" => value["descriptor"][0] = serde_json::json!(255),
                "both-records" => {
                    value[if kind == SNAPSHOT { "event" } else { "view" }] =
                        serde_json::json!({"version":21})
                }
                _ => unreachable!(),
            }
            journal
                .append(kind, &serde_json::to_vec(&value).expect("tampered payload"))
                .expect("valid checksum around invalid payload");
            let before = fs::read(&path).expect("before refusal");
            assert!(
                journal
                    .load_for_workspace(head.root(), &capability)
                    .is_err(),
                "kind={kind} tamper={tamper}"
            );
            assert_eq!(
                fs::read(&path).expect("refused bytes preserved"),
                before,
                "{tamper}"
            );
            fs::remove_file(path).expect("remove refused test journal");
        }
    }
}

#[test]
fn journal_v3_native_v21_fixture_appends_checked_current_and_reopens_twice() {
    assert_eq!(backend_library::DTO_VERSION, 26);
    // Exact historical codec output: native ARM64 source 9b0001b3af, whose
    // production persisted grammar is still v21. No field/version rewrite.
    let native = include_bytes!("fixtures/view-journal-v3-native-v21.bin");
    assert_eq!(
        blake3::hash(native).to_hex().as_str(),
        "faed3440288d7bb208272069220d1dcdb87a84a230302d0e331d6b917d2d1a8e"
    );
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("backend-native-journal-v21-current-{stamp}"));
    fs::write(&path, native).expect("copy historical operands into private test journal");
    let head = super::super::genesis().expect("checked head");
    let capability = super::super::test_builtin_view_capability().expect("coverage");
    let (base, cursor) = super::super::initial_view().expect("base");
    let (first, first_cursor, _) =
        journal_v3_test_advance(base, cursor, &capability, "legacy::first");
    let (second, second_cursor, _) =
        journal_v3_test_advance(first.clone(), first_cursor, &capability, "legacy::second");
    let mut journal = ViewJournal::open(&path).expect("historical journal");
    let recovered = journal
        .load_for_workspace(head.root(), &capability)
        .expect("strict historical grammar and proof admission")
        .expect("historical selected view");
    assert_eq!(recovered.view.descriptor(), second.descriptor());
    assert_eq!(recovered.view.rows(), second.rows());
    assert_eq!(recovered.cursor, second_cursor);
    assert_eq!(recovered.events.len(), 1);
    assert_eq!(
        fs::read(&path).expect("unchanged native bytes").as_slice(),
        native.as_slice()
    );
    let mut versions = Vec::new();
    journal
        .scan_frames(|kind, bytes| {
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(json_error)?;
            let (field, live_refused) = if kind == SNAPSHOT {
                (
                    "view",
                    ViewDto::decode_with_certificate(
                        &serde_json::to_vec(&value["view"]).map_err(json_error)?,
                        Some(capability.clone()),
                    )
                    .is_err(),
                )
            } else {
                (
                    "event",
                    backend_engine::decode_compact_view_event(
                        &serde_json::to_vec(&value["event"]).map_err(json_error)?,
                        first_cursor,
                        &first,
                    )
                    .is_err(),
                )
            };
            versions.push(value[field]["version"].as_u64().expect("native version"));
            assert!(
                live_refused,
                "historical bytes cannot enter the live current codec"
            );
            Ok(())
        })
        .expect("historical snapshot and event frames");
    assert_eq!(versions, [21, 21]);
    let (third, third_cursor, event) = journal_v3_test_advance(
        recovered.view,
        recovered.cursor,
        &capability,
        "current::third",
    );
    journal
        .persist(head.root(), &third, third_cursor, Some(&event))
        .expect("append checked native current transition");
    let mixed = fs::read(&path).expect("mixed grammar journal");
    assert!(
        mixed.starts_with(native),
        "historical bytes are never rewritten"
    );
    versions.clear();
    journal
        .scan_frames(|kind, bytes| {
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(json_error)?;
            let field = if kind == SNAPSHOT { "view" } else { "event" };
            versions.push(value[field]["version"].as_u64().expect("native version"));
            Ok(())
        })
        .expect("mixed native frames");
    assert_eq!(versions, [21, 21, u64::from(backend_library::DTO_VERSION)]);
    drop(journal);
    for _ in 0..2 {
        let cold = ViewJournal::open(&path).expect("independent cold journal");
        let recovered = cold
            .load_for_workspace(head.root(), &capability)
            .expect("cold exact proof/closure admission")
            .expect("current view");
        assert_eq!(recovered.view.descriptor(), third.descriptor());
        assert_eq!(recovered.view.rows(), third.rows());
        assert_eq!(recovered.cursor, third_cursor);
        assert_eq!(recovered.events.len(), 2);
        assert_eq!(fs::read(&path).expect("cold byte preservation"), mixed);
    }
    fs::remove_file(path).expect("remove private test journal");
}
