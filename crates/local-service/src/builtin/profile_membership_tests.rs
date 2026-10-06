//! Durable owner oracles for paged product source membership.
//!
//! These fixtures exercise relation storage and recovery. The synthetic file
//! rows do not claim that a compiler or source parser accepted any language.

use super::*;
use backend_engine::{CanonicalRelation, SourceLanguage};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

type TestFile = ([u8; 32], String, [u8; 32]);

#[test]
fn product_store_registry_admits_every_persisted_product_relation() {
    let registry = super::super::product_relation_registry().expect("product relation registry");
    for schema in [
        backend_version::SchemaIdentity::of_relation::<BuiltinWorkspaceRelation>(),
        backend_version::SchemaIdentity::of_relation::<BuiltinSemanticRelation>(),
        backend_version::SchemaIdentity::of_relation::<
            backend_engine::builtin::ProductSemanticCaptureRelation,
        >(),
        backend_version::SchemaIdentity::of_relation::<
            backend_engine::builtin::ProductSourceFileFactsRelation,
        >(),
    ] {
        assert!(registry.contains_schema(schema), "missing product relation {schema:?}");
    }
}

pub(super) struct TempWorkspace(pub(super) tempfile::TempDir);

impl TempWorkspace {
    pub(super) fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("nudox-membership-owner-")
            .tempdir()
            .expect("create unique private temporary workspace");
        Self(directory)
    }
}

pub(super) fn open_daemon(workspace: &Path) -> super::super::ProductDaemon {
    let profile = profile_descriptor(BuiltinProfile::Product).expect("product profile");
    let dispatcher = super::super::builtin_dispatcher(
        Some(super::super::ECHO_AUTHORITY_SECRET),
        Arc::clone(&profile),
        60_000,
    )
    .expect("test dispatcher");
    let registry = super::super::product_relation_registry().expect("product relation registry");
    crate::Locald::open_with_dispatcher_and_registry(
        workspace,
        BuiltinModel,
        super::super::genesis().expect("product genesis"),
        dispatcher,
        backend_engine::DaemonConfig::default(),
        registry,
    )
    .expect("open product daemon")
}

fn commit_without_capture_upgrade(
    daemon: &mut super::super::ProductDaemon,
    request_id: u64,
    intent: &BuiltinIntent,
) -> Result<(), BuiltinModelError> {
    let request = BuiltinModel.request_id(intent);
    let expected = daemon.engine().daemon().owner().head().expectation();
    let receiver = daemon
        .client()
        .request(
            request_id,
            crate::Request::Commit {
                request,
                expected,
                intent: intent.clone(),
            },
        )
        .map_err(|error| BuiltinModelError(format!("queue legacy capture intent: {error:?}")))?;
    if !daemon.serve_one() {
        return Err(BuiltinModelError(
            "legacy capture intent owner did not make progress".to_owned(),
        ));
    }
    match crate::service::wait_for_daemon_reply(daemon, &receiver)
        .map_err(|error| BuiltinModelError(error.to_string()))?
    {
        backend_engine::DaemonReply::Commit(Ok(_)) => Ok(()),
        backend_engine::DaemonReply::Commit(Err(error)) => {
            Err(BuiltinModelError(error.to_string()))
        }
        _ => Err(BuiltinModelError(
            "legacy capture intent was sent to the wrong owner lane".to_owned(),
        )),
    }
}

