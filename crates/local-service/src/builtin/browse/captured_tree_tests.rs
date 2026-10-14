//! Source-only tests: fixture construction uses the admitted on-disk relation,
//! and no compiler, Cargo child, or live source inventory is invoked.
#![allow(clippy::expect_used, clippy::panic)]

use super::captured_project_tree;
use backend_engine::{FileStore, ProductSourceRecord, ProductSourceRelation, RelationAdmissionRegistry, RelationState, WorkspaceHead, WorkspaceSnapshot};
use backend_library::{PackageReference, PackageSourceMembershipPageRequestV1 as Request, PackageSourceMembershipPageResultV1 as Page, PackageSourceMembershipUnavailableV1 as Unavailable, ProductText};
use std::collections::BTreeSet;
use std::sync::Arc;

struct Fixture {
    head: WorkspaceHead,
    snapshot: WorkspaceSnapshot,
    store: tempfile::TempDir,
}

impl Fixture {
    fn reopen(&self) -> WorkspaceSnapshot {
        let registry = RelationAdmissionRegistry::new().with_relation::<ProductSourceRelation>().expect("register source relation");
        let store = FileStore::open_with_registry(self.store.path(), 16 * 1024 * 1024, registry).expect("cold reopen");
        self.head.snapshot_in(Arc::new(store))
    }
}

fn fixture(root: &str, count: usize, source_version: u8, missing: bool) -> Fixture {
    let project = backend_engine::package_key(root).to_bytes();
    let mut entries = Vec::new();
    let mut keys = Vec::new();
    let languages = [backend_compile::SourceLanguage::Rust, backend_compile::SourceLanguage::TypeScript, backend_compile::SourceLanguage::Python, backend_compile::SourceLanguage::Go];
    let suffixes = ["rs", "ts", "py", "go"];
    for index in 0..count {
        let path = format!("src/group{}/file{index}.{}", index % 3, suffixes[index % 4]);
        let key = backend_engine::product_source_file_key(project, &path);
        keys.push(key);
        if !(missing && index == 0) {
            // Retained structural membership must survive refused analysis.
            let row = ProductSourceRecord::file_unavailable(project, path, languages[index % 4], [2; 32], backend_library::SourceUnavailableReason::Unparsed).expect("refused source file");
            entries.push((key, row));
        }
    }
    keys.sort_unstable();
    let update = ProductSourceRecord::project_with_membership_pages(root, [source_version; 32], keys, None).expect("captured project");
    entries.push((project, update.project_record().clone()));
    entries.extend(update.membership_pages().iter().cloned());
    let relation = RelationState::<ProductSourceRelation>::from_entries(entries, super::super::admitted_coverage().expect("coverage")).expect("admitted relation");
    let registry = RelationAdmissionRegistry::new().with_relation::<ProductSourceRelation>().expect("register source relation");
    let directory = crate::private_temp::create("captured-tree").expect("private source store");
    let store = FileStore::open_with_registry(directory.path(), 16 * 1024 * 1024, registry).expect("source store");
    store.write_relation_state(&relation).expect("persist capture");
    let head = super::super::head_from_relation(relation).expect("source head");
    let snapshot = head.snapshot_in(Arc::new(store));
    Fixture { head, snapshot, store: directory }
}

fn inventory(tree: &backend_library::browse::ProjectTree) -> &Page {
    let backend_library::browse::TreeSource::CapturedSource { membership } = &tree.source else { panic!("captured source reader required"); };
    membership
}

