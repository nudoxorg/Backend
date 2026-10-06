//! Source-relation paging that a warm search must not repeat.
//!
//! The typed corpus is a pure function of the workspace root. Once that corpus
//! is resident, search and graph queries clone it and leave this relation
//! unread. The measurement below is the page those queries used to pay first.

use super::{BuiltinModelError, IndexedSources, ProductSourceRecord};
use backend_engine::WorkspaceSnapshot;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct StoredSnapshot {
    snapshot: WorkspaceSnapshot,
    head: backend_engine::WorkspaceHead,
    directory: PathBuf,
}

impl StoredSnapshot {
    fn cold_reopen(&self) -> Result<WorkspaceSnapshot, BuiltinModelError> {
        let registry = backend_engine::RelationAdmissionRegistry::new()
            .with_relation::<backend_engine::ProductSourceRelation>()
            .map_err(|error| BuiltinModelError(format!("register source relation: {error:?}")))?;
        let store = backend_engine::FileStore::open_with_registry(
            &self.directory,
            16 * 1024 * 1024,
            registry,
        )
        .map_err(|error| BuiltinModelError(format!("reopen source store: {error:?}")))?;
        Ok(self.head.snapshot_in(Arc::new(store)))
    }
}

impl Drop for StoredSnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl std::ops::Deref for StoredSnapshot {
    type Target = WorkspaceSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

const PROJECTS: usize = 8;
const FILES_PER_PROJECT: usize = 8;
const DECLARATIONS_PER_FILE: usize = 4;

/// Times a full source-relation page against a resident corpus hit.
///
/// Fixture construction, including the checked workspace head, happens before
/// the timer. `page` is [`super::read_indexed_sources`]. `warm` is the corpus
/// admission a search uses when that workspace is already resident, and its
/// prepare closure must not run. Image reopen and the structural plan are not
/// in either timer.
///
/// The checked head retains every relation node. Eight projects of eight files
/// and eight projects of ten files both admit. The fixture stays at eight by
/// eight, which is the shape the page measurement uses.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_search_source_page() {
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let declarations = shared_declarations(DECLARATIONS_PER_FILE).expect("declarations");
    let snapshot = snapshot_holding(PROJECTS, FILES_PER_PROJECT, &declarations).expect("snapshot");
    let checked = super::read_indexed_sources(&snapshot).expect("page");
    let projects = checked.projects.len();
    let files = checked.files.len();
    let declarations_present = checked.files.iter().all(|(_, record)| {
        record.file_fields().is_some_and(|file| {
            file.declarations.len() == DECLARATIONS_PER_FILE
                && file
                    .declarations
                    .first()
                    .is_some_and(|declaration| declaration.name() == "item0")
        })
    });
    (projects == PROJECTS && files == PROJECTS * FILES_PER_PROJECT && declarations_present)
        .then_some(())
        .expect("source page fixture did not round-trip");
    drop(checked);

    let page = sample(WARMUPS, SAMPLES, || {
        super::read_indexed_sources(&snapshot)
            .expect("page")
            .files
            .len()
    });
    let workspace = snapshot.root();
    let corpus = backend_extension_trustfall::SemanticQueryCorpus::admit_with_limits(
        workspace,
        Vec::new(),
        backend_extension_trustfall::Limits::default(),
    )
    .expect("corpus");
    let mut owner = super::query::SearchSnapshotOwner::default();
    owner
        .shared_corpus(workspace, || Ok::<_, &str>(corpus))
        .expect("prime");
    let mut prepares = 0u64;
    let warm = sample(WARMUPS, SAMPLES, || {
        owner
            .admit_corpus(
                workspace,
                || {
                    prepares = prepares.saturating_add(1);
                    Err::<IndexedSources, &str>("warm path paged sources")
                },
                |_| Err("warm path rebuilt the corpus"),
            )
            .expect("reuse")
            .facts()
            .len()
    });
    let (page_median, page_p95) = percentiles(&page);
    let (warm_median, warm_p95) = percentiles(&warm);
    println!(
        "search_source_page projects={PROJECTS} files={} declarations={DECLARATIONS_PER_FILE} page_median_ns={page_median} page_p95_ns={page_p95} warm_median_ns={warm_median} warm_p95_ns={warm_p95} warm_prepares={prepares}",
        PROJECTS * FILES_PER_PROJECT
    );
}