fn file_frontier(count: usize, package: backend_engine::PackageKey) -> Vec<TestFile> {
    let mut files = (0..count)
        .map(|index| {
            let path = format!("src/membership-{index:05}.rs");
            let content = format!("synthetic storage fixture: {path}");
            (
                backend_engine::product_source_file_key(package.to_bytes(), &path),
                path,
                *blake3::hash(content.as_bytes()).as_bytes(),
            )
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(key, _, _)| *key);
    assert!(files.windows(2).all(|pair| pair[0].0 < pair[1].0));
    files
}

fn source_version(files: &[TestFile]) -> [u8; 32] {
    let mut source = blake3::Hasher::new();
    source.update(b"backend.project-snapshot.v2\0");
    let mut path_order = files.iter().collect::<Vec<_>>();
    path_order.sort_by(|left, right| left.1.cmp(&right.1));
    for (key, _, content_version) in path_order {
        source.update(key);
        source.update(content_version);
        source.update(&[0x61; 32]);
    }
    *source.finalize().as_bytes()
}

fn paged_index_intent(
    package: backend_engine::PackageKey,
    label: &str,
    files: &[TestFile],
) -> (BuiltinIntent, backend_engine::ProductSourceProjectUpdate) {
    let update = BuiltinPackageRecord::project_with_membership_pages(
        label,
        source_version(files),
        files.iter().map(|(key, _, _)| *key).collect::<Vec<_>>(),
        None,
    )
    .expect("bounded project membership update");
    let mut changes = Vec::with_capacity(files.len() + update.membership_pages().len() + 1);
    changes.push(BuiltinSourceChange {
        key: package.to_bytes(),
        after: Some(update.project_record().clone()),
    });
    changes.extend(
        update
            .membership_pages()
            .iter()
            .map(|(key, page)| BuiltinSourceChange {
                key: *key,
                after: Some(page.clone()),
            }),
    );
    changes.extend(files.iter().map(|(key, path, content_version)| {
        let declarations: Arc<[backend_compile::SourceDeclaration]> =
            Vec::new().into_boxed_slice().into();
        BuiltinSourceChange {
            key: *key,
            after: Some(
                BuiltinPackageRecord::file(
                    package.to_bytes(),
                    path.clone(),
                    SourceLanguage::Rust,
                    *content_version,
                    [0x61; 32],
                    declarations,
                )
                .expect("synthetic source file row"),
            ),
        }
    }));
    let intent = BuiltinIntent::index_with_semantics(package, label, changes, Vec::new())
        .expect("large indexed source intent");
    (intent, update)
}

fn paged_membership_edit_intent(
    package: backend_engine::PackageKey,
    label: &str,
    previous: &backend_engine::ProductSourceProjectUpdate,
    previous_files: &[TestFile],
    next_files: &[TestFile],
) -> (BuiltinIntent, backend_engine::ProductSourceProjectUpdate) {
    let update = BuiltinPackageRecord::project_with_membership_pages(
        label,
        source_version(next_files),
        next_files
            .iter()
            .map(|(key, _, _)| *key)
            .collect::<Vec<_>>(),
        None,
    )
    .expect("replacement project membership update");
    let previous_page_keys = previous
        .project_record()
        .project_fields()
        .expect("previous project fields")
        .files
        .page_keys()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let next_page_keys = update
        .project_record()
        .project_fields()
        .expect("replacement project fields")
        .files
        .page_keys()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let next_file_keys = next_files
        .iter()
        .map(|(key, _, _)| *key)
        .collect::<BTreeSet<_>>();

    let mut changes = vec![BuiltinSourceChange {
        key: package.to_bytes(),
        after: Some(update.project_record().clone()),
    }];
    changes.extend(
        update
            .membership_pages()
            .iter()
            .filter(|page| !previous_page_keys.contains(&page.0))
            .map(|(key, page)| BuiltinSourceChange {
                key: *key,
                after: Some(page.clone()),
            }),
    );
    changes.extend(
        previous_page_keys
            .difference(&next_page_keys)
            .copied()
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    changes.extend(
        previous_files
            .iter()
            .map(|(key, _, _)| *key)
            .filter(|key| !next_file_keys.contains(key))
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    let intent = BuiltinIntent::index_with_semantics(package, label, changes, Vec::new())
        .expect("replacement membership intent");
    (intent, update)
}

fn resolved_frontier(
    daemon: &super::super::ProductDaemon,
    package: backend_engine::PackageKey,
) -> Vec<[u8; 32]> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .expect("source relation");
    let project_key = package.to_bytes();
    let project = relation
        .lookup(&project_key)
        .expect("read project row")
        .expect("project row exists");
    super::resolve_project_file_keys(project_key, &project, |page_key| {
        relation
            .lookup(page_key)
            .map_err(|error| BuiltinModelError(format!("read membership page: {error}")))
    })
    .expect("complete authenticated file frontier")
}

fn assert_complete_frontier(
    daemon: &super::super::ProductDaemon,
    package: backend_engine::PackageKey,
    expected: &[[u8; 32]],
) {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .expect("source relation");
    let keys = resolved_frontier(daemon, package);
    assert_eq!(keys.as_slice(), expected);
    let records = relation
        .lookup_many_sorted(&keys)
        .expect("batch read all selected file rows");
    assert_eq!(records.len(), expected.len());
    for (key, record) in expected.iter().copied().zip(records) {
        let record = record.expect("every selected file row exists");
        super::validate_project_file(package.to_bytes(), key, &record)
            .expect("selected file row has its exact owning project and path key");
    }
}

fn assert_refused_without_publication(
    daemon: &mut super::super::ProductDaemon,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    changes: Vec<BuiltinSourceChange>,
    expected_frontier: &[[u8; 32]],
    expected_error: &str,
) {
    let before = daemon.engine().daemon().owner().head().root();
    let intent = BuiltinIntent::index_with_semantics(package, label, changes, Vec::new())
        .expect("hostile but well-formed source intent");
    let error = super::super::commands::commit_builtin_intent(daemon, request_id, &intent)
        .expect_err("invalid membership transition must be refused");
    let detail = error.to_string();
    assert!(
        detail.contains(expected_error),
        "expected refusal containing {expected_error:?}, got {detail:?}"
    );
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        before,
        "refusal leaves the durable workspace root unchanged"
    );
    assert_complete_frontier(daemon, package, expected_frontier);
}

#[test]
fn large_paged_membership_commits_edits_reopens_and_refuses_foreign_or_incomplete_transitions() {
    const FILE_COUNT: usize = 2048;
    let temp = TempWorkspace::new();
    let label = "fixture:membership-pages/main";
    let package = backend_engine::PackageKey::from_value(label);
    let foreign_label = "fixture:membership-pages/foreign";
    let foreign_package = backend_engine::PackageKey::from_value(foreign_label);
    let files = file_frontier(FILE_COUNT, package);
    let expected = files.iter().map(|(key, _, _)| *key).collect::<Vec<_>>();
    assert!(
        BuiltinPackageRecord::project(label, source_version(&files), expected.clone()).is_err(),
        "this exact frontier must exceed the actual legacy inline row capacity"
    );
    let foreign_count = BuiltinPackageRecord::MAX_FRONTIER_FILES.saturating_add(1);
    let foreign_files = file_frontier(foreign_count, foreign_package);
    let foreign_expected = foreign_files
        .iter()
        .map(|(key, _, _)| *key)
        .collect::<Vec<_>>();

    let mut daemon = open_daemon(temp.0.path());
    let main_add = BuiltinIntent::add(package, label).expect("main project intent");
    super::super::commands::commit_builtin_intent(&mut daemon, 1, &main_add)
        .expect("commit main project row");
    let foreign_add =
        BuiltinIntent::add(foreign_package, foreign_label).expect("foreign project intent");
    super::super::commands::commit_builtin_intent(&mut daemon, 2, &foreign_add)
        .expect("commit foreign project row");

    let (intent, initial_update) = paged_index_intent(package, label, &files);
    assert_eq!(
        initial_update
            .project_record()
            .project_fields()
            .expect("project fields")
            .files
            .file_count(),
        FILE_COUNT
    );
    assert!(!initial_update.membership_pages().is_empty());
    super::super::commands::commit_builtin_intent(&mut daemon, 3, &intent)
        .expect("commit complete paged source membership");
    let (foreign_intent, foreign_update) =
        paged_index_intent(foreign_package, foreign_label, &foreign_files);
    assert!(
        !foreign_update.membership_pages().is_empty(),
        "foreign project must have an independently paged frontier"
    );
    super::super::commands::commit_builtin_intent(&mut daemon, 4, &foreign_intent)
        .expect("commit paged foreign project membership");
    assert_complete_frontier(&daemon, package, &expected);
    assert_complete_frontier(&daemon, foreign_package, &foreign_expected);

    // A successful shrink must remove every page and file that leaves the
    // project's selected membership, while retaining the exact sorted prefix.
    let edited_files = files.iter().take(2000).cloned().collect::<Vec<_>>();
    let edited_expected = edited_files
        .iter()
        .map(|(key, _, _)| *key)
        .collect::<Vec<_>>();
    let (edit_intent, edited_update) =
        paged_membership_edit_intent(package, label, &initial_update, &files, &edited_files);
    let old_page_keys = initial_update
        .project_record()
        .project_fields()
        .expect("original project fields")
        .files
        .page_keys()
        .to_vec();
    let new_page_keys = edited_update
        .project_record()
        .project_fields()
        .expect("edited project fields")
        .files
        .page_keys()
        .to_vec();
    let removed_page_keys = old_page_keys
        .iter()
        .copied()
        .filter(|key| !new_page_keys.contains(key))
        .collect::<Vec<_>>();
    let reused_page_keys = old_page_keys
        .iter()
        .copied()
        .filter(|key| new_page_keys.contains(key))
        .collect::<Vec<_>>();
    let removed_file_keys = files
        .iter()
        .map(|(key, _, _)| *key)
        .filter(|key| !edited_expected.contains(key))
        .collect::<Vec<_>>();
    assert!(!removed_file_keys.is_empty());
    assert!(
        !new_page_keys.is_empty(),
        "the edited frontier remains paged"
    );
    assert!(!old_page_keys.is_empty());
    assert!(
        !removed_page_keys.is_empty(),
        "the final page changes or disappears"
    );
    assert!(
        !reused_page_keys.is_empty(),
        "unchanged page identities are reused"
    );
    let before_edit_root = daemon.engine().daemon().owner().head().root();
    super::super::commands::commit_builtin_intent(&mut daemon, 5, &edit_intent)
        .expect("commit successful membership shrink");
    assert_ne!(
        daemon.engine().daemon().owner().head().root(),
        before_edit_root
    );
    assert_complete_frontier(&daemon, package, &edited_expected);
    assert_complete_frontier(&daemon, foreign_package, &foreign_expected);

    let edited_snapshot = daemon.engine().daemon().owner().snapshot();
    let edited_relation = edited_snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .expect("edited source relation");
    for key in removed_page_keys.iter().chain(&removed_file_keys) {
        assert!(
            edited_relation
                .lookup(key)
                .expect("read removed source row")
                .is_none(),
            "successful edit deletes stale page and file rows"
        );
    }
    let edited_root = daemon.engine().daemon().owner().head().root();

    // Closing the owner forces journal recovery and persisted relation replay;
    // the replacement frontier and successful stale-row deletions survive both.
    drop(daemon);
    let mut daemon = open_daemon(temp.0.path());
    assert_eq!(daemon.engine().daemon().owner().head().root(), edited_root);
    assert_complete_frontier(&daemon, package, &edited_expected);
    assert_complete_frontier(&daemon, foreign_package, &foreign_expected);
    let reopened_relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinWorkspaceRelation>()
        .expect("reopened source relation");
    for key in removed_page_keys.iter().chain(&removed_file_keys) {
        assert!(
            reopened_relation
                .lookup(key)
                .expect("read replayed deletion")
                .is_none(),
            "journal replay retains successful page and file deletions"
        );
    }

    let original_root = daemon.engine().daemon().owner().head().root();
    let foreign_page_key = foreign_update
        .membership_pages()
        .first()
        .map(|(key, _)| *key)
        .expect("foreign membership page");
    let foreign_file_key = *foreign_expected.first().expect("foreign file key");
    let deletion_error = "source update deletes a row outside its prior project frontier";
    for (request_id, key, kind) in [
        (6, foreign_package.to_bytes(), "foreign project"),
        (7, foreign_page_key, "foreign membership page"),
        (8, foreign_file_key, "foreign file"),
    ] {
        assert_refused_without_publication(
            &mut daemon,
            package,
            label,
            request_id,
            vec![BuiltinSourceChange { key, after: None }],
            &edited_expected,
            deletion_error,
        );
        assert_complete_frontier(&daemon, foreign_package, &foreign_expected);
        assert_eq!(
            daemon.engine().daemon().owner().head().root(),
            original_root,
            "refused deletion of a {kind} leaves both projects selected"
        );
    }

    // An updated root that names newly derived pages without including those
    // page rows must be rejected before the owner can publish the new root.
    let mut expanded_files = files.clone();
    let extra_path = "src/membership-added.rs".to_owned();
    let extra_key = backend_engine::product_source_file_key(package.to_bytes(), &extra_path);
    let insert_at = expanded_files
        .binary_search_by_key(&extra_key, |(key, _, _)| *key)
        .unwrap_or_else(|index| index);
    let extra_content = format!("synthetic storage fixture: {extra_path}");
    expanded_files.insert(
        insert_at,
        (
            extra_key,
            extra_path,
            *blake3::hash(extra_content.as_bytes()).as_bytes(),
        ),
    );
    let (expanded_intent, expanded_update) = paged_index_intent(package, label, &expanded_files);
    let omitted_new_page = expanded_update
        .membership_pages()
        .iter()
        .find(|page| !new_page_keys.contains(&page.0))
        .map(|(key, _)| *key)
        .expect("expanded frontier derives at least one new page");
    let changed_project = expanded_intent
        .changes()
        .iter()
        .find(|change| change.key == package.to_bytes())
        .expect("updated project row");
    assert_refused_without_publication(
        &mut daemon,
        package,
        label,
        9,
        vec![changed_project.clone()],
        &edited_expected,
        "project membership page is missing",
    );
    let after_missing_page = daemon.engine().daemon().owner().head().root();
    assert_eq!(after_missing_page, original_root);
    assert!(!new_page_keys.contains(&omitted_new_page));
    assert_complete_frontier(&daemon, foreign_package, &foreign_expected);

    // A valid page body is still invalid as a change if no resulting project
    // references its content-derived key.
    let orphan_page = expanded_update
        .membership_pages()
        .iter()
        .find(|page| !new_page_keys.contains(&page.0))
        .expect("new page for orphan-row refusal");
    assert_refused_without_publication(
        &mut daemon,
        package,
        label,
        10,
        vec![BuiltinSourceChange {
            key: orphan_page.0,
            after: Some(orphan_page.1.clone()),
        }],
        &edited_expected,
        "source update adds an unreferenced membership page",
    );
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        original_root
    );
    assert_complete_frontier(&daemon, foreign_package, &foreign_expected);

    // Failed transitions do not leave a recovery record that could select a
    // partial project frontier or undo the successful stale-row deletions.
    drop(daemon);
    let daemon = open_daemon(temp.0.path());
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        original_root
    );
    assert_complete_frontier(&daemon, package, &edited_expected);
    assert_complete_frontier(&daemon, foreign_package, &foreign_expected);
    let final_relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinWorkspaceRelation>()
        .expect("source relation after refused transitions replays");
    for key in removed_page_keys.iter().chain(&removed_file_keys) {
        assert!(
            final_relation
                .lookup(key)
                .expect("read stale source row after final reopen")
                .is_none(),
            "successful page and file deletions survive later refused commits"
        );
    }
}

