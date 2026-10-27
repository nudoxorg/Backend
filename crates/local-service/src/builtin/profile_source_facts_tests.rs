//! Real source-parser facts through the production commit preparation and store.
//! These storage laws do not stand in for semantic compiler acceptance.
use super::membership_tests::{TempWorkspace, open_daemon};
use super::*;
use crate::builtin::{read_package_sources, view_build};
use crate::test_support::TestValue as _;
use backend_engine::builtin::{
    DeclarationRetention, ProductSourceFileFactsAdmission, ProductSourceFileFactsRecord,
    ProductSourceFileFactsUpdate, admit_product_source_file_facts, build_product_source_file_facts,
    product_source_file_facts_relation,
};
use backend_engine::{PackageKey, ProductSourceRecord, SourceLanguage};
use backend_version::{ContentId, SourceFactDomain};
use std::path::Path;

#[test]
fn staged_semantic_and_capture_descriptors_leave_the_queue_but_replay_exactly() -> Result<(), String>
{
    use backend_engine::QueueSized;
    use backend_engine::builtin::{ProductSemanticCaptureOutcome, SemanticSourceCapture};
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, TypeScriptSource};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    // This is a typed storage/queue counterexample, not compiler acceptance.
    // Every key/count uses the production constructors and unchanged limits.
    let count = 12_000;
    assert!(count <= backend_engine::application::MAX_MANIFEST_ENTRIES);
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let before = daemon.engine().daemon().owner().snapshot();
    let label = "pkg:npm/staged-semantic-descriptors@1.0.0";
    let package = PackageKey::from_value(label);
    let reference = backend_engine::PackageReference::parse(label.to_owned()).test_value()?;
    let prefix = "directory/".repeat(20);
    let keys = (0..count)
        .map(|index| {
            ProductSemanticPublicationKey::new(
                reference.clone(),
                PackageUrl::parse(format!("{label}#{prefix}file_{index:05}.ts")).test_value()?,
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            )
            .test_value()
        })
        .collect::<Result<Vec<_>, String>>()?;
    let capture = SemanticSourceCapture::new(None, [0x71; 32], [0x72; 32], 1, 1).test_value()?;
    let semantic = keys
        .iter()
        .map(|key| BuiltinSemanticChange {
            key: key.clone(),
            after: Some(ProductSemanticPublicationRecord::Unavailable(
                backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority,
            )),
        })
        .collect::<Vec<_>>();
    let captures = keys
        .iter()
        .map(|key| BuiltinCaptureChange {
            cargo_target: None,
            key: key.clone(),
            expected: None,
            capture,
            outcome: ProductSemanticCaptureOutcome::Pending { prior: None },
            compiler_failure: None,
        })
        .collect::<Vec<_>>();
    let inline = BuiltinIntent::index_with_capture(package, label, Vec::new(), semantic, captures)
        .test_value()?
        .with_capture_basis(capture_basis_for_snapshot(&before).test_value()?)
        .test_value()?;
    let canonical = inline.encode();
    let request = BuiltinModel.request_id(&inline);
    assert!(inline.queue_bytes() > 4 * 1024 * 1024);
    assert!(matches!(
        daemon.client().request(
            0,
            crate::Request::Commit {
                request,
                expected: daemon.engine().daemon().owner().head().expectation(),
                intent: inline.clone(),
            }
        ),
        Err(crate::LocaldError::Queue(backend_engine::QueueError::Bytes))
    ));
    // Prior staging retained precisely these two descriptor arrays.
    let mut prior_carrier = inline.clone();
    prior_carrier.changes = Box::new([]);
    prior_carrier.source_facts_changes = Box::new([]);
    let prior_descriptor_bytes = prior_carrier.staging_parts().0.len();
    assert!(prior_descriptor_bytes > 4 * 1024 * 1024);

    let cancellation = Arc::new(AtomicBool::new(false));
    let prepared =
        super::super::staged_transport::stage(inline, &before, None, Arc::clone(&cancellation))
            .test_value()?;
    assert_eq!(prepared.semantic_changes().len(), count);
    assert_eq!(prepared.capture_changes().len(), count);
    assert!(prepared.queue_bytes() > 4 * 1024 * 1024);
    // Keep the full prepared value for the actual production transfer below.
    let staged = prepared.clone().into_queue_carrier();
    eprintln!(
        "descriptor counterexample count={count} canonical={} prior retained={} carrier queue={}",
        canonical.len(),
        prior_descriptor_bytes,
        staged.queue_bytes()
    );
    assert!(staged.semantic_changes().is_empty());
    assert!(staged.capture_changes().is_empty());
    assert!(staged.semantic_selection.is_none());
    assert!(staged.checked_queue_bytes().test_value()? < 4096);
    assert_eq!(BuiltinModel.request_id(&staged), request);
    let evidence = staged.staged().test_value()?;
    let exact = evidence.hydrate(Some(&before)).test_value()?;
    assert_eq!(exact.encode(), canonical);
    let queue = backend_engine::BoundedQueue::new(backend_engine::DaemonConfig::default().commands);
    let owners = Arc::strong_count(evidence);
    queue.try_push(staged.clone()).test_value()?;
    assert_eq!(Arc::strong_count(evidence), owners + 1);
    assert_eq!(
        queue.usage().bytes,
        staged.checked_queue_bytes().test_value()?
    );
    cancellation.store(true, Ordering::Release);
    let draining = queue.try_pop().test_value()?;
    assert!(Arc::ptr_eq(draining.staged().test_value()?, evidence));
    assert!(
        draining
            .staged()
            .test_value()?
            .hydrate(Some(&before))
            .is_err()
    );
    assert_eq!(queue.usage().bytes, 0);
    drop(draining);
    assert_eq!(Arc::strong_count(evidence), owners);
    cancellation.store(false, Ordering::Release);
    super::super::commands::commit_builtin_intent(&mut daemon, 1, &prepared).test_value()?;
    let selected = daemon.engine().daemon().owner().snapshot();
    let root = selected.root();
    assert_eq!(
        daemon.engine().daemon().owner().head().request_identity(),
        request
    );
    assert!(
        evidence.hydrate(Some(&selected)).is_err(),
        "exact base fence refuses stale replay"
    );
    let assert_rows = |snapshot: &backend_engine::WorkspaceSnapshot| -> Result<(), String> {
        let semantic = snapshot
            .relation::<BuiltinSemanticRelation>()
            .test_value()?;
        let captures = semantic_capture_relation(snapshot)
            .test_value()?
            .test_value()?;
        for key in &keys {
            assert!(matches!(
                semantic.lookup(key).test_value()?,
                Some(ProductSemanticPublicationRecord::Unavailable(_))
            ));
            assert!(captures.lookup(key).test_value()?.is_some());
        }
        Ok(())
    };
    assert_rows(&selected)?;
    drop(selected);
    drop(exact);
    drop(staged);
    drop(prepared);
    drop(before);
    drop(daemon);
    let cold = open_daemon(workspace.0.path());
    let selected = cold.engine().daemon().owner().snapshot();
    assert_eq!(selected.root(), root);
    assert_rows(&selected)?;
    Ok(())
}

fn parsed_file(
    package: PackageKey,
    path: &str,
    count: usize,
) -> (ProductSourceRecord, ProductSourceFileFactsUpdate) {
    let source = (0..count).map(|index| format!(
        "/** Actual function {index}. */\nexport function function_{index:04}(): number {{ return {index}; }}\n"
    )).collect::<String>();
    let frontend =
        backend_frontend_typescript::syntax_frontend().expect("actual TypeScript frontend");
    let analysis = frontend
        .analyze(Path::new(path), source.as_bytes())
        .expect("real source parse");
    parsed_analysis_file(package, path, &source, &analysis)
}

fn parsed_analysis_file(
    package: PackageKey,
    path: &str,
    source: &str,
    analysis: &backend_compile::SourceAnalysis,
) -> (ProductSourceRecord, ProductSourceFileFactsUpdate) {
    let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
    let record = ProductSourceRecord::identified_file_within_row_capacity(
        package.to_bytes(),
        path,
        SourceLanguage::TypeScript,
        analysis.content().to_bytes(),
        [0x61; 32],
        analysis.declarations().clone(),
        identity,
    )
    .expect("capacity-aware source row");
    let facts = build_product_source_file_facts(
        package.to_bytes(),
        path,
        SourceLanguage::TypeScript,
        analysis.content().to_bytes(),
        [0x61; 32],
        identity,
        analysis.declarations(),
    )
    .expect("complete parser-produced facts");
    (record, facts)
}

fn prepare_files(
    daemon: &super::super::ProductDaemon,
    package: PackageKey,
    label: &str,
    request: u64,
    files: &[(ProductSourceRecord, Option<ProductSourceFileFactsUpdate>)],
) -> (BuiltinIntent, Vec<BuiltinSourceFactsChange>) {
    prepare_files_at_source_version(daemon, package, label, [request as u8; 32], files)
}

