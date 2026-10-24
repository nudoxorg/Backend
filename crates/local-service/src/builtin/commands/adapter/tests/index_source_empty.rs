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
fn unsupported_vue_count_change_publishes_without_supported_source_change_and_unchanged_repeat_is_noop()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    fs::write(
        Path::new(&label).join("App.vue"),
        "<template>unsupported</template>",
    )?;
    let (adapter, daemon) = fixture.parts();
    let initial_head = daemon.engine().daemon().owner().head();
    let mut previous = (*initial_head.root().as_bytes(), initial_head.sequence());
    let mut supported_source_version = None;
    for (request, expected) in [(171_u8, 1_u64), (172, 2), (173, 2)] {
        if request == 172 {
            fs::write(
                Path::new(&label).join("Other.vue"),
                "<script>not a TS compiler member</script>",
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
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while adapter.indexing.is_some() {
            assert!(adapter.poll_deferred(daemon).is_empty());
            assert!(
                std::time::Instant::now() < deadline,
                "Vue observation must settle without a compiler capture"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let observed = adapter
            .resolve_index_operation(daemon, operation, None)
            .map_err(|error| error.to_string())?;
        let backend_library::IndexOperationObservation::Known(status) = observed else {
            return Err(
                BuiltinModelError("Vue observation lost its durable operation".into()).into(),
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
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| error.to_string())?;
        let row = relation
            .lookup(&package.to_bytes())
            .map_err(|error| error.to_string())?
            .ok_or("published Vue observation has no project row")?;
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
            fields.source_selection.expect("newly committed source selection is present").unsupported_extensions[0].files,
            expected
        );
        let health = crate::builtin::ingest_progress(&snapshot)?;
        assert_eq!(health.files_discovered(), 0);
        assert_eq!(
            health.source_selection().expect("ingest progress exposes the committed source selection").unsupported_extensions[0].files,
            expected,
            "health reads the newly committed observation, including on the reuse/no-compile branch"
        );
        let head = daemon.engine().daemon().owner().head();
        let current = (*head.root().as_bytes(), head.sequence());
        if request == 173 {
            assert_eq!(
                current, previous,
                "an identical observation is a real no-op"
            );
        } else {
            assert_ne!(
                current, previous,
                "a changed exclusion count must publish its project row"
            );
        }
        finish_search_lane(adapter, daemon);
        let head = daemon.engine().daemon().owner().head();
        previous = (*head.root().as_bytes(), head.sequence());
    }
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
) -> Result<backend_engine::WorkspaceOwner<BuiltinModel>, Box<dyn std::error::Error>> {
    Ok(backend_engine::WorkspaceOwner::open_with_registry(
        workspace,
        BuiltinModel,
        genesis()?,
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
        manifest_digest: *blake3::hash(persisted.manifest_bytes()).as_bytes(),
        delta_bytes: persisted.delta_bytes().len() as u64,
        delta_digest: *blake3::hash(persisted.delta_bytes()).as_bytes(),
        commit_bytes: persisted.commit_bytes().len() as u64,
        commit_digest: *blake3::hash(persisted.commit_bytes()).as_bytes(),
        closure_bytes: closure.len() as u64,
        closure_digest: *blake3::hash(closure).as_bytes(),
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
        let scan = run_index_scan(capture_index_scan(
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
        )?)
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
