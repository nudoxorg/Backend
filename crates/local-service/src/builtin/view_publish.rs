//! Package-scoped view publication.
//!
//! A workspace edit names one package. Publication keeps every other package's
//! rows, reopens only the edited package's source frontier, and emits a row
//! patch. Full relation hydration remains the recovery path: a missing
//! witness, a transition that is not adjacent to that witness, or an edit
//! that reaches outside its package.

use super::{
    BuiltinIntent, BuiltinModelError, BuiltinSemanticRelation, BuiltinWorkspaceRelation,
    IndexedProject, IndexedSources, WorkspaceSnapshot,
};
use backend_engine::{PackageKey, Row, RowId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

/// Relation roots and semantic activations that produced the resident view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PublishedRoots {
    /// Selected product-source relation root.
    pub(super) source: [u8; 32],
    /// Selected semantic-publication relation root.
    pub(super) semantic: [u8; 32],
    /// Profiles whose images were actually opened for that view.
    pub(super) activated: super::coverage::ActivatedProfiles,
}

/// Which publication strategy produced a view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PublicationPath {
    /// The resident view already came from these relation roots.
    Reused,
    /// One package was reprojected and every sibling row was kept.
    Package {
        /// Source files opened for the edited package.
        files: usize,
    },
    /// The selected relations were paged and every row was rebuilt.
    Hydrated {
        /// Source files opened across the workspace.
        files: usize,
    },
}

/// Deltas, the witness for the next edit, and the strategy that built them.
pub(super) struct PublicationOutcome {
    pub(super) deltas: Vec<backend_engine::CommittedViewDelta>,
    pub(super) roots: PublishedRoots,
    /// Strategy that produced `deltas`. Callers that only apply deltas still
    /// keep the witness; the strategy is what a bench or test asserts.
    #[allow(dead_code)]
    pub(super) path: PublicationPath,
}

/// Base and target roots of the workspace transition that is about to publish.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TransitionRoots {
    /// Source relation root before this transition.
    pub(super) source_base: [u8; 32],
    /// Source relation root selected by this transition.
    pub(super) source_target: [u8; 32],
    /// Semantic relation root before this transition.
    pub(super) semantic_base: [u8; 32],
    /// Semantic relation root selected by this transition.
    pub(super) semantic_target: [u8; 32],
}

/// How the next publication should run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PublicationPlan {
    /// Roots match the witness. Do not read a relation.
    Reuse,
    /// Reproject one package and keep every other row.
    Package {
        /// Package the committed intent edited.
        package: PackageKey,
    },
    /// Page the relations and rebuild the target view.
    Hydrate,
}

/// Why a package splice cannot be admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowSpliceError {
    /// A replacement row reused an identity that belongs to another package.
    Collision,
}

/// Reads the selected source-relation root.
pub(super) fn source_root(snapshot: &WorkspaceSnapshot) -> Result<[u8; 32], BuiltinModelError> {
    relation_root::<BuiltinWorkspaceRelation>(snapshot, "source")
}

/// Reads the selected semantic-publication root.
pub(super) fn semantic_root(snapshot: &WorkspaceSnapshot) -> Result<[u8; 32], BuiltinModelError> {
    relation_root::<BuiltinSemanticRelation>(snapshot, "semantic")
}

fn relation_root<R>(snapshot: &WorkspaceSnapshot, lane: &str) -> Result<[u8; 32], BuiltinModelError>
where
    R: backend_version::CanonicalRelation,
{
    let relation = snapshot
        .relation::<R>()
        .map_err(|error| BuiltinModelError(format!("open {lane} relation root: {error}")))?;
    Ok(*relation.root().as_bytes())
}

/// Chooses reuse, one-package reprojection, or full hydration.
pub(super) fn publication_plan(
    prior: Option<&PublishedRoots>,
    roots: TransitionRoots,
    edit: Option<&BuiltinIntent>,
) -> PublicationPlan {
    let Some(prior) = prior else {
        return PublicationPlan::Hydrate;
    };
    if prior.source == roots.source_target && prior.semantic == roots.semantic_target {
        return PublicationPlan::Reuse;
    }
    let Some(edit) = edit else {
        return PublicationPlan::Hydrate;
    };
    if prior.source != roots.source_base || prior.semantic != roots.semantic_base {
        return PublicationPlan::Hydrate;
    }
    if !edit_is_package_local(edit) {
        return PublicationPlan::Hydrate;
    }
    PublicationPlan::Package {
        package: edit.package,
    }
}