#[test]
fn captured_tree_is_language_neutral_bounded_and_cold_replayable_after_refusal() {
    let root = crate::private_temp::create("captured-root").expect("root");
    let label = root.path().join("deleted-project").to_string_lossy().into_owned();
    let stored = fixture(&label, 140, 3, false);
    let text = ProductText::new(label.clone()).expect("root operand");
    let first = captured_project_tree(&stored.snapshot, &text).expect("captured tree");
    assert!(first.has_admissible_shape());
    assert!(first.retained_request_binding().is_none(), "no Cargo source capability");
    assert!(!std::path::Path::new(&label).exists(), "tree does not require a live source root");
    let reopened = stored.reopen();
    assert_eq!(captured_project_tree(&reopened, &text).expect("cold tree"), first);
    let Page::Page { package, source_relation_root, source_version, file_count, files, next, .. } = inventory(&first) else { panic!("first page"); };
    assert_eq!(*file_count, 140);
    assert_eq!(files.len(), backend_library::DEFAULT_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES as usize);
    assert!(files.iter().all(|file| file.source_identity.is_none()), "unavailable analysis never fabricates source identity");
    let mut selected: BTreeSet<_> = files.iter().map(|file| (file.file_key, file.path.clone())).collect();
    let mut languages: BTreeSet<_> = files.iter().map(|file| format!("{:?}", file.language)).collect();
    let mut cursor = next.clone();
    while let Some(next) = cursor {
        let request = Request::first(package.clone()).with_selection(*source_relation_root, *source_version).with_cursor(next);
        let page = super::super::commands::package_source_membership_page_from_snapshot(&reopened, &request).expect("original membership continuation");
        let Page::Page { files, next, .. } = page else { panic!("same-capture continuation"); };
        assert!(files.len() <= 128);
        selected.extend(files.iter().map(|file| (file.file_key, file.path.clone())));
        languages.extend(files.iter().map(|file| format!("{:?}", file.language)));
        cursor = next;
    }
    assert_eq!(selected.len(), 140, "exact complete union, no omitted or duplicated members");
    assert_eq!(languages, ["Go", "Python", "Rust", "TypeScript"].into_iter().map(str::to_owned).collect());
    let changed = fixture(&label, 140, 4, false);
    let request = Request::first(package.clone()).with_selection(*source_relation_root, *source_version).with_cursor(next.clone().expect("partial first page"));
    assert!(matches!(super::super::commands::package_source_membership_page_from_snapshot(&changed.snapshot, &request).expect("changed capture"), Page::Stale { .. }));
    let request = Request::first(PackageReference::Local(ProductText::new(format!("{label}-other")).expect("other root"))).with_selection(*source_relation_root, *source_version).with_cursor(next.clone().expect("partial first page"));
    assert!(super::super::commands::package_source_membership_page_from_snapshot(&reopened, &request).is_err(), "a cursor cannot be submitted under another root");
}

#[test]
fn captured_tree_preserves_missing_inventory_and_ignores_live_files() {
    let live = crate::private_temp::create("live-captured-tree").expect("live root");
    let label = live.path().to_string_lossy().into_owned();
    let text = ProductText::new(label.clone()).expect("root");
    let stored = fixture(&label, 4, 3, false);
    let before = captured_project_tree(&stored.snapshot, &text).expect("captured tree");
    std::fs::write(live.path().join("Cargo.toml"), "[workspace]\nmembers = [\"unowned\"]\n").expect("new live manifest");
    std::fs::write(live.path().join("new.py"), "print('outside captured inventory')\n").expect("new live source");
    assert_eq!(captured_project_tree(&stored.reopen(), &text).expect("same capture"), before, "live files and Cargo must not substitute for the selected capture");
    let other = ProductText::new(format!("{label}/unselected")).expect("other root");
    let tree = captured_project_tree(&stored.snapshot, &other).expect("typed unavailable");
    assert!(matches!(inventory(&tree), Page::Unavailable { reason: Unavailable::ProjectNotSelected, .. }));
    let broken = fixture(&label, 1, 3, true);
    let tree = captured_project_tree(&broken.snapshot, &text).expect("typed broken selection");
    assert!(matches!(inventory(&tree), Page::Unavailable { reason: Unavailable::SelectedRelationInvalid, .. }), "missing inventory is not an empty source tree");
}

#[cfg(unix)]
#[test]
fn captured_tree_does_not_follow_retargeted_submitted_alias() {
    let live = crate::private_temp::create("captured-alias").expect("fixture root");
    let a = live.path().join("a"); let b = live.path().join("b"); let alias = live.path().join("alias");
    std::fs::create_dir(&a).expect("a"); std::fs::create_dir(&b).expect("b");
    std::os::unix::fs::symlink(&a, &alias).expect("initial alias");
    let label = alias.to_string_lossy().into_owned(); let text = ProductText::new(label.clone()).expect("alias root");
    let stored = fixture(&label, 4, 3, false);
    let before = captured_project_tree(&stored.snapshot, &text).expect("capture");
    std::fs::remove_file(&alias).expect("retarget"); std::os::unix::fs::symlink(&b, &alias).expect("new target");
    assert_eq!(captured_project_tree(&stored.reopen(), &text).expect("captured alias"), before);
    assert!(before.retained_request_binding().is_none(), "alias capture cannot authorize current Cargo files");
}
