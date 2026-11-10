//! Empty projects use the same owned scan/publication path as populated ones.
use super::*;
use crate::builtin::BuiltinModelError;
use crate::builtin::commands::index::{
    IndexScanFailure, PreparedSourceScan, capture_index_scan, prepare_source_scan, run_index_scan,
};
use backend_engine::builtin::{ProductSourceFileFactsRecord, product_source_file_facts_relation};

#[test]
fn empty_project_publication_and_repeat_keep_exact_receipts_without_source_capture()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let (adapter, daemon) = fixture.parts();
    let mut previous = owner_cursor(daemon);
    for request in [151_u8, 152] {
        let operation = backend_library::IndexOperationKey::from_bytes([request; 32])
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            adapter.start_index_operation(
                daemon,
                operation,
                backend_library::PackageReference::parse(label.clone())
                    .map_err(|error| error.to_string())?,
                CompileExecutionIntent::Interactive,
                u64::from(request),
            )?,
            Executed::Reply(_)
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while adapter.indexing.is_some() {
            assert!(adapter.poll_deferred(daemon).is_empty());
            assert!(std::time::Instant::now() < deadline, "empty index terminal");
            std::thread::sleep(Duration::from_millis(2));
        }
        let retained = adapter
            .index_operations
            .entry(operation)
            .map_err(|error| error.to_string())?;
        let retained_state = retained.as_ref().and_then(|entry| match entry {
            JournalEntry::Retained(entry) => Some(&entry.state),
            JournalEntry::OutsideReceiptWindow { .. } => None,
        });
        assert!(
            matches!(
                &retained,
                Some(JournalEntry::Retained(entry))
                    if matches!(entry.state, StoredOperationState::Published { .. })
                        && entry.source_capture.is_none()
            ),
            "a real empty scan publishes without inventing a compiler capture: operation={operation:?}, state={retained_state:?}"
        );
        let observed = adapter
            .resolve_index_operation(daemon, operation, None)
            .map_err(|error| error.to_string())?;
        let backend_library::IndexOperationObservation::Known(status) = &observed else {
            return Err(BuiltinModelError("accepted operation lost its status".to_owned()).into());
        };
        let backend_library::IndexOperationState::Published(receipt) = &status.state else {
            return Err(
                BuiltinModelError(format!("empty index did not publish: {status:?}")).into(),
            );
        };
        assert_eq!(status.operation_key, operation);
        assert_eq!(status.package.as_str(), label);
        assert!(status.source_capture.is_none());
        let owner = daemon.engine().daemon().owner();
        let library = daemon.engine().daemon().library();
        assert_eq!(receipt.workspace_root(), owner.head().root().as_bytes());
        assert_eq!(receipt.workspace_sequence(), owner.head().sequence());
        assert_eq!(receipt.view_root(), library.view().root().as_bytes());
        assert_eq!(
            receipt.revision_cursor(),
            owner_cursor(daemon).encode_control().as_ref()
        );
        assert_eq!(receipt.request_identity().is_some(), request == 151);
        backend_library::SurfaceReply::IndexOperationStatus(observed)
            .admit(backend_library::CommandId::IndexProgress)
            .map_err(|error| error.to_string())?;
        assert!(project_is_admitted(daemon, package));
        let current = owner_cursor(daemon);
        if request == 151 {
            assert_ne!(current, previous, "the first empty scan admits its project");
        } else {
            assert_eq!(current, previous, "an unchanged empty scan is a no-op");
        }
        previous = current;
        finish_search_lane(adapter, daemon);
    }
    Ok(())
}

#[test]
fn owned_source_preparation_preserves_real_file_facts_with_pending_capture()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    fs::write(
        Path::new(&label).join("source.py"),
        "def retained_name():\n    return 7\n",
    )?;
    let (adapter, daemon) = fixture.parts();
    let work = capture_index_scan(
        daemon,
        package,
        &label,
        Path::new(&label),
        None,
        153,
        CompileExecutionIntent::Interactive,
        false,
        Arc::new(AtomicBool::new(false)),
        adapter.compiler.python_interpreter_admission(),
    )?;
    let scan = run_index_scan(work).map_err(|failure| match failure {
        IndexScanFailure::Cancelled => BuiltinModelError("unexpected scan cancellation".to_owned()),
        IndexScanFailure::Refused(error) => error,
    })?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let prepared = prepare_source_scan(
        &snapshot,
        scan,
        &adapter.compiler,
        &mut adapter.semantic_authority,
        None,
        None,
        &mut || {},
    )?;
    assert!(matches!(&prepared, PreparedSourceScan::Capture { .. }));
    let PreparedSourceScan::Capture { selection, .. } = prepared else {
        return Err(
            BuiltinModelError("real typed source requires a pending capture".to_owned()).into(),
        );
    };
    let intent = selection.intent.ok_or_else(|| {
        BuiltinModelError("source preparation must retain its product intent".to_owned())
    })?;
    assert!(!intent.source_facts_changes().is_empty());
    assert_eq!(intent.capture_changes().len(), 1);
    let file_key =
        backend_engine::builtin::product_source_file_key(package.to_bytes(), "source.py");
    let source = intent
        .changes()
        .iter()
        .find(|change| change.key == file_key)
        .and_then(|change| change.after.as_ref())
        .and_then(|record| record.file_fields())
        .ok_or_else(|| BuiltinModelError("real source row is absent".to_owned()))?;
    let facts = intent
        .source_facts_changes()
        .iter()
        .find(|change| change.key == file_key)
        .and_then(|change| change.after.as_ref())
        .ok_or_else(|| BuiltinModelError("real source facts are absent".to_owned()))?;
    assert!(matches!(
        facts,
        ProductSourceFileFactsRecord::Manifest(manifest)
            if manifest.file_key() == file_key && manifest.matches_file_identity(source)
    ));
    let expected = intent.source_facts_changes().to_vec();
    super::super::commit_builtin_intent(daemon, 153, &intent)?;
    let selected = daemon.engine().daemon().owner().snapshot();
    let relation = product_source_file_facts_relation(&selected)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| BuiltinModelError("selected facts relation is absent".to_owned()))?;
    for change in expected {
        assert_eq!(
            relation
                .lookup(&change.key)
                .map_err(|error| error.to_string())?,
            change.after
        );
    }
    Ok(())
}