/// A source or semantic change that names another package cannot be spliced.
fn edit_is_package_local(edit: &BuiltinIntent) -> bool {
    let package = edit.package.to_bytes();
    edit.changes().iter().all(|change| match &change.after {
        Some(record) => match record.file_fields() {
            Some(file) => file.project == package,
            None => change.key == package,
        },
        None => true,
    }) && edit
        .semantic_changes()
        .iter()
        .all(|change| change.key.package_key() == edit.package)
}

/// Loads one project's frontier by key lookup.
pub(super) fn read_project_sources(
    snapshot: &WorkspaceSnapshot,
    package: PackageKey,
) -> Result<IndexedSources, BuiltinModelError> {
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open package source: {error}")))?;
    let key = package.to_bytes();
    let Some(record) = relation
        .lookup(&key)
        .map_err(|error| BuiltinModelError(format!("read package frontier: {error}")))?
    else {
        return Ok(IndexedSources {
            projects: std::collections::BTreeMap::new(),
            files: Vec::new(),
        });
    };
    let fields = record.project_fields().ok_or_else(|| {
        BuiltinModelError("package key does not hold a project frontier".to_owned())
    })?;
    let mut files = Vec::with_capacity(fields.files.len());
    for file_key in fields.files.iter().copied() {
        let file = relation
            .lookup(&file_key)
            .map_err(|error| BuiltinModelError(format!("read package source file: {error}")))?
            .ok_or_else(|| {
                BuiltinModelError("project frontier refers to a missing source file".to_owned())
            })?;
        if file.file_fields().is_none() {
            return Err(BuiltinModelError(
                "project frontier refers to a non-file record".to_owned(),
            ));
        }
        files.push((file_key, file));
    }
    let mut projects = std::collections::BTreeMap::new();
    projects.insert(
        key,
        IndexedProject {
            package,
            label: fields.label.to_owned(),
            files: Arc::<[[u8; 32]]>::from(fields.files),
        },
    );
    Ok(IndexedSources { projects, files })
}

/// Drops one package's rows and admits its replacement.
pub(super) fn rows_replacing_package(
    current: &[Row],
    package: PackageKey,
    replacement: Vec<Row>,
) -> Result<Vec<Row>, RowSpliceError> {
    let mut rows = Vec::with_capacity(current.len().saturating_add(replacement.len()));
    for row in current {
        if !row_belongs_to_package(row, package) {
            rows.push(row.clone());
        }
    }
    let kept = BTreeSet::from_iter(rows.iter().map(|row| row.id));
    if replacement
        .iter()
        .any(|row| kept.contains(&row.id) || !row_belongs_to_package(row, package))
    {
        return Err(RowSpliceError::Collision);
    }
    rows.extend(replacement);
    rows.sort_by_key(|row| row.id);
    if rows.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(RowSpliceError::Collision);
    }
    Ok(rows)
}

/// Whether a row is the package frontier or a declaration that package owns.
pub(super) fn row_belongs_to_package(row: &Row, package: PackageKey) -> bool {
    row.package == Some(package) || row.id == RowId::Package(package)
}

/// File keys an edit can reproject without reopening the rest of the package.
///
/// Deletions, semantic image changes, a changed package label, a key outside
/// the frontier, and an edit that touches every file all return [`None`]. The
/// caller then reprojects the package. Project digest changes that keep the
/// label are ignored here: they are not declaration rows.
pub(super) fn changed_structural_files(
    edit: &BuiltinIntent,
    sources: &IndexedSources,
) -> Option<BTreeSet<[u8; 32]>> {
    if !edit.semantic_changes().is_empty() {
        return None;
    }
    let known = BTreeSet::from_iter(sources.files.iter().map(|(key, _)| *key));
    if known.len() < 2 {
        return None;
    }
    let mut changed = BTreeSet::new();
    for change in edit.changes() {
        let Some(after) = &change.after else {
            return None;
        };
        if after.file_fields().is_none() {
            let fields = after.project_fields()?;
            let label = sources
                .projects
                .get(&edit.package.to_bytes())
                .map(|project| project.label.as_str())?;
            if fields.label != label {
                return None;
            }
            continue;
        }
        if !known.contains(&change.key) {
            return None;
        }
        changed.insert(change.key);
    }
    if changed.is_empty() || changed.len() >= known.len() {
        return None;
    }
    Some(changed)
}