fn prepare_files_at_source_version(
    daemon: &super::super::ProductDaemon,
    package: PackageKey,
    label: &str,
    source_version: [u8; 32],
    files: &[(ProductSourceRecord, Option<ProductSourceFileFactsUpdate>)],
) -> (BuiltinIntent, Vec<BuiltinSourceFactsChange>) {
    let base = daemon.engine().daemon().owner().snapshot();
    let sources = base
        .relation::<BuiltinWorkspaceRelation>()
        .expect("selected source relation");
    let facts = product_source_file_facts_relation(&base).expect("selected facts relation");
    let old_files = sources
        .lookup(&package.to_bytes())
        .expect("prior project")
        .and_then(|row| {
            row.project_fields().map(|project| match project.files {
                backend_engine::ProductProjectFileMembership::Inline(files) => files.to_vec(),
                backend_engine::ProductProjectFileMembership::Paged { .. } => {
                    panic!("small fixture membership")
                }
            })
        })
        .unwrap_or_default();
    let mut selected = files
        .iter()
        .map(|(row, _)| {
            let file = row.file_fields().expect("source file");
            (
                backend_engine::product_source_file_key(package.to_bytes(), file.path),
                row.clone(),
            )
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|(key, _)| *key);
    let updates = files
        .iter()
        .filter_map(|(_, facts)| facts.clone())
        .collect::<Vec<_>>();
    let fact_changes = super::super::commands::prepare_source_facts_changes(
        &sources,
        facts.as_ref(),
        updates,
        &selected,
        &old_files,
    )
    .expect("production facts preparation");
    let keys = selected.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let project = ProductSourceRecord::project(label, source_version, keys.clone())
        .expect("real test project membership");
    let mut changes = vec![BuiltinSourceChange {
        key: package.to_bytes(),
        after: Some(project),
    }];
    changes.extend(selected.into_iter().map(|(key, row)| BuiltinSourceChange {
        key,
        after: Some(row),
    }));
    changes.extend(
        old_files
            .iter()
            .filter(|key| !keys.contains(key))
            .map(|key| BuiltinSourceChange {
                key: *key,
                after: None,
            }),
    );
    let intent = BuiltinIntent::index_with_source_facts(
        package,
        label,
        changes,
        Vec::new(),
        Vec::new(),
        fact_changes.clone(),
    )
    .expect("atomic source/facts intent");
    (intent, fact_changes)
}

#[test]
#[ignore = "requires an explicitly acquired official archive in NUDOX_STAGED_CORPUS_SOURCE"]
fn staged_official_archive_source_facts_use_real_queue_and_cold_replay() -> Result<(), String> {
    use backend_engine::QueueSized;
    use std::sync::{Arc, atomic::AtomicBool};
    let root = std::path::PathBuf::from(
        std::env::var_os("NUDOX_STAGED_CORPUS_SOURCE").expect("explicit official source path"),
    )
    .canonicalize()
    .expect("physical acquired source root");
    let label = root.to_str().expect("UTF-8 acquired source path");
    let package = PackageKey::from_value(label);
    let scan = super::super::ingest::scan_project_for_unproven_authorities_cancellable(
        label,
        package.to_bytes(),
        &std::collections::BTreeMap::new(),
        &AtomicBool::new(false),
    )
    .expect("actual production source discovery and parser facts");
    let mut facts = scan
        .source_facts
        .into_iter()
        .map(|facts| (facts.manifest_key(), facts))
        .collect::<std::collections::BTreeMap<_, _>>();
    let files = scan
        .files
        .into_iter()
        .map(|(key, record)| (record, facts.remove(&key)))
        .collect::<Vec<_>>();
    assert!(
        facts.is_empty(),
        "every facts manifest belongs to a scanned source file"
    );
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let (inline, expected_facts) =
        prepare_files_at_source_version(&daemon, package, label, scan.source_version, &files);
    eprintln!(
        "official source={label} scanned files={} parser fact changes={} inline queue bytes={} source bytes read={}",
        files.len(),
        expected_facts.len(),
        inline.queue_bytes(),
        scan.source_bytes_read
    );
    assert!(
        inline.queue_bytes() > 4 * 1024 * 1024,
        "actual archive exceeds the unchanged inline gate"
    );
    let before = daemon.engine().daemon().owner().snapshot();
    let request = BuiltinModel.request_id(&inline);
    assert!(
        daemon
            .client()
            .request(
                1,
                crate::Request::Commit {
                    request,
                    expected: daemon.engine().daemon().owner().head().expectation(),
                    intent: inline.clone(),
                }
            )
            .is_err()
    );
    let staged = super::super::staged_transport::stage(
        inline,
        &before,
        Some(&root),
        Arc::new(AtomicBool::new(false)),
    )
    .expect("actual captured archive bytes and full parser facts stage");
    eprintln!(
        "official staged queue bytes={} pointer bytes={}",
        staged.queue_bytes(),
        staged.encode().len()
    );
    assert_eq!(BuiltinModel.request_id(&staged), request);
    assert!(staged.queue_bytes() < 4 * 1024 * 1024);
    super::super::commands::commit_builtin_intent(&mut daemon, 2, &staged)
        .expect("real queue and owner publish the exact acquired source facts");
    let selected_root = daemon.engine().daemon().owner().snapshot().root();
    let assert_exact = |daemon: &super::super::ProductDaemon| -> Result<(), String> {
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let relation = product_source_file_facts_relation(&snapshot)
            .test_value()?
            .test_value()?;
        for expected in &expected_facts {
            assert_eq!(
                relation.lookup(&expected.key).test_value()?.as_ref(),
                expected.after.as_ref()
            );
        }
        Ok(())
    };
    assert_exact(&daemon)?;
    drop(staged);
    drop(before);
    drop(daemon);
    let cold = open_daemon(workspace.0.path());
    assert_eq!(cold.engine().daemon().owner().head().root(), selected_root);
    assert_exact(&cold)?;
    Ok(())
}

fn commit_files(
    daemon: &mut super::super::ProductDaemon,
    package: PackageKey,
    label: &str,
    request: u64,
    files: &[(ProductSourceRecord, Option<ProductSourceFileFactsUpdate>)],
) -> Vec<BuiltinSourceFactsChange> {
    let (intent, fact_changes) = prepare_files(daemon, package, label, request, files);
    super::super::commands::commit_builtin_intent(daemon, request, &intent)
        .expect("real owner commit");
    fact_changes
}

fn assert_complete_structural_query_corpus(
    label: &str,
    source: &str,
    check_compact: impl FnOnce(
        backend_engine::builtin::ProductFileRef<'_>,
        &[backend_compile::SourceDeclaration],
    ),
) -> Result<(), String> {
    use backend_compile::{Container, SourceBindingIdentity};
    use backend_engine::RowId;
    use backend_engine::application::{
        LocalCompilerClient, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths,
        LocalCompilerScratch, LocalCompilerTimeout, LocalRuntimePackageAuthority,
        LocalRuntimeToolchain,
    };
    use backend_semantic::vocabulary::NativeTool;
    use std::collections::{BTreeMap, BTreeSet};
    use std::num::NonZeroUsize;

    // Real parser output and production persistence/admission; no semantic
    // compiler publication is manufactured for this structural query.
    let path = "complete.ts";
    let package = PackageKey::from_value(label);
    let analysis = backend_frontend_typescript::syntax_frontend()
        .expect("actual TypeScript frontend")
        .analyze(Path::new(path), source.as_bytes())
        .expect("actual source analysis");
    let declarations = analysis.declarations();
    let (record, facts) = parsed_analysis_file(package, path, source, &analysis);
    check_compact(record.file_fields().expect("compact file"), declarations);
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    commit_files(
        &mut daemon,
        package,
        label,
        1,
        &[(record.clone(), Some(facts))],
    );
    drop(daemon);
    let cold = open_daemon(workspace.0.path());
    let snapshot = cold.engine().daemon().owner().snapshot();
    let sources = crate::builtin::read_indexed_sources(&snapshot).expect("cold selected sources");
    assert_eq!(sources.files.len(), 1);
    assert_eq!(
        sources.files[0].1, record,
        "compact row survives persistence"
    );
    assert!(matches!(
        sources
            .source_snapshot
            .as_ref()
            .expect("selected closure")
            .admit_complete_file_facts(&sources.files[0].1)
            .expect("verified selected facts")
            .expect("complete facts manifest"),
        ProductSourceFileFactsAdmission::PagedVerified(_)
    ));

    let compiler_root = workspace.0.path().join("query-compiler");
    let compiler = LocalCompilerClient::start(
        LocalCompilerRuntimeConfiguration::new(
            LocalCompilerRuntimePaths::new(
                compiler_root.join("artifacts"),
                compiler_root.join("journal"),
                compiler_root.join("native-work"),
            )
            .test_value()?,
            vec![LocalRuntimeToolchain::unavailable(NativeTool::Python)].into_boxed_slice(),
            Box::new([]),
            LocalRuntimePackageAuthority::default(),
            LocalCompilerTimeout::new(std::time::Duration::from_secs(2)).test_value()?,
            backend_store::journal::PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
                .test_value()?,
            LocalCompilerScratch::with_fragment_capacity(
                NonZeroUsize::new(1024 * 1024).test_value()?,
            )
            .test_value()?,
        )
        .test_value()?,
    )
    .expect("compiler storage client; no compilation is requested");
    let corpus = view_build::semantic_query_corpus(
        &snapshot,
        &compiler,
        &sources,
        &mut crate::builtin::generation_residence::SemanticGenerationResidence::default(),
        &mut view_build::ImageRowResidence::default(),
    )
    .expect("actual corpus admits every complete structural declaration and parent");

    let package_id = RowId::Package(package).stable_key();
    let module_coordinate = format!("{label}::{path}");
    let id_for = |declaration: &backend_compile::SourceDeclaration| {
        let coordinate = if declaration.kind() == backend_compile::DeclarationKind::Module {
            module_coordinate.clone()
        } else {
            format!(
                "{label}::{path}:{}::{}",
                declaration.line(),
                declaration.name()
            )
        };
        RowId::Symbol(backend_engine::symbol_key(&coordinate)).stable_key()
    };
    let binding_ids = declarations
        .iter()
        .filter_map(|declaration| match declaration.binding_identity() {
            SourceBindingIdentity::Exact(capture) => Some((capture.binding(), id_for(declaration))),
            SourceBindingIdentity::Legacy => None,
        })
        .collect::<BTreeMap<_, _>>();
    let module_id = RowId::Symbol(backend_engine::symbol_key(&module_coordinate)).stable_key();
    let actual = corpus
        .facts()
        .iter()
        .map(|fact| (fact.presentation().id.clone(), fact.presentation()))
        .collect::<BTreeMap<_, _>>();
    let mut expected_ids = declarations.iter().map(&id_for).collect::<BTreeSet<_>>();
    assert_eq!(
        expected_ids.len(),
        declarations.len(),
        "fixture has unique coordinates"
    );
    expected_ids.insert(package_id.clone());
    assert_eq!(
        actual.keys().cloned().collect::<BTreeSet<_>>(),
        expected_ids
    );
    for declaration in declarations.iter() {
        let presentation = actual
            .get(&id_for(declaration))
            .expect("complete declaration query fact");
        let is_module = declaration.kind() == backend_compile::DeclarationKind::Module;
        let parent = if is_module {
            None
        } else {
            Some(match declaration.container() {
                Container::Binding { binding } => binding_ids
                    .get(binding)
                    .expect("exact source parent")
                    .clone(),
                Container::Module => module_id.clone(),
                other => panic!("unexpected fixture parent {other:?}"),
            })
        };
        let documentation = if is_module {
            format!("{} source · {path}", SourceLanguage::TypeScript.name())
        } else if declaration.documentation().is_empty() {
            format!(
                "{} in {path}:{}",
                declaration.kind_name(),
                declaration.line()
            )
        } else {
            declaration.documentation().to_owned()
        };
        assert_eq!(presentation.name, declaration.name());
        assert_eq!(
            presentation.signature.as_deref(),
            Some(declaration.signature())
        );
        assert_eq!(presentation.documentation, documentation);
        assert_eq!(presentation.project.as_deref(), Some(package_id.as_str()));
        assert_eq!(presentation.parent, parent);
        assert!(presentation.related.is_empty());
    }
    Ok(())
}

#[test]
fn structural_query_reads_names_only_prose_from_persisted_complete_facts() -> Result<(), String> {
    let prose = "Documented source contract. ".repeat(40);
    let source = (0..160).map(|index| format!(
        "/** {prose} */\nexport function function_{index:04}(): number {{ return {index}; }}\n"
    )).collect::<String>();
    assert_complete_structural_query_corpus(
        "pkg:npm/names-only-query@1.0.0",
        &source,
        |compact, declarations| {
            assert_eq!(compact.retention, DeclarationRetention::NamesOnly);
            assert_eq!(compact.declarations.len(), declarations.len());
            assert!(
                compact
                    .declarations
                    .iter()
                    .all(|declaration| declaration.signature().is_empty()
                        && declaration.documentation().is_empty())
            );
            assert!(declarations.iter().skip(1).all(|declaration| {
                !declaration.signature().is_empty()
                    && declaration
                        .documentation()
                        .contains("Documented source contract.")
            }));
        },
    )?;
    Ok(())
}

#[test]
fn structural_query_reads_truncated_tail_and_exact_parents_from_persisted_complete_facts()
-> Result<(), String> {
    use backend_compile::{Container, SourceBindingIdentity};
    // All definitions share a line. Source sorting places each a-child before
    // every z-parent, so the compact prefix cannot close these parent edges.
    let source = (0..1200).map(|index| format!(
        "/** Parent {index}. */ function z{index:04}(): number {{ /** Child {index}. */ function a{index:04}(): number {{ return {index}; }} return a{index:04}(); }} "
    )).collect::<String>();
    assert_complete_structural_query_corpus(
        "pkg:npm/truncated-query@1.0.0",
        &source,
        |compact, declarations| {
            assert!(matches!(
                compact.retention,
                DeclarationRetention::Truncated(_)
            ));
            assert!(compact.declarations.len() < declarations.len());
            let retained = compact
                .declarations
                .iter()
                .filter_map(|declaration| match declaration.binding_identity() {
                    SourceBindingIdentity::Exact(capture) => Some(capture.binding()),
                    SourceBindingIdentity::Legacy => None,
                })
                .collect::<std::collections::BTreeSet<_>>();
            assert!(compact.declarations.iter().any(|declaration| matches!(declaration.container(), Container::Binding { binding } if !retained.contains(binding))), "old compact-prefix query emission would leave a missing exact parent");
            assert!(
                declarations
                    .iter()
                    .any(|declaration| declaration.name() == "z1199")
            );
            assert!(
                !compact
                    .declarations
                    .iter()
                    .any(|declaration| declaration.name() == "z1199")
            );
        },
    )?;
    Ok(())
}

fn assert_complete(
    daemon: &super::super::ProductDaemon,
    package: PackageKey,
    path: &str,
    expected_functions: usize,
) {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .expect("source relation");
    let facts = product_source_file_facts_relation(&snapshot)
        .expect("facts relation")
        .expect("present facts root");
    let key = backend_engine::product_source_file_key(package.to_bytes(), path);
    let source = sources
        .lookup(&key)
        .expect("read source")
        .expect("selected source");
    let Some(ProductSourceFileFactsRecord::Manifest(manifest)) =
        facts.lookup(&key).expect("read manifest")
    else {
        panic!("complete manifest lost for {path}")
    };
    let admitted = admit_product_source_file_facts(
        source.file_fields().expect("file"),
        key,
        manifest,
        |key| facts.lookup(key).map_err(|error| error.to_string()),
    )
    .expect("complete tree admission");
    let mut names = std::collections::BTreeSet::new();
    let mut record_function = |declaration: &backend_compile::SourceDeclaration| {
        if declaration.kind() == backend_compile::DeclarationKind::Function {
            let index = declaration
                .name()
                .strip_prefix("function_")
                .expect("source-backed name")
                .parse::<u32>()
                .expect("source function ordinal in this fixture only");
            assert_eq!(
                declaration.line(),
                2 * index + 2,
                "absolute current source line"
            );
            assert!(
                names.insert(declaration.name().to_owned()),
                "duplicate parser fact"
            );
        }
    };
    match admitted {
        ProductSourceFileFactsAdmission::Unavailable(_) => {
            panic!("complete fixture was unavailable")
        }
        ProductSourceFileFactsAdmission::InlineComplete(inline) => {
            for declaration in inline.declarations() {
                record_function(declaration);
            }
        }
        ProductSourceFileFactsAdmission::PagedVerified(mut paged) => {
            paged
                .visit_pages(|page| {
                    for index in 0..page.len() {
                        let declaration = page.declaration(index).expect("admitted declaration");
                        record_function(&declaration.to_owned()?);
                    }
                    Ok(())
                })
                .expect("every retained complete fact page");
        }
    }
    let expected = (0..expected_functions)
        .map(|index| format!("function_{index:04}"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names, expected,
        "complete current source functions survive storage"
    );
}

#[test]
fn staged_large_parser_facts_use_small_queue_exact_shared_cas_and_cold_replay() -> Result<(), String>
{
    use backend_engine::QueueSized;
    use std::sync::{Arc, atomic::AtomicBool};
    let workspace = TempWorkspace::new();
    let label = "pkg:npm/staged-large-parser-facts@1.0.0";
    let package = PackageKey::from_value(label);
    let count = 12_000;
    let (left, left_facts) = parsed_file(package, "left.ts", count);
    let (right, right_facts) = parsed_file(package, "right.ts", count);
    let mut daemon = open_daemon(workspace.0.path());
    let (inline, _) = prepare_files(
        &daemon,
        package,
        label,
        1,
        &[(left, Some(left_facts)), (right, Some(right_facts))],
    );
    assert!(inline.queue_bytes() > 4 * 1024 * 1024);
    eprintln!(
        "actual parser facts inline queue bytes={}",
        inline.queue_bytes()
    );
    let before = daemon.engine().daemon().owner().snapshot();
    let request = BuiltinModel.request_id(&inline);
    assert!(
        daemon
            .client()
            .request(
                1,
                crate::Request::Commit {
                    request,
                    expected: daemon.engine().daemon().owner().head().expectation(),
                    intent: inline.clone(),
                }
            )
            .is_err(),
        "the existing four MiB inline gate remains enforced"
    );
    let stage = |intent| {
        super::super::staged_transport::stage(
            intent,
            &before,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("real parser facts staged into immutable CAS")
    };
    let first = stage(inline.clone());
    #[cfg(unix)]
    let allocation = backend_store::PhysicalAllocationBudget::new(u64::MAX, 4096);
    #[cfg(unix)]
    let first_allocation = allocation
        .admit(before.durable_store().test_value()?)
        .test_value()?;
    let second = stage(inline.clone());
    #[cfg(unix)]
    assert_eq!(
        allocation
            .admit(before.durable_store().test_value()?)
            .test_value()?,
        first_allocation,
        "the exact shared stage allocates no second immutable payload or index"
    );
    assert!(first.queue_bytes() < 4 * 1024 * 1024);
    assert_eq!(BuiltinModel.request_id(&first), request);
    assert_eq!(BuiltinModel.request_id(&second), request);
    let first_evidence = first.staged().expect("first stage");
    let second_evidence = second.staged().expect("shared stage");
    assert_eq!(first_evidence.manifest, second_evidence.manifest);
    assert_eq!(
        first_evidence.membership.id(),
        second_evidence.membership.id()
    );
    assert_eq!(
        first_evidence.hydrate(Some(&before)).test_value()?.encode(),
        inline.encode()
    );
    assert_eq!(
        second_evidence
            .hydrate(Some(&before))
            .test_value()?
            .encode(),
        inline.encode()
    );
    assert_eq!(first.encode().len(), 180);
    #[cfg(unix)]
    {
        let orphan_label = "pkg:npm/staged-unselected-orphan@1.0.0";
        let orphan_bytes = BuiltinIntent::add(PackageKey::from_value(orphan_label), orphan_label)
            .test_value()?
            .encode();
        let orphan = backend_store::TypedObject::from_value(
            &backend_version::ObjectKey::<BuiltinIntentSchema>::from_value(&orphan_bytes),
            &orphan_bytes,
        );
        let store = before.durable_store().test_value()?;
        store.write_object(&orphan).test_value()?;
        let collector = backend_store::FileStore::open_with_registry(
            store.root(),
            64 * 1024 * 1024,
            store.relation_registry().clone(),
        )
        .test_value()?;
        let (completed, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            completed
                .send(collector.collect_garbage(
                    &backend_store::GcRoots::new(),
                    backend_store::GcLimits::default(),
                ))
                .test_value()
        });
        result
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("live staged admission does not hold a global GC barrier")
            .expect("independent normal GC retains exact leased stage pages");
        worker.join().test_value()??;
        assert!(!store.contains_object(orphan.id()).test_value()?);
        assert_eq!(
            first_evidence.hydrate(Some(&before)).test_value()?.encode(),
            inline.encode()
        );
    }
    eprintln!(
        "staged queue bytes={} pointer bytes={} exact members={}",
        first.queue_bytes(),
        first.encode().len(),
        first_evidence.membership.object_count()
    );
    super::super::commands::commit_builtin_intent(&mut daemon, 2, &first)
        .expect("small staged request passes the real owner queue and publication");
    let selected = daemon.engine().daemon().owner().snapshot();
    assert!(selected.closure().stored_membership().is_some());
    assert!(selected.closure().control_manifest().objects().len() <= 128);
    assert_eq!(selected.sequence(), 1);
    assert_complete(&daemon, package, "left.ts", count);
    assert_complete(&daemon, package, "right.ts", count);
    assert!(
        second_evidence.hydrate(Some(&selected)).is_err(),
        "stale base refuses"
    );
    let mut swapped = first.encode();
    swapped[40] ^= 1;
    assert!(
        super::super::staged_transport::StagedIntent::decode(
            &swapped,
            selected.durable_store().test_value()?,
            selected.closure().membership_id(),
        )
        .is_err(),
        "pointer fence cannot be swapped independently of the fixed manifest"
    );
    let selected_root = selected.root();
    let previous_stage_manifest = first_evidence.manifest;
    drop(selected);
    drop(before);
    drop(first);
    drop(second);
    drop(daemon);
    let mut cold = open_daemon(workspace.0.path());
    eprintln!("first selected staged closure reopened cold");
    assert_eq!(cold.engine().daemon().owner().head().root(), selected_root);
    assert_complete(&cold, package, "left.ts", count);
    assert_complete(&cold, package, "right.ts", count);
    let followup_label = "pkg:npm/staged-small-followup@1.0.0";
    let followup = BuiltinIntent::add(PackageKey::from_value(followup_label), followup_label)
        .expect("small followup intent");
    assert!(followup.encode().len() < 1024 * 1024);
    super::super::commands::commit_builtin_intent(&mut cold, 3, &followup)
        .expect("small followup replaces the previous staged evidence scope");
    let followup_snapshot = cold.engine().daemon().owner().snapshot();
    assert_eq!(followup_snapshot.sequence(), 2);
    assert!(
        !followup_snapshot
            .closure()
            .contains(previous_stage_manifest)
            .test_value()?,
        "old staged auxiliary evidence does not accumulate in the new selection"
    );
    let followup_root = followup_snapshot.root();
    drop(followup_snapshot);
    drop(cold);
    let next_cold = open_daemon(workspace.0.path());
    eprintln!("replacement staged closure reopened cold");
    assert_eq!(
        next_cold.engine().daemon().owner().head().root(),
        followup_root
    );
    assert_complete(&next_cold, package, "left.ts", count);
    assert_complete(&next_cold, package, "right.ts", count);
    Ok(())
}

#[test]
fn staged_parser_facts_refuse_changed_source_and_cancel_before_queue() -> Result<(), String> {
    use std::sync::{Arc, atomic::AtomicBool};
    let workspace = TempWorkspace::new();
    let label = "pkg:npm/staged-changed-source-facts@1.0.0";
    let package = PackageKey::from_value(label);
    let (record, facts) = parsed_file(package, "changed.ts", 12_000);
    let daemon = open_daemon(workspace.0.path());
    let (inline, _) = prepare_files(&daemon, package, label, 1, &[(record, Some(facts))]);
    let before = daemon.engine().daemon().owner().snapshot();
    assert!(inline.encode().len() > 1024 * 1024);
    std::fs::write(
        workspace.0.path().join("changed.ts"),
        b"export const changed = true;",
    )
    .test_value()?;
    assert!(
        super::super::staged_transport::stage(
            inline.clone(),
            &before,
            Some(workspace.0.path()),
            Arc::new(AtomicBool::new(false)),
        )
        .is_err(),
        "captured digest differs from actual bytes at admission"
    );
    assert!(
        super::super::staged_transport::stage(
            inline,
            &before,
            None,
            Arc::new(AtomicBool::new(true)),
        )
        .is_err(),
        "cancelled admission does not retry an Interrupted read forever"
    );
    assert_eq!(
        daemon.engine().daemon().owner().snapshot().root(),
        before.root()
    );
    assert_eq!(daemon.engine().daemon().owner().snapshot().sequence(), 0);
    Ok(())
}

#[test]
fn warm_facts_reuse_requires_the_exact_selected_source_and_manifest() -> Result<(), String> {
    let workspace = TempWorkspace::new();
    let label = "pkg:npm/source-facts-reuse-fence@1.0.0";
    let package = PackageKey::from_value(label);
    let (original, original_facts) = parsed_file(package, "source.ts", 2);
    let mut daemon = open_daemon(workspace.0.path());
    commit_files(
        &mut daemon,
        package,
        label,
        1,
        &[(original.clone(), Some(original_facts))],
    );
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let before = daemon.engine().daemon().owner().head().root();
    let sources = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .expect("actual selected sources");
    let facts = product_source_file_facts_relation(&snapshot)
        .expect("selected facts")
        .expect("complete facts root");
    let key = backend_engine::product_source_file_key(package.to_bytes(), "source.ts");
    let (changed, _) = parsed_file(package, "source.ts", 3);
    for (selected, selected_facts, previous) in [
        (vec![(key, changed)], Some(&facts), vec![key]),
        (vec![(key, original.clone())], None, vec![key]),
        (vec![(key, original)], Some(&facts), Vec::new()),
    ] {
        assert!(
            super::super::commands::prepare_source_facts_changes(
                &sources,
                selected_facts,
                Vec::new(),
                &selected,
                &previous,
            )
            .is_err(),
            "missing provenance or changed source cannot reuse an old complete tree"
        );
        assert_eq!(daemon.engine().daemon().owner().head().root(), before);
        assert_complete(&daemon, package, "source.ts", 2);
    }
    Ok(())
}

#[test]
fn warm_complete_facts_survive_real_commits_and_shrinking_pages_are_retired() -> Result<(), String>
{
    let workspace = TempWorkspace::new();
    let label = "pkg:npm/warm-source-facts@1.0.0";
    let package = PackageKey::from_value(label);
    let (large, large_facts) = parsed_file(package, "large.ts", 900);
    assert!(
        !large
            .file_fields()
            .expect("large source")
            .retention
            .is_complete()
    );
    assert!(!large_facts.pages().is_empty());
    let old_page_keys = large_facts
        .pages()
        .iter()
        .map(|(key, _)| *key)
        .collect::<Vec<_>>();
    let (other, other_facts) = parsed_file(package, "other.ts", 1);
    let mut daemon = open_daemon(workspace.0.path());
    commit_files(
        &mut daemon,
        package,
        label,
        1,
        &[
            (large.clone(), Some(large_facts)),
            (other, Some(other_facts)),
        ],
    );
    let (changed_other, changed_other_facts) = parsed_file(package, "other.ts", 2);
    let warm_changes = commit_files(
        &mut daemon,
        package,
        label,
        2,
        &[
            (large.clone(), None),
            (changed_other.clone(), Some(changed_other_facts)),
        ],
    );
    assert!(
        old_page_keys
            .iter()
            .all(|key| !warm_changes.iter().any(|change| change.key == *key)),
        "warm retention emits no deletions or copies of unchanged pages"
    );
    assert_complete(&daemon, package, "large.ts", 900);
    drop(daemon);
    let mut daemon = open_daemon(workspace.0.path());
    assert_complete(&daemon, package, "large.ts", 900);
    assert_complete(&daemon, package, "other.ts", 2);
    let (small, small_facts) = parsed_file(package, "large.ts", 1);
    assert!(small_facts.pages().is_empty());
    commit_files(
        &mut daemon,
        package,
        label,
        3,
        &[(small, Some(small_facts)), (changed_other.clone(), None)],
    );
    assert_complete(&daemon, package, "large.ts", 1);
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let facts = product_source_file_facts_relation(&snapshot)
        .expect("facts relation")
        .expect("facts root");
    for key in old_page_keys {
        assert!(
            facts.lookup(&key).expect("read old page").is_none(),
            "old unreferenced page must leave the selected relation"
        );
    }
    let unavailable = ProductSourceRecord::file_unavailable(
        package.to_bytes(),
        "large.ts",
        SourceLanguage::TypeScript,
        [0x61; 32],
        backend_engine::SourceUnavailableReason::TooLarge,
    )
    .expect("typed unavailable file");
    commit_files(
        &mut daemon,
        package,
        label,
        4,
        &[(unavailable.clone(), None), (changed_other, None)],
    );
    assert_complete(&daemon, package, "other.ts", 2);
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let facts = product_source_file_facts_relation(&snapshot)
        .expect("facts relation")
        .expect("facts root");
    assert!(
        facts
            .lookup(&backend_engine::product_source_file_key(
                package.to_bytes(),
                "large.ts"
            ))
            .expect("unavailable manifest")
            .is_none()
    );
    commit_files(&mut daemon, package, label, 5, &[(unavailable, None)]);
    drop(daemon);
    let daemon = open_daemon(workspace.0.path());
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let facts = product_source_file_facts_relation(&snapshot)
        .expect("facts relation")
        .expect("facts root");
    assert!(
        facts
            .lookup(&backend_engine::product_source_file_key(
                package.to_bytes(),
                "other.ts"
            ))
            .expect("deleted manifest")
            .is_none()
    );
    Ok(())
}

#[test]
fn typed_unavailable_source_facts_do_not_promote_compacted_rows() -> Result<(), String> {
    use backend_engine::builtin::{DeclarationRetention, RetainedDeclarations};
    let workspace = TempWorkspace::new();
    let label = "pkg:npm/unavailable-cold-control@1.0.0";
    let package = PackageKey::from_value(label);
    let mut daemon = open_daemon(workspace.0.path());
    let reasons = [
        backend_library::SourceUnavailableReason::Unreadable,
        backend_library::SourceUnavailableReason::NotText,
        backend_library::SourceUnavailableReason::TooLarge,
        backend_library::SourceUnavailableReason::Unparsed,
    ];
    let mut rows = reasons
        .iter()
        .enumerate()
        .map(|(index, reason)| {
            (
                ProductSourceRecord::file_unavailable(
                    package.to_bytes(),
                    format!("unavailable_{index}.c"),
                    SourceLanguage::Clang,
                    [7; 32],
                    *reason,
                )
                .expect("explicit unavailable row"),
                None,
            )
        })
        .collect::<Vec<_>>();
    // The real mixed-source capture also contains extracted sources. The
    // source-facts intent requires at least one authenticated facts change.
    let (parsed, facts) = parsed_file(package, "source.ts", 1);
    rows.push((parsed, Some(facts)));
    commit_files(&mut daemon, package, label, 1, &rows);
    let snapshot = backend_engine::ProductSourceSnapshot::from_workspace(
        &daemon.engine().daemon().owner().snapshot(),
    )
    .expect("selected source snapshot");
    for ((expected, _), reason) in rows.iter().zip(reasons) {
        let path = expected.file_fields().expect("file").path;
        let key = backend_engine::product_source_file_key(package.to_bytes(), path);
        let row = snapshot
            .relation()
            .lookup(&key)
            .expect("lookup")
            .expect("selected file");
        assert_eq!(&row, expected);
        match snapshot
            .admit_complete_file_facts(&row)
            .expect("typed unavailable admission")
            .expect("typed outcome")
        {
            ProductSourceFileFactsAdmission::Unavailable(actual) => assert_eq!(actual, reason),
            _ => panic!("unavailable source was promoted to complete facts"),
        }
        for retention in [
            DeclarationRetention::ExcerptsElided,
            DeclarationRetention::NamesOnly,
            DeclarationRetention::Truncated(RetainedDeclarations::new(1, 2).expect("counts")),
        ] {
            let mut corrupt = row.clone();
            let ProductSourceRecord::File {
                retention: actual, ..
            } = &mut corrupt
            else {
                panic!("file")
            };
            *actual = retention;
            assert!(
                snapshot.admit_complete_file_facts(&corrupt).is_err(),
                "compacted source requires its complete facts manifest"
            );
        }
        let mut contradictory = row.clone();
        let ProductSourceRecord::File {
            content_version, ..
        } = &mut contradictory
        else {
            panic!("file")
        };
        *content_version = [1; 32];
        assert!(
            snapshot.admit_complete_file_facts(&contradictory).is_err(),
            "Unavailable cannot carry known source content"
        );
    }
    let foreign = ProductSourceRecord::file_unavailable(
        package.to_bytes(),
        "foreign.c",
        SourceLanguage::Clang,
        [7; 32],
        backend_library::SourceUnavailableReason::Unparsed,
    )
    .expect("foreign row");
    assert!(
        snapshot.admit_complete_file_facts(&foreign).is_err(),
        "unavailability must bind the exact selected row"
    );
    Ok(())
}

#[test]
fn structural_calls_rebuild_negative_results_on_source_and_complete_manifest_changes()
-> Result<(), String> {
    fn file(
        package: PackageKey,
        path: &str,
        source: &str,
    ) -> (ProductSourceRecord, Option<ProductSourceFileFactsUpdate>) {
        let analysis = backend_frontend_typescript::syntax_frontend()
            .expect("frontend")
            .analyze(Path::new(path), source.as_bytes())
            .expect("actual parsed declarations");
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
        let record = ProductSourceRecord::identified_file_within_row_capacity(
            package.to_bytes(),
            path,
            SourceLanguage::TypeScript,
            analysis.content().to_bytes(),
            [0x61; 32],
            analysis.declarations().clone(),
            identity,
        )
        .expect("capacity-aware source row");
        let facts = build_product_source_file_facts(
            package.to_bytes(),
            path,
            SourceLanguage::TypeScript,
            analysis.content().to_bytes(),
            [0x61; 32],
            identity,
            analysis.declarations(),
        )
        .expect("complete manifest");
        (record, Some(facts))
    }
    let mut residence = view_build::StructuralCallResidence::default();
    let workspace = TempWorkspace::new();
    let label = "pkg:npm/structural-call-cache@1.0.0";
    let package = PackageKey::from_value(label);
    let mut daemon = open_daemon(workspace.0.path());
    let caller = file(
        package,
        "one/caller.ts",
        "import { validate as check } from './target';\nexport function caller() { check(); }\n",
    );
    let decoy = file(package, "two/target.ts", "export function validate() {}\n");
    commit_files(
        &mut daemon,
        package,
        label,
        1,
        &[caller.clone(), decoy.clone()],
    );
    let initial = read_package_sources(&daemon.engine().daemon().owner().snapshot(), package)
        .expect("exact initial source");
    assert!(
        residence
            .coordinate_pairs(&initial, package)
            .test_value()?
            .is_empty()
    );
    assert!(
        residence
            .coordinate_pairs(&initial, package)
            .test_value()?
            .is_empty(),
        "repeated negative result on the exact selected closure"
    );
    assert_eq!(
        residence.build_count(),
        1,
        "negative hit does no projection work"
    );
    // A second command owner with the same package name has its own exact root
    // and residence. Its requests cannot replace the first owner's projection.
    let other_workspace = TempWorkspace::new();
    let mut other_daemon = open_daemon(other_workspace.0.path());
    commit_files(
        &mut other_daemon,
        package,
        label,
        1,
        &[
            caller.clone(),
            file(package, "one/target.ts", "export function validate() {}\n"),
        ],
    );
    let other_sources =
        read_package_sources(&other_daemon.engine().daemon().owner().snapshot(), package)
            .expect("independent workspace source");
    assert_ne!(
        initial
            .source_snapshot
            .as_ref()
            .test_value()?
            .workspace_root(),
        other_sources
            .source_snapshot
            .as_ref()
            .test_value()?
            .workspace_root()
    );
    let mut other_residence = view_build::StructuralCallResidence::default();
    let other_expected = vec![(
        format!("{label}::one/caller.ts:2::caller"),
        format!("{label}::one/target.ts:1::validate"),
    )];
    assert_eq!(
        other_residence
            .coordinate_pairs(&other_sources, package)
            .test_value()?,
        other_expected
    );
    assert!(
        residence
            .coordinate_pairs(&initial, package)
            .test_value()?
            .is_empty()
    );
    assert_eq!(
        residence.build_count(),
        1,
        "another owner cannot evict this owner"
    );
    let prefix = (0..900)
        .map(|at| format!("export function filler_{at}() {{}}\n"))
        .collect::<String>();
    let target = file(
        package,
        "one/target.ts",
        &(prefix.clone() + "export function validate() {}\n"),
    );
    assert!(
        !target.1.as_ref().test_value()?.pages().is_empty(),
        "target must exercise authenticated paged complete facts"
    );
    commit_files(
        &mut daemon,
        package,
        label,
        2,
        &[caller.clone(), decoy.clone(), target],
    );
    let added = read_package_sources(&daemon.engine().daemon().owner().snapshot(), package)
        .expect("source after module addition");
    assert_ne!(
        initial
            .source_snapshot
            .as_ref()
            .test_value()?
            .workspace_root(),
        added
            .source_snapshot
            .as_ref()
            .test_value()?
            .workspace_root()
    );
    let expected = vec![(
        format!("{label}::one/caller.ts:2::caller"),
        format!("{label}::one/target.ts:901::validate"),
    )];
    assert_eq!(
        residence.coordinate_pairs(&added, package).test_value()?,
        expected,
        "new module invalidates the old negative import and uses the complete declaration beyond the compact row"
    );
    let changed = file(
        package,
        "one/target.ts",
        &(prefix + "export function unrelated() {}\n"),
    );
    // Keep the project version and membership unchanged. The exact file and
    // auxiliary complete-facts manifest alone must invalidate residence.
    let (intent, _) = prepare_files_at_source_version(
        &daemon,
        package,
        label,
        [2; 32],
        &[caller, decoy, changed],
    );
    crate::builtin::commands::commit_builtin_intent(&mut daemon, 3, &intent)
        .expect("commit changed complete facts at the same project version");
    let changed = read_package_sources(&daemon.engine().daemon().owner().snapshot(), package)
        .expect("source after complete manifest edit");
    assert_ne!(
        added
            .source_snapshot
            .as_ref()
            .test_value()?
            .workspace_root(),
        changed
            .source_snapshot
            .as_ref()
            .test_value()?
            .workspace_root()
    );
    assert!(
        residence
            .coordinate_pairs(&changed, package)
            .test_value()?
            .is_empty(),
        "changing the target facts manifest invalidates a positive call projection"
    );
    assert_eq!(
        residence.coordinate_pairs(&added, package).test_value()?,
        expected,
        "retained older exact snapshots remain independent of the latest resident entry"
    );
    assert_eq!(
        residence.build_count(),
        4,
        "each exact root change rebuilds once"
    );
    assert_eq!(
        other_residence
            .coordinate_pairs(&other_sources, package)
            .test_value()?,
        other_expected
    );
    assert_eq!(
        other_residence.build_count(),
        1,
        "interleaving root changes stay owner local"
    );
    Ok(())
}

#[test]
fn capture_only_refusal_survives_cold_reopen_and_qualifies_retained_indexed_rows()
-> Result<(), String> {
    use crate::builtin::{coverage, view_publish};
    use backend_engine::builtin::{
        ProductSemanticCaptureOutcome, SemanticSourceCapture, SemanticUnavailableReason,
    };
    use backend_engine::{Lane, Reason, Row, RowId, RowState, ViewCoverage};
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, TypeScriptSource};

    // Real parser facts, immutable capture relation, publication witness and
    // cold store path. This is not evidence of native compiler acceptance.
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let label = "pkg:npm/observed-capture@1.0.0";
    let package = PackageKey::from_value(label);
    let (file, facts) = parsed_file(package, "retained.ts", 3);
    commit_files(&mut daemon, package, label, 1, &[(file, Some(facts))]);
    let key = ProductSemanticPublicationKey::new(
        backend_engine::PackageReference::parse(label.to_owned()).test_value()?,
        PackageUrl::parse(label).test_value()?,
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
    )
    .test_value()?;
    let before = daemon.engine().daemon().owner().snapshot();
    let source_root = view_publish::source_root(&before).test_value()?;
    let semantic_root = view_publish::semantic_root(&before).test_value()?;
    let capture = SemanticSourceCapture::new(None, [1; 32], [0x72; 32], 1, 1).test_value()?;
    // Capture is the only changed authority. There is no selected semantic
    // row yet: the first pending capture must still qualify the view.
    let intent = BuiltinIntent::index_with_capture(
        package,
        label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            cargo_target: None,
            key: key.clone(),
            expected: None,
            capture,
            outcome: ProductSemanticCaptureOutcome::Pending { prior: None },
            compiler_failure: None,
        }],
    )
    .test_value()?
    .with_capture_basis(capture_basis_for_snapshot(&before).test_value()?)
    .test_value()?;
    super::super::commands::commit_builtin_intent(&mut daemon, 2, &intent).test_value()?;
    let pending = daemon.engine().daemon().owner().snapshot();
    let pending_row = semantic_capture_relation(&pending)
        .test_value()?
        .test_value()?
        .lookup(&key)
        .test_value()?
        .test_value()?;
    assert_eq!(
        view_publish::source_root(&pending).test_value()?,
        source_root
    );
    assert_eq!(
        view_publish::semantic_root(&pending).test_value()?,
        semantic_root
    );
    assert_eq!(
        coverage::view_coverage(
            &pending,
            &coverage::ActivatedProfiles::new(),
            coverage::SemanticDeployment::Unconfigured,
            &Default::default(),
        )
        .test_value()?,
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Partial {
                lane: Lane::Semantic,
                completed: 0,
                total: 1
            }
        ]
    );
    let capture_root = view_publish::capture_root(&pending).test_value()?;
    let witness = view_publish::PublishedRoots {
        source: source_root,
        semantic: semantic_root,
        capture: capture_root,
        activated: coverage::ActivatedProfiles::new(),
    };
    let roots = |capture_target| view_publish::TransitionRoots {
        source_base: source_root,
        source_target: source_root,
        semantic_base: semantic_root,
        semantic_target: semantic_root,
        capture_target,
    };
    assert_eq!(
        view_publish::publication_plan(Some(&witness), roots(capture_root), None),
        view_publish::PublicationPlan::Reuse
    );
    let terminal = BuiltinIntent::index_with_capture(
        package,
        label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            cargo_target: None,
            key: key.clone(),
            expected: Some(pending_row),
            capture,
            outcome: ProductSemanticCaptureOutcome::Unavailable {
                reason: SemanticUnavailableReason::Rejected,
            },
            compiler_failure: None,
        }],
    )
    .test_value()?
    .with_capture_basis(capture_basis_for_snapshot(&pending).test_value()?)
    .test_value()?;
    super::super::commands::commit_builtin_intent(&mut daemon, 3, &terminal).test_value()?;
    let refused = daemon.engine().daemon().owner().snapshot();
    assert_eq!(
        view_publish::source_root(&refused).test_value()?,
        source_root
    );
    assert_eq!(
        view_publish::semantic_root(&refused).test_value()?,
        semantic_root
    );
    let refused_capture_root = view_publish::capture_root(&refused).test_value()?;
    assert_ne!(refused_capture_root, capture_root);
    assert_eq!(
        view_publish::publication_plan(
            Some(&witness),
            roots(refused_capture_root),
            Some(&terminal)
        ),
        view_publish::PublicationPlan::Hydrate
    );
    // The pinned older snapshot keeps its independent in-flight outcome.
    assert_eq!(
        coverage::view_coverage(
            &pending,
            &coverage::ActivatedProfiles::new(),
            coverage::SemanticDeployment::Configured,
            &Default::default(),
        )
        .test_value()?,
        vec![
            ViewCoverage::Complete,
            ViewCoverage::Partial {
                lane: Lane::Semantic,
                completed: 0,
                total: 1
            }
        ]
    );
    assert_complete(&daemon, package, "retained.ts", 3);
    let final_root = refused.root();
    drop(refused);
    drop(pending);
    drop(before);
    drop(daemon);

    let cold = open_daemon(workspace.0.path());
    let snapshot = cold.engine().daemon().owner().snapshot();
    assert_eq!(snapshot.root(), final_root);
    assert_eq!(
        view_publish::capture_root(&snapshot).test_value()?,
        refused_capture_root
    );
    assert_eq!(
        view_publish::publication_plan(None, roots(refused_capture_root), None),
        view_publish::PublicationPlan::Hydrate
    );
    for deployment in [
        coverage::SemanticDeployment::Unconfigured,
        coverage::SemanticDeployment::Configured,
    ] {
        assert_eq!(
            coverage::view_coverage(
                &snapshot,
                &coverage::ActivatedProfiles::new(),
                deployment,
                &Default::default(),
            )
                .test_value()?,
            vec![
                ViewCoverage::Complete,
                ViewCoverage::Unavailable {
                    lane: Lane::Semantic,
                    reason: Reason::Incomplete
                }
            ]
        );
    }
    assert_complete(&cold, package, "retained.ts", 3);
    let sources = read_package_sources(&snapshot, package).test_value()?;
    let (initial, _) = super::super::initial_view().test_value()?;
    let only = sources.files.iter().map(|(key, _)| *key).collect();
    let mut rows =
        view_build::rows_for_structural_files(&initial, &sources, &only, &Default::default())
            .test_value()?;
    for index in 0..3 {
        assert!(
            rows.iter()
                .any(|row| row.label.ends_with(&format!("::function_{index:04}"))),
            "stored parser function {index} must stay readable"
        );
    }
    let declaration_count = rows.len();
    let declarations = rows.clone();
    let sibling = PackageKey::from_value("pkg:npm/unrelated@1.0.0");
    rows.push(Row::new(RowId::Package(package), initial.basis(), label));
    rows.push(Row::new(
        RowId::Package(sibling),
        initial.basis(),
        "unrelated",
    ));
    let sibling_row = rows.last().test_value()?.clone();
    coverage::qualify_package_rows(
        &snapshot,
        &mut rows,
        &Default::default(),
    ).test_value()?;
    assert_eq!(
        &rows[..declaration_count],
        declarations.as_slice(),
        "refusal changed retained declarations"
    );
    assert_eq!(rows[declaration_count].state, RowState::Failed);
    assert_eq!(rows[declaration_count].label, label);
    assert_eq!(
        rows[declaration_count + 1],
        sibling_row,
        "package refusal changed a sibling snapshot"
    );
    Ok(())
}