#[test]
fn sql_files_without_an_admitted_worker_publish_honest_unavailable_hosts_and_unchanged_repeat_is_noop()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    fs::write(
        Path::new(&label).join("App.sql"),
        "select 1;",
    )?;
    let (adapter, daemon) = fixture.parts();
    let mut supported_source_version = None;
    let mut prior_source_root = None;
    for (request, expected) in [(171_u8, 1_u64), (172, 2), (173, 2)] {
        if request == 172 {
            fs::write(
                Path::new(&label).join("Other.sql"),
                "select 2;",
            )?;
        }
        let operation = backend_library::IndexOperationKey::from_bytes([request; 32])
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            adapter.start_index_operation(
                daemon,
                operation,
                backend_library::PackageReference::parse(label.clone())
                    .map_err(|error| error.to_string())?,
                CompileExecutionIntent::Interactive,
                u64::from(request)
            )?,
            Executed::Reply(_)
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        while adapter.indexing.is_some() {
            assert!(adapter.poll_deferred(daemon).is_empty());
            assert!(
                std::time::Instant::now() < deadline,
                "SQL observation must settle without a compiler capture; last terminal: {:?}",
                adapter.index_terminals.back().map(|terminal| format!("{:?}", terminal.outcome))
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let observed = adapter
            .resolve_index_operation(daemon, operation, None)
            .map_err(|error| error.to_string())?;
        let backend_library::IndexOperationObservation::Known(status) = observed else {
            return Err(
                BuiltinModelError("SQL observation lost its durable operation".into()).into(),
            );
        };
        assert!(matches!(
            status.state,
            backend_library::IndexOperationState::Published(_)
        ));
        assert!(
            status.source_capture.is_none(),
            "unsupported files do not invent semantic profiles"
        );
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let relation = snapshot
            .relation::<crate::builtin::BuiltinWorkspaceRelation>()
            .map_err(|error| error.to_string())?;
        // SQL files are analysed (not merely counted): with no admitted worker each
        // one is recorded as an unavailable host, and the SQL relation is what
        // changes when a file is added; an unchanged repeat changes nothing.
        let sql = backend_engine::builtin::sql_analysis_relation(&snapshot)
            .map_err(|error| error.to_string())?
            .ok_or("SQL selection must be published for a project with SQL files")?;
        let source_root = *sql.root().as_bytes();
        if let Some(prior) = prior_source_root {
            if request == 173 { assert_eq!(source_root, prior); } else { assert_ne!(source_root, prior); }
        }
        prior_source_root = Some(source_root);
        let mut hosts = 0u64;
        let mut after = None;
        loop {
            let page = sql.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS).map_err(|error| error.to_string())?;
            for (_, row) in page.entries() {
                if let backend_engine::builtin::SqlAnalysisRecord::Host { terminal, .. } = row {
                    hosts += 1;
                    assert!(matches!(
                        terminal,
                        backend_engine::builtin::SqlHostTerminal::Unavailable(backend_engine::builtin::SqlUnavailable::EngineNotAdmitted)
                    ), "no worker is admitted, so the host is a recorded gap, never an empty result");
                }
            }
            match page.next().copied() {
                Some(next) if after != Some(next) => after = Some(next),
                _ => break,
            }
        }
        assert_eq!(hosts, expected);
        let row = relation
            .lookup(&package.to_bytes())
            .map_err(|error| error.to_string())?
            .ok_or("published SQL observation has no project row")?;
        let fields = row
            .project_fields()
            .ok_or("published row is not a project")?;
        assert_eq!(
            fields.files.file_count(),
            0,
            "no unsupported file enters the supported denominator"
        );
        if let Some(previous_source) = supported_source_version {
            assert_eq!(
                fields.source_version, previous_source,
                "count-only observation does not dirty the supported/compiler input digest"
            );
        }
        supported_source_version = Some(fields.source_version);
        assert_eq!(
            fields.source_selection.expect("newly committed source selection is present").unsupported_extensions.iter().find(|row| row.extension == "sql").expect("SQL scope").files,
            0,
            "files selected for SQL analysis are claimed, not left as unsupported"
        );
        let health = crate::builtin::ingest_progress(&snapshot)?;
        assert_eq!(health.files_discovered(), 0);
        assert_eq!(
            health.source_selection().expect("ingest progress exposes the committed source selection").unsupported_extensions.iter().find(|row| row.extension == "sql").expect("SQL scope").files,
            0,
            "health reads the newly committed observation, including on the reuse/no-compile branch"
        );
        finish_search_lane(adapter, daemon);
    }
    let expected_root = prior_source_root.ok_or("missing source root")?;
    close_journal_fixture(&mut fixture);
    let workspace = Path::new(&label).parent().ok_or("workspace missing")?;
    let owner = reopen_sql_journal_owner(workspace)?;
    let snapshot = owner.snapshot();
    let sql = backend_engine::builtin::sql_analysis_relation(&snapshot)?.ok_or("SQL selection survives a cold reopen")?;
    assert_eq!(*sql.root().as_bytes(), expected_root);
    let health = crate::builtin::ingest_progress(&snapshot)?;
    assert_eq!(health.source_selection_scope().ok_or("scope missing")?.extensions[1].excluded_files, 0);
    Ok(())
}

// These regressions use real source facts, checked store publication and owner
// recovery, without invoking a Rust compiler. They do not reproduce the
// reporter's unavailable 39 MB checkout or assert an unmeasured amplification.
fn close_journal_fixture(fixture: &mut AdapterFixture) {
    if let Some(mut adapter) = fixture.adapter.take() {
        adapter.close();
    }
    if let Some(mut daemon) = fixture.daemon.take() {
        daemon.close();
    }
}

fn reopen_journal_owner(
    workspace: &Path,
) -> Result<backend_engine::WorkspaceOwner<crate::builtin::BuiltinModel>, Box<dyn std::error::Error>>
{
    Ok(backend_engine::WorkspaceOwner::open_with_registry(
        workspace,
        crate::builtin::BuiltinModel,
        crate::builtin::genesis()?,
        crate::builtin::product_relation_registry()?,
    )?)
}