/// Coordinate labels of symbols already published for `package`.
///
/// Duplicate labels mean two declarations share a coordinate. The caller
/// replans the package instead of guessing which identity owns the parent.
pub(super) fn resident_symbols(
    rows: &[Row],
    package: PackageKey,
) -> Option<BTreeMap<String, backend_engine::SymbolKey>> {
    let mut labels = BTreeMap::new();
    for row in rows {
        if row.package != Some(package) {
            continue;
        }
        let RowId::Symbol(symbol) = row.id else {
            continue;
        };
        if labels.insert(row.label.clone(), symbol).is_some() {
            return None;
        }
    }
    Some(labels)
}

/// Source paths for the file keys a splice will replace.
pub(super) fn paths_for_files(
    sources: &IndexedSources,
    keys: &BTreeSet<[u8; 32]>,
) -> Result<BTreeSet<String>, BuiltinModelError> {
    let mut paths = BTreeSet::new();
    for (key, record) in &sources.files {
        if !keys.contains(key) {
            continue;
        }
        let path = record
            .file_fields()
            .map(|file| file.path.to_owned())
            .ok_or_else(|| {
                BuiltinModelError("structural file splice received a non-file record".to_owned())
            })?;
        if !paths.insert(path) {
            return Err(BuiltinModelError(
                "structural file splice collapsed two keys onto one path".to_owned(),
            ));
        }
    }
    if paths.len() != keys.len() {
        return Err(BuiltinModelError(
            "structural file splice is missing a requested source path".to_owned(),
        ));
    }
    Ok(paths)
}

/// Drops one package's rows at `paths` and admits their replacement.
pub(super) fn rows_replacing_paths(
    current: &[Row],
    package: PackageKey,
    paths: &BTreeSet<String>,
    replacement: Vec<Row>,
) -> Result<Vec<Row>, RowSpliceError> {
    let mut rows = Vec::with_capacity(current.len().saturating_add(replacement.len()));
    for row in current {
        let replaced = row.package == Some(package)
            && row
                .source
                .captured()
                .is_some_and(|location| paths.contains(location.path()));
        if !replaced {
            rows.push(row.clone());
        }
    }
    let kept = BTreeSet::from_iter(rows.iter().map(|row| row.id));
    if replacement.iter().any(|row| {
        kept.contains(&row.id)
            || row.package != Some(package)
            || row
                .source
                .captured()
                .is_none_or(|location| !paths.contains(location.path()))
    }) {
        return Err(RowSpliceError::Collision);
    }
    rows.extend(replacement);
    rows.sort_by_key(|row| row.id);
    if rows.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(RowSpliceError::Collision);
    }
    Ok(rows)
}

/// Times full structural projection against one package, then row splicing.
#[allow(clippy::expect_used)]
pub(super) fn measure_package_publication() {
    const SAMPLES: usize = 12;
    const WARMUPS: usize = 2;
    const PACKAGES: usize = 32;
    const FILES: usize = 16;
    const DECLARATIONS: usize = 4;
    let (workspace, one) = structural_fixtures(PACKAGES, FILES, DECLARATIONS);
    let full = time_samples(SAMPLES, WARMUPS, || {
        super::view_build::project_structural_plan(&workspace).expect("full structural plan")
    });
    let scoped = time_samples(SAMPLES, WARMUPS, || {
        super::view_build::project_structural_plan(&one).expect("package structural plan")
    });
    let (full_median, full_p95) = percentiles(&full);
    let (one_median, one_p95) = percentiles(&scoped);
    let files = PACKAGES * FILES;
    let declarations = files * DECLARATIONS;
    println!(
        "package_publication structural packages={PACKAGES} files={files} declarations={declarations} full_median_ns={full_median} full_p95_ns={full_p95} one_median_ns={one_median} one_p95_ns={one_p95}"
    );

    const ONE_FILES: usize = 128;
    const ONE_DECLARATIONS: usize = 8;
    let (package_sources, changed_key) = one_package_fixture(ONE_FILES, ONE_DECLARATIONS);
    let only = BTreeSet::from([changed_key]);
    let full_file = time_samples(SAMPLES, WARMUPS, || {
        super::view_build::project_structural_plan(&package_sources).expect("package plan")
    });
    let one_file = time_samples(SAMPLES, WARMUPS, || {
        super::view_build::project_structural_files(&package_sources, &only).expect("file plan")
    });
    let (file_full_median, file_full_p95) = percentiles(&full_file);
    let (file_one_median, file_one_p95) = percentiles(&one_file);
    let file_declarations = ONE_FILES * ONE_DECLARATIONS;
    println!(
        "package_publication file_plan files={ONE_FILES} declarations={file_declarations} full_median_ns={file_full_median} full_p95_ns={file_full_p95} one_median_ns={file_one_median} one_p95_ns={file_one_p95}"
    );

    let (base_rows, replacement, package) = row_fixtures(PACKAGES, FILES * DECLARATIONS);
    let total = base_rows.len();
    let replaced = replacement.len();
    let full_rows = time_samples(SAMPLES, WARMUPS, || rebuild_rows(&base_rows));
    let spliced = time_samples(SAMPLES, WARMUPS, || {
        rows_replacing_package(&base_rows, package, replacement.clone()).expect("splice")
    });
    let (rebuild_median, rebuild_p95) = percentiles(&full_rows);
    let (splice_median, splice_p95) = percentiles(&spliced);
    println!(
        "package_publication rows total={total} replaced={replaced} rebuild_median_ns={rebuild_median} rebuild_p95_ns={rebuild_p95} splice_median_ns={splice_median} splice_p95_ns={splice_p95}"
    );
}

