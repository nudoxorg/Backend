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