/// As above for a workspace that holds SQL: replaying a SQL receipt needs the
/// owner's receipt context bound to the leased store, exactly as in production.
fn reopen_sql_journal_owner(
    workspace: &Path,
) -> Result<backend_engine::WorkspaceOwner<crate::builtin::BuiltinModel>, Box<dyn std::error::Error>>
{
    let receipt = std::sync::Arc::new(crate::builtin::owner_receipt_context::OwnerReceiptContext::new(
        [0x67; 32],
        workspace.to_path_buf(),
    ));
    Ok(backend_engine::WorkspaceOwner::open_with_registry(
        workspace,
        crate::builtin::BuiltinModel::with_receipt_context(Some(receipt)),
        crate::builtin::genesis()?,
        crate::builtin::product_relation_registry()?,
    )?)
}

// These callbacks inspect freshly prepared publications, before owner reopen.
// The public manifest encoder supplies their canonical control bytes; stored
// membership uses the exact SCM1 envelope produced by
// PreparedTransition::with_stored_membership, rather than exporting membership.
fn fresh_prepared_closure_bytes(
    persisted: &backend_engine::PersistedTransition,
) -> Result<Vec<u8>, backend_store::StoreError> {
    let encoded = persisted.closure_manifest().encode(64 * 1024 * 1024)?;
    if let Some(membership) = persisted.membership_id() {
        let mut bytes = Vec::from(&b"SCM1"[..]);
        bytes.extend_from_slice(membership.as_bytes());
        bytes.extend_from_slice(&encoded);
        Ok(bytes)
    } else {
        Ok(encoded)
    }
}

fn prepared_summary(
    persisted: &backend_engine::PersistedTransition,
    closure: &[u8],
) -> backend_engine::WorkspaceRecord {
    backend_engine::WorkspaceRecord::PreparedSummary {
        request: persisted.request(),
        transaction: persisted.transaction(),
        manifest_bytes: persisted.manifest_bytes().len() as u64,
        manifest_digest: *backend_engine::blake3::hash(persisted.manifest_bytes()).as_bytes(),
        delta_bytes: persisted.delta_bytes().len() as u64,
        delta_digest: *backend_engine::blake3::hash(persisted.delta_bytes()).as_bytes(),
        commit_bytes: persisted.commit_bytes().len() as u64,
        commit_digest: *backend_engine::blake3::hash(persisted.commit_bytes()).as_bytes(),
        closure_bytes: closure.len() as u64,
        closure_digest: *backend_engine::blake3::hash(closure).as_bytes(),
    }
}

#[test]
fn rust_ingest_publication_summary_reopens_with_exact_authenticated_source_facts()
-> Result<(), Box<dyn std::error::Error>> {
    use backend_engine::schema::WorkspaceLog;
    use backend_engine::{HashChainJournal, JournalCodec, WorkspaceRecord};

    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let source_root = Path::new(&label);
    let workspace = source_root
        .parent()
        .ok_or("workspace directory missing")?
        .to_path_buf();
    fs::create_dir(source_root.join("src"))?;
    fs::write(source_root.join("src/site.css"), ".button { color: red }")?;
    fs::write(source_root.join("src/query.sql"), "select 1;")?;
    fs::write(
        source_root.join("Cargo.toml"),
        "[package]\nname = \"journal_rust_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    // About 0.8 MB of authored source, in individually bounded Rust files.
    // Docs survive the real parser's 4 KiB per-text limit; no synthetic record
    // lengths or unchecked relation rows manufacture the persisted payload.
    let mut modules = String::new();
    let documentation = "d".repeat(3 * 1024);
    let mut source_bytes = 0usize;
    for module in 0..64 {
        let mut text = String::new();
        for function in 0..4 {
            text.push_str(&format!("/// {documentation}\npub fn retained_{module}_{function}() -> usize {{ {function} }}\n"));
        }
        source_bytes += text.len();
        fs::write(source_root.join(format!("src/module_{module}.rs")), text)?;
        modules.push_str(&format!("pub mod module_{module};\n"));
    }
    source_bytes += modules.len();
    fs::write(source_root.join("src/lib.rs"), modules)?;
    assert!((750_000..850_000).contains(&source_bytes));
    let (expected_facts, expected_summary, selected_root, selected_sequence) = {
        let (adapter, daemon) = fixture.parts();
        // This test commits through the synchronous route, which carries no SQL
        // custody; `query.sql` stays counted as unsupported here, and the owned
        // route's SQL analysis is covered by the SQL journeys.
        let mut work = capture_index_scan(
            daemon,
            package,
            &label,
            source_root,
            None,
            0xB401,
            CompileExecutionIntent::Interactive,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
        )?;
        work.capture_sql = false;
        let scan = run_index_scan(work)
        .map_err(|failure| match failure {
            IndexScanFailure::Cancelled => {
                BuiltinModelError("Rust scan unexpectedly cancelled".into())
            }
            IndexScanFailure::Refused(error) => error,
        })?;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let prepared = prepare_source_scan(
            &snapshot,
            scan,
            &adapter.compiler,
            &mut adapter.semantic_authority,
            None,
            None,
            &mut || {},
        )?;
        let PreparedSourceScan::Capture { selection, .. } = prepared else {
            return Err("valid Rust source did not produce a capture".into());
        };
        let intent = selection.intent.ok_or("Rust preparation lost its intent")?;
        let expected_facts = intent.source_facts_changes().to_vec();
        assert_eq!(
            expected_facts.len(),
            65,
            "64 modules and lib.rs retain full facts"
        );
        assert!(
            intent
                .changes()
                .iter()
                .filter_map(|change| change.after.as_ref())
                .filter_map(|record| record.file_fields())
                .flat_map(|file| file.declarations.iter())
                .any(|declaration| declaration.name() == "retained_63_3")
        );
        super::super::commit_builtin_intent(daemon, 0xB402, &intent)?;
        let selected = daemon.engine().daemon().owner().snapshot();
        let summary = selected.with_persisted_transition(|persisted, _store| {
            let closure = fresh_prepared_closure_bytes(persisted)?;
            let lengths = [
                persisted.manifest_bytes().len(),
                persisted.delta_bytes().len(),
                persisted.commit_bytes().len(),
                closure.len(),
            ];
            assert!(lengths.iter().all(|length| *length <= 64 * 1024 * 1024));
            assert!(lengths.iter().sum::<usize>() > 225);
            Ok::<_, backend_store::StoreError>(prepared_summary(persisted, &closure))
        })?;
        let summary = summary.map_err(|error| {
            format!("encode selected closure for prepared-summary fixture: {error:?}")
        })?;
        (
            expected_facts,
            summary,
            *selected.root().as_bytes(),
            selected.sequence(),
        )
    };
    close_journal_fixture(&mut fixture);
    let (journal, history) =
        HashChainJournal::<WorkspaceLog>::open(workspace.join("workspace.journal"))?;
    let summaries = history
        .frames
        .iter()
        .filter(|frame| frame.payload.first() == Some(&4))
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].payload.len(), 225);
    assert_eq!(
        WorkspaceLog::decode(&summaries[0].payload)?,
        expected_summary
    );
    assert!(matches!(
        expected_summary,
        WorkspaceRecord::PreparedSummary { .. }
    ));
    let report = backend_engine::workspace::recovery::inspect(&journal)?;
    assert_eq!(
        (report.prepared, report.selected, report.published),
        (1, 1, 1)
    );
    drop(journal);
    let owner = reopen_journal_owner(&workspace)?;
    assert_eq!(*owner.snapshot().root().as_bytes(), selected_root);
    assert_eq!(owner.snapshot().sequence(), selected_sequence);
    let recovered = owner.snapshot();
    let progress = crate::builtin::progress::ingest_progress(&recovered)?;
    let scope = progress.source_selection_scope().ok_or("selection scope missing")?;
    assert_eq!(scope.project_count, 1);
    assert_eq!(scope.extensions.iter().map(|row| (row.extension.as_str(), row.projects_observed, row.projects_unknown, row.excluded_files)).collect::<Vec<_>>(),
        [("css", 1, 0, 1), ("sql", 1, 0, 1), ("vue", 1, 0, 0)]);
    let relation = product_source_file_facts_relation(&recovered)
        .map_err(|error| error.to_string())?
        .ok_or("recovered facts relation missing")?;
    for change in expected_facts {
        assert_eq!(
            relation
                .lookup(&change.key)
                .map_err(|error| error.to_string())?,
            change.after
        );
    }
    drop(relation);
    drop(recovered);
    drop(owner);
    Ok(())
}