fn time_samples<T>(samples: usize, warmups: usize, mut body: impl FnMut() -> T) -> Vec<u128> {
    for _ in 0..warmups {
        let _ = body();
    }
    let mut measured = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let _ = body();
        measured.push(started.elapsed().as_nanos());
    }
    measured
}

fn percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

fn structural_fixtures(
    packages: usize,
    files_per: usize,
    declarations: usize,
) -> (IndexedSources, IndexedSources) {
    let mut projects = std::collections::BTreeMap::new();
    let mut records = Vec::new();
    let mut first = None;
    for project_index in 0..packages {
        let label = format!("pkg:bench-{project_index}");
        let package = backend_engine::package_key(&label);
        let project = package.to_bytes();
        let mut file_keys = Vec::with_capacity(files_per);
        let mut file_records = Vec::with_capacity(files_per);
        for file_index in 0..files_per {
            let path = format!("src/p{project_index}/f{file_index}.rs");
            let mut decls = Vec::with_capacity(declarations);
            for declaration in 0..declarations {
                let name = format!("item_{project_index}_{file_index}_{declaration}");
                decls.push(
                    backend_compile::SourceDeclaration::at_path(
                        path.clone(),
                        name.clone(),
                        backend_compile::DeclarationKind::Function,
                        1,
                        format!("fn {name}()"),
                        "bench",
                    )
                    .expect("declaration"),
                );
            }
            let key = backend_engine::product_source_file_key(project, &path);
            file_keys.push(key);
            file_records.push((
                key,
                backend_engine::ProductSourceRecord::file(
                    project,
                    path,
                    backend_compile::SourceLanguage::Rust,
                    [u8::try_from(file_index).unwrap_or(0); 32],
                    [3; 32],
                    decls,
                )
                .expect("file record"),
            ));
        }
        file_keys.sort_unstable();
        file_records.sort_unstable_by_key(|(key, _)| *key);
        let frontier =
            backend_engine::ProductSourceRecord::project(label.clone(), [4; 32], file_keys)
                .expect("project frontier");
        let fields = frontier.project_fields().expect("project fields");
        let files = Arc::<[[u8; 32]]>::from(fields.files);
        if first.is_none() {
            first = Some((
                IndexedProject {
                    package,
                    label: label.clone(),
                    files: Arc::clone(&files),
                },
                file_records.clone(),
            ));
        }
        projects.insert(
            project,
            IndexedProject {
                package,
                label,
                files,
            },
        );
        records.extend(file_records);
    }
    let workspace = IndexedSources {
        projects,
        files: records,
    };
    let (project, files) = first.expect("one package");
    let mut projects = std::collections::BTreeMap::new();
    projects.insert(project.package.to_bytes(), project);
    let one = IndexedSources { projects, files };
    (workspace, one)
}

fn one_package_fixture(files_per: usize, declarations: usize) -> (IndexedSources, [u8; 32]) {
    let (sources, _) = structural_fixtures(1, files_per, declarations);
    let key = sources.files.first().expect("package has a file").0;
    (sources, key)
}

