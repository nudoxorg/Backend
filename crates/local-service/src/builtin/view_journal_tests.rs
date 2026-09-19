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
        let intent =
            BuiltinIntent::add(backend_engine::package_key(label), label).expect("intent");
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