#[test]
fn retained_published_capture_is_correlated_with_exact_package_source_frontier()
-> Result<(), String> {
    use crate::builtin::{coverage, view_publish};
    use backend_engine::builtin::{
        ProductSemanticCaptureOutcome, SemanticPublicationClaim, SemanticPublicationCoverage,
        SemanticPublicationVersion, SemanticSourceCapture, SemanticUnavailableReason,
    };
    use backend_engine::publication::{
        binding::{COMPILATION_BINDING_BYTES, CompilationBindingView},
        manifest::{CompilationManifestFacts, CompilationManifestFormat},
    };
    use backend_engine::{Lane, Reason, RowState, ViewCoverage, WorkspaceSnapshot};
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, TypeScriptSource};
    use backend_store::hydration::VerifiedGenerationFacts;
    use backend_version::{
        DependencySetDomain, GenerationId, IrManifestDomain, IrManifestEncoding,
    };

    // This is an authenticated storage/correlation law. The structurally valid
    // claim below is not an activated compiler artifact or native acceptance.
    let generation = VerifiedGenerationFacts {
        pinned_root: GenerationId::from_canonical_bytes(b"source-correlation-generation"),
        dep_set: ContentId::<DependencySetDomain>::from_canonical_bytes(b"source-correlation-deps"),
    };
    let manifest =
        backend_version::ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
            b"source-correlation-manifest",
        );
    let mut binding_bytes = [0; COMPILATION_BINDING_BYTES];
    let binding = CompilationBindingView::write_into(generation, manifest, &mut binding_bytes)
        .test_value()?;
    let claim = SemanticPublicationClaim::admit(
        CompilationManifestFacts {
            identity: manifest,
            format: CompilationManifestFormat::SemanticV2,
            fragment_count: 1,
            byte_length: 1,
        },
        *binding,
    )
    .test_value()?;
    let selected_record = ProductSemanticPublicationRecord::Published {
        coverage: SemanticPublicationCoverage::Complete,
        claim,
    };
    let version = SemanticPublicationVersion::new(SemanticPublicationCoverage::Complete, claim);
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let label = "pkg:npm/source-correlated-capture@1.0.0";
    let package = PackageKey::from_value(label);
    let key = ProductSemanticPublicationKey::new(
        backend_engine::PackageReference::parse(label.to_owned()).test_value()?,
        PackageUrl::parse(label).test_value()?,
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
    )
    .test_value()?;
    let active = coverage::ActivatedProfiles::from_iter([(package, key.profile())]);
    use backend_engine::application::{
        LocalCompilerClient, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths,
        LocalCompilerScratch, LocalCompilerTimeout, LocalRuntimePackageAuthority,
        LocalRuntimeToolchain,
    };
    use backend_semantic::vocabulary::NativeTool;
    use std::num::NonZeroUsize;
    let compiler_root = workspace.0.path().join("projection-compiler");
    let compiler = LocalCompilerClient::start(
        LocalCompilerRuntimeConfiguration::new(
            LocalCompilerRuntimePaths::new(
                compiler_root.join("artifacts"),
                compiler_root.join("journal"),
                compiler_root.join("native-work"),
            )
            .test_value()?,
            vec![LocalRuntimeToolchain::unavailable(NativeTool::Python)].into_boxed_slice(),
            Box::new([]),
            LocalRuntimePackageAuthority::default(),
            LocalCompilerTimeout::new(std::time::Duration::from_secs(2)).test_value()?,
            backend_store::journal::PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
                .test_value()?,
            LocalCompilerScratch::with_fragment_capacity(
                NonZeroUsize::new(1024 * 1024).test_value()?,
            )
            .test_value()?,
        )
        .test_value()?,
    )
    .test_value()?;
    let (file, facts) = parsed_file(package, "retained.ts", 3);
    commit_files(
        &mut daemon,
        package,
        label,
        1,
        &[(file.clone(), Some(facts))],
    );
    let capture = SemanticSourceCapture::new(None, [1; 32], [0x72; 32], 1, 1).test_value()?;
    let source_only_intent =
        |package: PackageKey, label: &str, source_version, file: ProductSourceRecord| {
            let fields = file.file_fields().test_value()?;
            let key = backend_engine::product_source_file_key(package.to_bytes(), fields.path);
            let project =
                ProductSourceRecord::project(label, source_version, vec![key]).test_value()?;
            BuiltinIntent::index_with_semantics(
                package,
                label,
                vec![
                    BuiltinSourceChange {
                        key: package.to_bytes(),
                        after: Some(project),
                    },
                    BuiltinSourceChange {
                        key,
                        after: Some(file),
                    },
                ],
                Vec::new(),
            )
            .test_value()
        };
    let commit_capture = |daemon: &mut super::super::ProductDaemon,
                          request,
                          capture,
                          outcome,
                          semantic_changes|
     -> Result<(), String> {
        let base = daemon.engine().daemon().owner().snapshot();
        let expected = match semantic_capture_relation(&base).test_value()? {
            Some(captures) => captures.lookup(&key).test_value()?,
            None => None,
        };
        let intent = BuiltinIntent::index_with_capture(
            package,
            label,
            Vec::new(),
            semantic_changes,
            vec![BuiltinCaptureChange {
                cargo_target: None,
                key: key.clone(),
                expected,
                capture,
                outcome,
                compiler_failure: None,
            }],
        )
        .test_value()?
        .with_capture_basis(capture_basis_for_snapshot(&base).test_value()?)
        .test_value()?;
        super::super::commands::commit_builtin_intent(daemon, request, &intent).test_value()?;
        Ok(())
    };
    let assert_remaining_projection =
        |snapshot: &WorkspaceSnapshot, sibling| -> Result<(), String> {
            let mut generations = crate::builtin::SemanticGenerationResidence::default();
            let (view, current_activated, _) = crate::builtin::view_for_snapshot(
                snapshot,
                &compiler,
                coverage::SemanticDeployment::Configured,
                workspace.0.path(),
                &mut view_build::ImageRowResidence::default(),
                &mut generations,
            )
            .test_value()?;
            assert_eq!(
                generations.owner_calls(),
                0,
                "removed historical generation was activated"
            );
            assert!(
                view.row_ref(backend_engine::RowId::Package(package))
                    .is_none(),
                "removed package was resurrected"
            );
            assert!(
                view.row_refs().all(|row| row.package != Some(package)),
                "removed declarations were resurrected"
            );
            assert!(
                view.row_ref(backend_engine::RowId::Package(sibling))
                    .is_some(),
                "remaining package disappeared"
            );
            assert_eq!(view.coverage(), &[ViewCoverage::Complete]);
            assert!(current_activated.is_empty());
            assert_eq!(
                coverage::view_coverage(
                    snapshot,
                    &current_activated,
                    coverage::SemanticDeployment::Configured,
                    &Default::default(),
                )
                .test_value()?,
                vec![ViewCoverage::Complete]
            );
            Ok(())
        };
    let assert_scope = |snapshot: &WorkspaceSnapshot,
                        state,
                        expected: Vec<ViewCoverage>|
     -> Result<(), String> {
        assert_eq!(
            coverage::package_row_state(
                snapshot,
                key.package(),
                &Default::default(),
            ).test_value()?,
            Some(state)
        );
        assert_eq!(
            coverage::view_coverage(
                snapshot,
                &active,
                coverage::SemanticDeployment::Configured,
                &Default::default(),
            )
                .test_value()?,
            expected
        );
        Ok(())
    };
    let partial = vec![
        ViewCoverage::Complete,
        ViewCoverage::Partial {
            lane: Lane::Semantic,
            completed: 0,
            total: 1,
        },
    ];
    let stale = vec![
        ViewCoverage::Complete,
        ViewCoverage::Unavailable {
            lane: Lane::Semantic,
            reason: Reason::Incomplete,
        },
    ];
    commit_capture(
        &mut daemon,
        2,
        capture,
        ProductSemanticCaptureOutcome::Pending { prior: None },
        Vec::new(),
    )?;
    let pending = daemon.engine().daemon().owner().snapshot();
    assert_scope(&pending, RowState::Loading, partial.clone())?;
    assert_eq!(
        coverage::view_coverage(
            &pending,
            &coverage::ActivatedProfiles::new(),
            coverage::SemanticDeployment::Configured,
            &Default::default(),
        )
        .test_value()?,
        partial,
        "existing pending scope was incorrectly excluded by absent activation"
    );
    let sibling_label = "pkg:npm/source-correlation-sibling@1.0.0";
    let sibling = PackageKey::from_value(sibling_label);
    let (sibling_file, sibling_facts) = parsed_file(sibling, "sibling.ts", 1);
    commit_files(
        &mut daemon,
        sibling,
        sibling_label,
        3,
        &[(sibling_file, Some(sibling_facts))],
    );
    let sibling_edit = daemon.engine().daemon().owner().snapshot();
    assert_ne!(pending.root(), sibling_edit.root());
    assert_eq!(
        view_publish::capture_root(&pending).test_value()?,
        view_publish::capture_root(&sibling_edit).test_value()?
    );
    assert_scope(&sibling_edit, RowState::Loading, partial.clone())?;
    commit_capture(
        &mut daemon,
        4,
        capture,
        ProductSemanticCaptureOutcome::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim,
        },
        vec![BuiltinSemanticChange {
            key: key.clone(),
            after: Some(selected_record.clone()),
        }],
    )?;
    let published = daemon.engine().daemon().owner().snapshot();
    assert_scope(&published, RowState::Ready, vec![ViewCoverage::Complete])?;
    let capture_root = view_publish::capture_root(&published).test_value()?;
    let semantic_root = view_publish::semantic_root(&published).test_value()?;
    let (sibling_file, _) = parsed_file(sibling, "sibling.ts", 1);
    let sibling_source_only = source_only_intent(sibling, sibling_label, [0x45; 32], sibling_file)?;
    super::super::commands::commit_builtin_intent(&mut daemon, 45, &sibling_source_only)
        .test_value()?;
    let sibling_after_published = daemon.engine().daemon().owner().snapshot();
    assert_ne!(sibling_after_published.root(), published.root());
    assert_eq!(
        view_publish::capture_root(&sibling_after_published).test_value()?,
        capture_root
    );
    assert_scope(
        &sibling_after_published,
        RowState::Ready,
        vec![ViewCoverage::Complete],
    )?;
    // The production dirty-empty path can commit a new authenticated project
    // frontier without changing either capture or semantic publication root.
    let source_only = source_only_intent(package, label, [5; 32], file.clone())?;
    super::super::commands::commit_builtin_intent(&mut daemon, 5, &source_only).test_value()?;
    let changed = daemon.engine().daemon().owner().snapshot();
    assert_ne!(
        view_publish::source_root(&changed).test_value()?,
        view_publish::source_root(&published).test_value()?
    );
    assert_eq!(
        view_publish::capture_root(&changed).test_value()?,
        capture_root
    );
    assert_eq!(
        view_publish::semantic_root(&changed).test_value()?,
        semantic_root
    );
    assert_eq!(
        changed
            .relation::<BuiltinSemanticRelation>()
            .test_value()?
            .lookup(&key)
            .test_value()?,
        Some(selected_record.clone())
    );
    assert_scope(&changed, RowState::Failed, stale.clone())?;
    assert_scope(&published, RowState::Ready, vec![ViewCoverage::Complete])?;
    for deployment in [
        coverage::SemanticDeployment::Configured,
        coverage::SemanticDeployment::Unconfigured,
    ] {
        assert_eq!(
            coverage::view_coverage(
                &changed,
                &active,
                deployment,
                &Default::default(),
            ).test_value()?,
            stale
        );
    }
    assert_complete(&daemon, package, "retained.ts", 3);
    assert_eq!(
        coverage::package_row_state(
            &changed,
            &backend_engine::PackageReference::parse(sibling_label).test_value()?,
            &Default::default(),
        )
        .test_value()?,
        None,
        "unrelated package acquired foreign failure scope"
    );

    let file_key = backend_engine::product_source_file_key(package.to_bytes(), "retained.ts");
    let remove_published =
        BuiltinIntent::remove_project_with_semantics(package, label, &[file_key], &[], Vec::new())
            .test_value()?;
    super::super::commands::commit_builtin_intent(&mut daemon, 46, &remove_published)
        .test_value()?;
    let missing_published = daemon.engine().daemon().owner().snapshot();
    assert_eq!(
        view_publish::capture_root(&missing_published).test_value()?,
        capture_root
    );
    assert_eq!(
        view_publish::semantic_root(&missing_published).test_value()?,
        semantic_root
    );
    assert_scope(
        &missing_published,
        RowState::Failed,
        vec![ViewCoverage::Complete],
    )?;
    assert_remaining_projection(&missing_published, sibling)?;
    let historical_membership = backend_library::PackageSourceMembershipPageRequestV1::first(
        backend_engine::PackageReference::Local(
            backend_library::ProductText::new(label).test_value()?,
        ),
    )
    .with_selection(view_publish::source_root(&published).test_value()?, [1; 32]);
    assert!(
        matches!(
            crate::builtin::commands::package_source_membership_page_from_snapshot(
                &missing_published,
                &historical_membership
            )
            .test_value()?,
            backend_library::PackageSourceMembershipPageResultV1::Stale {
                current_source_version: None,
                ..
            }
        ),
        "explicit removed source scope must expose a stale old selection"
    );
    assert_eq!(
        coverage::view_coverage(
            &missing_published,
            &coverage::ActivatedProfiles::new(),
            coverage::SemanticDeployment::Configured,
            &Default::default(),
        )
        .test_value()?,
        vec![ViewCoverage::Complete],
        "removed history poisoned current remaining scope"
    );
    assert!(
        read_package_sources(&missing_published, package)
            .test_value()?
            .projects
            .is_empty()
    );

    // Restore a newer source frontier while keeping the old selection and
    // capture. A distinct version gives this actual mutation a new request
    // identity rather than replaying the previously applied source-only edit.
    let restored_source = source_only_intent(package, label, [0x47; 32], file.clone())?;
    super::super::commands::commit_builtin_intent(&mut daemon, 47, &restored_source)
        .test_value()?;

    let refresh_capture =
        SemanticSourceCapture::new(None, [0x47; 32], [0x73; 32], 2, 1).test_value()?;
    commit_capture(
        &mut daemon,
        6,
        refresh_capture,
        ProductSemanticCaptureOutcome::Pending {
            prior: Some(version),
        },
        Vec::new(),
    )?;
    let current_pending = daemon.engine().daemon().owner().snapshot();
    assert_scope(&current_pending, RowState::Loading, partial)?;
    let source_only = source_only_intent(package, label, [7; 32], file)?;
    super::super::commands::commit_builtin_intent(&mut daemon, 7, &source_only).test_value()?;
    let old_pending = daemon.engine().daemon().owner().snapshot();
    assert_scope(&old_pending, RowState::Failed, stale.clone())?;
    commit_capture(
        &mut daemon,
        8,
        refresh_capture,
        ProductSemanticCaptureOutcome::Failed {
            prior: version,
            reason: SemanticUnavailableReason::Cancelled,
        },
        Vec::new(),
    )?;
    let failed = daemon.engine().daemon().owner().snapshot();
    assert_scope(&failed, RowState::Failed, stale.clone())?;
    assert_eq!(
        failed
            .relation::<BuiltinSemanticRelation>()
            .test_value()?
            .lookup(&key)
            .test_value()?,
        Some(selected_record)
    );

    let file_key = backend_engine::product_source_file_key(package.to_bytes(), "retained.ts");
    let remove =
        BuiltinIntent::remove_project_with_semantics(package, label, &[file_key], &[], Vec::new())
            .test_value()?
            .with_operation_key(
                backend_library::IndexOperationKey::from_bytes([9; 32]).test_value()?,
            )
            .test_value()?;
    super::super::commands::commit_builtin_intent(&mut daemon, 9, &remove).test_value()?;
    let removed = daemon.engine().daemon().owner().snapshot();
    assert!(
        removed
            .relation::<BuiltinWorkspaceRelation>()
            .test_value()?
            .lookup(&package.to_bytes())
            .test_value()?
            .is_none()
    );
    assert_scope(&removed, RowState::Failed, vec![ViewCoverage::Complete])?;
    assert!(
        read_package_sources(&removed, package)
            .test_value()?
            .projects
            .is_empty(),
        "removed package became a ghost source frontier"
    );
    let final_root = removed.root();
    drop(removed);
    drop(failed);
    drop(old_pending);
    drop(current_pending);
    drop(changed);
    drop(published);
    drop(missing_published);
    drop(sibling_after_published);
    drop(sibling_edit);
    drop(pending);
    drop(daemon);
    let cold = open_daemon(workspace.0.path());
    let snapshot = cold.engine().daemon().owner().snapshot();
    assert_eq!(snapshot.root(), final_root);
    assert_scope(&snapshot, RowState::Failed, vec![ViewCoverage::Complete])?;
    assert_eq!(
        coverage::view_coverage(
            &snapshot,
            &coverage::ActivatedProfiles::new(),
            coverage::SemanticDeployment::Configured,
            &Default::default(),
        )
        .test_value()?,
        vec![ViewCoverage::Complete]
    );
    assert_remaining_projection(&snapshot, sibling)?;
    assert_complete(&cold, sibling, "sibling.ts", 1);
    Ok(())
}