fn row_fixtures(packages: usize, symbols_per: usize) -> (Vec<Row>, Vec<Row>, PackageKey) {
    let head = super::genesis().expect("genesis");
    let (basis_view, _) = super::initial_view_for_workspace(&head.snapshot()).expect("basis");
    let basis = basis_view.basis();
    let mut rows = Vec::new();
    let mut edited = None;
    let mut replacement = Vec::new();
    for project_index in 0..packages {
        let label = format!("pkg:rows-{project_index}");
        let package = backend_engine::package_key(&label);
        rows.push(Row::new(RowId::Package(package), basis, label));
        for symbol_index in 0..symbols_per {
            let name = format!("pkg:rows-{project_index}::item_{symbol_index}");
            rows.push(
                Row::in_package(
                    RowId::Symbol(backend_engine::symbol_key(&name)),
                    basis,
                    package,
                    name,
                )
                .with_signature(format!("fn item_{symbol_index}()")),
            );
        }
        if edited.is_none() {
            edited = Some(package);
            replacement.push(Row::new(
                RowId::Package(package),
                basis,
                format!("pkg:rows-{project_index}"),
            ));
            for symbol_index in 0..symbols_per {
                let name = format!("pkg:rows-{project_index}::next_{symbol_index}");
                replacement.push(
                    Row::in_package(
                        RowId::Symbol(backend_engine::symbol_key(&name)),
                        basis,
                        package,
                        name,
                    )
                    .with_signature(format!("fn next_{symbol_index}()")),
                );
            }
        }
    }
    rows.sort_by_key(|row| row.id);
    (rows, replacement, edited.expect("edited package"))
}

fn rebuild_rows(rows: &[Row]) -> Vec<Row> {
    let mut rebuilt = Vec::with_capacity(rows.len());
    for row in rows {
        rebuilt.push(row.clone());
    }
    rebuilt.sort_by_key(|row| row.id);
    rebuilt
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unreachable)]
mod tests {
    use super::*;
    use crate::builtin::{BuiltinIntent, BuiltinSourceChange};
    use backend_engine::{RowChange, ViewRoot};

    fn roots(
        source_base: u8,
        source_target: u8,
        semantic_base: u8,
        semantic_target: u8,
    ) -> TransitionRoots {
        TransitionRoots {
            source_base: [source_base; 32],
            source_target: [source_target; 32],
            semantic_base: [semantic_base; 32],
            semantic_target: [semantic_target; 32],
        }
    }

    fn witness(source: u8, semantic: u8) -> PublishedRoots {
        PublishedRoots {
            source: [source; 32],
            semantic: [semantic; 32],
            activated: BTreeSet::new(),
        }
    }

    fn local_intent(foreign_file: bool) -> BuiltinIntent {
        let label = "pkg:edited";
        let package = backend_engine::package_key(label);
        let project = if foreign_file {
            backend_engine::package_key("pkg:other").to_bytes()
        } else {
            package.to_bytes()
        };
        let file = backend_engine::ProductSourceRecord::file(
            project,
            "src/lib.rs",
            backend_compile::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            Vec::<backend_compile::SourceDeclaration>::new(),
        )
        .expect("file");
        BuiltinIntent::index_with_semantics(
            package,
            label,
            vec![BuiltinSourceChange {
                key: backend_engine::product_source_file_key(project, "src/lib.rs"),
                after: Some(file),
            }],
            Vec::new(),
        )
        .expect("intent")
    }

    #[test]
    fn matching_roots_reuse_and_a_foreign_file_hydrates() {
        let prior = witness(1, 2);
        let same = roots(1, 1, 2, 2);
        assert_eq!(
            publication_plan(Some(&prior), same, None),
            PublicationPlan::Reuse
        );
        assert_eq!(
            publication_plan(None, roots(1, 9, 2, 2), None),
            PublicationPlan::Hydrate
        );
        let moved = roots(1, 9, 2, 2);
        let local = local_intent(false);
        assert_eq!(
            publication_plan(Some(&prior), moved, Some(&local)),
            PublicationPlan::Package {
                package: local.package,
            }
        );
        let foreign = local_intent(true);
        assert_eq!(
            publication_plan(Some(&prior), moved, Some(&foreign)),
            PublicationPlan::Hydrate
        );
        assert_eq!(
            publication_plan(Some(&prior), moved, None),
            PublicationPlan::Hydrate
        );
        let stale = witness(8, 2);
        assert_eq!(
            publication_plan(Some(&stale), moved, Some(&local)),
            PublicationPlan::Hydrate
        );
    }

    #[test]
    fn structural_fixture_plans_the_workspace_and_one_package() {
        let (workspace, one) = structural_fixtures(2, 2, 1);
        assert_eq!(workspace.files.len(), 4);
        assert_eq!(one.files.len(), 2);
        super::super::view_build::project_structural_plan(&workspace).expect("workspace plan");
        super::super::view_build::project_structural_plan(&one).expect("package plan");
    }

