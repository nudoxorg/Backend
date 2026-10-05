//! Durable owner oracles for paged product source membership.
//!
//! These fixtures exercise relation storage and recovery. The synthetic file
//! rows do not claim that a compiler or source parser accepted any language.

use super::*;
use backend_engine::{CanonicalRelation, SourceLanguage};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

type TestFile = ([u8; 32], String, [u8; 32]);

struct TempWorkspace(PathBuf);

impl TempWorkspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nudox-membership-owner-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temporary workspace directory");
        Self(path.canonicalize().expect("physical workspace directory"))
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open_daemon(workspace: &Path) -> super::super::ProductDaemon {
    let profile = profile_descriptor(BuiltinProfile::Product).expect("product profile");
    let dispatcher = super::super::builtin_dispatcher(
        Some(super::super::ECHO_AUTHORITY_SECRET),
        Arc::clone(&profile),
        60_000,
    )
    .expect("test dispatcher");
    let registry = backend_engine::RelationAdmissionRegistry::new()
        .with_relation::<BuiltinWorkspaceRelation>()
        .expect("workspace relation registry")
        .with_relation::<BuiltinSemanticRelation>()
        .expect("semantic relation registry");
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
        files.iter().map(|(key, _, _)| *key).collect(),
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
        BuiltinSourceChange {
            key: *key,
            after: Some(
                BuiltinPackageRecord::file(
                    package.to_bytes(),
                    path.clone(),
                    SourceLanguage::Rust,
                    *content_version,
                    [0x61; 32],
                    Arc::from(Vec::<backend_compile::SourceDeclaration>::new().into_boxed_slice()),
                )
                .expect("synthetic source file row"),
            ),
        }
    }));
    let intent = BuiltinIntent::index_with_semantics(package, label, changes, Vec::new())
        .expect("large indexed source intent");
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

fn assert_refused_without_publication(
    daemon: &mut super::super::ProductDaemon,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    changes: Vec<BuiltinSourceChange>,
    expected_frontier: &[[u8; 32]],
) {
    let before = daemon.engine().daemon().owner().head().root();
    let intent = BuiltinIntent::index_with_semantics(package, label, changes, Vec::new())
        .expect("hostile but well-formed source intent");
    let error = super::super::commands::commit_builtin_intent(daemon, request_id, &intent)
        .expect_err("invalid membership transition must be refused");
    assert!(!error.to_string().is_empty());
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        before,
        "refusal leaves the durable workspace root unchanged"
    );
    assert_eq!(
        resolved_frontier(daemon, package).as_slice(),
        expected_frontier
    );
}

#[test]
fn large_paged_membership_commits_reopens_and_refuses_incomplete_transitions_atomically() {
    const FILE_COUNT: usize = 2043;
    let temp = TempWorkspace::new();
    let label = "fixture:membership-pages/main";
    let package = backend_engine::PackageKey::from_value(label);
    let foreign_label = "fixture:membership-pages/foreign";
    let foreign_package = backend_engine::PackageKey::from_value(foreign_label);
    let files = file_frontier(FILE_COUNT, package);
    let expected = files.iter().map(|(key, _, _)| *key).collect::<Vec<_>>();

    let mut daemon = open_daemon(&temp.0);
    let main_add = BuiltinIntent::add(package, label).expect("main project intent");
    super::super::commands::commit_builtin_intent(&mut daemon, 1, &main_add)
        .expect("commit main project row");
    let foreign_add =
        BuiltinIntent::add(foreign_package, foreign_label).expect("foreign project intent");
    super::super::commands::commit_builtin_intent(&mut daemon, 2, &foreign_add)
        .expect("commit foreign project row");

    let (intent, update) = paged_index_intent(package, label, &files);
    assert_eq!(
        update
            .project_record()
            .project_fields()
            .expect("project fields")
            .files
            .file_count(),
        FILE_COUNT
    );
    assert!(!update.membership_pages().is_empty());
    super::super::commands::commit_builtin_intent(&mut daemon, 3, &intent)
        .expect("commit complete paged source membership");
    assert_eq!(resolved_frontier(&daemon, package), expected);
    let published_root = daemon.engine().daemon().owner().head().root();

    // Closing the owner forces journal recovery and persisted relation replay;
    // the exact selected file keys must survive both.
    drop(daemon);
    let mut daemon = open_daemon(&temp.0);
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        published_root
    );
    assert_eq!(resolved_frontier(&daemon, package), expected);
    let reopened_snapshot = daemon.engine().daemon().owner().snapshot();
    let reopened_relation = reopened_snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .expect("reopened source relation");
    let file_rows = reopened_relation
        .lookup_many_sorted(&expected)
        .expect("batch read reopened file rows");
    assert_eq!(file_rows.len(), FILE_COUNT);
    for ((key, _, _), record) in files.iter().zip(file_rows) {
        let record = record.expect("every selected file row replays");
        super::validate_project_file(package.to_bytes(), *key, &record)
            .expect("replayed file remains owned at its canonical path");
    }

    let original_root = daemon.engine().daemon().owner().head().root();
    let foreign_deletion = BuiltinSourceChange {
        key: foreign_package.to_bytes(),
        after: None,
    };
    assert_refused_without_publication(
        &mut daemon,
        package,
        label,
        4,
        vec![foreign_deletion],
        &expected,
    );
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        original_root
    );

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
    let old_page_keys = update
        .project_record()
        .project_fields()
        .expect("original project fields")
        .files
        .page_keys()
        .to_vec();
    let omitted_new_page = expanded_update
        .membership_pages()
        .iter()
        .find(|(key, _)| !old_page_keys.contains(key))
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
        5,
        vec![changed_project.clone()],
        &expected,
    );
    let after_missing_page = daemon.engine().daemon().owner().head().root();
    assert_eq!(after_missing_page, original_root);
    assert!(!old_page_keys.contains(&omitted_new_page));

    // A valid page body is still invalid as a change if no resulting project
    // references its content-derived key.
    let orphan_page = expanded_update
        .membership_pages()
        .iter()
        .find(|(key, _)| !old_page_keys.contains(key))
        .expect("new page for orphan-row refusal");
    assert_refused_without_publication(
        &mut daemon,
        package,
        label,
        6,
        vec![BuiltinSourceChange {
            key: orphan_page.0,
            after: Some(orphan_page.1.clone()),
        }],
        &expected,
    );
    assert_eq!(
        daemon.engine().daemon().owner().head().root(),
        original_root
    );
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
    let excessive_refs = vec![[0x33; 32]; ProductSourceRecord::MAX_PROJECT_MEMBERSHIP_PAGES + 1];
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