#[test]
fn one_package_state_seeks_its_typed_prefix_without_folding_foreign_refusals() -> Result<(), String>
{
    use crate::builtin::coverage;
    use backend_engine::RowState;
    use backend_engine::builtin::{
        ProductSemanticCaptureOutcome, SemanticSourceCapture, SemanticUnavailableReason,
    };
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, TypeScriptSource};
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let target_label = "pkg:npm/mm-scoped-state@1.0.0";
    let labels = [
        "pkg:npm/aa-foreign-state@1.0.0",
        "pkg:npm/bb-foreign-state@1.0.0",
        "pkg:npm/cc-foreign-state@1.0.0",
        target_label,
        "pkg:npm/xx-foreign-state@1.0.0",
        "pkg:npm/yy-foreign-state@1.0.0",
        "pkg:npm/zz-foreign-state@1.0.0",
    ];
    let mut target_key = None;
    let mut target_capture = None;
    // Authenticated relation-range/storage control, not native acceptance.
    for (index, label) in labels.into_iter().enumerate() {
        let package = PackageKey::from_value(label);
        let key = ProductSemanticPublicationKey::new(
            backend_engine::PackageReference::parse(label).test_value()?,
            PackageUrl::parse(label).test_value()?,
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        )
        .test_value()?;
        let version = [index as u8 + 1; 32];
        let changes = vec![BuiltinSourceChange {
            key: package.to_bytes(),
            after: Some(ProductSourceRecord::project(label, version, Vec::new()).test_value()?),
        }];
        let selected = vec![BuiltinSemanticChange {
            key: key.clone(),
            after: Some(ProductSemanticPublicationRecord::Unavailable(
                if label == target_label {
                    SemanticUnavailableReason::Toolchain
                } else {
                    SemanticUnavailableReason::Rejected
                },
            )),
        }];
        let intent = if label == target_label {
            let capture =
                SemanticSourceCapture::new(None, version, [0x71; 32], 1, 0).test_value()?;
            target_key = Some(key.clone());
            target_capture = Some(capture);
            let base = daemon.engine().daemon().owner().snapshot();
            BuiltinIntent::index_with_capture(
                package,
                label,
                changes,
                selected,
                vec![BuiltinCaptureChange {
                    cargo_target: None,
                    key,
                    expected: None,
                    capture,
                    outcome: ProductSemanticCaptureOutcome::Pending { prior: None },
                    compiler_failure: None,
                }],
            )
            .test_value()?
            .with_capture_basis(capture_basis_for_snapshot(&base).test_value()?)
            .test_value()?
        } else {
            BuiltinIntent::index_with_semantics(package, label, changes, selected).test_value()?
        };
        super::super::commands::commit_builtin_intent(&mut daemon, index as u64 + 1, &intent)
            .test_value()?;
    }
    let key = target_key.test_value()?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    coverage::reset_package_state_visits();
    assert_eq!(
        coverage::package_row_state(
            &snapshot,
            key.package(),
            &Default::default(),
        ).test_value()?,
        Some(RowState::Loading)
    );
    assert_eq!(
        coverage::package_state_visits(),
        2,
        "foreign selected keys were folded into one package's state"
    );
    let (initial, _) = crate::builtin::initial_view().test_value()?;
    let mut rows = vec![backend_engine::Row::new(
        backend_engine::RowId::Package(key.package_key()),
        initial.basis(),
        target_label,
    )];
    coverage::qualify_package_rows(
        &snapshot,
        &mut rows,
        &Default::default(),
    ).test_value()?;
    assert_eq!(
        rows[0].state,
        RowState::Loading,
        "scoped and workspace folds diverged"
    );
    let missing =
        backend_engine::PackageReference::parse("pkg:npm/nn-missing-state@1.0.0").test_value()?;
    coverage::reset_package_state_visits();
    assert_eq!(
        coverage::package_row_state(
            &snapshot,
            &missing,
            &Default::default(),
        ).test_value()?,
        None
    );
    assert_eq!(
        coverage::package_state_visits(),
        0,
        "absent prefix folded the next package"
    );
    let expected = semantic_capture_relation(&snapshot)
        .test_value()?
        .test_value()?
        .lookup(&key)
        .test_value()?
        .test_value()?;
    let terminal = BuiltinIntent::index_with_capture(
        key.package_key(),
        target_label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            cargo_target: None,
            key: key.clone(),
            expected: Some(expected),
            capture: target_capture.test_value()?,
            outcome: ProductSemanticCaptureOutcome::Unavailable {
                reason: SemanticUnavailableReason::Cancelled,
            },
            compiler_failure: None,
        }],
    )
    .test_value()?
    .with_capture_basis(capture_basis_for_snapshot(&snapshot).test_value()?)
    .test_value()?;
    super::super::commands::commit_builtin_intent(&mut daemon, 8, &terminal).test_value()?;
    let failed = daemon.engine().daemon().owner().snapshot();
    coverage::reset_package_state_visits();
    assert_eq!(
        coverage::package_row_state(
            &failed,
            key.package(),
            &Default::default(),
        ).test_value()?,
        Some(RowState::Failed)
    );
    assert_eq!(coverage::package_state_visits(), 2);
    Ok(())
}

