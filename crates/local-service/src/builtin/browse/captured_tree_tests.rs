//! Source-only tests: fixture construction uses the admitted on-disk relation,
//! and no compiler, Cargo child, or live source inventory is invoked.

use super::captured_project_tree;
use backend_engine::{
    FileStore, ProductSourceRecord, ProductSourceRelation, RelationAdmissionRegistry,
    RelationState, WorkspaceHead, WorkspaceSnapshot,
};
use backend_library::browse::{
    PackageSourceMembershipPageRequestV1 as Request, PackageSourceMembershipPageResultV1 as Page,
    PackageSourceMembershipUnavailableV1 as Unavailable,
};
use backend_library::{PackageReference, ProductText};
use std::collections::BTreeSet;
use std::sync::Arc;

type TestResult<T = ()> = Result<T, String>;

struct Fixture {
    head: WorkspaceHead,
    snapshot: WorkspaceSnapshot,
    store: tempfile::TempDir,
}

impl Fixture {
    fn reopen(&self) -> TestResult<WorkspaceSnapshot> {
        let registry = RelationAdmissionRegistry::new()
            .with_relation::<ProductSourceRelation>()
            .map_err(|error| error.to_string())?;
        let store = FileStore::open_with_registry(self.store.path(), 16 * 1024 * 1024, registry)
            .map_err(|error| error.to_string())?;
        Ok(self.head.snapshot_in(Arc::new(store)))
    }
}

fn fixture(root: &str, count: usize, source_version: u8, missing: bool) -> TestResult<Fixture> {
    let project = backend_engine::package_key(root).to_bytes();
    let mut entries = Vec::new();
    let mut keys = Vec::new();
    let languages = [
        backend_compile::SourceLanguage::Rust,
        backend_compile::SourceLanguage::TypeScript,
        backend_compile::SourceLanguage::Python,
        backend_compile::SourceLanguage::Go,
    ];
    let suffixes = ["rs", "ts", "py", "go"];
    for index in 0..count {
        let path = format!("src/group{}/file{index}.{}", index % 3, suffixes[index % 4]);
        let key = backend_engine::product_source_file_key(project, &path);
        keys.push(key);
        if !(missing && index == 0) {
            // Retained structural membership must survive refused analysis.
            let row = ProductSourceRecord::file_unavailable(
                project,
                path,
                languages[index % 4],
                [2; 32],
                backend_library::SourceUnavailableReason::Unparsed,
            )
            .map_err(|error| error.to_string())?;
            entries.push((key, row));
        }
    }
    keys.sort_unstable();
    let update =
        ProductSourceRecord::project_with_membership_pages(root, [source_version; 32], keys, None)
            .map_err(|error| error.to_string())?;
    entries.push((project, update.project_record().clone()));
    entries.extend(update.membership_pages().iter().cloned());
    let coverage = super::super::admitted_coverage().map_err(|error| error.to_string())?;
    let relation = RelationState::<ProductSourceRelation>::from_entries(entries, coverage)
        .map_err(|error| error.to_string())?;
    let registry = RelationAdmissionRegistry::new()
        .with_relation::<ProductSourceRelation>()
        .map_err(|error| error.to_string())?;
    let directory =
        crate::private_temp::create("captured-tree").map_err(|error| error.to_string())?;
    let store = FileStore::open_with_registry(directory.path(), 16 * 1024 * 1024, registry)
        .map_err(|error| error.to_string())?;
    store
        .write_relation_state(&relation)
        .map_err(|error| error.to_string())?;
    let head = super::super::head_from_relation(relation).map_err(|error| error.to_string())?;
    let snapshot = head.snapshot_in(Arc::new(store));
    Ok(Fixture {
        head,
        snapshot,
        store: directory,
    })
}

fn inventory(tree: &backend_library::browse::ProjectTree) -> TestResult<&Page> {
    match &tree.source {
        backend_library::browse::TreeSource::CapturedSource { membership } => Ok(membership),
        _ => Err("captured source reader required".to_owned()),
    }
}