    #[test]
    fn genesis_manifest_roots_reuse_without_opening_a_relation() {
        let head = super::super::genesis().expect("genesis");
        let snapshot = head.snapshot();
        let (source_base, source_target) = snapshot
            .transition_relation_roots::<BuiltinWorkspaceRelation>()
            .expect("source transition");
        let (semantic_base, semantic_target) = snapshot
            .transition_relation_roots::<BuiltinSemanticRelation>()
            .expect("semantic transition");
        let prior = PublishedRoots {
            source: source_target,
            semantic: semantic_target,
            activated: BTreeSet::new(),
        };
        let plan = publication_plan(
            Some(&prior),
            TransitionRoots {
                source_base,
                source_target,
                semantic_base,
                semantic_target,
            },
            None,
        );
        assert_eq!(plan, PublicationPlan::Reuse);
    }

    fn admitted(rows: Vec<Row>) -> ViewRoot {
        let (base, _) = super::super::initial_view().expect("initial");
        let capability = super::super::test_builtin_view_capability().expect("capability");
        ViewRoot::new_checked(
            base.recipe(),
            base.basis(),
            base.frontier(),
            rows,
            base.coverage().to_vec(),
            capability,
        )
        .expect("admit view")
    }

    #[test]
    fn splicing_one_package_keeps_the_sibling_bytes_and_rejects_a_stolen_id() {
        let (base, _) = super::super::initial_view().expect("initial");
        let basis = base.basis();
        let alpha = backend_engine::package_key("pkg:alpha");
        let beta = backend_engine::package_key("pkg:beta");
        let sibling = Row::in_package(
            RowId::Symbol(backend_engine::symbol_key("pkg:beta::kept")),
            basis,
            beta,
            "pkg:beta::kept",
        )
        .with_signature("fn kept()")
        .with_document(vec![backend_engine::Fragment::Text(
            "untouched sibling".to_owned(),
        )]);
        let current = admitted(vec![
            Row::new(RowId::Package(alpha), basis, "pkg:alpha"),
            Row::in_package(
                RowId::Symbol(backend_engine::symbol_key("pkg:alpha::old")),
                basis,
                alpha,
                "pkg:alpha::old",
            ),
            Row::new(RowId::Package(beta), basis, "pkg:beta"),
            sibling.clone(),
        ]);
        let replacement = vec![
            Row::new(RowId::Package(alpha), basis, "pkg:alpha"),
            Row::in_package(
                RowId::Symbol(backend_engine::symbol_key("pkg:alpha::new")),
                basis,
                alpha,
                "pkg:alpha::new",
            )
            .with_signature("fn new()"),
        ];
        let merged = rows_replacing_package(current.rows(), alpha, replacement).expect("splice");
        let target = admitted(merged);
        let changes = super::super::changed_rows(&current, &target);
        assert!(changes.iter().any(|change| matches!(
            change,
            RowChange::Remove(RowId::Symbol(id)) if *id == backend_engine::symbol_key("pkg:alpha::old")
        )));
        assert!(changes.iter().any(|change| matches!(
            change,
            RowChange::Upsert(row) if row.label == "pkg:alpha::new"
        )));
        assert!(changes.iter().all(|change| match change {
            RowChange::Remove(id) => !matches!(id, RowId::Symbol(symbol) if *symbol == backend_engine::symbol_key("pkg:beta::kept")),
            RowChange::Upsert(row) => row.package != Some(beta) && row.id != RowId::Package(beta),
        }));
        let kept = target
            .rows()
            .iter()
            .find(|row| row.label == "pkg:beta::kept")
            .expect("sibling");
        assert_eq!(kept, &sibling);
        let stolen = vec![sibling.clone()];
        let error = rows_replacing_package(current.rows(), alpha, stolen).expect_err("collision");
        assert_eq!(error, RowSpliceError::Collision);
    }

    fn declaration(
        path: &str,
        name: &str,
        kind: backend_compile::DeclarationKind,
        line: u32,
        signature: &str,
        container: backend_compile::Container,
    ) -> backend_compile::SourceDeclaration {
        backend_compile::SourceDeclaration::at_path(path, name, kind, line, signature, "doc")
            .expect("declaration")
            .with_container(container)
    }

