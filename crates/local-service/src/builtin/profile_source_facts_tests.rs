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

fn commit_files(
    daemon: &mut super::super::ProductDaemon,
    package: PackageKey,
    label: &str,
    request: u64,
    files: &[(ProductSourceRecord, Option<ProductSourceFileFactsUpdate>)],
) -> Vec<BuiltinSourceFactsChange> {
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
