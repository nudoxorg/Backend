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
    directory: PathBuf,
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
        let mut files = Vec::with_capacity(fields.files.len());
        let records = tree
            .lookup_many_sorted(fields.files)
            .expect("lookup package files");
        for (key, record) in fields.files.iter().copied().zip(records) {
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
        tree.lookup_many_sorted(fields.files)
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
        let project =
            ProductSourceRecord::project(label, [3; 32], file_keys).map_err(BuiltinModelError)?;
        entries.push((project_key, project));
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

    use super::{shared_declarations, snapshot_holding};

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