    fn two_file_sources(
        method: backend_compile::SourceDeclaration,
    ) -> (IndexedSources, PackageKey, [u8; 32], [u8; 32]) {
        let label = "pkg:alpha";
        let package = backend_engine::package_key(label);
        let project = package.to_bytes();
        let widget_path = "src/widget.rs";
        let impl_path = "src/impl.rs";
        let widget = declaration(
            widget_path,
            "Widget",
            backend_compile::DeclarationKind::Struct,
            1,
            "struct Widget",
            backend_compile::Container::Module,
        );
        let widget_key = backend_engine::product_source_file_key(project, widget_path);
        let impl_key = backend_engine::product_source_file_key(project, impl_path);
        let mut file_keys = vec![widget_key, impl_key];
        file_keys.sort_unstable();
        let mut files = vec![
            (
                widget_key,
                backend_engine::ProductSourceRecord::file(
                    project,
                    widget_path,
                    backend_compile::SourceLanguage::Rust,
                    [1; 32],
                    [2; 32],
                    vec![widget],
                )
                .expect("widget file"),
            ),
            (
                impl_key,
                backend_engine::ProductSourceRecord::file(
                    project,
                    impl_path,
                    backend_compile::SourceLanguage::Rust,
                    [3; 32],
                    [4; 32],
                    vec![method],
                )
                .expect("impl file"),
            ),
        ];
        files.sort_unstable_by_key(|(key, _)| *key);
        let mut projects = BTreeMap::new();
        projects.insert(
            project,
            IndexedProject {
                package,
                label: label.to_owned(),
                files: Arc::<[[u8; 32]]>::from(file_keys),
            },
        );
        (
            IndexedSources { projects, files },
            package,
            widget_key,
            impl_key,
        )
    }