#[test]
fn diagnostic_grammar_quarantine_preserves_real_published_store_head()
-> Result<(), Box<dyn std::error::Error>> {
    use backend_engine::schema::WorkspaceLog;
    use backend_engine::{HashChainJournal, JournalCodec, JournalDomain, JournalError};

    // A future closed record grammar models the same rejection boundary that
    // the previous tags-1–3 reader encounters on today's tag 4. The engine
    // regression separately exercises the exact old decoder and quarantine.
    struct FutureLog;
    impl JournalDomain for FutureLog {
        const DOMAIN: u8 = <WorkspaceLog as JournalDomain>::DOMAIN;
        const TYPE: u16 = <WorkspaceLog as JournalDomain>::TYPE;
        const VERSION: u8 = <WorkspaceLog as JournalDomain>::VERSION;
    }
    impl JournalCodec for FutureLog {
        type Record = Vec<u8>;
        fn encode(record: &Self::Record, output: &mut Vec<u8>) {
            output.extend_from_slice(record);
        }
        fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
            Ok(bytes.to_vec())
        }
    }
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let workspace = Path::new(&label)
        .parent()
        .ok_or("workspace directory missing")?
        .to_path_buf();
    let (root, sequence, legacy) = {
        let (_, daemon) = fixture.parts();
        super::super::commit_builtin_intent(daemon, 0xB501, &BuiltinIntent::add(package, &label)?)?;
        let selected = daemon.engine().daemon().owner().snapshot();
        let legacy = selected.with_persisted_transition(|persisted, _| {
            Ok::<_, backend_store::StoreError>(backend_engine::WorkspaceRecord::Prepared {
                request: persisted.request(),
                transaction: persisted.transaction(),
                manifest: persisted.manifest_bytes().to_vec(),
                delta: persisted.delta_bytes().to_vec(),
                commit: persisted.commit_bytes().to_vec(),
                closure: fresh_prepared_closure_bytes(persisted)?,
            })
        })?;
        let legacy = legacy.map_err(|error| {
            format!("encode selected closure for legacy-journal fixture: {error:?}")
        })?;
        (*selected.root().as_bytes(), selected.sequence(), legacy)
    };
    close_journal_fixture(&mut fixture);
    let path = workspace.join("workspace.journal");
    let (journal, _) = HashChainJournal::<WorkspaceLog>::open(&path)?;
    journal.append(&legacy)?;
    drop(journal);
    // The real owner admits a mixed historical/full and compact-summary log.
    let owner = reopen_journal_owner(&workspace)?;
    assert_eq!(
        (
            *owner.snapshot().root().as_bytes(),
            owner.snapshot().sequence()
        ),
        (root, sequence)
    );
    drop(owner);
    let (journal, _) = HashChainJournal::<WorkspaceLog>::open(&path)?;
    let report = backend_engine::workspace::recovery::inspect(&journal)?;
    assert_eq!(
        (report.prepared, report.selected, report.published),
        (2, 1, 1)
    );
    drop(journal);
    let (future, _) = HashChainJournal::<FutureLog>::open(&path)?;
    let mut unsupported = vec![5];
    unsupported.extend_from_slice(&[0; 32]);
    future.append(&unsupported)?;
    drop(future);
    let original = fs::read(&path)?;
    assert!(matches!(
        HashChainJournal::<WorkspaceLog>::open(&path),
        Err(JournalError::Corrupt("record tag"))
    ));
    assert_eq!(fs::read(&path)?, original);
    // Real WorkspaceOwner recovery must select the authenticated store HEAD
    // before it quarantines unsupported diagnostic history.
    let owner = reopen_journal_owner(&workspace)?;
    assert_eq!(
        (
            *owner.snapshot().root().as_bytes(),
            owner.snapshot().sequence()
        ),
        (root, sequence)
    );
    let quarantine = fs::read_dir(&workspace)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("workspace.diagnostic.corrupt.")
        })
        .collect::<Vec<_>>();
    assert_eq!(quarantine.len(), 1);
    assert_eq!(fs::read(quarantine[0].path())?, original);
    let (fresh, history) = HashChainJournal::<WorkspaceLog>::open(&path)?;
    assert!(history.frames.is_empty());
    assert_eq!(
        backend_engine::workspace::recovery::inspect(&fresh)?.frames,
        0
    );
    drop(fresh);
    drop(owner);
    Ok(())
}