fn raw_psrd_project(file_count: u32, page_keys: &[[u8; 32]]) -> Vec<u8> {
    let label = b"fixture:decoder";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"PSRD");
    bytes.push(1);
    bytes.extend_from_slice(
        &u32::try_from(label.len())
            .expect("label length")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(label);
    bytes.extend_from_slice(&[0x71; 32]);
    bytes.push(1);
    bytes.extend_from_slice(&file_count.to_be_bytes());
    bytes.extend_from_slice(
        &u32::try_from(page_keys.len())
            .expect("page reference count")
            .to_be_bytes(),
    );
    for key in page_keys {
        bytes.extend_from_slice(key);
    }
    bytes.push(0);
    bytes
}

fn raw_psrd_page(files: &[[u8; 32]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"PSRD");
    bytes.push(3);
    bytes.extend_from_slice(&[0x72; 32]);
    bytes.extend_from_slice(
        &u32::try_from(files.len())
            .expect("page file count")
            .to_be_bytes(),
    );
    for key in files {
        bytes.extend_from_slice(key);
    }
    bytes
}

#[test]
fn paged_source_facts_and_typed_semantic_refusal_survive_cold_capture_reopen() {
    use backend_engine::builtin::{
        ProductSemanticCaptureOutcome, ProductSemanticCaptureRecord, ProductSemanticPublicationKey,
        ProductSourceFileFactsAdmission, ProductSourceFileFactsRecord, ProductSourceSnapshot,
        SemanticSourceCapture, build_product_source_file_facts, semantic_capture_relation,
    };
    use backend_library::interface::{CompilerFragmentFailure, SourceAuthority};
    use backend_semantic::ir::{BuildError, EntityId};
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, TypeScriptSource};
    use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain};

    let temp = TempWorkspace::new();
    let label = "pkg:npm/paged-panels@1.0.0";
    let package = backend_engine::PackageKey::from_value(label);
    let path = "src/Panels.tsx";
    let frontend = backend_frontend_typescript::syntax_frontend().expect("TypeScript frontend");
    let mut source = String::new();
    for index in 0..900 {
        source.push_str(&format!(
            "/** Catalog panel {index}; retained prose. */\n\
             export function Panel_{index:04}({{ title }}: {{ title: string }}) {{\n\
               return <article data-panel=\"{index}\">{{title}}</article>;\n\
             }}\n"
        ));
    }
    let analysis = frontend
        .analyze(Path::new(path), source.as_bytes())
        .expect("actual TSX source analysis");
    let declarations = analysis.declarations().to_vec();
    let expected_declaration_count = declarations.len();
    assert_eq!(
        declarations
            .iter()
            .filter(|declaration| declaration.kind() == backend_compile::DeclarationKind::Function)
            .count(),
        900,
        "the producer emits all 900 actual panel functions"
    );
    let source_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
    let content_version = *source_identity.as_ref();
    let analysis_version = [0x61; 32];
    let project_key = package.to_bytes();
    let file_key = backend_engine::product_source_file_key(project_key, path);
    let frontier = vec![(file_key, path.to_owned(), content_version)];
    let source_version = source_version(&frontier);
    let project_record = BuiltinPackageRecord::project(label, source_version, vec![file_key])
        .expect("one-file project frontier");
    let file_record = backend_engine::ProductSourceRecord::identified_file_within_row_capacity(
        project_key,
        path,
        SourceLanguage::TypeScript,
        content_version,
        analysis_version,
        declarations.clone(),
        source_identity,
    )
    .expect("bounded compact file row with exact source identity");
    assert!(
        !file_record
            .file_fields()
            .expect("compact file fields")
            .retention
            .is_complete(),
        "900 source declarations overflow the compact summary and need complete facts pages"
    );
    let facts = build_product_source_file_facts(
        project_key,
        path,
        SourceLanguage::TypeScript,
        content_version,
        analysis_version,
        source_identity,
        &declarations,
    )
    .expect("complete paged declaration facts");
    assert_eq!(facts.declaration_count(), expected_declaration_count);
    assert!(
        facts
            .pages()
            .iter()
            .any(|(_, row)| { matches!(row, ProductSourceFileFactsRecord::Page(_)) })
    );

    let recipe_identity =
        ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"fixture TSX recipe");
    let attempt = backend_library::interface::CompilerAttempt {
        source: SourceAuthority {
            identity: source_identity,
            byte_len: u32::try_from(source.len()).expect("bounded TSX source length"),
        },
        recipe: recipe_identity,
    };
    let compile_failure = CompilerFragmentFailure::build(BuildError::InvalidOccurrenceSpan {
        owner: EntityId::new(7),
        start: 18,
        end: 24,
    });
    let failure = backend_library::PackageCompilerFailure::from_fragment_failure(
        path,
        attempt,
        &compile_failure,
    )
    .expect("typed package compiler refusal bound to exact source");

    let package_reference =
        backend_engine::PackageReference::parse(label.to_owned()).expect("package reference");
    let coordinate = PackageUrl::parse(label.to_owned()).expect("npm coordinate");
    let capture_key = ProductSemanticPublicationKey::new(
        package_reference,
        coordinate,
        LanguageProfile::TypeScript(TypeScriptSource::Tsx),
    )
    .expect("TSX compiler profile key");
    let capture =
        SemanticSourceCapture::new(None, source_version, [0x85; 32], 1, 1).expect("source capture");

    let mut source_facts_changes = vec![BuiltinSourceFactsChange {
        key: facts.manifest_key(),
        expected: None,
        after: Some(ProductSourceFileFactsRecord::Manifest(
            facts.manifest().clone(),
        )),
    }];
    source_facts_changes.extend(
        facts
            .pages()
            .iter()
            .map(|(key, row)| BuiltinSourceFactsChange {
                key: *key,
                expected: None,
                after: Some(row.clone()),
            }),
    );
    let intent = BuiltinIntent::index_with_capture(
        package,
        label,
        vec![
            BuiltinSourceChange {
                key: project_key,
                after: Some(project_record),
            },
            BuiltinSourceChange {
                key: file_key,
                after: Some(file_record),
            },
        ],
        Vec::new(),
        vec![BuiltinCaptureChange {
            key: capture_key.clone(),
            expected: None,
            capture,
            outcome: ProductSemanticCaptureOutcome::Pending { prior: None },
            compiler_failure: None,
        }],
    )
    .expect("source plus pending capture intent")
    .with_source_facts(source_facts_changes.clone())
    .expect("atomic complete facts update");

    let mut daemon = open_daemon(temp.0.path());
    let initial_basis = capture_basis_for_snapshot(&daemon.engine().daemon().owner().snapshot())
        .expect("exact selected genesis basis");
    let intent = intent
        .with_capture_basis(initial_basis)
        .expect("bind source-first capture to exact selected genesis closure");
    assert_eq!(intent.encode()[4], 9, "capture intent uses BPI9");
    let roundtrip = BuiltinIntent::decode(&intent.encode()).expect("BPI9 roundtrip");
    assert_eq!(roundtrip.capture_basis(), Some(initial_basis));
    assert_eq!(roundtrip.encode()[4], 9);
    let recomposed = roundtrip
        .with_source_facts(source_facts_changes.clone())
        .expect("source-facts composition preserves authenticated basis");
    assert_eq!(recomposed.encode()[4], 9);
    assert_eq!(recomposed.capture_basis(), Some(initial_basis));
    super::super::commands::commit_builtin_intent(&mut daemon, 1, &intent)
        .expect("atomically commit structural source facts and pending capture");
    let source_capture_root = daemon.engine().daemon().owner().head().root();
    let pending_capture = semantic_capture_relation(&daemon.engine().daemon().owner().snapshot())
        .expect("pending semantic capture relation")
        .expect("pending capture relation exists")
        .lookup(&capture_key)
        .expect("read pending source capture")
        .expect("pending source capture persisted");
    assert_eq!(
        pending_capture.outcome(),
        ProductSemanticCaptureOutcome::Pending { prior: None }
    );
    let source_capture_sequence = pending_capture.source_workspace_sequence();
    let basis_before_unrelated =
        capture_basis_for_snapshot(&daemon.engine().daemon().owner().snapshot())
            .expect("exact selected basis before unrelated capture");
    let unrelated_label = "pkg:npm/intervening-project@1.0.0";
    let unrelated_package = backend_engine::PackageKey::from_value(unrelated_label);
    let unrelated_reference = backend_engine::PackageReference::parse(unrelated_label.to_owned())
        .expect("independent capture package reference");
    let unrelated_coordinate =
        PackageUrl::parse(unrelated_label.to_owned()).expect("independent capture coordinate");
    let unrelated_capture_key = ProductSemanticPublicationKey::new(
        unrelated_reference,
        unrelated_coordinate,
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
    )
    .expect("independent TypeScript capture key");
    let unrelated_capture = SemanticSourceCapture::new(None, [0xB3; 32], [0xB4; 32], 1, 1)
        .expect("independent source capture marker");
    let unrelated_intent = BuiltinIntent::index_with_capture(
        unrelated_package,
        unrelated_label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            key: unrelated_capture_key.clone(),
            expected: None,
            capture: unrelated_capture,
            outcome: ProductSemanticCaptureOutcome::Pending { prior: None },
            compiler_failure: None,
        }],
    )
    .expect("independent product transition");
    super::super::commands::commit_builtin_intent(&mut daemon, 2, &unrelated_intent)
        .expect("commit unrelated product change while capture is pending");
    assert_ne!(
        daemon.engine().daemon().owner().head().root(),
        source_capture_root,
        "the intervening product transition advances the workspace root"
    );
    let basis_after_unrelated =
        capture_basis_for_snapshot(&daemon.engine().daemon().owner().snapshot())
            .expect("exact selected basis after unrelated capture");
    assert_ne!(
        basis_before_unrelated.capture_root(),
        basis_after_unrelated.capture_root(),
        "the unrelated capture changes the selected capture relation root"
    );
    assert_eq!(
        basis_before_unrelated.workspace_root(),
        basis_after_unrelated.workspace_root(),
        "capture-only commits preserve the workspace manifest root"
    );
    assert_ne!(
        basis_before_unrelated.workspace_sequence(),
        basis_after_unrelated.workspace_sequence(),
        "capture-only commits still advance the selected sequence"
    );
    let legacy_v6 = BuiltinIntent::index_with_capture(
        package,
        label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            key: capture_key.clone(),
            expected: Some(pending_capture.clone()),
            capture,
            outcome: ProductSemanticCaptureOutcome::Unavailable {
                reason: backend_engine::builtin::SemanticUnavailableReason::Rejected,
            },
            compiler_failure: None,
        }],
    )
    .expect("decodeable legacy BPI6 capture intent");
    let legacy_v7 = legacy_v6
        .clone()
        .with_source_facts(Vec::new())
        .expect("decodeable legacy BPI7 capture intent");
    let legacy_v8 = BuiltinIntent::index_with_capture(
        package,
        label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            key: capture_key.clone(),
            expected: Some(pending_capture.clone()),
            capture,
            outcome: ProductSemanticCaptureOutcome::Unavailable {
                reason: backend_engine::builtin::SemanticUnavailableReason::Rejected,
            },
            compiler_failure: Some(failure.clone()),
        }],
    )
    .expect("decodeable legacy BPI8 typed capture refusal");
    let before_legacy = daemon.engine().daemon().owner().head().root();
    for (offset, (version, legacy_intent)) in [(6, legacy_v6), (7, legacy_v7), (8, legacy_v8)]
        .into_iter()
        .enumerate()
    {
        assert_eq!(legacy_intent.encode()[4], version);
        let request_id = 3 + u64::try_from(offset).expect("legacy request offset");
        let legacy_error = commit_without_capture_upgrade(&mut daemon, request_id, &legacy_intent)
            .expect_err("legacy capture cannot publish without an authenticated base closure");
        assert!(
            legacy_error.to_string().contains(&format!("BPI{version}"))
                && legacy_error.to_string().contains("BPI9 is required"),
            "legacy capture refusal states the compatibility boundary: {legacy_error}"
        );
    }
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        before_legacy
    );
    assert_eq!(
        semantic_capture_relation(&daemon.engine().daemon().owner().snapshot())
            .expect("selected legacy refusal base relation")
            .expect("selected capture relation")
            .lookup(&capture_key)
            .expect("selected legacy refusal base receipt"),
        Some(pending_capture.clone()),
        "legacy refusal preserves the selected source receipt"
    );
    let forged_before = ProductSemanticCaptureRecord::new(
        pending_capture.operation_key().copied(),
        [0xFE; 32],
        pending_capture.capture(),
        *pending_capture.base_workspace_root(),
        pending_capture.base_workspace_sequence(),
        *pending_capture.source_workspace_root(),
        pending_capture.source_workspace_sequence(),
        *pending_capture.source_commit(),
        pending_capture.outcome(),
    )
    .expect("well-formed but nonmatching pending receipt");
    let forged_intent = BuiltinIntent::index_with_capture(
        package,
        label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            key: capture_key.clone(),
            expected: Some(forged_before),
            capture,
            outcome: ProductSemanticCaptureOutcome::Unavailable {
                reason: backend_engine::builtin::SemanticUnavailableReason::Rejected,
            },
            compiler_failure: None,
        }],
    )
    .expect("forged before receipt intent")
    .with_capture_basis(basis_before_unrelated)
    .expect("substitute a valid earlier selected capture root");
    let before_forged = daemon.engine().daemon().owner().head().root();
    let forged_error =
        super::super::commands::commit_builtin_intent(&mut daemon, 6, &forged_intent)
            .expect_err("a forged expected receipt cannot replace the selected Pending row");
    assert!(
        forged_error.to_string().contains("before-state basis"),
        "valid but stale basis is refused by the live selected-before check: {forged_error}"
    );
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        before_forged,
        "refusal leaves the selected capture root unchanged"
    );
    let terminal_intent = BuiltinIntent::index_with_capture(
        package,
        label,
        Vec::new(),
        Vec::new(),
        vec![BuiltinCaptureChange {
            key: capture_key.clone(),
            expected: Some(pending_capture),
            capture,
            outcome: ProductSemanticCaptureOutcome::Unavailable {
                reason: backend_engine::builtin::SemanticUnavailableReason::Rejected,
            },
            compiler_failure: Some(failure.clone()),
        }],
    )
    .expect("typed terminal semantic refusal intent");
    let stale_snapshot = daemon.engine().daemon().owner().snapshot();
    super::super::commands::commit_builtin_intent(&mut daemon, 7, &terminal_intent)
        .expect("commit terminal typed refusal against its exact pending capture");
    assert!(
        stale_snapshot.selected_base_publication().is_err(),
        "a snapshot that is no longer selected cannot resolve the current base publication"
    );
    let terminal_snapshot = daemon.engine().daemon().owner().snapshot();
    let selected_base = terminal_snapshot
        .selected_base_publication()
        .expect("resolve exact selected base publication")
        .expect("terminal capture has a durable base publication")
        .descriptor();
    assert_eq!(
        selected_base.target(),
        basis_after_unrelated.workspace_root(),
        "the authenticated descriptor binds the basis workspace root"
    );
    assert_eq!(
        selected_base.target_generation(),
        basis_after_unrelated.workspace_sequence(),
        "the authenticated descriptor binds the basis sequence"
    );
    assert_eq!(
        *selected_base.closure().as_bytes(),
        basis_after_unrelated.closure_id(),
        "the journal selects the actual current base closure"
    );
    let hybrid_basis = BuiltinCaptureBasis::new(
        basis_after_unrelated.workspace_root(),
        basis_after_unrelated.workspace_sequence(),
        basis_before_unrelated.closure_id(),
        basis_before_unrelated.capture_root(),
    )
    .expect("well-shaped hybrid basis with current root/sequence and old closure");
    let hybrid_error =
        super::validate_capture_basis_closure_id(hybrid_basis, selected_base.closure())
            .expect_err("a valid alternate old closure is not the selected base descriptor");
    assert!(
        hybrid_error
            .to_string()
            .contains("authenticated selected base publication"),
        "same-manifest/current-sequence basis rejects an old authenticated closure: {hybrid_error}"
    );
    let selected_root = daemon.engine().daemon().owner().head().root();
    drop(daemon);

    let daemon = open_daemon(temp.0.path());
    let snapshot = daemon.engine().daemon().owner().snapshot();
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        selected_root
    );
    let source_snapshot =
        ProductSourceSnapshot::from_workspace(&snapshot).expect("cold selected source closure");
    let source_relation = source_snapshot.relation();
    let source_row = source_relation
        .lookup(&file_key)
        .expect("cold source file lookup")
        .expect("cold source file row remains selected");
    let mut admitted = match source_snapshot
        .admit_complete_file_facts(&source_row)
        .expect("cold manifest and every page validate against exact source")
        .expect("overflow source row requires its complete facts manifest")
    {
        ProductSourceFileFactsAdmission::PagedVerified(paged) => paged,
        ProductSourceFileFactsAdmission::InlineComplete(_) => {
            panic!("900 declarations must remain page bounded")
        }
    };
    assert_eq!(
        usize::try_from(admitted.declaration_count()).expect("bounded declaration count"),
        expected_declaration_count
    );
    let mut visited = 0usize;
    let mut complete_panel_facts = 0usize;
    admitted
        .visit_pages(|page| {
            for index in 0..page.len() {
                let admitted = page
                    .declaration(index)
                    .ok_or_else(|| "cold admitted declaration".to_owned())?;
                let declaration = admitted.to_owned()?;
                let expected = declarations
                    .get(visited)
                    .ok_or_else(|| "cold source facts exceeded syntax producer output".to_owned())?;
                if &declaration != expected {
                    return Err(format!(
                        "cold source facts differ from SyntaxFrontend output at ordinal {visited}: {} {:?}",
                        expected.name(),
                        expected.kind()
                    ));
                }
                visited += 1;
                if expected.kind() == backend_compile::DeclarationKind::Function
                    && (expected.documentation().is_empty()
                        || expected.source_excerpt().text().is_none())
                {
                    return Err(format!(
                        "cold function fact lost prose or excerpt for {}",
                        expected.name()
                    ));
                }
                if declaration.kind() == backend_compile::DeclarationKind::Function {
                    complete_panel_facts += 1;
                }
            }
            Ok(())
        })
        .expect("cold bounded page visitation");
    assert_eq!(visited, expected_declaration_count);
    assert_eq!(complete_panel_facts, 900);

    let captures = semantic_capture_relation(&snapshot)
        .expect("cold semantic capture relation")
        .expect("terminal capture relation exists");
    let terminal = captures
        .lookup(&capture_key)
        .expect("cold typed semantic terminal lookup")
        .expect("semantic refusal retained");
    assert_eq!(
        terminal.outcome(),
        ProductSemanticCaptureOutcome::Unavailable {
            reason: backend_engine::builtin::SemanticUnavailableReason::Rejected,
        }
    );
    assert_eq!(terminal.compiler_failure(), Some(&failure));
    assert_eq!(
        terminal.source_workspace_root(),
        source_capture_root.as_bytes()
    );
    assert_eq!(
        terminal.source_workspace_sequence(),
        source_capture_sequence,
        "terminal capture preserves the original source receipt across unrelated commits"
    );
    assert_eq!(failure.source_identity(), source_identity);
    assert_eq!(failure.relative_path(), path);
    assert_eq!(
        failure.source_byte_len(),
        u32::try_from(source.len()).expect("bounded TSX source length")
    );
    assert_eq!(failure.recipe_identity(), Some(recipe_identity));
    assert_eq!(
        captures
            .lookup(&unrelated_capture_key)
            .expect("cold unrelated capture lookup")
            .expect("unrelated Pending capture remains selected")
            .outcome(),
        ProductSemanticCaptureOutcome::Pending { prior: None }
    );
}