    #[test]
    fn one_file_keeps_the_attached_parent_and_the_sibling_bytes() {
        let draw = declaration(
            "src/impl.rs",
            "draw",
            backend_compile::DeclarationKind::Method,
            4,
            "fn draw(&self)",
            backend_compile::Container::attached("Widget"),
        );
        let (before, package, widget_key, impl_key) = two_file_sources(draw);
        let (initial, _) = super::super::initial_view().expect("initial");
        let all = BTreeSet::from([widget_key, impl_key]);
        let published = super::super::view_build::rows_for_structural_files(
            &initial,
            &before,
            &all,
            &BTreeMap::new(),
        )
        .expect("full rows");
        let widget = published
            .iter()
            .find(|row| row.label.ends_with("::Widget"))
            .expect("widget")
            .clone();
        let draw_row = published
            .iter()
            .find(|row| row.label.ends_with("::draw"))
            .expect("draw")
            .clone();
        let RowId::Symbol(widget_symbol) = widget.id else {
            unreachable!("widget row is a symbol");
        };
        assert_eq!(draw_row.parent, Some(widget_symbol));
        let resident = resident_symbols(&published, package).expect("labels");
        let paint = declaration(
            "src/impl.rs",
            "paint",
            backend_compile::DeclarationKind::Method,
            4,
            "fn paint(&self)",
            backend_compile::Container::attached("Widget"),
        );
        let (after, _, _, impl_key) = two_file_sources(paint);
        let only = BTreeSet::from([impl_key]);
        let detached = super::super::view_build::rows_for_structural_files(
            &initial,
            &after,
            &only,
            &BTreeMap::new(),
        )
        .expect("detached");
        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].parent, None);
        let replacement =
            super::super::view_build::rows_for_structural_files(&initial, &after, &only, &resident)
                .expect("attached");
        assert_eq!(replacement[0].parent, Some(widget_symbol));
        assert!(replacement[0].label.ends_with("::paint"));
        let beta = backend_engine::package_key("pkg:beta");
        let sibling = Row::in_package(
            RowId::Symbol(backend_engine::symbol_key("pkg:beta::kept")),
            initial.basis(),
            beta,
            "pkg:beta::kept",
        )
        .with_document(vec![backend_engine::Fragment::Text(
            "untouched sibling".to_owned(),
        )]);
        let mut current = vec![
            Row::new(RowId::Package(package), initial.basis(), "pkg:alpha"),
            sibling.clone(),
        ];
        current.extend(published);
        let paths = paths_for_files(&after, &only).expect("paths");
        let merged =
            rows_replacing_paths(&current, package, &paths, replacement.clone()).expect("splice");
        let kept_widget = merged
            .iter()
            .find(|row| row.label.ends_with("::Widget"))
            .expect("kept widget");
        assert_eq!(kept_widget, &widget);
        let kept_sibling = merged
            .iter()
            .find(|row| row.label == "pkg:beta::kept")
            .expect("sibling");
        assert_eq!(kept_sibling, &sibling);
        assert!(merged.iter().all(|row| row.id != draw_row.id));
        assert!(merged.iter().any(|row| row.label.ends_with("::paint")));
        let mut stolen = replacement[0].clone();
        stolen.id = widget.id;
        let error =
            rows_replacing_paths(&current, package, &paths, vec![stolen]).expect_err("stolen");
        assert_eq!(error, RowSpliceError::Collision);
    }

    #[test]
    fn changed_files_require_a_proper_structural_subset() {
        let draw = declaration(
            "src/impl.rs",
            "draw",
            backend_compile::DeclarationKind::Method,
            4,
            "fn draw(&self)",
            backend_compile::Container::attached("Widget"),
        );
        let (sources, package, widget_key, impl_key) = two_file_sources(draw);
        let label = "pkg:alpha";
        let impl_file = sources
            .files
            .iter()
            .find(|(key, _)| *key == impl_key)
            .expect("impl")
            .1
            .clone();
        let widget_file = sources
            .files
            .iter()
            .find(|(key, _)| *key == widget_key)
            .expect("widget")
            .1
            .clone();
        let one = BuiltinIntent::index_with_semantics(
            package,
            label,
            vec![BuiltinSourceChange {
                key: impl_key,
                after: Some(impl_file.clone()),
            }],
            Vec::new(),
        )
        .expect("one file");
        assert_eq!(
            changed_structural_files(&one, &sources),
            Some(BTreeSet::from([impl_key]))
        );
        let both = BuiltinIntent::index_with_semantics(
            package,
            label,
            vec![
                BuiltinSourceChange {
                    key: impl_key,
                    after: Some(impl_file),
                },
                BuiltinSourceChange {
                    key: widget_key,
                    after: Some(widget_file),
                },
            ],
            Vec::new(),
        )
        .expect("both files");
        assert_eq!(changed_structural_files(&both, &sources), None);
        let deleted = BuiltinIntent::index_with_semantics(
            package,
            label,
            vec![BuiltinSourceChange {
                key: impl_key,
                after: None,
            }],
            Vec::new(),
        )
        .expect("deletion");
        assert_eq!(changed_structural_files(&deleted, &sources), None);
        let missing = BuiltinIntent::index_with_semantics(
            package,
            label,
            vec![BuiltinSourceChange {
                key: [9; 32],
                after: Some(
                    backend_engine::ProductSourceRecord::file(
                        package.to_bytes(),
                        "src/missing.rs",
                        backend_compile::SourceLanguage::Rust,
                        [1; 32],
                        [2; 32],
                        Vec::<backend_compile::SourceDeclaration>::new(),
                    )
                    .expect("missing file"),
                ),
            }],
            Vec::new(),
        )
        .expect("missing");
        assert_eq!(changed_structural_files(&missing, &sources), None);
        let mut frontier = vec![widget_key, impl_key];
        frontier.sort_unstable();
        let renamed =
            backend_engine::ProductSourceRecord::project("pkg:renamed", [4; 32], frontier)
                .expect("renamed project");
        let relabel = BuiltinIntent::index_with_semantics(
            package,
            label,
            vec![
                BuiltinSourceChange {
                    key: package.to_bytes(),
                    after: Some(renamed),
                },
                BuiltinSourceChange {
                    key: impl_key,
                    after: Some(
                        sources
                            .files
                            .iter()
                            .find(|(key, _)| *key == impl_key)
                            .expect("impl")
                            .1
                            .clone(),
                    ),
                },
            ],
            Vec::new(),
        );
        let relabel = relabel.expect("relabel");
        assert_eq!(changed_structural_files(&relabel, &sources), None);
        let duplicate = published_duplicate_label(package, &initial_basis());
        assert_eq!(resident_symbols(&duplicate, package), None);
    }

    fn initial_basis() -> backend_engine::Basis {
        let (initial, _) = super::super::initial_view().expect("initial");
        initial.basis()
    }

    fn published_duplicate_label(package: PackageKey, basis: &backend_engine::Basis) -> Vec<Row> {
        let shared = "pkg:alpha::src/lib.rs:1::Item";
        vec![
            Row::in_package(
                RowId::Symbol(backend_engine::symbol_key("left")),
                *basis,
                package,
                shared,
            ),
            Row::in_package(
                RowId::Symbol(backend_engine::symbol_key("right")),
                *basis,
                package,
                shared,
            ),
        ]
    }
}