#[test]
#[ignore = "requires NUDOX_TEST_STYLO_WORKER and completed native owner admission"]
fn stylo_css_only_owner_edit_noop_retirement_and_reopen() -> Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let css = Path::new(&label).join("site.css");
    fs::write(&css, ".one { --ink: red; }")?;
    let worker =
        PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_WORKER").ok_or("worker fixture missing")?);
    let artifact = backend_compile::ToolchainArtifact::from_path(&worker, Vec::new())?;
    let permit = Arc::new(
        crate::builtin::stylo_worker::StyloWorkerPermit::from_admitted(
            worker,
            artifact,
            backend_frontend_stylo::support_fingerprint(),
        )
        .map_err(|error| format!("worker permit: {error:?}"))?,
    );
    let mut first_manifest = None;
    let mut first_native = None;
    let mut first_pointer = None;
    let mut edited_intent = None;
    for step in 0..3 {
        if step == 2 {
            fs::write(&css, ".two { --ink: red; }")?;
        }
        let (adapter, daemon) = fixture.parts();
        let mut work = capture_index_scan(
            daemon,
            package,
            &label,
            Path::new(&label),
            None,
            800 + step,
            CompileExecutionIntent::Interactive,
            false,
            Arc::new(AtomicBool::new(false)),
            adapter.compiler.python_interpreter_admission(),
        )?;
        work.stylo_worker = Some(Arc::clone(&permit));
        let scan = run_index_scan(work).map_err(|failure| match failure {
            IndexScanFailure::Cancelled => BuiltinModelError("Stylo scan cancelled".into()),
            IndexScanFailure::Refused(error) => error,
        })?;
        let base = daemon.engine().daemon().owner().snapshot();
        let prepared = prepare_source_scan(
            &base,
            scan,
            &adapter.compiler,
            &mut adapter.semantic_authority,
            None,
            None,
            &mut || {},
        )?;
        let PreparedSourceScan::Ready(selection) = prepared else {
            return Err("CSS-only scan requested compiler capture".into());
        };
        if step == 0 {
            let original = selection.intent.as_ref().ok_or("missing initial native intent")?;
            let decoded = BuiltinIntent::decode(&original.encode())?;
            let refusal = super::super::commit_builtin_intent(daemon, 899, &decoded)
                .expect_err("decoded native claims lack the private capture proof");
            assert!(refusal.to_string().contains("native Stylo publication has no live source proof"),
                "unexpected refusal: {refusal}");
        }
        match selection.intent {
            Some(intent) if step != 1 => {
                if step == 2 { edited_intent = Some(intent.clone()); }
                if step == 0 {
                    let base = daemon.engine().daemon().owner().snapshot();
                    let staged = crate::builtin::staged_transport::stage_for_test(
                        intent, &base, Some(Path::new(&label)), Arc::new(AtomicBool::new(false)),
                    )?;
                    assert!(staged.staged().is_some(), "initial native owner fixture must exercise staged hydration");
                    super::super::commit_builtin_intent(daemon, 900 + step, &staged)?;
                } else {
                    super::super::commit_builtin_intent(daemon, 900 + step, &intent)?;
                }
            }
            None if step == 1 => {}
            _ => return Err("native no-op or changed-source decision was incorrect".into()),
        }
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?
            .ok_or("native Stylo selection missing")?;
        let source = snapshot.relation::<crate::builtin::BuiltinWorkspaceRelation>()?;
        let project = source.lookup(&package.to_bytes())?.ok_or("source Project missing")?;
        let observed = project.project_fields().and_then(|fields| fields.source_selection).ok_or("source selection missing")?;
        assert_eq!(observed.unsupported_extensions.iter().find(|row| row.extension == "css").ok_or("CSS scope missing")?.files, 0);
        let selection = native.lookup(&backend_engine::builtin::stylo_selection_key(package.to_bytes()))?.ok_or("native selection missing")?;
        assert!(matches!(selection, backend_engine::builtin::StyloAnalysisRecord::Selection { policy_complete: false, .. }), "CSS scope zero must not imply complete analysis");
        let native_root = *native.root().as_bytes();
        let manifest_root = *snapshot.root().as_bytes();
        if step == 0 {
            first_native = Some(native_root);
            first_manifest = Some(manifest_root);
            first_pointer = Some(backend_engine::builtin::stylo_analysis_root_object(native.root()));
        }
        if step == 1 {
            assert_eq!(Some(native_root), first_native);
            assert_eq!(Some(manifest_root), first_manifest);
        }
        if step == 2 {
            assert_ne!(Some(native_root), first_native);
            assert_eq!(Some(manifest_root), first_manifest);
            crate::builtin::profile::check_native_stylo_cold_falsifiers(
                &snapshot, edited_intent.as_ref().ok_or("edited intent missing")?,
                first_pointer.clone().ok_or("first pointer missing")?,
            )?;
        }
        let page = native.page(None, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)?;
        let expected = if step == 2 { "two" } else { "one" };
        assert!(page.entries().iter().any(|(_, row)| matches!(row,
            backend_engine::builtin::StyloAnalysisRecord::Fact { fact: backend_engine::builtin::StyloProjectedRow::SourceOccurrence(site), .. } if site.raw == expected)));
    }
    // A CSS-only candidate must recheck its private host fence after a
    // blocking publication barrier, even though the compiler fence is quiet.
    fs::write(&css, ".six { --ink: red; }")?;
    {
        let (adapter, daemon) = fixture.parts();
        let mut work = capture_index_scan(
            daemon, package, &label, Path::new(&label), None, 908,
            CompileExecutionIntent::Interactive, false, Arc::new(AtomicBool::new(false)),
            adapter.compiler.python_interpreter_admission(),
        )?;
        work.stylo_worker = Some(Arc::clone(&permit));
        let scan = run_index_scan(work).map_err(|failure| match failure {
            IndexScanFailure::Cancelled => BuiltinModelError("Stylo scan cancelled".into()),
            IndexScanFailure::Refused(error) => error,
        })?;
        let base = daemon.engine().daemon().owner().snapshot();
        let PreparedSourceScan::Ready(selection) = prepare_source_scan(
            &base, scan, &adapter.compiler, &mut adapter.semantic_authority,
            None, None, &mut || {},
        )? else { return Err("CSS-only scan requested compiler capture".into()); };
        let intent = selection.intent.ok_or("expected changed native intent")?;
        crate::builtin::profile::revalidate_stylo_intent(&base, &intent)?;
        fs::write(&css, ".ten { --ink: red; }")?;
        assert!(crate::builtin::profile::revalidate_stylo_intent(&base, &intent).is_err(),
            "same-length edit must invalidate a native candidate after preparation");
    }
    let selected_root = {
        let (_, daemon) = fixture.parts();
        let snapshot = daemon.engine().daemon().owner().snapshot();
        *backend_engine::builtin::stylo_analysis_relation(&snapshot)?
            .ok_or("missing native relation")?
            .root()
            .as_bytes()
    };
    close_journal_fixture(&mut fixture);
    let workspace = Path::new(&label).parent().ok_or("workspace missing")?;
    let owner = reopen_journal_owner(workspace)?;
    let reopened = owner.snapshot();
    assert_eq!(
        *backend_engine::builtin::stylo_analysis_relation(&reopened)?
            .ok_or("native relation lost on reopen")?
            .root()
            .as_bytes(),
        selected_root
    );
    drop(reopened);
    drop(owner);
    let mut fixture = fixture.reopen();
    {
        let (adapter, daemon) = fixture.parts();
        let forged = BuiltinIntent::remove(package, &label)?;
        let refusal = super::super::commit_builtin_intent(daemon, 909, &forged)
            .expect_err("legacy source-only removal must not orphan native rows");
        assert!(refusal.to_string().contains("native Stylo selection has no target source Project"),
            "unexpected refusal: {refusal}");
        let removal = crate::builtin::commands::index::remove_project_intent(
            daemon,
            package,
            &label,
            &mut adapter.semantic_authority,
        )?
        .ok_or("source project missing before retirement")?;
        super::super::commit_builtin_intent(daemon, 910, &removal)?;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?
            .ok_or("retirement lost native root")?;
        assert!(
            native
                .lookup(&backend_engine::builtin::stylo_selection_key(
                    package.to_bytes()
                ))?
                .is_none()
        );
    }
    close_journal_fixture(&mut fixture);
    let owner = reopen_journal_owner(workspace)?;
    let snapshot = owner.snapshot();
    let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?
        .ok_or("retired root lost on reopen")?;
    assert!(
        native
            .lookup(&backend_engine::builtin::stylo_selection_key(
                package.to_bytes()
            ))?
            .is_none()
    );
    drop(native);
    drop(snapshot);
    drop(owner);
    let mut fixture = fixture.reopen();
    {
        let (_, daemon) = fixture.parts();
        let add = BuiltinIntent::add(package, &label)?;
        super::super::commit_builtin_intent(daemon, 911, &add)?;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?.ok_or("native root missing after re-add")?;
        assert!(native.lookup(&backend_engine::builtin::stylo_selection_key(package.to_bytes()))?.is_none());
        assert!(native.lookup(&backend_engine::builtin::stylo_attempt_key(package.to_bytes()))?.is_none(), "re-add before indexing remains unknown");
    }
    Ok(())
}