/// Times a full source-relation page against one package's key lookups.
///
/// The relation is written before the timer. `page` clones every file, which
/// is what a package graph query used to do. `lookup` opens the same tree,
/// reads one project's frontier, and batches its sorted file keys so shared
/// branch and leaf paths are loaded once.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_package_source_lookup() -> (u128, u128) {
    const PROJECTS: usize = 64;
    const FILES_PER_PROJECT: usize = 16;
    const DECLARATIONS_PER_FILE: usize = 4;
    const SAMPLES: usize = 16;
    const WARMUPS: usize = 2;
    let declarations = shared_declarations(DECLARATIONS_PER_FILE).expect("declarations");
    let entries = source_entries(PROJECTS, FILES_PER_PROJECT, &declarations).expect("entries");
    let relation =
        backend_engine::RelationState::<backend_engine::ProductSourceRelation>::from_entries(
            entries,
            super::admitted_coverage().expect("coverage"),
        )
        .expect("relation");
    let directory = std::env::temp_dir().join(format!(
        "nudox-package-source-lookup-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("store directory");
    let registry = backend_engine::RelationAdmissionRegistry::new()
        .with_relation::<backend_engine::ProductSourceRelation>()
        .expect("register");
    let store =
        backend_engine::FileStore::open_with_registry(&directory, 64 * 1024 * 1024, registry)
            .expect("store");
    store
        .write_relation_state(&relation)
        .expect("write relation");
    let root = *relation.root().as_bytes();
    let target = backend_engine::package_key("pkg-63");
    let target_key = target.to_bytes();
    let claim = || {
        backend_engine::UntrustedId::<backend_engine::ProductSourceRelation>::from_wire(
            &root,
            backend_engine::IdContext::relation::<backend_engine::ProductSourceRelation>(),
        )
        .expect("root claim")
    };
    let open = || backend_engine::LazyTree::open(&store, claim()).expect("open");
    let paged = {
        let tree = open();
        let mut after = None;
        let mut files = Vec::new();
        loop {
            let page = tree
                .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .expect("page");
            for (key, record) in page.entries() {
                if record
                    .file_fields()
                    .is_some_and(|file| file.project == target_key)
                {
                    files.push((*key, record.clone()));
                }
            }
            let Some(next) = page.next().copied() else {
                break;
            };
            after = Some(next);
        }
        files.sort_by_key(|(key, _)| *key);
        files
    };
    let looked_up = {
        let tree = open();
        let project = tree
            .lookup(&target_key)
            .expect("lookup project")
            .expect("project row");
        let fields = project.project_fields().expect("project fields");
        let file_keys = fields
            .iter_file_keys(target_key, |page_key| {
                tree.lookup(page_key).map_err(|error| format!("{error:?}"))
            })
            .collect::<Result<Vec<_>, _>>()
            .expect("resolve complete package frontier");
        let mut files = Vec::with_capacity(file_keys.len());
        let records = tree
            .lookup_many_sorted(&file_keys)
            .expect("lookup package files");
        for (key, record) in file_keys.into_iter().zip(records) {
            files.push((key, record.expect("file row")));
        }
        files.sort_by_key(|(key, _)| *key);
        files
    };
    (paged.len() == FILES_PER_PROJECT
        && looked_up.len() == FILES_PER_PROJECT
        && paged == looked_up)
        .then_some(())
        .expect("package lookup did not match the filtered page");

    let page = sample(WARMUPS, SAMPLES, || {
        let tree = open();
        let mut after = None;
        let mut files = Vec::new();
        loop {
            let page = tree
                .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
                .expect("page");
            for (key, record) in page.entries() {
                if record.file_fields().is_some() {
                    files.push((*key, record.clone()));
                }
            }
            let Some(next) = page.next().copied() else {
                break;
            };
            after = Some(next);
        }
        files.len()
    });
    let lookup = sample(WARMUPS, SAMPLES, || {
        let tree = open();
        let project = tree
            .lookup(&target_key)
            .expect("lookup project")
            .expect("project row");
        let fields = project.project_fields().expect("project fields");
        let file_keys = fields
            .iter_file_keys(target_key, |page_key| {
                tree.lookup(page_key).map_err(|error| format!("{error:?}"))
            })
            .collect::<Result<Vec<_>, _>>()
            .expect("resolve complete package frontier");
        tree.lookup_many_sorted(&file_keys)
            .expect("lookup package files")
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .expect("file rows")
            .len()
    });
    let (page_median, page_p95) = percentiles(&page);
    let (lookup_median, lookup_p95) = percentiles(&lookup);
    println!(
        "package_source_lookup projects={PROJECTS} files={} target_files={FILES_PER_PROJECT} declarations={DECLARATIONS_PER_FILE} page_median_ns={page_median} page_p95_ns={page_p95} lookup_median_ns={lookup_median} lookup_p95_ns={lookup_p95}",
        PROJECTS * FILES_PER_PROJECT
    );
    let admitted = snapshot_holding(8, 8, &declarations).expect("admitted snapshot");
    let admitted_package = backend_engine::package_key("pkg-7");
    let admitted_page = sample(WARMUPS, SAMPLES, || {
        super::read_indexed_sources(&admitted)
            .expect("page")
            .files
            .len()
    });
    let admitted_lookup = sample(WARMUPS, SAMPLES, || {
        super::read_package_sources(&admitted, admitted_package)
            .expect("lookup")
            .files
            .len()
    });
    let (admitted_page_median, admitted_page_p95) = percentiles(&admitted_page);
    let (admitted_lookup_median, admitted_lookup_p95) = percentiles(&admitted_lookup);
    println!(
        "package_source_lookup_admitted projects=8 files=64 target_files=8 declarations={DECLARATIONS_PER_FILE} page_median_ns={admitted_page_median} page_p95_ns={admitted_page_p95} lookup_median_ns={admitted_lookup_median} lookup_p95_ns={admitted_lookup_p95}"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&directory);
    (page_median, lookup_median)
}

fn snapshot_holding(
    projects: usize,
    files_per_project: usize,
    declarations: &Arc<[backend_compile::SourceDeclaration]>,
) -> Result<StoredSnapshot, BuiltinModelError> {
    let entries = source_entries(projects, files_per_project, declarations)?;
    let relation =
        backend_engine::RelationState::<backend_engine::ProductSourceRelation>::from_entries(
            entries,
            super::admitted_coverage()?,
        )
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    store_relation(
        relation,
        format!("populated-{projects}-{files_per_project}"),
    )
}

fn store_relation(
    relation: backend_engine::RelationState<backend_engine::ProductSourceRelation>,
    label: String,
) -> Result<StoredSnapshot, BuiltinModelError> {
    static NEXT_STORE: AtomicU64 = AtomicU64::new(0);
    let generation = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "nudox-source-page-{}-{generation}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory)
        .map_err(|error| BuiltinModelError(format!("create source store directory: {error}")))?;
    let registry = backend_engine::RelationAdmissionRegistry::new()
        .with_relation::<backend_engine::ProductSourceRelation>()
        .map_err(|error| BuiltinModelError(format!("register source relation: {error:?}")))?;
    let store =
        backend_engine::FileStore::open_with_registry(&directory, 16 * 1024 * 1024, registry)
            .map_err(|error| BuiltinModelError(format!("open source store: {error:?}")))?;
    store
        .write_relation_state(&relation)
        .map_err(|error| BuiltinModelError(format!("write source relation: {error:?}")))?;
    let head = super::head_from_relation(relation)?;
    Ok(StoredSnapshot {
        snapshot: head.snapshot_in(Arc::new(store)),
        head,
        directory,
    })
}

/// A stored snapshot whose source relation names exactly `labels`, one file
/// each: the fixture for residences keyed by the relation root.
#[cfg(test)]
pub(super) fn snapshot_of_projects(labels: &[String]) -> Result<StoredSnapshot, BuiltinModelError> {
    let declarations = shared_declarations(1).map_err(BuiltinModelError)?;
    let entries = labelled_source_entries(labels.iter().cloned(), 1, &declarations)?;
    let relation =
        backend_engine::RelationState::<backend_engine::ProductSourceRelation>::from_entries(
            entries,
            super::admitted_coverage()?,
        )
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    store_relation(relation, format!("projects-{}", labels.len()))
}

fn source_entries(
    projects: usize,
    files_per_project: usize,
    declarations: &Arc<[backend_compile::SourceDeclaration]>,
) -> Result<Vec<([u8; 32], ProductSourceRecord)>, BuiltinModelError> {
    labelled_source_entries(
        (0..projects).map(|project_index| format!("pkg-{project_index}")),
        files_per_project,
        declarations,
    )
}

fn labelled_source_entries(
    labels: impl ExactSizeIterator<Item = String>,
    files_per_project: usize,
    declarations: &Arc<[backend_compile::SourceDeclaration]>,
) -> Result<Vec<([u8; 32], ProductSourceRecord)>, BuiltinModelError> {
    let projects = labels.len();
    let mut entries =
        Vec::with_capacity(projects.saturating_add(projects.saturating_mul(files_per_project)));
    for label in labels {
        let project_key = backend_engine::package_key(&label).to_bytes();
        let mut file_keys = Vec::with_capacity(files_per_project);
        for file_index in 0..files_per_project {
            let path = format!("src/f{file_index}.rs");
            let key = backend_engine::product_source_file_key(project_key, &path);
            if key == project_key {
                return Err(BuiltinModelError(
                    "source file key collided with its project coordinate".to_owned(),
                ));
            }
            let record = ProductSourceRecord::file(
                project_key,
                path,
                backend_compile::SourceLanguage::Rust,
                [1; 32],
                [2; 32],
                Arc::clone(declarations),
            )
            .map_err(BuiltinModelError)?;
            file_keys.push(key);
            entries.push((key, record));
        }
        file_keys.sort_unstable();
        let project_update =
            ProductSourceRecord::project_with_membership_pages(label, [3; 32], file_keys, None)
                .map_err(BuiltinModelError)?;
        entries.push((project_key, project_update.project_record().clone()));
        entries.extend(project_update.membership_pages().iter().cloned());
    }
    Ok(entries)
}

fn shared_declarations(count: usize) -> Result<Arc<[backend_compile::SourceDeclaration]>, String> {
    let mut declarations = Vec::with_capacity(count);
    for index in 0..count {
        let line = u32::try_from(index)
            .map_err(|_| "declaration line overflows".to_owned())?
            .saturating_add(1);
        let name = format!("item{index}");
        declarations.push(backend_compile::SourceDeclaration::new(
            name,
            backend_compile::DeclarationKind::Function,
            line,
            format!("fn item{index}()"),
            "doc",
        )?);
    }
    Ok(declarations.into())
}

#[allow(clippy::indexing_slicing)]
fn file_key(index: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    let bytes = index.to_be_bytes();
    key[0] = 0xA5;
    key[28] = bytes[0];
    key[29] = bytes[1];
    key[30] = bytes[2];
    key[31] = bytes[3];
    key
}

fn sample<T>(warmups: usize, samples: usize, mut body: impl FnMut() -> T) -> Vec<u128> {
    for _ in 0..warmups {
        let _ = body();
    }
    let mut samples_ns = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = std::time::Instant::now();
        let _ = body();
        samples_ns.push(started.elapsed().as_nanos());
    }
    samples_ns
}

