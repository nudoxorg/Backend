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
use std::collections::BTreeSet;
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

fn row_fixtures(packages: usize, symbols_per: usize) -> (Vec<Row>, Vec<Row>, PackageKey) {
    let (basis_view, _) = super::initial_view().expect("basis");
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
#[allow(clippy::expect_used)]
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
}