#[test]
#[ignore = "requires NUDOX_TEST_STYLO_WORKER and completed native owner admission"]
fn stylo_latest_attempt_preserves_last_served_and_rejects_stale_scan()
-> Result<(), Box<dyn std::error::Error>> {
    use backend_engine::builtin::{
        StyloAnalysisRecord, StyloAttemptArtifact, StyloAttemptOutcome, StyloUnavailable,
    };
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let css = Path::new(&label).join("site.css");
    fs::write(&css, ".one { --ink: red; }")?;
    let worker =
        PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_WORKER").ok_or("worker fixture missing")?);
    let artifact = backend_compile::ToolchainArtifact::from_path(&worker, Vec::new())?;
    let permit = Arc::new(
        crate::builtin::stylo_worker::StyloWorkerPermit::from_admitted(
            worker,
            artifact,
            backend_frontend_stylo::support_fingerprint(),
        )
        .map_err(|error| format!("worker permit: {error:?}"))?,
    );
    fn prepare(
        fixture: &mut AdapterFixture,
        package: backend_engine::PackageKey,
        label: &str,
        permit: Option<Arc<crate::builtin::stylo_worker::StyloWorkerPermit>>,
        request: u64,
    ) -> Result<Option<BuiltinIntent>, Box<dyn std::error::Error>> {
        let (adapter, daemon) = fixture.parts();
        let mut work = capture_index_scan(
            daemon,
            package,
            label,
            Path::new(label),
            None,
            request,
            CompileExecutionIntent::Interactive,
            false,
            Arc::new(AtomicBool::new(false)),
            adapter.compiler.python_interpreter_admission(),
        )?;
        work.stylo_worker = permit;
        let scan = run_index_scan(work).map_err(|failure| match failure {
            IndexScanFailure::Cancelled => BuiltinModelError("Stylo scan cancelled".into()),
            IndexScanFailure::Refused(error) => error,
        })?;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let prepared = prepare_source_scan(
            &snapshot,
            scan,
            &adapter.compiler,
            &mut adapter.semantic_authority,
            None,
            None,
            &mut || {},
        )?;
        let PreparedSourceScan::Ready(selection) = prepared else {
            return Err("CSS-only scan requested compiler capture".into());
        };
        Ok(selection.intent)
    }
    fn prepare_noop(
        fixture: &mut AdapterFixture, package: backend_engine::PackageKey, label: &str,
        permit: &Arc<crate::builtin::stylo_worker::StyloWorkerPermit>, request: u64,
    ) -> Result<crate::builtin::commands::index::PreparedProductSelection, Box<dyn std::error::Error>> {
        let (adapter, daemon) = fixture.parts();
        let mut work = capture_index_scan(daemon, package, label, Path::new(label), None, request,
            CompileExecutionIntent::Interactive, false, Arc::new(AtomicBool::new(false)),
            adapter.compiler.python_interpreter_admission())?;
        work.stylo_worker = Some(Arc::clone(permit));
        let scan = run_index_scan(work).map_err(|failure| match failure {
            IndexScanFailure::Cancelled => BuiltinModelError("no-op scan cancelled".into()),
            IndexScanFailure::Refused(error) => error,
        })?;
        let selected = daemon.engine().daemon().owner().snapshot();
        let PreparedSourceScan::Ready(prepared) = prepare_source_scan(&selected, scan, &adapter.compiler,
            &mut adapter.semantic_authority, None, None, &mut || {})? else {
            return Err("expected no-op source preparation".into());
        };
        assert!(prepared.intent.is_none() && prepared.native_noop_expectation.is_some() && prepared.native_noop_proof.is_some());
        Ok(prepared)
    }
    let first = prepare(
        &mut fixture,
        package,
        &label,
        Some(Arc::clone(&permit)),
        1001,
    )?
    .ok_or("first native intent missing")?;
    {
        let (_, daemon) = fixture.parts();
        super::super::commit_builtin_intent(daemon, 1002, &first)?;
    }
    let observed_root = {
        let (_, daemon) = fixture.parts();
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?
            .ok_or("native root missing")?;
        assert!(matches!(
            native.lookup(&backend_engine::builtin::stylo_attempt_key(
                package.to_bytes()
            ))?,
            Some(StyloAnalysisRecord::Attempt {
                outcome: StyloAttemptOutcome::Observed,
                ..
            })
        ));
        *native.root().as_bytes()
    };
    let unavailable =
        prepare(&mut fixture, package, &label, None, 1003)?.ok_or("unavailable attempt missing")?;
    {
        let (_, daemon) = fixture.parts();
        super::super::commit_builtin_intent(daemon, 1004, &unavailable)?;
    }
    let unavailable_root = {
        let (_, daemon) = fixture.parts();
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?
            .ok_or("native root missing")?;
        assert!(matches!(
            native.lookup(&backend_engine::builtin::stylo_attempt_key(
                package.to_bytes()
            ))?,
            Some(StyloAnalysisRecord::Attempt {
                outcome: StyloAttemptOutcome::Unavailable(StyloUnavailable::ParserUnavailable),
                worker_artifact: StyloAttemptArtifact::NoPermit,
                served_selection: Some(_),
                ..
            })
        ));
        assert!(
            native
                .lookup(&backend_engine::builtin::stylo_selection_key(
                    package.to_bytes()
                ))?
                .is_some()
        );
        *native.root().as_bytes()
    };
    assert_ne!(observed_root, unavailable_root);
    assert!(prepare(&mut fixture, package, &label, None, 1005)?.is_none());
    fs::remove_file(&css)?;
    let deletion = prepare(&mut fixture, package, &label, None, 1006)?
        .ok_or("deleted-host attempt missing")?;
    {
        let (_, daemon) = fixture.parts();
        super::super::commit_builtin_intent(daemon, 1007, &deletion)?;
    }
    {
        let (_, daemon) = fixture.parts();
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?
            .ok_or("native root missing")?;
        assert!(matches!(
            native.lookup(&backend_engine::builtin::stylo_attempt_key(
                package.to_bytes()
            ))?,
            Some(StyloAnalysisRecord::Attempt {
                outcome: StyloAttemptOutcome::Unavailable(_),
                inventory_count: Some(0),
                served_selection: Some(_),
                ..
            })
        ));
        assert!(
            native
                .lookup(&backend_engine::builtin::stylo_selection_key(
                    package.to_bytes()
                ))?
                .is_some(),
            "old facts remain explicitly last-served"
        );
    }
    fs::write(&css, ".one { --ink: red; }")?;
    let recovered = prepare(
        &mut fixture,
        package,
        &label,
        Some(Arc::clone(&permit)),
        1008,
    )?
    .ok_or("recovered attempt missing")?;
    {
        let (_, daemon) = fixture.parts();
        super::super::commit_builtin_intent(daemon, 1009, &recovered)?;
    }
    // An unavailable A is fully prepared on this base but must not rebase
    // over a later successful B publication.
    let old_unavailable = prepare(&mut fixture, package, &label, None, 10089)?
        .ok_or("old unavailable attempt missing")?;
    // A same-length disk edit after a successful no-op scan must fail even
    // while the selected native root itself has not changed.
    let stale_file_noop = prepare_noop(&mut fixture, package, &label, &permit, 10090)?;
    fs::write(&css, ".two { --ink: red; }")?;
    {
        let (adapter, daemon) = fixture.parts();
        let reference = backend_library::PackageReference::parse(label.clone()).map_err(|error| error.to_string())?;
        let error = match adapter.finish_add(daemon, &stale_file_noop, 10091, package, reference, None, Arc::new(AtomicBool::new(false))) {
            Err(super::super::FinishAddError::Failed(error)) => error,
            _ => return Err("stale CSS no-op unexpectedly acknowledged".into()),
        };
        assert!(error.to_string().contains("native Stylo no-op proof"), "unexpected CSS fence refusal: {error}");
    }
    fs::write(&css, ".one { --ink: red; }")?;
    // Prepare another genuine no-op, then delay acknowledgement until after
    // a different native root is committed under the same workspace manifest.
    let old_noop = prepare_noop(&mut fixture, package, &label, &permit, 10095)?;
    let stale_scan = {
        let (adapter, daemon) = fixture.parts();
        let work = capture_index_scan(
            daemon,
            package,
            &label,
            Path::new(&label),
            None,
            1010,
            CompileExecutionIntent::Interactive,
            false,
            Arc::new(AtomicBool::new(false)),
            adapter.compiler.python_interpreter_admission(),
        )?;
        run_index_scan(work).map_err(|failure| match failure {
            IndexScanFailure::Cancelled => BuiltinModelError("stale scan cancelled".into()),
            IndexScanFailure::Refused(error) => error,
        })?
    };
    fs::write(&css, ".two { --ink: red; }")?;
    let newer = prepare(&mut fixture, package, &label, Some(permit), 1011)?
        .ok_or("newer native intent missing")?;
    {
        let (adapter, daemon) = fixture.parts();
        super::super::commit_builtin_intent(daemon, 1012, &newer)?;
        let error = super::super::commit_builtin_intent(daemon, 10125, &old_unavailable)
            .expect_err("older unavailable attempt must not overwrite a newer observed root");
        assert!(error.to_string().contains("native Stylo predecessor changed"), "unexpected stale-attempt refusal: {error}");
        let reference = backend_library::PackageReference::parse(label.clone()).map_err(|error| error.to_string())?;
        let error = match adapter.finish_add(daemon, &old_noop, 1013, package, reference, None, Arc::new(AtomicBool::new(false))) {
            Err(super::super::FinishAddError::Failed(error)) => error,
            _ => return Err("stale native no-op unexpectedly acknowledged".into()),
        };
        assert!(error.to_string().contains("native Stylo selection changed before no-op acknowledgement"), "unexpected no-op refusal: {error}");
        let selected = daemon.engine().daemon().owner().snapshot();
        let error = match prepare_source_scan(
            &selected,
            stale_scan,
            &adapter.compiler,
            &mut adapter.semantic_authority,
            None,
            None,
            &mut || {},
        ) {
            Ok(_) => return Err("stale same-manifest scan unexpectedly admitted".into()),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("native Stylo selection changed while the project scan was running"),
            "unexpected stale refusal: {error}"
        );
    }
    close_journal_fixture(&mut fixture);
    let owner = reopen_journal_owner(Path::new(&label).parent().ok_or("workspace missing")?)?;
    let snapshot = owner.snapshot();
    let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?
        .ok_or("native root lost on reopen")?;
    assert!(matches!(
        native.lookup(&backend_engine::builtin::stylo_attempt_key(
            package.to_bytes()
        ))?,
        Some(StyloAnalysisRecord::Attempt {
            outcome: StyloAttemptOutcome::Observed,
            ..
        })
    ));
    Ok(())
}