#[test]
fn search_corpus_cancellation_never_admits_partial_facts_and_resumes_identically()
-> Result<(), String> {
    use backend_engine::application::{
        LocalCompilerClient, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths,
        LocalCompilerScratch, LocalCompilerTimeout, LocalRuntimePackageAuthority,
        LocalRuntimeToolchain,
    };
    use backend_semantic::vocabulary::NativeTool;
    use std::num::NonZeroUsize;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let label = "pkg:npm/cancelled-corpus@1";
    let package = PackageKey::from_value(label);
    let path = "complete.ts";
    let source = (0..2048)
        .map(|index| format!("export function captured{index}() {{ return {index}; }}\n"))
        .collect::<String>();
    let analysis = backend_frontend_typescript::syntax_frontend()
        .map_err(|error| error.to_string())?
        .analyze(Path::new(path), source.as_bytes())
        .map_err(|error| error.to_string())?;
    let (record, facts) = parsed_analysis_file(package, path, &source, &analysis);
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    commit_files(&mut daemon, package, label, 1, &[(record, Some(facts))]);
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources =
        crate::builtin::read_indexed_sources(&snapshot).map_err(|error| error.to_string())?;
    let compiler_root = workspace.0.path().join("query-compiler");
    let compiler = LocalCompilerClient::start(
        LocalCompilerRuntimeConfiguration::new(
            LocalCompilerRuntimePaths::new(
                compiler_root.join("artifacts"),
                compiler_root.join("journal"),
                compiler_root.join("native-work"),
            )
            .map_err(|error| error.to_string())?,
            vec![LocalRuntimeToolchain::unavailable(NativeTool::Python)].into_boxed_slice(),
            Box::new([]),
            LocalRuntimePackageAuthority::default(),
            LocalCompilerTimeout::new(std::time::Duration::from_secs(2))
                .map_err(|error| error.to_string())?,
            backend_store::journal::PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
                .map_err(|error| error.to_string())?,
            LocalCompilerScratch::with_fragment_capacity(
                NonZeroUsize::new(1024 * 1024).ok_or("nonzero scratch capacity")?,
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let build = |cancelled| {
        view_build::semantic_query_corpus_controlled(
            &snapshot,
            &compiler,
            &sources,
            &mut crate::builtin::generation_residence::SemanticGenerationResidence::default(),
            &mut view_build::ImageRowResidence::default(),
            cancelled,
        )
    };
    let baseline = build(None).map_err(|error| error.to_string())?;
    assert!(baseline.facts().len() >= 2048);
    let token = Arc::new(AtomicBool::new(true));
    assert!(
        build(Some(token.as_ref())).is_err(),
        "pre-cancelled preparation is refused"
    );
    for (point, visits) in [
        ("source-availability-file", 1),
        ("semantic-page", 1),
        ("structural-sites-file", 1),
        ("corpus-structural-plan", 1),
        ("structural-type", 1024),
        ("structural-containment-declaration", 1024),
        ("structural-duplicate", 1024),
        ("structural-plan-declaration", 1024),
        ("structural-unify-declaration", 1024),
        ("structural-declaration", 1024),
        ("corpus-admit", 1),
        ("corpus-complete", 1),
    ] {
        token.store(false, Ordering::Release);
        let _cancel =
            crate::builtin::cancel_search_preparation_after(point, visits, Arc::clone(&token));
        let error = build(Some(token.as_ref()))
            .err()
            .ok_or("cancelled preparation returned a partial corpus")?;
        assert!(
            error.to_string().contains("search preparation cancelled"),
            "{point}: {error}"
        );
        assert!(
            token.load(Ordering::Acquire),
            "production path reached {point}"
        );
        assert_eq!(
            daemon.engine().daemon().owner().snapshot().root(),
            snapshot.root()
        );
    }
    token.store(false, Ordering::Release);
    let resumed = build(Some(token.as_ref())).map_err(|error| error.to_string())?;
    assert_eq!(resumed.workspace(), baseline.workspace());
    assert_eq!(resumed.evidence_digest(), baseline.evidence_digest());
    assert_eq!(resumed.facts(), baseline.facts());
    Ok(())
}