#[test]
fn captured_tree_is_language_neutral_bounded_and_cold_replayable_after_refusal() -> TestResult {
    let root = crate::private_temp::create("captured-root").map_err(|error| error.to_string())?;
    let label = root
        .path()
        .join("deleted-project")
        .to_string_lossy()
        .into_owned();
    let stored = fixture(&label, 140, 3, false)?;
    let text = ProductText::new(label.clone()).map_err(|error| error.to_string())?;
    let first =
        captured_project_tree(&stored.snapshot, &text).map_err(|error| error.to_string())?;
    assert!(first.has_admissible_shape());
    assert!(
        first.retained_request_binding().is_none(),
        "no Cargo source capability"
    );
    assert!(
        !std::path::Path::new(&label).exists(),
        "tree does not require a live source root"
    );
    let reopened = stored.reopen()?;
    assert_eq!(
        captured_project_tree(&reopened, &text).map_err(|error| error.to_string())?,
        first
    );
    let Page::Page {
        package,
        source_relation_root,
        source_version,
        file_count,
        files,
        next,
        ..
    } = inventory(&first)?
    else {
        return Err("expected first membership page".to_owned());
    };
    assert_eq!(*file_count, 140);
    assert_eq!(
        files.len(),
        backend_library::DEFAULT_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES as usize
    );
    assert!(
        files.iter().all(|file| file.source_identity.is_none()),
        "unavailable analysis never fabricates source identity"
    );
    let mut selected: BTreeSet<_> = files
        .iter()
        .map(|file| (file.file_key, file.path.clone()))
        .collect();
    let mut languages: BTreeSet<_> = files
        .iter()
        .map(|file| format!("{:?}", file.language))
        .collect();
    let mut cursor = next.clone();
    while let Some(next) = cursor {
        let request = Request::first(package.clone())
            .with_selection(*source_relation_root, *source_version)
            .with_cursor(next);
        let page = super::super::commands::package_source_membership_page_from_snapshot(
            &reopened, &request,
        )
        .map_err(|error| error.to_string())?;
        let Page::Page { files, next, .. } = page else {
            return Err("expected same-capture continuation page".to_owned());
        };
        assert!(files.len() <= 128);
        selected.extend(files.iter().map(|file| (file.file_key, file.path.clone())));
        languages.extend(files.iter().map(|file| format!("{:?}", file.language)));
        cursor = next;
    }
    assert_eq!(
        selected.len(),
        140,
        "exact complete union, no omitted or duplicated members"
    );
    assert_eq!(
        languages,
        ["Go", "Python", "Rust", "TypeScript"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let changed = fixture(&label, 140, 4, false)?;
    let cursor = next
        .clone()
        .ok_or_else(|| "partial first page cursor missing".to_owned())?;
    let request = Request::first(package.clone())
        .with_selection(*source_relation_root, *source_version)
        .with_cursor(cursor.clone());
    assert!(matches!(
        super::super::commands::package_source_membership_page_from_snapshot(
            &changed.snapshot,
            &request
        )
        .map_err(|error| error.to_string())?,
        Page::Stale { .. }
    ));
    let other = ProductText::new(format!("{label}-other")).map_err(|error| error.to_string())?;
    let request = Request::first(PackageReference::Local(other))
        .with_selection(*source_relation_root, *source_version)
        .with_cursor(cursor);
    assert!(
        super::super::commands::package_source_membership_page_from_snapshot(&reopened, &request)
            .is_err(),
        "a cursor cannot be submitted under another root"
    );
    Ok(())
}

#[test]
fn captured_tree_preserves_missing_inventory_and_ignores_live_files() -> TestResult {
    let live =
        crate::private_temp::create("live-captured-tree").map_err(|error| error.to_string())?;
    let label = live.path().to_string_lossy().into_owned();
    let text = ProductText::new(label.clone()).map_err(|error| error.to_string())?;
    let stored = fixture(&label, 4, 3, false)?;
    let before =
        captured_project_tree(&stored.snapshot, &text).map_err(|error| error.to_string())?;
    std::fs::write(
        live.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"unowned\"]\n",
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(
        live.path().join("new.py"),
        "print('outside captured inventory')\n",
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        captured_project_tree(&stored.reopen()?, &text).map_err(|error| error.to_string())?,
        before,
        "live files and Cargo must not substitute for the selected capture"
    );
    let other =
        ProductText::new(format!("{label}/unselected")).map_err(|error| error.to_string())?;
    let tree =
        captured_project_tree(&stored.snapshot, &other).map_err(|error| error.to_string())?;
    assert!(matches!(
        inventory(&tree)?,
        Page::Unavailable {
            reason: Unavailable::ProjectNotSelected,
            ..
        }
    ));
    let broken = fixture(&label, 1, 3, true)?;
    let tree = captured_project_tree(&broken.snapshot, &text).map_err(|error| error.to_string())?;
    assert!(
        matches!(
            inventory(&tree)?,
            Page::Unavailable {
                reason: Unavailable::SelectedRelationInvalid,
                ..
            }
        ),
        "missing inventory is not an empty source tree"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn captured_tree_does_not_follow_retargeted_submitted_alias() -> TestResult {
    let live = crate::private_temp::create("captured-alias").map_err(|error| error.to_string())?;
    let a = live.path().join("a");
    let b = live.path().join("b");
    let alias = live.path().join("alias");
    std::fs::create_dir(&a).map_err(|error| error.to_string())?;
    std::fs::create_dir(&b).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&a, &alias).map_err(|error| error.to_string())?;
    let label = alias.to_string_lossy().into_owned();
    let text = ProductText::new(label.clone()).map_err(|error| error.to_string())?;
    let stored = fixture(&label, 4, 3, false)?;
    let before =
        captured_project_tree(&stored.snapshot, &text).map_err(|error| error.to_string())?;
    std::fs::remove_file(&alias).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&b, &alias).map_err(|error| error.to_string())?;
    assert_eq!(
        captured_project_tree(&stored.reopen()?, &text).map_err(|error| error.to_string())?,
        before
    );
    assert!(
        before.retained_request_binding().is_none(),
        "alias capture cannot authorize current Cargo files"
    );
    Ok(())
}