#[test]
#[ignore = "requires completed native owner admission"]
fn stylo_attempt_only_unavailable_noop_and_reopen() -> Result<(), Box<dyn std::error::Error>> {
    use backend_engine::builtin::{StyloAnalysisRecord, StyloAttemptArtifact, StyloAttemptOutcome, StyloUnavailable};
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let css = Path::new(&label).join("site.css");
    fs::write(&css, ".one { --ink: red; }")?;
    fn prepare(
        fixture: &mut AdapterFixture, package: backend_engine::PackageKey, label: &str, request: u64,
    ) -> Result<crate::builtin::commands::index::PreparedProductSelection, Box<dyn std::error::Error>> {
        let (adapter, daemon) = fixture.parts();
        let work = capture_index_scan(daemon, package, label, Path::new(label), None, request,
            CompileExecutionIntent::Interactive, false, Arc::new(AtomicBool::new(false)),
            adapter.compiler.python_interpreter_admission())?;
        let scan = run_index_scan(work).map_err(|failure| match failure {
            IndexScanFailure::Cancelled => BuiltinModelError("attempt-only scan cancelled".into()),
            IndexScanFailure::Refused(error) => error,
        })?;
        let selected = daemon.engine().daemon().owner().snapshot();
        let PreparedSourceScan::Ready(prepared) = prepare_source_scan(&selected, scan, &adapter.compiler,
            &mut adapter.semantic_authority, None, None, &mut || {})? else {
            return Err("attempt-only scan requested compiler capture".into());
        };
        Ok(prepared)
    }
    let initial = prepare(&mut fixture, package, &label, 1101)?;
    let intent = initial.intent.ok_or("initial unavailable attempt missing")?;
    { let (_, daemon) = fixture.parts(); super::super::commit_builtin_intent(daemon, 1102, &intent)?; }
    let selected_root = {
        let (_, daemon) = fixture.parts(); let snapshot = daemon.engine().daemon().owner().snapshot();
        let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?.ok_or("attempt-only root missing")?;
        assert!(native.lookup(&backend_engine::builtin::stylo_selection_key(package.to_bytes()))?.is_none());
        assert!(matches!(native.lookup(&backend_engine::builtin::stylo_attempt_key(package.to_bytes()))?,
            Some(StyloAnalysisRecord::Attempt { outcome: StyloAttemptOutcome::Unavailable(StyloUnavailable::ParserUnavailable),
                worker_artifact: StyloAttemptArtifact::NoPermit, served_selection: None, .. })));
        *native.root().as_bytes()
    };
    let noop = prepare(&mut fixture, package, &label, 1103)?;
    assert!(noop.intent.is_none() && noop.native_noop_expectation == Some(Some(selected_root)) && noop.native_noop_proof.is_some());
    fs::write(&css, ".two { --ink: red; }")?;
    {
        let (adapter, daemon) = fixture.parts();
        let reference = backend_library::PackageReference::parse(label.clone()).map_err(|error| error.to_string())?;
        let error = match adapter.finish_add(daemon, &noop, 1104, package, reference, None, Arc::new(AtomicBool::new(false))) {
            Err(super::super::FinishAddError::Failed(error)) => error,
            _ => return Err("stale attempt-only no-op unexpectedly acknowledged".into()),
        };
        assert!(error.to_string().contains("native Stylo no-op proof"), "unexpected refusal: {error}");
    }
    fs::remove_file(&css)?;
    let deleted = prepare(&mut fixture, package, &label, 1105)?.intent.ok_or("deleted scope attempt missing")?;
    { let (_, daemon) = fixture.parts(); super::super::commit_builtin_intent(daemon, 1106, &deleted)?; }
    close_journal_fixture(&mut fixture);
    let owner = reopen_journal_owner(Path::new(&label).parent().ok_or("workspace missing")?)?;
    let snapshot = owner.snapshot();
    let native = backend_engine::builtin::stylo_analysis_relation(&snapshot)?.ok_or("attempt-only root lost on reopen")?;
    assert!(native.lookup(&backend_engine::builtin::stylo_selection_key(package.to_bytes()))?.is_none());
    assert!(matches!(native.lookup(&backend_engine::builtin::stylo_attempt_key(package.to_bytes()))?,
        Some(StyloAnalysisRecord::Attempt { outcome: StyloAttemptOutcome::Unavailable(_), inventory_count: Some(0), served_selection: None, .. })));
    Ok(())
}
