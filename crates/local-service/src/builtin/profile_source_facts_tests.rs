//! Real source-parser facts through the production commit preparation and store.
//! These storage laws do not stand in for semantic compiler acceptance.
use super::membership_tests::{TempWorkspace, open_daemon};
use super::*;
use backend_engine::builtin::{
    ProductSourceFileFactsAdmission, ProductSourceFileFactsRecord, ProductSourceFileFactsUpdate,
    admit_product_source_file_facts, build_product_source_file_facts,
    product_source_file_facts_relation,
};
use backend_engine::{PackageKey, ProductSourceRecord, SourceLanguage};
use backend_version::{ContentId, SourceFactDomain};
use std::path::Path;

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
        &updates,
        &selected,
        &old_files,
    )
    .expect("production facts preparation");
    let keys = selected.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let project = ProductSourceRecord::project(label, [request as u8; 32], keys.clone())
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
fn staged_large_parser_facts_use_small_queue_exact_shared_cas_and_cold_replay() {
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
    let first_allocation = allocation.admit(before.durable_store().unwrap()).unwrap();
    let second = stage(inline.clone());
    #[cfg(unix)]
    assert_eq!(
        allocation.admit(before.durable_store().unwrap()).unwrap(),
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
        first_evidence.hydrate(Some(&before)).unwrap().encode(),
        inline.encode()
    );
    assert_eq!(
        second_evidence.hydrate(Some(&before)).unwrap().encode(),
        inline.encode()
    );
    assert_eq!(first.encode().len(), 180);
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
            selected.durable_store().unwrap(),
            selected.closure().membership_id(),
        )
        .is_err(),
        "pointer fence cannot be swapped independently of the fixed manifest"
    );
    let selected_root = selected.root();
    drop(selected);
    drop(before);
    drop(first);
    drop(second);
    drop(daemon);
    let cold = open_daemon(workspace.0.path());
    assert_eq!(cold.engine().daemon().owner().head().root(), selected_root);
    assert_complete(&cold, package, "left.ts", count);
    assert_complete(&cold, package, "right.ts", count);
}

#[test]
fn staged_parser_facts_refuse_changed_source_and_cancel_before_queue() {
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
    .unwrap();
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
}

#[test]
fn warm_facts_reuse_requires_the_exact_selected_source_and_manifest() {
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
                &[],
                &selected,
                &previous,
            )
            .is_err(),
            "missing provenance or changed source cannot reuse an old complete tree"
        );
        assert_eq!(daemon.engine().daemon().owner().head().root(), before);
        assert_complete(&daemon, package, "source.ts", 2);
    }
}

#[test]
fn warm_complete_facts_survive_real_commits_and_shrinking_pages_are_retired() {
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
}

#[test]
fn typed_unavailable_source_facts_do_not_promote_compacted_rows() {
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
}