#[allow(clippy::indexing_slicing)]
fn percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{shared_declarations, snapshot_holding, store_relation};
    use backend_library::{
        PackageReference, PackageSourceMembershipPageRequestV1,
        PackageSourceMembershipPageResultV1, PackageSourceMembershipUnavailableV1,
    };
    use backend_version::{ContentId, SourceFactDomain};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn membership_page(
        snapshot: &backend_engine::WorkspaceSnapshot,
        request: &PackageSourceMembershipPageRequestV1,
    ) -> PackageSourceMembershipPageResultV1 {
        super::super::commands::package_source_membership_page_from_snapshot(snapshot, request)
            .expect("typed membership page")
    }

    fn local_package(label: &str) -> PackageReference {
        PackageReference::parse(label).expect("local package reference")
    }

    #[test]
    fn source_membership_pages_are_bounded_root_bound_and_cold_replayable() {
        let declarations = shared_declarations(1).expect("declarations");
        let stored = snapshot_holding(1, 2_043, &declarations).expect("indexed snapshot");
        let package = local_package("pkg-0");
        let mut first_request = PackageSourceMembershipPageRequestV1::first(package.clone());
        first_request.limit = 128;
        let first = membership_page(&stored, &first_request);
        let (source_relation_root, source_version, file_count, first_files, first_cursor) =
            match first {
                PackageSourceMembershipPageResultV1::Page {
                    source_relation_root,
                    source_version,
                    file_count,
                    start_offset,
                    files,
                    next,
                    ..
                } => {
                    assert_eq!(start_offset, 0);
                    assert_eq!(files.len(), 128);
                    assert_eq!(file_count, 2_043);
                    (
                        source_relation_root,
                        source_version,
                        file_count,
                        files,
                        next.expect("first page continuation"),
                    )
                }
                other => panic!("expected first membership page, got {other:?}"),
            };

        // Open the same stored head through a fresh FileStore, then resume with
        // only the typed cursor. The API keeps no page state between calls.
        let reopened = stored.cold_reopen().expect("cold source store reopen");
        let mut continuation_request = PackageSourceMembershipPageRequestV1::first(package.clone())
            .with_selection(source_relation_root, source_version)
            .with_cursor(first_cursor.clone());
        continuation_request.limit = 128;
        let continuation = membership_page(&reopened, &continuation_request);
        let (second_files, second_cursor) = match continuation {
            PackageSourceMembershipPageResultV1::Page {
                source_relation_root: resumed_root,
                source_version: resumed_version,
                file_count: resumed_count,
                start_offset,
                files,
                next,
                ..
            } => {
                assert_eq!(resumed_root, source_relation_root);
                assert_eq!(resumed_version, source_version);
                assert_eq!(resumed_count, file_count);
                assert_eq!(start_offset, 128);
                assert_eq!(files.len(), 128);
                (files, next)
            }
            other => panic!("expected resumed membership page, got {other:?}"),
        };

        let changed = snapshot_holding(1, 2_044, &declarations).expect("changed snapshot");
        assert!(
            matches!(
                membership_page(&changed, &continuation_request),
                PackageSourceMembershipPageResultV1::Stale { .. }
            ),
            "a changed selected relation root must refuse the old cursor"
        );

        let mut selected = first_files
            .iter()
            .map(|file| (file.file_key, file.path.clone()))
            .collect::<BTreeSet<_>>();
        selected.extend(
            second_files
                .iter()
                .map(|file| (file.file_key, file.path.clone())),
        );
        let mut next = second_cursor;
        let mut expected_offset = 256_u32;
        let mut pages = 2;
        while let Some(cursor) = next {
            let mut request = PackageSourceMembershipPageRequestV1::first(package.clone())
                .with_selection(source_relation_root, source_version)
                .with_cursor(cursor);
            request.limit = 128;
            let result = membership_page(&reopened, &request);
            let PackageSourceMembershipPageResultV1::Page {
                start_offset,
                files,
                next: following,
                ..
            } = result
            else {
                panic!("same-root page must remain readable");
            };
            assert_eq!(start_offset, expected_offset);
            assert!(!files.is_empty());
            assert!(files.len() <= 128);
            expected_offset += u32::try_from(files.len()).expect("bounded page count");
            selected.extend(files.iter().map(|file| (file.file_key, file.path.clone())));
            next = following;
            pages += 1;
        }
        assert!(
            pages > 2,
            "the fixture must cross several bounded API pages"
        );
        assert_eq!(expected_offset, file_count);
        assert_eq!(selected.len(), file_count as usize);
        let expected =
            super::super::read_package_sources(&reopened, backend_engine::package_key("pkg-0"))
                .expect("fixture's selected Project members")
                .files
                .into_iter()
                .map(|(key, record)| {
                    let path = record.file_fields().expect("file fields").path.to_owned();
                    (key, path)
                })
                .collect::<BTreeSet<_>>();
        assert_eq!(selected, expected);
    }

    #[test]
    fn source_membership_preserves_typed_digest_and_rejects_absent_members() {
        let label = "pkg-identities";
        let project = backend_engine::package_key(label).to_bytes();
        let path = "src/lib.ts";
        let bytes = b"export const answer = 42;\n";
        let content_version =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(bytes).to_bytes();
        let source_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(bytes);
        let file_key = backend_engine::product_source_file_key(project, path);
        let declarations = shared_declarations(1).expect("declarations");
        let file = backend_engine::ProductSourceRecord::identified_file_within_row_capacity(
            project,
            path,
            backend_compile::SourceLanguage::TypeScript,
            content_version,
            [2; 32],
            Arc::clone(&declarations),
            source_identity,
        )
        .expect("identified source file");
        let project_record =
            backend_engine::ProductSourceRecord::project(label, [3; 32], vec![file_key])
                .expect("selected Project");
        let relation =
            backend_engine::RelationState::<backend_engine::ProductSourceRelation>::from_entries(
                vec![(project, project_record), (file_key, file)],
                super::super::admitted_coverage().expect("coverage"),
            )
            .expect("source relation");
        let snapshot = store_relation(relation, "source-membership-identities".to_owned())
            .expect("stored source relation");
        let result = membership_page(
            &snapshot,
            &PackageSourceMembershipPageRequestV1::first(local_package(label)),
        );
        let PackageSourceMembershipPageResultV1::Page { files, .. } = result else {
            panic!("identified member page");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, path);
        assert_eq!(files[0].content_version, content_version);
        assert_eq!(files[0].source_identity, Some(*source_identity.as_ref()));

        let missing_label = "pkg-absent-member";
        let missing_project = backend_engine::package_key(missing_label).to_bytes();
        let absent_key = backend_engine::product_source_file_key(missing_project, "src/missing.ts");
        let project_record =
            backend_engine::ProductSourceRecord::project(missing_label, [4; 32], vec![absent_key])
                .expect("Project referring to absent row");
        let relation =
            backend_engine::RelationState::<backend_engine::ProductSourceRelation>::from_entries(
                vec![(missing_project, project_record)],
                super::super::admitted_coverage().expect("coverage"),
            )
            .expect("relation with absent selected member");
        let snapshot = store_relation(relation, "source-membership-absent-file".to_owned())
            .expect("stored incomplete relation");
        assert_eq!(
            membership_page(
                &snapshot,
                &PackageSourceMembershipPageRequestV1::first(local_package(missing_label)),
            ),
            PackageSourceMembershipPageResultV1::Unavailable {
                package: local_package(missing_label),
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            }
        );
    }

    #[test]
    fn one_package_lookup_matches_its_rows_in_the_full_page() {
        let declarations = shared_declarations(4).expect("declarations");
        let snapshot = snapshot_holding(8, 8, &declarations).expect("snapshot");
        let full = super::super::read_indexed_sources(&snapshot).expect("page");
        let package = backend_engine::package_key("pkg-7");
        let scoped = super::super::read_package_sources(&snapshot, package).expect("lookup");
        let project_key = package.to_bytes();
        let mut expected = full
            .files
            .iter()
            .filter(|(_, record)| {
                record
                    .file_fields()
                    .is_some_and(|file| file.project == project_key)
            })
            .map(|(key, record)| (*key, record.clone()))
            .collect::<Vec<_>>();
        expected.sort_by_key(|(key, _)| *key);
        let mut actual = scoped.files.clone();
        actual.sort_by_key(|(key, _)| *key);
        assert_eq!(full.files.len(), 64);
        assert_eq!(scoped.projects.len(), 1);
        assert_eq!(
            scoped
                .projects
                .values()
                .next()
                .map(|project| project.label.as_str()),
            Some("pkg-7")
        );
        assert_eq!(actual, expected);
        let missing = super::super::read_package_sources(
            &snapshot,
            backend_engine::package_key("pkg-missing"),
        )
        .expect("missing package");
        assert!(missing.projects.is_empty());
        assert!(missing.files.is_empty());
        let (page_median, lookup_median) = super::measure_package_source_lookup();
        assert!(
            lookup_median < page_median,
            "package lookup {lookup_median} ns was not cheaper than paging every file {page_median} ns"
        );
    }

    #[test]
    fn paged_sources_keep_project_labels_paths_and_declarations() {
        let declarations = shared_declarations(2).expect("declarations");
        let snapshot = snapshot_holding(2, 3, &declarations).expect("snapshot");
        let sources = super::super::read_indexed_sources(&snapshot).expect("page");
        assert_eq!(sources.projects.len(), 2);
        assert_eq!(sources.files.len(), 6);
        for (key, project) in &sources.projects {
            assert_eq!(*key, backend_engine::package_key(&project.label).to_bytes());
            assert_eq!(project.files.len(), 3);
        }
        let mut labels = sources
            .projects
            .values()
            .map(|project| project.label.clone())
            .collect::<Vec<_>>();
        labels.sort();
        assert_eq!(labels, ["pkg-0".to_owned(), "pkg-1".to_owned()]);
        let mut paths = sources
            .files
            .iter()
            .filter_map(|(_, record)| record.file_fields().map(|file| file.path.to_owned()))
            .collect::<Vec<_>>();
        paths.sort();
        let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                "src/f0.rs",
                "src/f0.rs",
                "src/f1.rs",
                "src/f1.rs",
                "src/f2.rs",
                "src/f2.rs",
            ]
        );
        assert!(sources.files.iter().all(|(_, record)| {
            record.file_fields().is_some_and(|file| {
                file.declarations.len() == 2
                    && file
                        .declarations
                        .get(1)
                        .is_some_and(|declaration| declaration.name() == "item1")
            })
        }));
    }

    #[test]
    fn an_empty_workspace_pages_no_sources() {
        let relation =
            backend_engine::RelationState::<backend_engine::ProductSourceRelation>::empty(
                super::super::admitted_coverage().expect("coverage"),
            );
        let snapshot = super::store_relation(relation, "empty".to_owned()).expect("snapshot");
        let sources = super::super::read_indexed_sources(&snapshot).expect("page");
        assert!(sources.projects.is_empty());
        assert!(sources.files.is_empty());
    }

    #[test]
    fn a_project_stored_under_another_key_is_rejected() {
        let label = "orphan";
        let mut key = backend_engine::package_key(label).to_bytes();
        if let Some(first) = key.first_mut() {
            *first ^= 0xff;
        }
        let record = backend_engine::ProductSourceRecord::project(label, [0; 32], Vec::new())
            .expect("project");
        let relation = backend_engine::RelationState::from_entries(
            vec![(key, record)],
            super::super::admitted_coverage().expect("coverage"),
        )
        .expect("relation");
        let snapshot = super::store_relation(relation, "miskeyed".to_owned()).expect("snapshot");
        let Err(error) = super::super::read_indexed_sources(&snapshot) else {
            panic!("miskeyed project must be rejected");
        };
        assert_eq!(
            error.to_string(),
            "project record does not match its canonical coordinate"
        );
    }

    #[test]
    fn eight_projects_of_eight_and_ten_files_page() {
        let declarations = shared_declarations(4).expect("declarations");
        let stored = snapshot_holding(8, 8, &declarations).expect("8x8");
        let sources = super::super::read_indexed_sources(&stored).expect("page");
        assert_eq!(sources.projects.len(), 8);
        assert_eq!(sources.files.len(), 64);
        assert!(sources.files.iter().all(|(_, record)| {
            record.file_fields().is_some_and(|file| {
                file.declarations.len() == 4
                    && file
                        .declarations
                        .first()
                        .is_some_and(|declaration| declaration.name() == "item0")
            })
        }));
        let wider = snapshot_holding(8, 10, &declarations).expect("8x10");
        let wider_sources = super::super::read_indexed_sources(&wider).expect("page 8x10");
        assert_eq!(wider_sources.projects.len(), 8);
        assert_eq!(wider_sources.files.len(), 80);
    }

    #[test]
    fn reopened_source_pages_resolve_2043_and_10000_file_projects_exactly() {
        let declarations = shared_declarations(1).expect("declarations");
        for count in [2_043, 10_000] {
            let snapshot = snapshot_holding(1, count, &declarations).expect("stored snapshot");
            let all = super::super::read_indexed_sources(&snapshot).expect("full source page");
            assert_eq!(all.projects.len(), 1);
            assert_eq!(all.files.len(), count);
            let project = all.projects.values().next().expect("one project");
            assert_eq!(project.files.len(), count);
            assert!(all.files.iter().all(|(key, record)| {
                record.file_fields().is_some_and(|fields| {
                    fields.project == project.package.to_bytes()
                        && backend_engine::product_source_file_key(fields.project, fields.path)
                            == *key
                })
            }));
            let scoped = super::super::read_package_sources(&snapshot, project.package)
                .expect("scoped source page");
            assert_eq!(scoped.files.len(), count);
            assert_eq!(
                scoped.projects[&project.package.to_bytes()].files.len(),
                count
            );
        }
    }

    #[test]
    fn full_source_page_rejects_missing_and_unreferenced_membership_pages() {
        let declarations = shared_declarations(1).expect("declarations");
        let update = backend_engine::ProductSourceRecord::project_with_membership_pages(
            "pkg-0",
            [3; 32],
            (0..2_043)
                .map(|index| {
                    backend_engine::product_source_file_key(
                        backend_engine::package_key("pkg-0").to_bytes(),
                        &format!("src/f{index}.rs"),
                    )
                })
                .collect::<Vec<_>>(),
            None,
        )
        .expect("paged project update");
        let project_key = update.project_key();
        let missing_page_key = update.membership_pages()[0].0;
        let mut entries = update
            .membership_pages()
            .iter()
            .filter(|(key, _)| *key != missing_page_key)
            .cloned()
            .collect::<Vec<_>>();
        entries.push((project_key, update.project_record().clone()));
        let relation =
            backend_engine::RelationState::<backend_engine::ProductSourceRelation>::from_entries(
                entries,
                super::super::admitted_coverage().expect("coverage"),
            )
            .expect("relation with missing page");
        let snapshot = super::store_relation(relation, "missing-membership-page".to_owned())
            .expect("snapshot");
        let Err(error) = super::super::read_indexed_sources(&snapshot) else {
            panic!("missing membership page must reject the source page");
        };
        assert!(error.to_string().contains("missing"));

        let one_file = backend_engine::product_source_file_key(project_key, "src/only.rs");
        let inline = backend_engine::ProductSourceRecord::project("pkg-0", [4; 32], vec![one_file])
            .expect("inline project");
        let file = backend_engine::ProductSourceRecord::file(
            project_key,
            "src/only.rs",
            backend_compile::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            Arc::clone(&declarations),
        )
        .expect("source file");
        let mut orphaned = vec![(project_key, inline), (one_file, file)];
        orphaned.push(update.membership_pages()[0].clone());
        let relation =
            backend_engine::RelationState::<backend_engine::ProductSourceRelation>::from_entries(
                orphaned,
                super::super::admitted_coverage().expect("coverage"),
            )
            .expect("relation with orphan page");
        let snapshot =
            super::store_relation(relation, "orphan-membership-page".to_owned()).expect("snapshot");
        let Err(error) = super::super::read_indexed_sources(&snapshot) else {
            panic!("unreferenced membership page must reject the source page");
        };
        assert!(error.to_string().contains("unreferenced membership page"));
    }

    #[test]
    fn a_duplicate_file_frontier_is_rejected_before_paging() {
        let key = super::file_key(7);
        let error = backend_engine::ProductSourceRecord::project("pkg", [0; 32], vec![key, key])
            .expect_err("duplicate frontier");
        assert_eq!(
            error,
            "project file frontier is unordered or holds a duplicate"
        );
    }
}