fn psrd_decodes(bytes: &[u8]) -> bool {
    <BuiltinWorkspaceRelation as CanonicalRelation>::decode_value(bytes).is_ok()
}

#[test]
fn raw_psrd_decoder_rejects_truncation_count_reference_and_order_corruption() {
    let first_ref = [0x11; 32];
    let second_ref = [0x22; 32];
    let mut project = raw_psrd_project(257, &[first_ref, second_ref]);
    assert!(psrd_decodes(&project));

    let mut truncated_project = project.clone();
    truncated_project.pop();
    assert!(!psrd_decodes(&truncated_project));

    // file_count is at a fixed position following the PSRD tag, row tag,
    // label, and 32-byte source version.
    let file_count_offset = 4 + 1 + 4 + b"fixture:decoder".len() + 32 + 1;
    project[file_count_offset..file_count_offset + 4].copy_from_slice(&0_u32.to_be_bytes());
    assert!(
        !psrd_decodes(&project),
        "zero-file paged projects are invalid"
    );

    let mut excessive_count = raw_psrd_project(257, &[first_ref, second_ref]);
    excessive_count[file_count_offset..file_count_offset + 4]
        .copy_from_slice(&100_001_u32.to_be_bytes());
    assert!(
        !psrd_decodes(&excessive_count),
        "logical file bound is enforced"
    );

    let duplicate_refs = raw_psrd_project(257, &[first_ref, first_ref]);
    assert!(!psrd_decodes(&duplicate_refs), "page references are unique");
    let empty_refs = raw_psrd_project(1, &[]);
    assert!(
        !psrd_decodes(&empty_refs),
        "a paged project names at least one page"
    );
    let excessive_refs = vec![[0x33; 32]; BuiltinPackageRecord::MAX_PROJECT_MEMBERSHIP_PAGES + 1];
    assert!(
        !psrd_decodes(&raw_psrd_project(100_000, &excessive_refs)),
        "page-reference count is bounded"
    );
    assert!(
        !psrd_decodes(&raw_psrd_project(1, &[first_ref, second_ref])),
        "page-reference count cannot exceed the file-count bound"
    );

    let sorted_files = [[0x10; 32], [0x20; 32]];
    assert!(psrd_decodes(&raw_psrd_page(&sorted_files)));
    assert!(
        !psrd_decodes(&raw_psrd_page(&[sorted_files[1], sorted_files[0]])),
        "page file keys must be in strict relation order"
    );
    assert!(!psrd_decodes(&raw_psrd_page(&[
        sorted_files[0],
        sorted_files[0]
    ])));
    assert!(
        !psrd_decodes(&raw_psrd_page(&[])),
        "empty pages are invalid"
    );
    let mut truncated_page = raw_psrd_page(&sorted_files);
    truncated_page.truncate(truncated_page.len() - 1);
    assert!(!psrd_decodes(&truncated_page));
}
