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
use backend_engine::{PackageKey, Row, RowChange, RowId};
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
    /// A semantic row has no source path, so a file splice cannot tell which
    /// image it came from. The caller reprojects the package.
    UnscopedSemantic,
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

/// Projects one package replacement into row changes.
///
/// Sibling rows stay in the resident view. The result is the removes and
/// upserts a patch applies, in identity order. An empty result means the
/// replacement does not change a row.
pub(super) fn row_changes_replacing_package<'row>(
    current: impl IntoIterator<Item = &'row Row>,
    package: PackageKey,
    replacement: &[Row],
) -> Result<Vec<RowChange>, RowSpliceError> {
    let mut resident = BTreeMap::new();
    for row in current {
        if resident.insert(row.id, row).is_some() {
            return Err(RowSpliceError::Collision);
        }
    }
    let mut incoming = BTreeMap::new();
    for row in replacement {
        if !row_belongs_to_package(row, package) {
            return Err(RowSpliceError::Collision);
        }
        if incoming.insert(row.id, row).is_some() {
            return Err(RowSpliceError::Collision);
        }
        if resident
            .get(&row.id)
            .is_some_and(|existing| !row_belongs_to_package(existing, package))
        {
            return Err(RowSpliceError::Collision);
        }
    }
    let mut changes = Vec::new();
    for (id, row) in &resident {
        if !row_belongs_to_package(row, package) {
            continue;
        }
        match incoming.remove(id) {
            Some(next) if next == *row => {}
            Some(next) => changes.push(RowChange::Upsert(Box::new(next.clone()))),
            None => changes.push(RowChange::Remove(*id)),
        }
    }
    for row in incoming.into_values() {
        changes.push(RowChange::Upsert(Box::new(row.clone())));
    }
    changes.sort_by_key(RowChange::id);
    Ok(changes)
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
///
/// A replacement row must belong to `package` and carry one of those paths,
/// unless it is a projected semantic row that no longer names a file.
pub(super) fn rows_replacing_paths(
    current: &[Row],
    package: PackageKey,
    paths: &BTreeSet<String>,
    replacement: Vec<Row>,
) -> Result<Vec<Row>, RowSpliceError> {
    let mut rows = Vec::with_capacity(current.len().saturating_add(replacement.len()));
    for row in current {
        let replaced = row_on_package_path(row, package, paths);
        if !replaced {
            rows.push(row.clone());
        }
    }
    let kept = BTreeSet::from_iter(rows.iter().map(|row| row.id));
    if replacement.iter().any(|row| {
        let on_path = row
            .source
            .file_path()
            .is_some_and(|path| paths.contains(path));
        let unscoped_semantic = row.source.file_path().is_none() && row_is_projected_semantic(row);
        kept.contains(&row.id) || row.package != Some(package) || (!on_path && !unscoped_semantic)
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

/// Replaces structural rows at `paths` and marks that file's semantic rows stale.
///
/// Semantic rows are the ones whose coordinate contains `::semantic::`. A
/// semantic row with no file path belongs to some image in the package, and
/// this splice cannot tell whether that image is the file that changed. A
/// stale row keeps its path and drops the line.
pub(super) fn rows_splicing_changed_files(
    current: &[Row],
    package: PackageKey,
    paths: &BTreeSet<String>,
    mut structural: Vec<Row>,
) -> Result<Vec<Row>, RowSpliceError> {
    if current.iter().any(|row| semantic_row_lacks_file(row, package)) {
        return Err(RowSpliceError::UnscopedSemantic);
    }
    let structural_ids = BTreeSet::from_iter(structural.iter().map(|row| row.id));
    for row in current {
        let on_path = row_on_package_path(row, package, paths);
        if !on_path || !row_is_projected_semantic(row) {
            continue;
        }
        if structural_ids.contains(&row.id) {
            return Err(RowSpliceError::Collision);
        }
        structural.push(mark_semantic_row_stale(row));
    }
    rows_replacing_paths(current, package, paths, structural)
}

/// Projects one file splice into row changes.
///
/// Unchanged resident rows stay in the current view. The result is the removes
/// and upserts a patch applies, in identity order. An empty result means the
/// splice does not change a row.
pub(super) fn row_changes_splicing_changed_files(
    current: &[Row],
    package: PackageKey,
    paths: &BTreeSet<String>,
    structural: &[Row],
) -> Result<Vec<RowChange>, RowSpliceError> {
    let mut resident = BTreeMap::new();
    for row in current {
        if resident.insert(row.id, row).is_some() {
            return Err(RowSpliceError::Collision);
        }
        if semantic_row_lacks_file(row, package) {
            return Err(RowSpliceError::UnscopedSemantic);
        }
    }
    let mut replacement = Vec::with_capacity(structural.len());
    let mut structural_ids = BTreeSet::new();
    for row in structural {
        if !structural_ids.insert(row.id) {
            return Err(RowSpliceError::Collision);
        }
        replacement.push(row.clone());
    }
    for row in current {
        if !row_on_package_path(row, package, paths) || !row_is_projected_semantic(row) {
            continue;
        }
        if structural_ids.contains(&row.id) {
            return Err(RowSpliceError::Collision);
        }
        replacement.push(mark_semantic_row_stale(row));
    }
    let mut replacement_ids = BTreeSet::new();
    for row in &replacement {
        let on_path = row
            .source
            .file_path()
            .is_some_and(|path| paths.contains(path));
        let stale_semantic = row.source.file_path().is_none() && row_is_projected_semantic(row);
        let kept = resident
            .get(&row.id)
            .is_some_and(|existing| !row_on_package_path(existing, package, paths));
        if kept || row.package != Some(package) || (!on_path && !stale_semantic) {
            return Err(RowSpliceError::Collision);
        }
        if !replacement_ids.insert(row.id) {
            return Err(RowSpliceError::Collision);
        }
    }
    let mut changes = Vec::new();
    for row in current {
        if row_on_package_path(row, package, paths) && !replacement_ids.contains(&row.id) {
            changes.push(RowChange::Remove(row.id));
        }
    }
    for row in replacement {
        if resident
            .get(&row.id)
            .is_some_and(|existing| *existing == &row)
        {
            continue;
        }
        changes.push(RowChange::Upsert(Box::new(row)));
    }
    changes.sort_by_key(RowChange::id);
    Ok(changes)
}

/// Whether one splice can be committed as a direct row patch.
///
/// An empty resident view and a change set past the transition limit still
/// publish through a full target root.
pub(super) fn row_patch_fits(resident_rows: u64, changes: usize) -> bool {
    resident_rows > 0 && (1..=backend_engine::MAX_VIEW_PATCH_ROWS).contains(&changes)
}

fn row_on_package_path(row: &Row, package: PackageKey, paths: &BTreeSet<String>) -> bool {
    row.package == Some(package)
        && row
            .source
            .file_path()
            .is_some_and(|path| paths.contains(path))
}

fn semantic_row_lacks_file(row: &Row, package: PackageKey) -> bool {
    row.package == Some(package)
        && row_is_projected_semantic(row)
        && row.source.file_path().is_none()
}

/// Changed files that still need structural rows.
///
/// A file whose language lane is already activated is owned by the semantic
/// image. The splice restamps that image's rows and does not emit a second
/// structural answer for the same file.
pub(super) fn structural_splice_keys(
    activated: &super::coverage::ActivatedProfiles,
    package: PackageKey,
    sources: &IndexedSources,
    changed: &BTreeSet<[u8; 32]>,
) -> Result<BTreeSet<[u8; 32]>, BuiltinModelError> {
    let mut keys = BTreeSet::new();
    for (key, record) in &sources.files {
        if !changed.contains(key) {
            continue;
        }
        let path = record
            .file_fields()
            .ok_or_else(|| {
                BuiltinModelError("structural file splice received a non-file record".to_owned())
            })?
            .path;
        if !semantic_lane_owns_file(activated, package, path)? {
            keys.insert(*key);
        }
    }
    Ok(keys)
}

fn semantic_lane_owns_file(
    activated: &super::coverage::ActivatedProfiles,
    package: PackageKey,
    path: &str,
) -> Result<bool, BuiltinModelError> {
    let Some(profile) = super::ingest::source_profile(std::path::Path::new(path))
        .map_err(BuiltinModelError)?
    else {
        return Ok(false);
    };
    let lane = super::ingest::lane_profile(profile);
    Ok(activated.iter().any(|(active_package, active_profile)| {
        *active_package == package && super::ingest::lane_profile(*active_profile) == lane
    }))
}

fn row_is_projected_semantic(row: &Row) -> bool {
    row.label.contains("::semantic::")
}

fn mark_semantic_row_stale(row: &Row) -> Row {
    let mut stale = row.clone();
    let noted = stale.document.iter().any(|fragment| {
        matches!(fragment, backend_engine::Fragment::Text(text) if text == super::view_build::STALE_NOTE)
    });
    if !noted {
        let mut document = stale.document.to_vec();
        document.push(backend_engine::Fragment::Text(
            super::view_build::STALE_NOTE.to_owned(),
        ));
        stale.document = document.into();
    }
    if let Some(path) = row.source.file_path() {
        stale.source = backend_library::SourceAvailability::StaleFile {
            path: Arc::from(path),
        };
    }
    stale.excerpt = backend_compile::SourceExcerpt::NotCaptured;
    stale
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

    let patch_head = super::genesis().expect("genesis");
    let (patch_initial, _) =
        super::initial_view_for_workspace(&patch_head.snapshot()).expect("basis");
    let patch_capability =
        super::builtin_view_capability_for_workspace(&patch_head.snapshot()).expect("capability");
    let resident = backend_engine::ViewRoot::new_checked(
        patch_initial.recipe(),
        patch_initial.basis(),
        patch_initial.frontier(),
        base_rows.clone(),
        patch_initial.coverage().to_vec(),
        patch_capability.clone(),
    )
    .expect("resident package view");
    const PATCH_SAMPLES: usize = 16;
    const PATCH_WARMUPS: usize = 4;
    let owned_admit = time_samples(PATCH_SAMPLES, PATCH_WARMUPS, || {
        let merged =
            rows_replacing_package(&base_rows, package, replacement.clone()).expect("splice");
        std::hint::black_box(
            backend_engine::ViewRoot::new_checked(
                patch_initial.recipe(),
                patch_initial.basis(),
                patch_initial.frontier(),
                merged,
                patch_initial.coverage().to_vec(),
                patch_capability.clone(),
            )
            .expect("owned admit"),
        );
    });
    let direct_patch = time_samples(PATCH_SAMPLES, PATCH_WARMUPS, || {
        let changes =
            row_changes_replacing_package(base_rows.iter(), package, &replacement).expect("patch");
        std::hint::black_box(
            resident
                .prepare(
                    backend_engine::ViewDelta::Patch {
                        changes: Arc::from(changes),
                    },
                    patch_capability.clone(),
                )
                .expect("direct patch"),
        );
    });
    let (owned_median, owned_p95) = percentiles(&owned_admit);
    let (direct_median, direct_p95) = percentiles(&direct_patch);
    let direct_changes =
        row_changes_replacing_package(base_rows.iter(), package, &replacement).expect("size");
    println!(
        "package_row_patch rows={total} changes={} owned_median_ns={owned_median} owned_p95_ns={owned_p95} patch_median_ns={direct_median} patch_p95_ns={direct_p95}",
        direct_changes.len()
    );

    const ACTIVATED_FILES: usize = 128;
    const ACTIVATED_DECLARATIONS: usize = 8;
    const ACTIVATED_SAMPLES: usize = 32;
    const ACTIVATED_WARMUPS: usize = 4;
    let (activated_sources, changed_key) =
        one_package_fixture(ACTIVATED_FILES, ACTIVATED_DECLARATIONS);
    let package = activated_sources
        .projects
        .values()
        .next()
        .expect("package")
        .package;
    let head = super::genesis().expect("genesis");
    let (initial, _) = super::initial_view_for_workspace(&head.snapshot()).expect("basis");
    let all_keys = BTreeSet::from_iter(activated_sources.files.iter().map(|(key, _)| *key));
    let mut semantic_rows = Vec::with_capacity(activated_sources.files.len());
    for (_, record) in &activated_sources.files {
        let path = record.file_fields().expect("file").path;
        let location = backend_compile::SourceLocation::new(path, 1).expect("location");
        semantic_rows.push(
            Row::in_package(
                RowId::Symbol(backend_engine::symbol_key(&format!("semantic-{path}"))),
                initial.basis(),
                package,
                format!("bench::semantic::{path}"),
            )
            .with_source(location)
            .with_document(vec![backend_engine::Fragment::Text("typed".to_owned())]),
        );
    }
    let all_paths = paths_for_files(&activated_sources, &all_keys).expect("all paths");
    let one_paths =
        paths_for_files(&activated_sources, &BTreeSet::from([changed_key])).expect("one path");
    let all_files = time_samples(ACTIVATED_SAMPLES, ACTIVATED_WARMUPS, || {
        std::hint::black_box(
            rows_splicing_changed_files(&semantic_rows, package, &all_paths, Vec::new())
                .expect("all files"),
        );
    });
    let one_file = time_samples(ACTIVATED_SAMPLES, ACTIVATED_WARMUPS, || {
        std::hint::black_box(
            rows_splicing_changed_files(&semantic_rows, package, &one_paths, Vec::new())
                .expect("one file"),
        );
    });
    let (all_median, all_p95) = percentiles(&all_files);
    let (one_median, one_p95) = percentiles(&one_file);
    println!(
        "activated_file_splice files={ACTIVATED_FILES} semantic={} all_median_ns={all_median} all_p95_ns={all_p95} one_median_ns={one_median} one_p95_ns={one_p95}",
        semantic_rows.len()
    );

    let capability =
        super::builtin_view_capability_for_workspace(&head.snapshot()).expect("capability");
    let resident = backend_engine::ViewRoot::new_checked(
        initial.recipe(),
        initial.basis(),
        initial.frontier(),
        semantic_rows.clone(),
        initial.coverage().to_vec(),
        capability.clone(),
    )
    .expect("resident view");
    let full_admit = time_samples(ACTIVATED_SAMPLES, ACTIVATED_WARMUPS, || {
        let merged = rows_splicing_changed_files(&semantic_rows, package, &one_paths, Vec::new())
            .expect("full splice");
        std::hint::black_box(
            backend_engine::ViewRoot::new_checked(
                initial.recipe(),
                initial.basis(),
                initial.frontier(),
                merged,
                initial.coverage().to_vec(),
                capability.clone(),
            )
            .expect("full admit"),
        );
    });
    let patch_admit = time_samples(ACTIVATED_SAMPLES, ACTIVATED_WARMUPS, || {
        let changes = row_changes_splicing_changed_files(&semantic_rows, package, &one_paths, &[])
            .expect("row patch");
        std::hint::black_box(
            resident
                .prepare(
                    backend_engine::ViewDelta::Patch {
                        changes: Arc::from(changes),
                    },
                    capability.clone(),
                )
                .expect("patch admit"),
        );
    });
    let (splice_median, splice_p95) = percentiles(&full_admit);
    let (patch_median, patch_p95) = percentiles(&patch_admit);
    let patch_changes =
        row_changes_splicing_changed_files(&semantic_rows, package, &one_paths, &[])
            .expect("patch size")
            .len();
    println!(
        "activated_file_patch files={ACTIVATED_FILES} semantic={} changes={patch_changes} splice_median_ns={splice_median} splice_p95_ns={splice_p95} patch_median_ns={patch_median} patch_p95_ns={patch_p95}",
        semantic_rows.len()
    );

    let other_path = all_paths
        .difference(&one_paths)
        .next()
        .expect("another file")
        .clone();
    let other_paths = BTreeSet::from([other_path]);
    let stale_rows = rows_splicing_changed_files(&semantic_rows, package, &one_paths, Vec::new())
        .expect("stale file");
    let stale_view = backend_engine::ViewRoot::new_checked(
        initial.recipe(),
        initial.basis(),
        initial.frontier(),
        stale_rows.clone(),
        initial.coverage().to_vec(),
        capability.clone(),
    )
    .expect("stale view");
    let stale_full = time_samples(ACTIVATED_SAMPLES, ACTIVATED_WARMUPS, || {
        let merged = rows_splicing_changed_files(&stale_rows, package, &other_paths, Vec::new())
            .expect("stale splice");
        std::hint::black_box(
            backend_engine::ViewRoot::new_checked(
                initial.recipe(),
                initial.basis(),
                initial.frontier(),
                merged,
                initial.coverage().to_vec(),
                capability.clone(),
            )
            .expect("stale admit"),
        );
    });
    let stale_patch = time_samples(ACTIVATED_SAMPLES, ACTIVATED_WARMUPS, || {
        let changes =
            row_changes_splicing_changed_files(&stale_rows, package, &other_paths, &[])
                .expect("stale patch");
        std::hint::black_box(
            stale_view
                .prepare(
                    backend_engine::ViewDelta::Patch {
                        changes: Arc::from(changes),
                    },
                    capability.clone(),
                )
                .expect("stale prepare"),
        );
    });
    let (stale_splice_median, stale_splice_p95) = percentiles(&stale_full);
    let (stale_patch_median, stale_patch_p95) = percentiles(&stale_patch);
    let stale_changes =
        row_changes_splicing_changed_files(&stale_rows, package, &other_paths, &[])
            .expect("stale size")
            .len();
    println!(
        "stale_file_splice files={ACTIVATED_FILES} semantic={} changes={stale_changes} splice_median_ns={stale_splice_median} splice_p95_ns={stale_splice_p95} patch_median_ns={stale_patch_median} patch_p95_ns={stale_patch_p95}",
        stale_rows.len()
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
    use backend_engine::{RowChange, ViewDelta, ViewRoot};
    use std::sync::Arc;

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

    #[test]
    fn a_package_patch_matches_the_merged_view_and_leaves_the_sibling() {
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
        let current_rows = vec![
            Row::new(RowId::Package(alpha), basis, "pkg:alpha"),
            Row::in_package(
                RowId::Symbol(backend_engine::symbol_key("pkg:alpha::old")),
                basis,
                alpha,
                "pkg:alpha::old",
            ),
            Row::new(RowId::Package(beta), basis, "pkg:beta"),
            sibling.clone(),
        ];
        let current = admitted(current_rows.clone());
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
        let changes = row_changes_replacing_package(current.row_refs(), alpha, &replacement)
            .expect("package patch");
        let merged =
            rows_replacing_package(current.rows(), alpha, replacement.clone()).expect("splice");
        let target = admitted(merged);
        assert_eq!(super::super::changed_rows(&current, &target), changes);
        assert!(changes.iter().all(|change| match change {
            RowChange::Remove(id) => *id != sibling.id && *id != RowId::Package(beta),
            RowChange::Upsert(row) => row.id != sibling.id && row.id != RowId::Package(beta),
        }));
        let stolen = vec![sibling.clone()];
        let error = row_changes_replacing_package(current.row_refs(), alpha, &stolen)
            .expect_err("stolen");
        assert_eq!(error, RowSpliceError::Collision);
        let alpha_rows = current_rows
            .iter()
            .filter(|row| row_belongs_to_package(row, alpha))
            .cloned()
            .collect::<Vec<_>>();
        let unchanged =
            row_changes_replacing_package(current.row_refs(), alpha, &alpha_rows).expect("same");
        assert!(unchanged.is_empty());
        let capability = super::super::test_builtin_view_capability().expect("capability");
        let prepared = current
            .prepare(
                ViewDelta::Patch {
                    changes: Arc::from(changes),
                },
                capability,
            )
            .expect("prepare");
        let (patched, _) = current.commit(prepared).expect("commit");
        assert_eq!(patched.rows(), target.rows());
        let kept = patched
            .rows()
            .iter()
            .find(|row| row.label == "pkg:beta::kept")
            .expect("sibling");
        assert_eq!(kept, &sibling);

        let (base_rows, replacement, package) = row_fixtures(32, 64);
        let resident = admitted(base_rows.clone());
        let capability = super::super::test_builtin_view_capability().expect("capability");
        let owned = time_samples(8, 2, || {
            let merged =
                rows_replacing_package(&base_rows, package, replacement.clone()).expect("splice");
            std::hint::black_box(
                ViewRoot::new_checked(
                    resident.recipe(),
                    resident.basis(),
                    resident.frontier(),
                    merged,
                    resident.coverage().to_vec(),
                    capability.clone(),
                )
                .expect("owned admit"),
            );
        });
        let direct = time_samples(8, 2, || {
            let changes = row_changes_replacing_package(resident.row_refs(), package, &replacement)
                .expect("patch");
            std::hint::black_box(
                resident
                    .prepare(
                        ViewDelta::Patch {
                            changes: Arc::from(changes),
                        },
                        capability.clone(),
                    )
                    .expect("prepare"),
            );
        });
        let (owned_median, _) = percentiles(&owned);
        let (direct_median, _) = percentiles(&direct);
        let changes =
            row_changes_replacing_package(resident.row_refs(), package, &replacement).expect("size");
        eprintln!(
            "package_row_patch rows={} changes={} owned_median_ns={owned_median} patch_median_ns={direct_median}",
            base_rows.len(),
            changes.len()
        );
        assert!(direct_median < owned_median);
        assert!(changes.len() <= backend_engine::MAX_VIEW_PATCH_ROWS);
        assert!(changes.iter().all(|change| match change {
            RowChange::Upsert(row) => row_belongs_to_package(row, package),
            RowChange::Remove(id) => base_rows
                .iter()
                .find(|row| row.id == *id)
                .is_some_and(|row| row_belongs_to_package(row, package)),
        }));
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
        let spliced = rows_splicing_changed_files(&current, package, &paths, replacement.clone())
            .expect("semantic-free splice");
        assert_eq!(spliced, merged);
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
    fn an_activated_file_keeps_the_other_files_semantic_row_and_marks_its_own_stale() {
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
        let widget = semantic_row(&initial, package, "src/widget.rs", "Widget");
        let draw_semantic = semantic_row(&initial, package, "src/impl.rs", "draw");
        let mut current = published;
        current.push(widget.clone());
        current.push(draw_semantic.clone());
        let resident = resident_symbols(&current, package).expect("labels");
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
        let replacement =
            super::super::view_build::rows_for_structural_files(&initial, &after, &only, &resident)
                .expect("attached");
        let paths = paths_for_files(&after, &only).expect("paths");
        let merged =
            rows_splicing_changed_files(&current, package, &paths, replacement).expect("splice");
        let kept_widget = merged
            .iter()
            .find(|row| row.label == widget.label)
            .expect("kept widget semantic");
        assert_eq!(kept_widget, &widget);
        let stale = merged
            .iter()
            .find(|row| row.label == draw_semantic.label)
            .expect("stale draw");
        assert!(stale.document.iter().any(|fragment| {
            matches!(fragment, backend_engine::Fragment::Text(text) if text == super::super::view_build::STALE_NOTE)
        }));
        assert!(stale.source.captured().is_none());
        assert_eq!(stale.source.file_path(), Some("src/impl.rs"));
        assert_eq!(stale.excerpt, backend_compile::SourceExcerpt::NotCaptured);
        assert_eq!(
            stale
                .document
                .iter()
                .filter(|fragment| {
                    matches!(fragment, backend_engine::Fragment::Text(text) if text == super::super::view_build::STALE_NOTE)
                })
                .count(),
            1
        );
        let again = rows_splicing_changed_files(&merged, package, &paths, Vec::new()).expect("second");
        assert_eq!(
            again.iter().find(|row| row.label == widget.label),
            Some(&widget)
        );
        let again_draw = again
            .iter()
            .find(|row| row.label == draw_semantic.label)
            .expect("stale draw remains");
        assert_eq!(again_draw.source.file_path(), Some("src/impl.rs"));
        assert!(again_draw.source.captured().is_none());
        let unscoped = semantic_row(&initial, package, "src/widget.rs", "bare");
        let mut unscoped = unscoped;
        unscoped.source = backend_library::SourceAvailability::NotCaptured;
        current.push(unscoped);
        let error = rows_splicing_changed_files(&current, package, &paths, Vec::new())
            .expect_err("unscoped input");
        assert_eq!(error, RowSpliceError::UnscopedSemantic);
    }

    fn semantic_row(
        initial: &backend_engine::ViewRoot,
        package: PackageKey,
        path: &str,
        name: &str,
    ) -> Row {
        let location = backend_compile::SourceLocation::new(path, 1).expect("location");
        Row::in_package(
            RowId::Symbol(backend_engine::symbol_key(&format!("semantic-{path}-{name}"))),
            initial.basis(),
            package,
            format!("pkg:alpha::semantic::{name}"),
        )
        .with_source(location)
        .with_document(vec![backend_engine::Fragment::Text("typed".to_owned())])
    }

    #[test]
    fn a_semantic_owned_file_is_not_given_a_second_structural_answer() {
        let draw = declaration(
            "src/impl.rs",
            "draw",
            backend_compile::DeclarationKind::Method,
            4,
            "fn draw(&self)",
            backend_compile::Container::attached("Widget"),
        );
        let (sources, package, _widget_key, impl_key) = two_file_sources(draw);
        let (initial, _) = super::super::initial_view().expect("initial");
        let widget = semantic_row(&initial, package, "src/widget.rs", "Widget");
        let draw_semantic = semantic_row(&initial, package, "src/impl.rs", "draw");
        let current = vec![widget.clone(), draw_semantic];
        let changed = BTreeSet::from([impl_key]);
        let mut activated = BTreeSet::new();
        activated.insert((
            package,
            backend_semantic::vocabulary::LanguageProfile::Rust(
                backend_semantic::vocabulary::RustEdition::Rust2021,
            ),
        ));
        let structural_keys =
            structural_splice_keys(&activated, package, &sources, &changed).expect("owned");
        assert!(structural_keys.is_empty());
        let paths = paths_for_files(&sources, &changed).expect("paths");
        let merged =
            rows_splicing_changed_files(&current, package, &paths, Vec::new()).expect("restamp");
        assert!(merged.iter().all(|row| row_is_projected_semantic(row)));
        assert_eq!(
            merged.iter().find(|row| row.label == widget.label),
            Some(&widget)
        );
        let stale = merged
            .iter()
            .find(|row| row.label.ends_with("::draw"))
            .expect("stale draw");
        assert!(stale.document.iter().any(|fragment| {
            matches!(fragment, backend_engine::Fragment::Text(text) if text == super::super::view_build::STALE_NOTE)
        }));
        assert!(stale.source.captured().is_none());
        let structural =
            structural_splice_keys(&BTreeSet::new(), package, &sources, &changed).expect("structural");
        assert_eq!(structural, changed);
        activated.clear();
        activated.insert((
            package,
            backend_semantic::vocabulary::LanguageProfile::TypeScript(
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
            ),
        ));
        let structural =
            structural_splice_keys(&activated, package, &sources, &changed).expect("other lane");
        assert_eq!(structural, changed);
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

    #[test]
    fn a_file_patch_matches_the_merged_view_and_leaves_the_sibling() {
        let (initial, _) = super::super::initial_view().expect("initial");
        let package = backend_engine::package_key("pkg:alpha");
        let sibling_package = backend_engine::package_key("pkg:beta");
        let widget = semantic_row(&initial, package, "src/widget.rs", "Widget");
        let draw = semantic_row(&initial, package, "src/impl.rs", "draw");
        let sibling = Row::in_package(
            RowId::Symbol(backend_engine::symbol_key("pkg:beta::kept")),
            initial.basis(),
            sibling_package,
            "pkg:beta::kept",
        )
        .with_document(vec![backend_engine::Fragment::Text(
            "untouched sibling".to_owned(),
        )]);
        let current = vec![widget.clone(), draw.clone(), sibling.clone()];
        let paths = BTreeSet::from(["src/impl.rs".to_owned()]);
        let changes =
            row_changes_splicing_changed_files(&current, package, &paths, &[]).expect("patch");
        assert_eq!(changes.len(), 1);
        assert!(changes.iter().all(|change| match change {
            RowChange::Upsert(row) =>
                row.label.ends_with("::draw") && row_is_projected_semantic(row),
            RowChange::Remove(_) => false,
        }));
        assert!(row_patch_fits(current.len() as u64, changes.len()));
        let merged =
            rows_splicing_changed_files(&current, package, &paths, Vec::new()).expect("merge");
        assert!(
            merged
                .iter()
                .all(|row| row.package != Some(package) || row_is_projected_semantic(row))
        );
        assert_eq!(
            super::super::admitted_bytes_after_row_changes(&current, &changes)
                .expect("patch bytes"),
            super::super::admitted_view_bytes(&merged).expect("merged bytes")
        );
        let mut stolen = draw;
        stolen.id = sibling.id;
        let error = row_changes_splicing_changed_files(&current, package, &paths, &[stolen])
            .expect_err("stolen");
        assert_eq!(error, RowSpliceError::Collision);
        let mut unscoped = semantic_row(&initial, package, "src/widget.rs", "bare");
        unscoped.source = backend_library::SourceAvailability::NotCaptured;
        let mut with_unscoped = current.clone();
        with_unscoped.push(unscoped);
        let error = row_changes_splicing_changed_files(&with_unscoped, package, &paths, &[])
            .expect_err("unscoped");
        assert_eq!(error, RowSpliceError::UnscopedSemantic);
        let current_view = admitted(current);
        let target = admitted(merged);
        assert_eq!(super::super::changed_rows(&current_view, &target), changes);
        let capability = super::super::test_builtin_view_capability().expect("capability");
        let prepared = current_view
            .prepare(
                ViewDelta::Patch {
                    changes: Arc::from(changes),
                },
                capability,
            )
            .expect("prepare");
        let (patched, _) = current_view.commit(prepared).expect("commit");
        assert_eq!(patched.rows(), target.rows());
        let kept = patched
            .rows()
            .iter()
            .find(|row| row.label == "pkg:beta::kept")
            .expect("sibling");
        assert_eq!(kept, &sibling);
        let kept_widget = patched
            .rows()
            .iter()
            .find(|row| row.label == widget.label)
            .expect("widget");
        assert_eq!(kept_widget, &widget);
        let stale_draw = patched
            .rows()
            .iter()
            .find(|row| row.label.ends_with("::draw"))
            .expect("stale draw");
        assert_eq!(stale_draw.source.file_path(), Some("src/impl.rs"));
        assert!(stale_draw.source.captured().is_none());
        let again = row_changes_splicing_changed_files(patched.rows(), package, &paths, &[])
            .expect("second splice");
        assert!(again.is_empty());
    }

    #[test]
    fn a_second_file_patches_without_touching_the_stale_file() {
        let (initial, _) = super::super::initial_view().expect("initial");
        let package = backend_engine::package_key("pkg:alpha");
        let widget = semantic_row(&initial, package, "src/widget.rs", "Widget");
        let draw = semantic_row(&initial, package, "src/impl.rs", "draw");
        let current = vec![widget.clone(), draw];
        let impl_paths = BTreeSet::from(["src/impl.rs".to_owned()]);
        let first = rows_splicing_changed_files(&current, package, &impl_paths, Vec::new())
            .expect("first");
        let stale_draw = first
            .iter()
            .find(|row| row.label.ends_with("::draw"))
            .expect("stale draw")
            .clone();
        let widget_paths = BTreeSet::from(["src/widget.rs".to_owned()]);
        let changes =
            row_changes_splicing_changed_files(&first, package, &widget_paths, &[]).expect("second");
        assert_eq!(changes.len(), 1);
        assert!(changes.iter().all(|change| {
            matches!(change, RowChange::Upsert(row) if row.label == widget.label)
        }));
        let merged =
            rows_splicing_changed_files(&first, package, &widget_paths, Vec::new()).expect("merge");
        let kept_draw = merged
            .iter()
            .find(|row| row.label == stale_draw.label)
            .expect("kept draw");
        assert_eq!(kept_draw, &stale_draw);
        assert_eq!(
            super::super::admitted_bytes_after_row_changes(&first, &changes).expect("patch bytes"),
            super::super::admitted_view_bytes(&merged).expect("merged bytes")
        );
        let current_view = admitted(first);
        let target = admitted(merged);
        assert_eq!(super::super::changed_rows(&current_view, &target), changes);
        let capability = super::super::test_builtin_view_capability().expect("capability");
        let prepared = current_view
            .prepare(
                ViewDelta::Patch {
                    changes: Arc::from(changes),
                },
                capability,
            )
            .expect("prepare");
        let (patched, _) = current_view.commit(prepared).expect("commit");
        assert_eq!(patched.rows(), target.rows());
    }

    #[test]
    fn a_structural_file_patch_removes_only_the_changed_file() {
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
        let widget = semantic_row(&initial, package, "src/widget.rs", "Widget");
        let draw_semantic = semantic_row(&initial, package, "src/impl.rs", "draw");
        let mut current = published;
        current.push(widget.clone());
        current.push(draw_semantic);
        let resident = resident_symbols(&current, package).expect("labels");
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
        let replacement =
            super::super::view_build::rows_for_structural_files(&initial, &after, &only, &resident)
                .expect("attached");
        let paths = paths_for_files(&after, &only).expect("paths");
        let changes = row_changes_splicing_changed_files(&current, package, &paths, &replacement)
            .expect("patch");
        let merged =
            rows_splicing_changed_files(&current, package, &paths, replacement).expect("merge");
        assert!(changes.iter().all(|change| match change {
            RowChange::Upsert(row) => row.label != widget.label,
            RowChange::Remove(id) => merged.iter().all(|row| row.id != *id),
        }));
        assert!(changes.iter().any(|change| {
            matches!(change, RowChange::Upsert(row) if row.label.ends_with("::paint"))
        }));
        assert_eq!(
            super::super::admitted_bytes_after_row_changes(&current, &changes)
                .expect("patch bytes"),
            super::super::admitted_view_bytes(&merged).expect("merged bytes")
        );
        let current_view = admitted(current);
        let target = admitted(merged);
        assert_eq!(super::super::changed_rows(&current_view, &target), changes);
        let capability = super::super::test_builtin_view_capability().expect("capability");
        let prepared = current_view
            .prepare(
                ViewDelta::Patch {
                    changes: Arc::from(changes),
                },
                capability,
            )
            .expect("prepare");
        let (patched, _) = current_view.commit(prepared).expect("commit");
        assert_eq!(patched.rows(), target.rows());
    }

    #[test]
    fn a_row_patch_over_the_budget_stays_on_the_full_merge() {
        let (initial, _) = super::super::initial_view().expect("initial");
        let package = backend_engine::package_key("pkg:alpha");
        let count = backend_engine::MAX_VIEW_PATCH_ROWS + 1;
        let current = (0..count)
            .map(|index| semantic_row(&initial, package, "src/impl.rs", &format!("n{index}")))
            .collect::<Vec<_>>();
        let paths = BTreeSet::from(["src/impl.rs".to_owned()]);
        let changes =
            row_changes_splicing_changed_files(&current, package, &paths, &[]).expect("patch");
        assert_eq!(changes.len(), count);
        assert!(!row_patch_fits(current.len() as u64, changes.len()));
        assert!(!row_patch_fits(0, 1));
        assert!(!row_patch_fits(1, 0));
        assert!(row_patch_fits(1, 1));
        let merged =
            rows_splicing_changed_files(&current, package, &paths, Vec::new()).expect("merge");
        assert_eq!(merged.len(), count);
        assert_eq!(
            super::super::admitted_bytes_after_row_changes(&current, &changes)
                .expect("patch bytes"),
            super::super::admitted_view_bytes(&merged).expect("merged bytes")
        );
        let current_view = admitted(current);
        let target = admitted(merged);
        assert_eq!(super::super::changed_rows(&current_view, &target), changes);
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
