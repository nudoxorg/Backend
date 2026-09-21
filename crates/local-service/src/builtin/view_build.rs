//! Typed source-to-document row projection.
//!
//! This module is the enforcement point of the two-lane answer contract
//! (`super::lanes`): published semantic images are projected first, files
//! their profile covers keep no structural rows, files without complete
//! coverage fall back to the tree-sitter baseline, and a stale publication
//! answers semantic and typed stale rather than being silently replaced.

use super::{
    BuiltinModelError, BuiltinSemanticRelation, FileLane, IndexedSources, ProjectionLedger,
    SemanticFreshness, StructuralCause, MAX_REBUILD_BYTES, MAX_REBUILD_PACKAGES,
    WorkspaceSnapshot, activate_semantic_publication,
};
use backend_engine::builtin::{
    ProductSemanticPublicationRecord, SemanticUnavailableReason,
};
use backend_engine::{DeclarationKind, Fragment, Row, RowId, ViewRoot, product_source_file_key};
use backend_engine::application::{DocumentationFragment, DocumentationSession, LocalCompilerClient};
use backend_semantic::ir::{
    DeclarationIdentity, ExternalTargetIdentity, ItemKind, LinkTarget, SemanticCoreReader as _,
    SemanticImageView, SemanticReader as _,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
};

const MAX_SEMANTIC_TYPE_DEPTH: usize = 256;
const MAX_SEMANTIC_SIGNATURE_BYTES: usize = 16 * 1024;
const MAX_SEMANTIC_DOCUMENT_BYTES: usize = 256 * 1024;
const MAX_SEMANTIC_QUERY_ROWS: usize = 65_536;

/// Document note appended to every row projected from a stale semantic image.
const STALE_NOTE: &str =
    "stale semantic image: compiled from an earlier source snapshot; re-index to refresh";

type DeclarationOccurrenceKey = (String, String, String);

fn duplicate_declaration_coordinates(
    containment: &FileContainment<'_>,
    declarations: &[backend_compile::SourceDeclaration],
) -> BTreeSet<String> {
    let mut counts = BTreeMap::<String, u32>::new();
    for declaration in declarations {
        let coordinate = containment.coordinate(declaration);
        let count = counts.entry(coordinate).or_default();
        *count = count.saturating_add(1);
    }
    counts
        .into_iter()
        .filter_map(|(coordinate, count)| (count > 1).then_some(coordinate))
        .collect()
}

fn declaration_symbol(
    coordinate: &str,
    kind: DeclarationKind,
    signature: &str,
    duplicate_coordinate: bool,
    occurrences: &mut BTreeMap<DeclarationOccurrenceKey, u32>,
) -> (RowId, Option<String>) {
    let canonical = RowId::Symbol(backend_engine::symbol_key(coordinate));
    if !duplicate_coordinate {
        return (canonical, None);
    }
    let occurrence = occurrences
        .entry((
            coordinate.to_owned(),
            kind.name().to_owned(),
            signature.to_owned(),
        ))
        .or_default();
    let preimage = {
        let preimage = format!("{coordinate}\0{}\0{signature}\0{occurrence}", kind.name());
        *occurrence = occurrence.saturating_add(1);
        preimage
    };
    (
        RowId::Symbol(backend_engine::symbol_key(&preimage)),
        Some(preimage),
    )
}

/// Returns the canonical coordinate of one non-file declaration row.
///
/// Both the row projection and the query-fact projection address a
/// declaration the same way, and a parent link is exactly this coordinate of
/// another declaration, so the format has one definition.
fn declaration_coordinate(label: &str, path: &str, line: u32, name: &str) -> String {
    format!("{label}::{path}:{line}::{name}")
}

/// Returns whether a declaration is the synthetic row standing for its file.
///
/// The file module keeps the bare `project::path` coordinate it has always
/// had - symbol keys and journal certificates depend on it - while a real
/// `mod`, `namespace`, or `package` declaration is addressed by line and name
/// like any other declaration, so that its contents can be parented to it.
fn is_file_module(declaration: &backend_compile::SourceDeclaration, path: &str) -> bool {
    declaration.kind() == DeclarationKind::Module
        && declaration.line() == 1
        && std::path::Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem == declaration.name())
}

/// Returns whether a declaration can own members that name it.
///
/// An `impl` block, a method receiver, and an out-of-line `Type::member`
/// definition all attach to a *declared type*, so only a type-like
/// declaration answers such an attachment. Resolving to anything else - a
/// function that happens to share the name, say - would silently nest a
/// method under a call.
const fn declares_a_type(kind: DeclarationKind) -> bool {
    matches!(
        kind,
        DeclarationKind::Struct
            | DeclarationKind::Enum
            | DeclarationKind::Union
            | DeclarationKind::Class
            | DeclarationKind::Trait
            | DeclarationKind::Interface
            | DeclarationKind::Type
    )
}

/// Every type-like declaration in a project, addressed by name.
///
/// A `Container::Attached` names a type without saying where it is, because
/// an `impl` block routinely sits in a different file from its `struct`. The
/// index is built once per rebuild and resolves the project-wide fallback the
/// per-file search cannot answer. The lowest `(path, line)` wins so that two
/// same-named types in one project resolve deterministically.
/// One project's key together with a declared type's name.
type TypeIdentity = ([u8; 32], String);

/// The file and line one declaration was declared at.
type DeclarationSite = (String, u32);

#[derive(Default)]
struct ProjectTypeIndex {
    by_name: BTreeMap<TypeIdentity, DeclarationSite>,
}

impl ProjectTypeIndex {
    fn of(sources: &IndexedSources) -> Self {
        let mut index = Self::default();
        for (_, record) in &sources.files {
            let Some(file) = record.file_fields() else {
                continue;
            };
            for declaration in file.declarations.iter() {
                if !declares_a_type(declaration.kind()) {
                    continue;
                }
                let entry = (file.path.to_owned(), declaration.line());
                index
                    .by_name
                    .entry((file.project, declaration.name().to_owned()))
                    .and_modify(|held| {
                        if entry < *held {
                            *held = entry.clone();
                        }
                    })
                    .or_insert(entry);
            }
        }
        index
    }

    fn resolve(&self, project: [u8; 32], type_name: &str) -> Option<(&str, u32)> {
        self.by_name
            .get(&(project, type_name.to_owned()))
            .map(|(path, line)| (path.as_str(), *line))
    }
}

/// Resolves the coordinates of one file's declarations and of their parents.
///
/// The frontend states containment structurally - a name and a line, or a
/// type name to look up - because only a row projection knows what a row's
/// coordinate is. Turning that into a parent coordinate is therefore done
/// here, once, for both the row path and the query-fact path.
struct FileContainment<'a> {
    label: &'a str,
    path: &'a str,
    project: [u8; 32],
    module_coordinate: String,
    local_types: BTreeMap<&'a str, u32>,
}

impl<'a> FileContainment<'a> {
    fn new(
        label: &'a str,
        path: &'a str,
        project: [u8; 32],
        declarations: &'a [backend_compile::SourceDeclaration],
    ) -> Self {
        let mut local_types = BTreeMap::new();
        for declaration in declarations {
            if !declares_a_type(declaration.kind()) {
                continue;
            }
            local_types
                .entry(declaration.name())
                .and_modify(|line: &mut u32| *line = (*line).min(declaration.line()))
                .or_insert(declaration.line());
        }
        Self {
            label,
            path,
            project,
            module_coordinate: format!("{label}::{path}"),
            local_types,
        }
    }

    /// Returns the coordinate a declaration's own row is addressed by.
    fn coordinate(&self, declaration: &backend_compile::SourceDeclaration) -> String {
        if is_file_module(declaration, self.path) {
            return self.module_coordinate.clone();
        }
        declaration_coordinate(
            self.label,
            self.path,
            declaration.line(),
            declaration.name(),
        )
    }

    /// Returns the coordinate of the row a declaration hangs under.
    ///
    /// Every unresolved containment falls back to the file module rather than
    /// to no parent at all: a declaration that vanished from every outline
    /// would be worse than one shown at file level.
    fn parent_coordinate(
        &self,
        declaration: &backend_compile::SourceDeclaration,
        types: &ProjectTypeIndex,
    ) -> Option<String> {
        if is_file_module(declaration, self.path) {
            return None;
        }
        Some(match declaration.container() {
            backend_compile::Container::Module => self.module_coordinate.clone(),
            backend_compile::Container::Enclosing { name, line } => {
                declaration_coordinate(self.label, self.path, line.get(), name)
            }
            backend_compile::Container::Attached { type_name } => self
                .attached_coordinate(type_name, types)
                .unwrap_or_else(|| self.module_coordinate.clone()),
        })
    }

    fn attached_coordinate(&self, type_name: &str, types: &ProjectTypeIndex) -> Option<String> {
        if let Some(line) = self.local_types.get(type_name) {
            return Some(declaration_coordinate(
                self.label,
                self.path,
                *line,
                type_name,
            ));
        }
        let (path, line) = types.resolve(self.project, type_name)?;
        Some(declaration_coordinate(self.label, path, line, type_name))
    }
}

fn projected_source_capacity(
    sources: &IndexedSources,
    complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
) -> Result<usize, BuiltinModelError> {
    let count = sources
        .files
        .iter()
        .try_fold(sources.projects.len(), |count, (_, record)| {
            let rows = match record.file_fields() {
                Some(fields)
                    if semantic_profile_is_complete(
                        complete,
                        sources
                            .projects
                            .get(&fields.project)
                            .map(|project| project.package),
                        fields.path,
                    )? =>
                {
                    0
                }
                Some(fields) => fields.declarations.len(),
                None => 0,
            };
            count
                .checked_add(rows)
                .ok_or_else(|| BuiltinModelError("workspace view row count overflow".to_owned()))
        })?;
    if count > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace source declarations exceed the rebuild row bound".to_owned(),
        ));
    }
    Ok(count)
}

pub(super) struct ProjectedRows {
    pub(super) rows: Vec<Row>,
    pub(super) activated: BTreeSet<(
        backend_engine::PackageKey,
        backend_semantic::vocabulary::LanguageProfile,
    )>,
    /// Typed record of which lane answered for every projected source file.
    /// Diagnostics and tests read it instead of inferring lanes from
    /// coordinates; the published view deliberately does not carry it.
    #[allow(dead_code)]
    pub(super) ledger: ProjectionLedger,
}

/// Current compiled-source paths per (project relation key, semantic profile).
type ProfileSourcePaths =
    BTreeMap<([u8; 32], backend_semantic::vocabulary::LanguageProfile), BTreeSet<String>>;

/// The persisted semantic source content identity of every current file that
/// states one, per (project relation key, semantic profile).
///
/// A file scanned before identities were persisted - or a file whose bytes
/// could not be read - is absent from the inner map, so identity comparison
/// is trusted only when it covers the profile's whole path set.
type ProfileSourceIdentities = BTreeMap<
    ([u8; 32], backend_semantic::vocabulary::LanguageProfile),
    BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>,
>;

/// Current paths per (project, profile) whose published image is stale.
type ProfileStalePaths =
    BTreeMap<([u8; 32], backend_semantic::vocabulary::LanguageProfile), BTreeSet<String>>;

/// Maps every (project, semantic profile) pair to the exact relative paths a
/// compile of that profile would consume right now. The staleness comparison
/// uses it against the paths retained by a published image.
fn profile_source_paths(
    sources: &IndexedSources,
) -> Result<ProfileSourcePaths, BuiltinModelError> {
    let mut paths = BTreeMap::new();
    for record in &sources.files {
        let Some(file) = record.1.file_fields() else {
            continue;
        };
        let Some(profile) =
            super::ingest::source_profile(std::path::Path::new(file.path)).map_err(BuiltinModelError)?
        else {
            continue;
        };
        paths
            .entry((file.project, profile))
            .or_insert_with(BTreeSet::new)
            .insert(file.path.to_owned());
    }
    Ok(paths)
}

/// Maps every (project, semantic profile) pair to each current file's
/// persisted `SourceFactDomain` content identity.
fn profile_source_identities(
    sources: &IndexedSources,
) -> Result<ProfileSourceIdentities, BuiltinModelError> {
    let mut identities = BTreeMap::new();
    for record in &sources.files {
        let Some(file) = record.1.file_fields() else {
            continue;
        };
        let Some(identity) = file.source_identity else {
            continue;
        };
        let Some(profile) =
            super::ingest::source_profile(std::path::Path::new(file.path)).map_err(BuiltinModelError)?
        else {
            continue;
        };
        identities
            .entry((file.project, profile))
            .or_insert_with(BTreeMap::new)
            .insert(file.path.to_owned(), identity);
    }
    Ok(identities)
}

pub(super) fn rows_for_indexed_sources(
    initial: &ViewRoot,
    sources: &IndexedSources,
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
) -> Result<ProjectedRows, BuiltinModelError> {
    if sources.projects.len() > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace package rows exceed the rebuild row bound".to_owned(),
        ));
    }
    let current_paths = profile_source_paths(sources)?;
    let current_identities = profile_source_identities(sources)?;
    let semantics = semantic_rows(
        snapshot,
        compiler,
        &sources.projects,
        &current_paths,
        &current_identities,
        initial,
        MAX_REBUILD_PACKAGES - sources.projects.len(),
    )?;
    let source_capacity = projected_source_capacity(sources, &semantics.complete)?;
    let total_capacity = source_capacity
        .checked_add(semantics.rows.len())
        .filter(|count| *count <= MAX_REBUILD_PACKAGES)
        .ok_or_else(|| {
            BuiltinModelError(
                "workspace semantic declarations exceed the rebuild row bound".to_owned(),
            )
        })?;
    let types = ProjectTypeIndex::of(sources);
    let mut projection = SourceRowProjection::new(
        initial,
        &sources.projects,
        total_capacity,
        &semantics.targets,
    )?;
    for (file_key, record) in &sources.files {
        projection.append_file(
            *file_key,
            record,
            &semantics.complete,
            &semantics.stale_paths,
            &types,
        )?;
    }
    let mut ledger = semantics.ledger;
    ledger.extend(projection.take_ledger());
    let rows = projection.finish(semantics.rows)?;
    Ok(ProjectedRows {
        rows,
        activated: semantics.activated,
        ledger,
    })
}

struct SourceRowProjection<'a> {
    initial: &'a ViewRoot,
    projects: &'a BTreeMap<[u8; 32], super::IndexedProject>,
    rows: Vec<Row>,
    selected_files: BTreeSet<([u8; 32], [u8; 32])>,
    ledger: ProjectionLedger,
    targets: &'a SemanticTargets,
}

impl<'a> SourceRowProjection<'a> {
    fn new(
        initial: &'a ViewRoot,
        projects: &'a BTreeMap<[u8; 32], super::IndexedProject>,
        capacity: usize,
        targets: &'a SemanticTargets,
    ) -> Result<Self, BuiltinModelError> {
        let mut rows = Vec::with_capacity(capacity);
        let mut selected_files = BTreeSet::new();
        for (project_key, project) in projects {
            rows.push(Row::new(
                RowId::Package(project.package),
                initial.basis(),
                &project.label,
            ));
            for file_key in project.files.iter().copied() {
                if !selected_files.insert((*project_key, file_key)) {
                    return Err(BuiltinModelError(
                        "project file frontier contains an overlapping file key".to_owned(),
                    ));
                }
            }
        }
        Ok(Self {
            initial,
            projects,
            rows,
            selected_files,
            ledger: ProjectionLedger::default(),
            targets,
        })
    }

    /// Takes the structural lane decisions recorded during projection.
    fn take_ledger(&mut self) -> ProjectionLedger {
        std::mem::take(&mut self.ledger)
    }

    fn append_file(
        &mut self,
        file_key: [u8; 32],
        record: &super::ProductSourceRecord,
        complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
        stale_paths: &ProfileStalePaths,
        types: &ProjectTypeIndex,
    ) -> Result<(), BuiltinModelError> {
        let file = record
            .file_fields()
            .ok_or_else(|| BuiltinModelError("expected a source file record".to_owned()))?;
        let project_key = file.project;
        let path = file.path;
        let language = file.language;
        let declarations = file.declarations;
        let project = self.projects.get(&project_key).ok_or_else(|| {
            BuiltinModelError("source file refers to a missing project record".to_owned())
        })?;
        if product_source_file_key(project_key, path) != file_key
            || project.files.binary_search(&file_key).is_err()
            || !self.selected_files.remove(&(project_key, file_key))
        {
            return Err(BuiltinModelError(
                "source file is outside its project's canonical frontier".to_owned(),
            ));
        }
        let profile =
            super::ingest::source_profile(std::path::Path::new(path)).map_err(BuiltinModelError)?;
        if semantic_profile_is_complete(complete, Some(project.package), path)? {
            // The semantic lane owns this file: its published image answered,
            // so the structural baseline is suppressed entirely. Even a stale
            // image answers (typed stale) rather than falling back silently.
            // Staleness is per file: this file is stale exactly when the
            // image compiled from this path no longer matches its persisted
            // content identity.
            let freshness = if profile.is_some_and(|profile| {
                stale_paths
                    .get(&(project.package.to_bytes(), profile))
                    .is_some_and(|paths| paths.contains(path))
            }) {
                SemanticFreshness::Stale
            } else {
                SemanticFreshness::Fresh
            };
            self.ledger
                .record(path, FileLane::Semantic { freshness });
            return Ok(());
        }
        self.ledger.record(
            path,
            FileLane::Structural {
                cause: match profile {
                    None => StructuralCause::NoSemanticProfile,
                    Some(profile) => targets_unavailable_cause(
                        self.targets,
                        project.package,
                        profile,
                    ),
                },
            },
        );
        let containment = FileContainment::new(&project.label, path, project_key, declarations);
        let package = project.package;
        let duplicate_coordinates = duplicate_declaration_coordinates(&containment, declarations);
        let mut occurrences = BTreeMap::new();
        for declaration in declarations.iter() {
            let row = self.declaration_row(
                declaration,
                &containment,
                types,
                package,
                language,
                &duplicate_coordinates,
                &mut occurrences,
            )?;
            self.rows.push(row);
        }
        Ok(())
    }

    /// Builds one declaration's row, including the parent it hangs under.
    fn declaration_row(
        &mut self,
        declaration: &backend_compile::SourceDeclaration,
        containment: &FileContainment<'_>,
        types: &ProjectTypeIndex,
        package: backend_engine::PackageKey,
        language: backend_engine::SourceLanguage,
        duplicate_coordinates: &BTreeSet<String>,
        occurrences: &mut BTreeMap<DeclarationOccurrenceKey, u32>,
    ) -> Result<Row, BuiltinModelError> {
        let path = containment.path;
        let coordinate = containment.coordinate(declaration);
        let (symbol, identity_preimage) = declaration_symbol(
            &coordinate,
            declaration.kind(),
            declaration.signature(),
            duplicate_coordinates.contains(&coordinate),
            occurrences,
        );
        let prose = if is_file_module(declaration, path) {
            format!("{} source · {path}", language.name())
        } else if declaration.documentation().is_empty() {
            format!(
                "{} in {path}:{}",
                declaration.kind_name(),
                declaration.line()
            )
        } else {
            declaration.documentation().to_owned()
        };
        let row = Row::in_package(symbol, self.initial.basis(), package, coordinate)
            .with_document(vec![Fragment::Text(prose)])
            .with_signature(declaration.signature())
            .with_kind(declaration.kind())
            .with_source(declaration.location().clone())
            .with_excerpt(declaration.source_excerpt().clone());
        let row = match identity_preimage {
            Some(preimage) => row.with_identity_preimage(
                backend_engine::RowIdentityPreimage::try_from(preimage).map_err(|error| {
                    BuiltinModelError(format!("structural row identity preimage: {error}"))
                })?,
            ),
            None => row,
        };
        Ok(match containment.parent_coordinate(declaration, types) {
            Some(parent) => row.with_parent(backend_engine::symbol_key(&parent)),
            None => row,
        })
    }

    fn finish(mut self, semantic_rows: Vec<Row>) -> Result<Vec<Row>, BuiltinModelError> {
        if !self.selected_files.is_empty() {
            return Err(BuiltinModelError(
                "project frontier refers to a missing source file".to_owned(),
            ));
        }
        self.rows.extend(semantic_rows);
        if self.rows.len() > MAX_REBUILD_PACKAGES {
            return Err(BuiltinModelError(
                "workspace semantic declarations exceed the rebuild row bound".to_owned(),
            ));
        }
        Ok(self.rows)
    }
}

/// Returns the structural fallback cause for a file whose profile has no
/// complete publication: an explicit terminal reason when one was recorded,
/// otherwise the plain absence of a completed compile.
fn targets_unavailable_cause(
    targets: &SemanticTargets,
    package: backend_engine::PackageKey,
    profile: backend_semantic::vocabulary::LanguageProfile,
) -> StructuralCause {
    targets
        .unavailable
        .get(&(package.to_bytes(), profile))
        .map_or(
            StructuralCause::NoCompletePublication,
            |reason| StructuralCause::PublicationUnavailable(*reason),
        )
}

fn semantic_profile_is_complete(
    complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
    package: Option<backend_engine::PackageKey>,
    path: &str,
) -> Result<bool, BuiltinModelError> {
    let Some(package) = package else {
        return Ok(false);
    };
    // A file whose extension has no semantic profile is never covered by a
    // completed compile, so it keeps its structural facts.
    let Some(profile) =
        super::ingest::source_profile(std::path::Path::new(path)).map_err(BuiltinModelError)?
    else {
        return Ok(false);
    };
    Ok(complete.contains(&(package.to_bytes(), profile)))
}

struct SemanticRows {
    rows: Vec<Row>,
    complete: BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
    /// Current paths, per profile, whose published images no longer match
    /// their persisted content identity (or that no image compiled at all).
    stale_paths: ProfileStalePaths,
    activated: BTreeSet<(
        backend_engine::PackageKey,
        backend_semantic::vocabulary::LanguageProfile,
    )>,
    ledger: ProjectionLedger,
    targets: SemanticTargets,
}

/// Typed record of one (project, profile) target's semantic state, read from
/// the selected publication rows before row projection starts.
#[derive(Default)]
struct SemanticTargets {
    /// Targets whose selected publication is a terminal unavailable cause.
    unavailable: BTreeMap<
        ([u8; 32], backend_semantic::vocabulary::LanguageProfile),
        SemanticUnavailableReason,
    >,
}

fn semantic_rows(
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    projects: &BTreeMap<[u8; 32], super::IndexedProject>,
    current_paths: &ProfileSourcePaths,
    current_identities: &ProfileSourceIdentities,
    initial: &ViewRoot,
    row_capacity: usize,
) -> Result<SemanticRows, BuiltinModelError> {
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| {
            BuiltinModelError(format!("open semantic publication relation: {error}"))
        })?;
    let mut rows = Vec::new();
    let mut complete = BTreeSet::new();
    let mut stale_paths = ProfileStalePaths::new();
    let mut activated_publications = BTreeSet::new();
    let mut ledger = ProjectionLedger::default();
    let mut symbols = BTreeSet::new();
    let mut remaining_bytes = MAX_REBUILD_BYTES;
    let mut targets = SemanticTargets::default();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| {
                BuiltinModelError(format!("read semantic publication page: {error}"))
            })?;
        for (key, record) in page.entries() {
            if !key.is_selected() {
                continue;
            }
            let target = (*key.package_key().as_bytes(), key.profile());
            let ProductSemanticPublicationRecord::Published {
                coverage: backend_engine::builtin::SemanticPublicationCoverage::Complete,
                claim,
            } = record
            else {
                if let ProductSemanticPublicationRecord::Unavailable(reason) = record {
                    targets.unavailable.entry(target).or_insert(*reason);
                }
                continue;
            };
            let project = projects.get(key.package_key().as_bytes()).ok_or_else(|| {
                BuiltinModelError(
                    "semantic publication refers to a missing package frontier".to_owned(),
                )
            })?;
            let activated = activate_semantic_publication(compiler, key, *claim)?;
            let decision = {
                let compiled_sources = compiled_source_identities(&activated)?;
                let empty = BTreeSet::new();
                freshness_decision(
                    &compiled_sources,
                    current_paths.get(&target).unwrap_or(&empty),
                    current_identities.get(&target),
                )
            };
            for image in activated.images() {
                let view = SemanticImageView::reopen(image.as_ref()).map_err(|error| {
                    BuiltinModelError(format!("reopen activated semantic image: {error}"))
                })?;
                let path = compiled_source_path(&view)?;
                // Staleness is per image: this image is stale exactly when the
                // current file compiled from its path no longer hashes to the
                // image's own source identity. A legacy scan without persisted
                // identities falls back to the coarse path-set comparison.
                let stale = match decision.compiled.get(&path) {
                    Some(identity) => decision.image_stale(&path, *identity),
                    None => decision.path_sets_differ,
                };
                let mut sink = SemanticRowSink {
                    initial,
                    symbols: &mut symbols,
                    rows: &mut rows,
                    capacity: row_capacity,
                    remaining_bytes: &mut remaining_bytes,
                    stale,
                };
                append_image_rows(&view, project, key.profile(), &mut sink)?;
            }
            complete.insert((project.package.to_bytes(), key.profile()));
            if !decision.stale_paths.is_empty() {
                stale_paths.insert(target, decision.stale_paths.clone());
                for path in &decision.stale_paths {
                    ledger.record(
                        path,
                        FileLane::Semantic {
                            freshness: SemanticFreshness::Stale,
                        },
                    );
                }
            }
            activated_publications.insert((key.package_key(), key.profile()));
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    Ok(SemanticRows {
        rows,
        complete,
        stale_paths,
        activated: activated_publications,
        ledger,
        targets,
    })
}

/// The relative source path and semantic content identity each compiled image
/// carries, read from the publication's activated images.
type CompiledSources =
    BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>;

/// Typed freshness decisions for one (project, profile) publication target.
///
/// The comparison is content against content: each image carries the exact
/// `SourceFactDomain` identity its source bytes hash to, and every current
/// file record persists the same identity for its own bytes. An in-place edit
/// under an unchanged path set is therefore visible, which the older path-set
/// comparison could never prove. Where a current record predates identity
/// persistence the decision falls back to the coarse compiled-versus-current
/// path-set comparison, which stays the truth those records can support.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FreshnessDecision {
    /// Each image's compiled path and its source content identity.
    compiled: CompiledSources,
    /// The persisted identities behind this decision, present exactly when
    /// the comparison was decisive.
    identities:
        Option<BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>>,
    /// Current paths of this profile whose content identity mismatches the
    /// image compiled from that path, or that no image compiled at all.
    stale_paths: BTreeSet<String>,
    /// Whether every current file of the profile carries a persisted
    /// identity, making the content comparison decisive.
    identity_decisive: bool,
    /// Whether the compiled path set already differs from the current scan.
    path_sets_differ: bool,
}

impl FreshnessDecision {
    /// Returns whether the image compiled from `path` with `identity`
    /// predates the current file content.
    fn image_stale(
        &self,
        path: &str,
        identity: backend_version::ContentId<backend_version::SourceFactDomain>,
    ) -> bool {
        match &self.identities {
            Some(identities) => identities
                .get(path)
                .is_none_or(|current| *current != identity),
            None => self.path_sets_differ,
        }
    }
}

/// Compares each compiled image's (path, content identity) against the
/// current scan.
///
/// Identity comparison decides freshness only when every current file of the
/// profile states its identity; otherwise the decision is the honest fallback
/// those records support: the compiled and current path sets must agree.
fn freshness_decision(
    compiled: &CompiledSources,
    current_paths: &BTreeSet<String>,
    current_identities: Option<
        &BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>,
    >,
) -> FreshnessDecision {
    let identity_decisive =
        current_identities.is_some_and(|identities| identities.len() == current_paths.len());
    let path_sets_differ = {
        let compiled_paths = compiled
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<&str>>();
        let current = current_paths.iter().map(String::as_str).collect::<BTreeSet<_>>();
        compiled_paths != current
    };
    let mut stale_paths = BTreeSet::new();
    if identity_decisive {
        let identities = current_identities.expect("decisive above");
        for path in current_paths {
            let stale = compiled.get(path).is_none_or(|identity| {
                identities.get(path).is_none_or(|current| current != identity)
            });
            if stale {
                stale_paths.insert(path.clone());
            }
        }
    } else if path_sets_differ {
        stale_paths.clone_from(current_paths);
    }
    FreshnessDecision {
        compiled: compiled.clone(),
        identities: if identity_decisive {
            current_identities.cloned()
        } else {
            None
        },
        stale_paths,
        identity_decisive,
        path_sets_differ,
    }
}

/// Returns the relative source path and semantic content identity of every
/// activated image.
///
/// The publication binding already proves each image carries captured
/// provenance, so a missing path atom or identity is a broken invariant, not
/// a display gap.
fn compiled_source_identities(
    activated: &super::ActivatedProductSemantics,
) -> Result<CompiledSources, BuiltinModelError> {
    let mut compiled = BTreeMap::new();
    for image in activated.images() {
        let view = SemanticImageView::reopen(image.as_ref()).map_err(|error| {
            BuiltinModelError(format!("reopen activated semantic image: {error}"))
        })?;
        let (path, identity) = compiled_source(&view)?;
        compiled.insert(path, identity);
    }
    Ok(compiled)
}

/// Returns one image's relative source path and its source content identity.
fn compiled_source(
    image: &SemanticImageView<'_>,
) -> Result<(String, backend_version::ContentId<backend_version::SourceFactDomain>), BuiltinModelError>
{
    let backend_semantic::ir::ImageProvenance::Captured { source, scope, .. } =
        image.image_facts().provenance
    else {
        return Err(BuiltinModelError(
            "semantic image lost its captured provenance before projection".to_owned(),
        ));
    };
    let path = image
        .atom(scope.path)
        .ok_or_else(|| BuiltinModelError("semantic image lost its source path".to_owned()))?;
    let path = std::str::from_utf8(path)
        .map(str::to_owned)
        .map_err(|_| BuiltinModelError("semantic source path is not UTF-8".to_owned()))?;
    Ok((path, source.identity))
}

/// Returns the exact relative source path one image was compiled from.
fn compiled_source_path(image: &SemanticImageView<'_>) -> Result<String, BuiltinModelError> {
    compiled_source(image).map(|(path, _)| path)
}

struct SemanticRowSink<'a> {
    initial: &'a ViewRoot,
    symbols: &'a mut BTreeSet<RowId>,
    rows: &'a mut Vec<Row>,
    capacity: usize,
    remaining_bytes: &'a mut usize,
    /// Whether the image behind this sink predates the current scan. Stale
    /// rows stay semantic (never silently structural) and carry an explicit
    /// staleness note in their document.
    stale: bool,
}

struct SemanticRowContent {
    document: Vec<Fragment>,
    signature: Option<String>,
    encoded_bytes: usize,
}

fn append_image_rows(
    image: &SemanticImageView<'_>,
    project: &super::IndexedProject,
    profile: backend_semantic::vocabulary::LanguageProfile,
    sink: &mut SemanticRowSink<'_>,
) -> Result<(), BuiltinModelError> {
    let session = DocumentationSession::new(image);
    let image_identity = *blake3::hash(image.as_ref()).as_bytes();
    for entity in session.canonical_entities() {
        if sink.rows.len() == sink.capacity {
            return Err(BuiltinModelError(
                "workspace semantic declarations exceed the rebuild row bound".to_owned(),
            ));
        }
        let entity = entity
            .map_err(|error| BuiltinModelError(format!("project semantic declaration: {error}")))?;
        let name = std::str::from_utf8(entity.name)
            .map_err(|_| BuiltinModelError("semantic declaration name is not UTF-8".to_owned()))?;
        let identity = entity.entity.version.identity();
        let coordinate = semantic_coordinate(&project.label, identity, name);
        let symbol = semantic_symbol(project.package, identity);
        if !sink.symbols.insert(RowId::Symbol(symbol)) {
            return Err(BuiltinModelError(
                "semantic publication contains a duplicate declaration identity".to_owned(),
            ));
        }
        let content =
            semantic_row_content(profile, image, &entity, project.package, image_identity)?;
        let row_bytes = coordinate
            .len()
            .checked_add(content.encoded_bytes)
            .ok_or_else(|| {
                BuiltinModelError("project semantic row byte count overflow".to_owned())
            })?;
        *sink.remaining_bytes = sink.remaining_bytes.checked_sub(row_bytes).ok_or_else(|| {
            BuiltinModelError(
                "workspace semantic declarations exceed the rebuild byte bound".to_owned(),
            )
        })?;
        let mut document = content.document;
        if sink.stale {
            *sink.remaining_bytes = sink
                .remaining_bytes
                .checked_add(STALE_NOTE.len())
                .ok_or_else(|| {
                    BuiltinModelError(
                        "workspace semantic declarations exceed the rebuild byte bound".to_owned(),
                    )
                })?;
            document.push(Fragment::Text(STALE_NOTE.to_owned()));
        }
        let mut row = Row::in_package(
            RowId::Symbol(symbol),
            sink.initial.basis(),
            project.package,
            coordinate,
        )
        .try_with_identity_preimage(&semantic_identity(project.package, identity))
        .map_err(|error| BuiltinModelError(format!("semantic row identity preimage: {error}")))?
        .with_kind(declaration_kind(entity.entity.kind))
        .with_document(document);
        if let Some(signature) = content.signature {
            row = row.with_signature(signature);
        }
        if let Some(parent) = entity.entity.parent {
            let parent = session
                .entity(parent)
                .map_err(|error| BuiltinModelError(format!("project semantic parent: {error}")))?;
            row = row.with_parent(semantic_symbol(
                project.package,
                parent.entity.version.identity(),
            ));
        }
        sink.rows.push(row);
        for (_, link) in image.links_from(entity.entity.id) {
            let LinkTarget::External(target) = link.target else {
                continue;
            };
            let identity = ExternalTargetIdentity::capture(image, target).map_err(|error| {
                BuiltinModelError(format!(
                    "identify project semantic external target: {error}"
                ))
            })?;
            let symbol = external_semantic_symbol(project.package, image_identity, identity);
            if !sink.symbols.insert(RowId::Symbol(symbol)) {
                continue;
            }
            if sink.rows.len() == sink.capacity {
                return Err(BuiltinModelError(
                    "workspace semantic declarations exceed the rebuild row bound".to_owned(),
                ));
            }
            let label = "external semantic target";
            *sink.remaining_bytes =
                sink.remaining_bytes
                    .checked_sub(label.len())
                    .ok_or_else(|| {
                        BuiltinModelError(
                            "workspace semantic declarations exceed the rebuild byte bound"
                                .to_owned(),
                        )
                    })?;
            let preimage = backend_engine::encode_id(
                identity
                    .in_scope(project.package.to_bytes(), image_identity)
                    .as_bytes(),
            );
            sink.rows.push(
                Row::new(RowId::Symbol(symbol), sink.initial.basis(), label)
                    .try_with_identity_preimage(&preimage)
                    .map_err(|error| {
                        BuiltinModelError(format!("external row identity preimage: {error}"))
                    })?,
            );
        }
    }
    Ok(())
}

fn semantic_row_content<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    profile: backend_semantic::vocabulary::LanguageProfile,
    reader: &Reader,
    entity: &backend_engine::application::DocumentationEntity<'_, Reader>,
    package: backend_engine::PackageKey,
    image_identity: [u8; 32],
) -> Result<SemanticRowContent, BuiltinModelError> {
    let type_depth = NonZeroUsize::new(MAX_SEMANTIC_TYPE_DEPTH)
        .ok_or_else(|| BuiltinModelError("semantic type depth bound must be nonzero".to_owned()))?;
    let prepared = backend_semantic::ir::prepare_semantic_document(
        profile,
        reader,
        entity.entity.id,
        backend_semantic::ir::CanonicalTypeRenderLimits::new(type_depth),
    )
    .map_err(|error| BuiltinModelError(format!("prepare project semantic document: {error}")))?;
    if prepared.encoded_len > MAX_SEMANTIC_DOCUMENT_BYTES {
        return Err(BuiltinModelError(
            "project semantic document exceeds its output bound".to_owned(),
        ));
    }
    let documentation = entity
        .documentation()
        .map_err(|error| BuiltinModelError(format!("project semantic documentation: {error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| BuiltinModelError(format!("project semantic documentation: {error}")))?;
    let documentation_bytes = documentation_bytes(&documentation)?;
    let signature = semantic_signature(entity)?;
    let encoded_bytes = prepared
        .encoded_len
        .checked_add(documentation_bytes)
        .and_then(|bytes| bytes.checked_add(signature.as_ref().map_or(0, String::len)))
        .ok_or_else(|| BuiltinModelError("project semantic row byte count overflow".to_owned()))?;
    let mut output = vec![0; prepared.encoded_len];
    let rendered = prepared
        .write_into(&mut output)
        .map_err(|error| BuiltinModelError(format!("write project semantic document: {error}")))?
        .to_owned();
    let mut document = Vec::with_capacity(documentation.len().saturating_add(1));
    document.push(Fragment::Code(rendered));
    document.extend(
        documentation
            .into_iter()
            .map(|fragment| documentation_fragment(reader, package, image_identity, fragment))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(SemanticRowContent {
        document,
        signature,
        encoded_bytes,
    })
}

fn documentation_bytes(
    documentation: &[DocumentationFragment<'_>],
) -> Result<usize, BuiltinModelError> {
    documentation.iter().try_fold(0_usize, |bytes, fragment| {
        let fragment_bytes = match fragment {
            DocumentationFragment::Text(text) | DocumentationFragment::Code(text) => text.len(),
            DocumentationFragment::SoftBreak | DocumentationFragment::HardBreak => 0,
            DocumentationFragment::Link { label, .. } => label.len(),
        };
        bytes.checked_add(fragment_bytes).ok_or_else(|| {
            BuiltinModelError("project semantic documentation byte count overflow".to_owned())
        })
    })
}

fn documentation_fragment<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &Reader,
    package: backend_engine::PackageKey,
    image: [u8; 32],
    fragment: DocumentationFragment<'_>,
) -> Result<Fragment, BuiltinModelError> {
    Ok(match fragment {
        DocumentationFragment::Text(text) => Fragment::Text(text.to_owned()),
        DocumentationFragment::Code(code) => Fragment::Code(code.to_owned()),
        DocumentationFragment::SoftBreak | DocumentationFragment::HardBreak => Fragment::Break,
        DocumentationFragment::Link { label, target } => Fragment::Link {
            label: label.to_owned(),
            target: match target {
                backend_engine::application::DocumentationTarget::Local(target) => {
                    semantic_symbol(package, target.entity.version.identity())
                }
                backend_engine::application::DocumentationTarget::External { id, .. } => {
                    let identity =
                        ExternalTargetIdentity::capture(reader, id).map_err(|error| {
                            BuiltinModelError(format!(
                                "identify documentation external target: {error}"
                            ))
                        })?;
                    external_semantic_symbol(package, image, identity)
                }
            },
        },
    })
}

pub(super) fn external_semantic_symbol(
    package: backend_engine::PackageKey,
    image: [u8; 32],
    target: ExternalTargetIdentity,
) -> backend_engine::SymbolKey {
    let scoped = target.in_scope(package.to_bytes(), image);
    backend_engine::symbol_key(&backend_engine::encode_id(scoped.as_bytes()))
}

fn semantic_signature<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    entity: &backend_engine::application::DocumentationEntity<'_, Reader>,
) -> Result<Option<String>, BuiltinModelError> {
    let Some(semantic_type) = entity
        .semantic_type()
        .map_err(|error| BuiltinModelError(format!("project semantic type: {error}")))?
    else {
        return Ok(None);
    };
    let type_depth = NonZeroUsize::new(MAX_SEMANTIC_TYPE_DEPTH)
        .ok_or_else(|| BuiltinModelError("semantic type depth bound must be nonzero".to_owned()))?;
    let prepared = semantic_type
        .prepare_canonical(backend_semantic::ir::CanonicalTypeRenderLimits::new(type_depth))
        .map_err(|error| BuiltinModelError(format!("render project semantic type: {error}")))?;
    if prepared.encoded_len > MAX_SEMANTIC_SIGNATURE_BYTES {
        return Err(BuiltinModelError(
            "project semantic signature exceeds its output bound".to_owned(),
        ));
    }
    let mut output = vec![0; prepared.encoded_len];
    let rendered = prepared
        .write_into(&mut output)
        .map_err(|error| BuiltinModelError(format!("write project semantic type: {error}")))?;
    Ok(Some(rendered.to_owned()))
}

fn declaration_kind(kind: ItemKind) -> DeclarationKind {
    match kind {
        ItemKind::Function => DeclarationKind::Function,
        ItemKind::Constant | ItemKind::Variant => DeclarationKind::Constant,
        ItemKind::Record => DeclarationKind::Struct,
        ItemKind::Module | ItemKind::Namespace => DeclarationKind::Module,
        ItemKind::Field => DeclarationKind::Field,
        ItemKind::Alias | ItemKind::Implementation => DeclarationKind::Type,
        ItemKind::Trait => DeclarationKind::Trait,
        ItemKind::Enum => DeclarationKind::Enum,
        ItemKind::Static | ItemKind::Parameter => DeclarationKind::Variable,
        ItemKind::Reexport => DeclarationKind::Import,
        ItemKind::Macro => DeclarationKind::Macro,
    }
}

pub(super) fn semantic_symbol(
    package: backend_engine::PackageKey,
    identity: DeclarationIdentity,
) -> backend_engine::SymbolKey {
    backend_engine::symbol_key(&semantic_identity(package, identity))
}

fn semantic_coordinate(project: &str, identity: DeclarationIdentity, name: &str) -> String {
    let mut encoded = String::with_capacity(project.len() + name.len() + 78);
    encoded.push_str(project);
    encoded.push_str("::semantic::");
    push_declaration_identity(&mut encoded, identity);
    encoded.push_str("::");
    encoded.push_str(name);
    encoded
}

fn semantic_identity(package: backend_engine::PackageKey, identity: DeclarationIdentity) -> String {
    let mut encoded = package_token(package);
    encoded.push_str("::");
    push_declaration_identity(&mut encoded, identity);
    encoded
}

fn push_declaration_identity(output: &mut String, identity: DeclarationIdentity) {
    push_hex(output, identity.family.as_bytes());
    push_hex(output, identity.variant.as_bytes());
}

fn push_hex(output: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
}

pub(super) fn package_token(package: backend_engine::PackageKey) -> String {
    backend_engine::encode_id(package.as_bytes())
}

/// Builds Trustfall's immutable input from typed compiler images and the
/// explicitly structural fallback lane. No product-view row can enter this
/// boundary, so presentation data cannot be mistaken for semantic authority.
pub(super) fn semantic_query_corpus(
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    sources: &IndexedSources,
) -> Result<backend_extension_trustfall::SemanticQueryCorpus, BuiltinModelError> {
    if sources.projects.len() > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace package rows exceed the semantic query bound".to_owned(),
        ));
    }
    let mut facts = Vec::with_capacity(sources.projects.len());
    for project in sources.projects.values() {
        facts.push(backend_extension_trustfall::SemanticQueryFact::new(
            backend_extension_trustfall::SemanticQueryEvidence::Package(
                backend_extension_trustfall::PackageScopeEvidence::new(project.package),
            ),
            backend_extension_trustfall::SemanticQueryPresentation {
                id: query_package_id(project.package),
                kind: "project".to_owned(),
                coordinate: project.label.clone(),
                name: project.label.clone(),
                signature: None,
                documentation: String::new(),
                score: None,
                project: None,
                parent: None,
                related: Box::new([]),
            },
        ));
    }

    let complete = append_compiler_query_facts(snapshot, compiler, sources, &mut facts)?;
    append_structural_query_facts(sources, &complete, &mut facts)?;
    if facts.len() > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace semantic query facts exceed their row bound".to_owned(),
        ));
    }
    backend_extension_trustfall::SemanticQueryCorpus::admit_with_limits(
        snapshot.root(),
        facts,
        backend_extension_trustfall::Limits {
            max_rows: MAX_SEMANTIC_QUERY_ROWS,
            max_fields_per_row: MAX_SEMANTIC_QUERY_ROWS,
            max_field_bytes: MAX_SEMANTIC_DOCUMENT_BYTES,
            max_total_bytes: MAX_REBUILD_BYTES,
            ..backend_extension_trustfall::Limits::default()
        },
    )
    .map_err(|error| BuiltinModelError(error.to_string()))
}

fn append_compiler_query_facts(
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    sources: &IndexedSources,
    facts: &mut Vec<backend_extension_trustfall::SemanticQueryFact>,
) -> Result<BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>, BuiltinModelError> {
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic query relation: {error}")))?;
    let mut complete = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("read semantic query page: {error}")))?;
        for (key, record) in page.entries() {
            if !key.is_selected() {
                continue;
            }
            let ProductSemanticPublicationRecord::Published {
                coverage: backend_engine::builtin::SemanticPublicationCoverage::Complete,
                claim,
            } = record
            else {
                continue;
            };
            let project = sources
                .projects
                .get(key.package_key().as_bytes())
                .ok_or_else(|| {
                    BuiltinModelError(
                        "semantic query publication refers to a missing package frontier"
                            .to_owned(),
                    )
                })?;
            let activated = activate_semantic_publication(compiler, key, *claim)?;
            for image_bytes in activated.images() {
                let image = SemanticImageView::reopen(image_bytes.as_ref()).map_err(|error| {
                    BuiltinModelError(format!("reopen semantic query image: {error}"))
                })?;
                let session = DocumentationSession::new(&image);
                let identities = session
                    .canonical_entities()
                    .map(|entity| {
                        entity
                            .map(|entity| entity.entity.version.identity())
                            .map_err(|error| {
                                BuiltinModelError(format!(
                                    "read semantic query declaration: {error}"
                                ))
                            })
                    })
                    .collect::<Result<BTreeSet<_>, _>>()?;
                let image_digest = *blake3::hash(image_bytes.as_ref()).as_bytes();
                let mut external_targets = BTreeMap::<String, ExternalTargetIdentity>::new();
                for entity in session.canonical_entities() {
                    let entity = entity.map_err(|error| {
                        BuiltinModelError(format!("read semantic query declaration: {error}"))
                    })?;
                    let identity = entity.entity.version.identity();
                    let id = query_semantic_id(project.package, identity);
                    if !ids.insert(id.clone()) {
                        return Err(BuiltinModelError(
                            "semantic query publication contains a duplicate declaration identity"
                                .to_owned(),
                        ));
                    }
                    let name = std::str::from_utf8(entity.name)
                        .map_err(|_| {
                            BuiltinModelError(
                                "semantic query declaration name is not UTF-8".to_owned(),
                            )
                        })?
                        .to_owned();
                    let content = semantic_row_content(
                        key.profile(),
                        &image,
                        &entity,
                        project.package,
                        image_digest,
                    )?;
                    let parent = entity.entity.parent.map(|parent| {
                        query_semantic_id(
                            project.package,
                            session
                                .entity(parent)
                                .expect("validated semantic parent")
                                .entity
                                .version
                                .identity(),
                        )
                    });
                    let mut related = Vec::new();
                    for (_, link) in image.links_from(entity.entity.id) {
                        match link.target {
                            LinkTarget::Local(target) => {
                                let Some(target) = image.entity(target) else {
                                    return Err(BuiltinModelError(
                                        "semantic query local target is absent".to_owned(),
                                    ));
                                };
                                let target = target.version.identity();
                                if identities.contains(&target) {
                                    related.push(query_semantic_id(project.package, target));
                                }
                            }
                            LinkTarget::External(target) => {
                                let identity = ExternalTargetIdentity::capture(&image, target)
                                    .map_err(|error| {
                                        BuiltinModelError(format!(
                                            "identify semantic query external target: {error}"
                                        ))
                                    })?;
                                let target_id =
                                    query_external_id(project.package, image_digest, identity);
                                external_targets
                                    .entry(target_id.clone())
                                    .or_insert(identity);
                                related.push(target_id);
                            }
                        }
                    }
                    related.sort_unstable();
                    related.dedup();
                    facts.push(backend_extension_trustfall::SemanticQueryFact::new(
                        backend_extension_trustfall::SemanticQueryEvidence::Compiler(
                            backend_extension_trustfall::CompilerSemanticEvidence::new(
                                project.package,
                                key.coordinate().clone(),
                                key.profile(),
                                identity,
                                image_digest,
                                image.image_facts(),
                            ),
                        ),
                        backend_extension_trustfall::SemanticQueryPresentation {
                            id,
                            kind: declaration_kind(entity.entity.kind).name().to_owned(),
                            coordinate: semantic_coordinate(&project.label, identity, &name),
                            name,
                            signature: content.signature,
                            documentation: fragment_text(&content.document),
                            score: None,
                            project: Some(query_package_id(project.package)),
                            parent,
                            related: related.into_boxed_slice(),
                        },
                    ));
                }
                for (id, target) in external_targets {
                    if !ids.insert(id.clone()) {
                        return Err(BuiltinModelError(
                            "semantic query publication contains a duplicate external target identity"
                                .to_owned(),
                        ));
                    }
                    facts.push(backend_extension_trustfall::SemanticQueryFact::new(
                        backend_extension_trustfall::SemanticQueryEvidence::CompilerExternalTarget(
                            backend_extension_trustfall::CompilerExternalTargetEvidence::new(
                                project.package,
                                key.coordinate().clone(),
                                key.profile(),
                                target,
                                image_digest,
                                image.image_facts(),
                            ),
                        ),
                        backend_extension_trustfall::SemanticQueryPresentation {
                            coordinate: format!("{}::external::{id}", project.label),
                            id,
                            kind: "external".to_owned(),
                            name: "external semantic target".to_owned(),
                            signature: None,
                            documentation: String::new(),
                            score: None,
                            project: Some(query_package_id(project.package)),
                            parent: None,
                            related: Box::new([]),
                        },
                    ));
                }
            }
            complete.insert((project.package.to_bytes(), key.profile()));
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    Ok(complete)
}

fn append_structural_query_facts(
    sources: &IndexedSources,
    complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
    facts: &mut Vec<backend_extension_trustfall::SemanticQueryFact>,
) -> Result<(), BuiltinModelError> {
    let types = ProjectTypeIndex::of(sources);
    for (_, record) in &sources.files {
        let file = record
            .file_fields()
            .ok_or_else(|| BuiltinModelError("expected structural source file".to_owned()))?;
        let project = sources.projects.get(&file.project).ok_or_else(|| {
            BuiltinModelError("structural source refers to a missing project".to_owned())
        })?;
        // An extension with no semantic profile has no profile to attribute
        // structural evidence to either, so it contributes no query facts.
        // Skipping one odd file is the whole point: failing here is what made
        // a single unprofiled file refuse the entire rebuild.
        let Some(profile) = super::ingest::source_profile(std::path::Path::new(file.path))
            .map_err(BuiltinModelError)?
        else {
            continue;
        };
        if complete.contains(&(project.package.to_bytes(), profile)) {
            continue;
        }
        let containment = FileContainment::new(
            &project.label,
            file.path,
            file.project,
            file.declarations,
        );
        let duplicate_coordinates =
            duplicate_declaration_coordinates(&containment, file.declarations);
        let mut occurrences = BTreeMap::new();
        for declaration in file.declarations.iter() {
            let coordinate = containment.coordinate(declaration);
            let (id, _) = declaration_symbol(
                &coordinate,
                declaration.kind(),
                declaration.signature(),
                duplicate_coordinates.contains(&coordinate),
                &mut occurrences,
            );
            let id = id.stable_key();
            let documentation = if is_file_module(declaration, file.path) {
                format!("{} source · {}", file.language.name(), file.path)
            } else if declaration.documentation().is_empty() {
                format!(
                    "{} in {}:{}",
                    declaration.kind_name(),
                    file.path,
                    declaration.line()
                )
            } else {
                declaration.documentation().to_owned()
            };
            let parent = containment
                .parent_coordinate(declaration, &types)
                .map(|coordinate| {
                    RowId::Symbol(backend_engine::symbol_key(&coordinate)).stable_key()
                });
            facts.push(backend_extension_trustfall::SemanticQueryFact::new(
                backend_extension_trustfall::SemanticQueryEvidence::StructuralFallback(
                    backend_extension_trustfall::StructuralFallbackEvidence::new(
                        project.package,
                        profile,
                        file.content_version,
                        file.analysis_version,
                    ),
                ),
                backend_extension_trustfall::SemanticQueryPresentation {
                    id,
                    kind: declaration.kind().name().to_owned(),
                    coordinate,
                    name: declaration.name().to_owned(),
                    signature: (!declaration.signature().is_empty())
                        .then(|| declaration.signature().to_owned()),
                    documentation,
                    score: None,
                    project: Some(query_package_id(project.package)),
                    parent,
                    related: Box::new([]),
                },
            ));
        }
    }
    Ok(())
}

fn query_package_id(package: backend_engine::PackageKey) -> String {
    RowId::Package(package).stable_key()
}

fn query_semantic_id(package: backend_engine::PackageKey, identity: DeclarationIdentity) -> String {
    RowId::Symbol(semantic_symbol(package, identity)).stable_key()
}

fn query_external_id(
    package: backend_engine::PackageKey,
    image: [u8; 32],
    target: ExternalTargetIdentity,
) -> String {
    RowId::Symbol(external_semantic_symbol(package, image, target)).stable_key()
}

fn fragment_text(fragments: &[Fragment]) -> String {
    let mut output = String::new();
    for fragment in fragments {
        match fragment {
            Fragment::Text(value) | Fragment::Code(value) => output.push_str(value),
            Fragment::Link { label, .. } => output.push_str(label),
            Fragment::Break => output.push('\n'),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectTypeIndex, SourceRowProjection, STALE_NOTE, semantic_profile_is_complete,
    };
    use super::super::{
        FileLane, IndexedProject, SemanticFreshness, StructuralCause,
    };
    use super::super::initial_view;
    use super::{ProfileStalePaths};
    use backend_engine::{Row, RowId, package_key, product_source_file_key, symbol_key};
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity,
        EntityAuthorityFacts, EntityVersion, FactAvailability, IrBuilder, ItemKind,
        ParentageAuthority, SemanticImageView, SourceIdentity, TreeEntityId, TreeItemInput,
        VariantFingerprint, Visibility, encode_full_semantic_image, full_semantic_image_len,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, CStandard, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    const FIXTURE_PATH: &str = "src/worker.rs";

    const FIXTURE_SOURCE: &str = r"
pub struct Worker {
    pub name: String,
}

pub enum Event {
    Started,
}

impl Worker {
    pub fn run(&self) {}
}

pub fn execute() {}
";

    fn fixture_version(identity: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([identity; 16]),
            variant: VariantFingerprint::from_raw([identity; 16]),
            core_payload: CorePayloadHash::from_raw([identity; 16]),
        }
    }

    fn fixture_authority(parentage: ParentageAuthority) -> EntityAuthorityFacts {
        EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        }
    }

    /// The exact declared identity `fixture_version(identity)` carries.
    fn fixture_identity(identity: u8) -> ParentageAuthority {
        ParentageAuthority::Bound(DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([identity; 16]),
            variant: VariantFingerprint::from_raw([identity; 16]),
        })
    }

    /// Encodes one Rust semantic image whose IR contains a struct field and
    /// an enum variant — the members the TAGS baseline cannot see.
    fn fixture_semantic_image(path: &str) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"fixture-source"),
            byte_len: 14,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture-toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let struct_id = TreeEntityId::new(0);
        let enum_id = TreeEntityId::new(2);
        let items = [
            TreeItemInput {
                name: b"Worker",
                kind: ItemKind::Record,
                visibility: Visibility::Public,
                authority: fixture_authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"name",
                kind: ItemKind::Field,
                visibility: Visibility::Public,
                authority: fixture_authority(fixture_identity(1)),
                parent: Some(struct_id),
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"Event",
                kind: ItemKind::Enum,
                visibility: Visibility::Public,
                authority: fixture_authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"Started",
                kind: ItemKind::Variant,
                visibility: Visibility::Public,
                authority: fixture_authority(fixture_identity(3)),
                parent: Some(enum_id),
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[fixture_version(1), fixture_version(2), fixture_version(3), fixture_version(4)],
                items: &items,
                links: &[],
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    /// Projects the fixture through the real row projection.
    ///
    /// The declarations come from the real Rust frontend rather than from
    /// hand-written containment, so the test fails if extraction and
    /// projection ever disagree about what a parent is addressed by.
    fn projected_rows() -> Result<(Vec<Row>, super::ProjectionLedger), String> {
        let frontend = backend_frontend_rust::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(std::path::Path::new(FIXTURE_PATH), FIXTURE_SOURCE.as_bytes())
            .map_err(|e| e.to_string())?;
        let project_key = [7u8; 32];
        let file_key = product_source_file_key(project_key, FIXTURE_PATH);
        let record = super::super::ProductSourceRecord::file(
            project_key,
            FIXTURE_PATH,
            backend_engine::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            analysis.declarations().clone(),
        )?;
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package: package_key("fixture"),
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let types = ProjectTypeIndex::of(&sources);
        let targets = super::SemanticTargets::default();
        let mut projection =
            SourceRowProjection::new(&initial, &sources.projects, 64, &targets)
                .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &BTreeSet::new(), &ProfileStalePaths::new(), &types)
                .map_err(|e| e.to_string())?;
        }
        let ledger = projection.take_ledger();
        projection
            .finish(Vec::new())
            .map(|rows| (rows, ledger))
            .map_err(|e| e.to_string())
    }

    /// Projects the fixture image through the real semantic row sink, with
    /// the requested staleness, and returns the projected rows.
    fn projected_semantic_rows(stale: bool) -> Result<Vec<Row>, String> {
        let bytes = fixture_semantic_image(FIXTURE_PATH)?;
        let view = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let project = IndexedProject {
            package: package_key("fixture"),
            label: "fixture".to_owned(),
            files: Arc::<[[u8; 32]]>::from([]),
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = usize::MAX;
        let mut sink = super::SemanticRowSink {
            initial: &initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 64,
            remaining_bytes: &mut remaining,
            stale,
        };
        super::append_image_rows(
            &view,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
        )
        .map_err(|error| error.to_string())?;
        Ok(rows)
    }

    /// Finds one row whose coordinate ends with the given name.
    fn row_named<'a>(rows: &'a [Row], name: &str) -> Result<&'a Row, String> {
        rows.iter()
            .find(|row| row.label.ends_with(&format!("::{name}")))
            .ok_or_else(|| {
                let rendered = rows
                    .iter()
                    .map(|row| row.label.clone())
                    .collect::<Vec<_>>()
                    .join("\n  ");
                format!("no row named {name}\nprojected:\n  {rendered}")
            })
    }

    #[test]
    fn repeated_tag_coordinates_keep_distinct_checked_identity_preimages() {
        let coordinate = "fixture::src/lib.rs:1::run";
        let mut occurrences = BTreeMap::new();
        let (first, first_preimage) = super::declaration_symbol(
            coordinate,
            backend_engine::DeclarationKind::Method,
            "fn run()",
            true,
            &mut occurrences,
        );
        let (second, second_preimage) = super::declaration_symbol(
            coordinate,
            backend_engine::DeclarationKind::Function,
            "fn run()",
            true,
            &mut occurrences,
        );
        let first_preimage = first_preimage.expect("overlapping tag gets a preimage");
        assert_eq!(first, RowId::Symbol(symbol_key(&first_preimage)));
        let second_preimage = second_preimage.expect("overlapping tag gets a preimage");
        assert_eq!(second, RowId::Symbol(symbol_key(&second_preimage)));
        assert_ne!(first, second);

        let mut reverse = BTreeMap::new();
        let (_, reverse_function) = super::declaration_symbol(
            coordinate,
            backend_engine::DeclarationKind::Function,
            "fn run()",
            true,
            &mut reverse,
        );
        let (_, reverse_method) = super::declaration_symbol(
            coordinate,
            backend_engine::DeclarationKind::Method,
            "fn run()",
            true,
            &mut reverse,
        );
        assert_eq!(
            second_preimage,
            reverse_function.expect("function identity is order independent")
        );
        assert_eq!(
            first_preimage,
            reverse_method.expect("method identity is order independent")
        );

        let mut unique = BTreeMap::new();
        let (unique_id, unique_preimage) = super::declaration_symbol(
            "fixture::src/index.ts:4::run",
            backend_engine::DeclarationKind::Function,
            "function run(): void",
            false,
            &mut unique,
        );
        assert_eq!(
            unique_id,
            RowId::Symbol(symbol_key("fixture::src/index.ts:4::run"))
        );
        assert!(unique_preimage.is_none(), "unique TypeScript rows stay compact");
    }

    /// Renders one row's parent as the coordinate it points at, or `-`.
    fn parent_of(rows: &[Row], coordinate: &str) -> Result<String, String> {
        let row = rows
            .iter()
            .find(|row| row.label == coordinate)
            .ok_or_else(|| {
                let rendered = rows
                    .iter()
                    .map(|row| row.label.clone())
                    .collect::<Vec<_>>()
                    .join("\n  ");
                format!("no row for {coordinate}\nprojected:\n  {rendered}")
            })?;
        let Some(parent) = row.parent else {
            return Ok("-".to_owned());
        };
        Ok(rows
            .iter()
            .find(|candidate| candidate.id == RowId::Symbol(parent))
            .map_or_else(|| "<dangling>".to_owned(), |found| found.label.clone()))
    }

    #[test]
    fn declarations_are_parented_to_their_type_and_not_to_the_file() -> Result<(), String> {
        let (rows, _) = projected_rows()?;
        let module = "fixture::src/worker.rs";
        let worker = "fixture::src/worker.rs:2::Worker";
        let event = "fixture::src/worker.rs:6::Event";
        for (row, expected) in [
            (module, "-"),
            (worker, module),
            ("fixture::src/worker.rs:3::name", worker),
            (event, module),
            ("fixture::src/worker.rs:7::Started", event),
            ("fixture::src/worker.rs:11::run", worker),
            ("fixture::src/worker.rs:14::execute", module),
        ] {
            let parent = parent_of(&rows, row)?;
            if parent != expected {
                return Err(format!("{row} is parented to {parent}, expected {expected}"));
            }
        }
        Ok(())
    }

    #[test]
    fn an_attached_method_row_keeps_the_type_symbol_as_its_parent() -> Result<(), String> {
        let (rows, _) = projected_rows()?;
        let method = rows
            .iter()
            .find(|row| row.label == "fixture::src/worker.rs:11::run")
            .ok_or("no method row")?;
        if method.parent != Some(symbol_key("fixture::src/worker.rs:2::Worker")) {
            return Err(format!("method parent is {:?}", method.parent));
        }
        if method.kind != Some(backend_engine::DeclarationKind::Method) {
            return Err(format!("method row kind is {:?}", method.kind));
        }
        Ok(())
    }

    #[test]
    fn the_structural_fallback_answers_with_typed_provenance() -> Result<(), String> {
        let (rows, ledger) = projected_rows()?;
        if rows
            .iter()
            .any(|row| row.label.contains("::semantic::"))
        {
            return Err("a structural fallback projection emitted semantic rows".to_owned());
        }
        let lane = ledger
            .file_lane(FIXTURE_PATH)
            .ok_or("the fixture file has no lane decision")?;
        if lane
            != (FileLane::Structural {
                cause: StructuralCause::NoCompletePublication,
            })
        {
            return Err(format!("structural fallback lane is {lane:?}"));
        }
        if ledger.all_semantic_fresh() {
            return Err("a structural answer was recorded as semantic".to_owned());
        }
        if ledger.files().collect::<Vec<_>>().len() != 1 {
            return Err(format!(
                "the single-file projection recorded {:?}",
                ledger.files().collect::<Vec<_>>()
            ));
        }
        Ok(())
    }

    #[test]
    fn the_semantic_lane_answers_fields_and_variants_the_baseline_cannot_see()
    -> Result<(), String> {
        let rows = projected_semantic_rows(false)?;
        for name in ["Worker", "name", "Event", "Started"] {
            let row = row_named(&rows, name)?;
            if !row.label.contains("::semantic::") {
                return Err(format!("{name} row is not addressed as semantic: {row:?}"));
            }
        }
        let field = row_named(&rows, "name")?;
        if field.kind != Some(backend_engine::DeclarationKind::Field) {
            return Err(format!("field row kind is {:?}", field.kind));
        }
        let worker = row_named(&rows, "Worker")?;
        let expected_parent = match worker.id {
            RowId::Symbol(key) => key,
            other => return Err(format!("worker row id is {other:?}")),
        };
        if field.parent != Some(expected_parent) {
            return Err(format!("field parent is {:?}", field.parent));
        }
        let variant = row_named(&rows, "Started")?;
        let event = row_named(&rows, "Event")?;
        let event_parent = match event.id {
            RowId::Symbol(key) => key,
            other => return Err(format!("event row id is {other:?}")),
        };
        if variant.parent != Some(event_parent) {
            return Err(format!("variant parent is {:?}", variant.parent));
        }
        Ok(())
    }

    #[test]
    fn a_stale_image_answers_semantic_with_a_typed_stale_note() -> Result<(), String> {
        let fresh = projected_semantic_rows(false)?;
        let stale = projected_semantic_rows(true)?;
        if stale.len() != fresh.len() {
            return Err("staleness changed the projected row set".to_owned());
        }
        for row in &stale {
            let documented = row
                .document
                .iter()
                .any(|fragment| matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image")));
            if !documented {
                return Err(format!("stale row {} lost its typed note", row.label));
            }
            if row.label.contains("::semantic::") {
                continue;
            }
            return Err(format!("stale row {} fell back to a structural address", row.label));
        }
        if fresh.iter().any(|row| {
            row.document
                .iter()
                .any(|fragment| matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image")))
        }) {
            return Err("a fresh row carried the stale note".to_owned());
        }
        Ok(())
    }

    #[test]
    fn a_stale_publication_suppresses_structural_rows_instead_of_silently_falling_back()
    -> Result<(), String> {
        let frontend = backend_frontend_rust::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(std::path::Path::new(FIXTURE_PATH), FIXTURE_SOURCE.as_bytes())
            .map_err(|e| e.to_string())?;
        let project_key = [7u8; 32];
        let file_key = product_source_file_key(project_key, FIXTURE_PATH);
        let record = super::super::ProductSourceRecord::file(
            project_key,
            FIXTURE_PATH,
            backend_engine::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            analysis.declarations().clone(),
        )?;
        let package = package_key("fixture");
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let types = ProjectTypeIndex::of(&sources);
        let complete = BTreeSet::from([(package.to_bytes(), LanguageProfile::Rust(RustEdition::Rust2024))]);
        let stale = ProfileStalePaths::from([(
            (package.to_bytes(), LanguageProfile::Rust(RustEdition::Rust2024)),
            BTreeSet::from([FIXTURE_PATH.to_owned()]),
        )]);
        let targets = super::SemanticTargets::default();
        let mut projection = SourceRowProjection::new(&initial, &sources.projects, 64, &targets)
            .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &complete, &stale, &types)
                .map_err(|e| e.to_string())?;
        }
        let ledger = projection.take_ledger();
        let rows = projection.finish(Vec::new()).map_err(|e| e.to_string())?;
        if rows
            .iter()
            .any(|row| row.label.contains(FIXTURE_PATH) && !row.label.contains("::semantic::"))
        {
            return Err("a stale semantic file was silently answered structurally".to_owned());
        }
        if ledger.file_lane(FIXTURE_PATH)
            != Some(FileLane::Semantic {
                freshness: SemanticFreshness::Stale,
            })
        {
            return Err(format!(
                "stale file lane is {:?}",
                ledger.file_lane(FIXTURE_PATH)
            ));
        }
        Ok(())
    }

    /// Per-file precision at the projection level: an in-place edit under an
    /// unchanged path set marks exactly the edited file's lane stale, keeps
    /// both files on the semantic lane, and never lets either fall back to
    /// structural rows.
    #[test]
    fn an_in_place_edit_marks_only_the_edited_file_stale_and_suppresses_structure_for_both()
    -> Result<(), String> {
        const EDITED: &str = "src/edited.rs";
        const UNTOUCHED: &str = "src/untouched.rs";
        let frontend = backend_frontend_rust::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(std::path::Path::new(FIXTURE_PATH), FIXTURE_SOURCE.as_bytes())
            .map_err(|e| e.to_string())?;
        let project_key = [7u8; 32];
        let package = package_key("fixture");
        let mut files = Vec::new();
        let mut keys = Vec::new();
        for path in [EDITED, UNTOUCHED] {
            let key = product_source_file_key(project_key, path);
            keys.push(key);
            let record = super::super::ProductSourceRecord::file(
                project_key,
                path,
                backend_engine::SourceLanguage::Rust,
                [1; 32],
                [2; 32],
                analysis.declarations().clone(),
            )
            .map_err(|e| e.to_string())?;
            files.push((key, record));
        }
        files.sort_by_key(|(key, _)| *key);
        keys.sort();
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from(keys.into_boxed_slice()),
                },
            )]),
            files,
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let types = ProjectTypeIndex::of(&sources);
        let rust = LanguageProfile::Rust(RustEdition::Rust2024);
        let complete = BTreeSet::from([(package.to_bytes(), rust)]);
        let stale = ProfileStalePaths::from([(
            (package.to_bytes(), rust),
            BTreeSet::from([EDITED.to_owned()]),
        )]);
        let targets = super::SemanticTargets::default();
        let mut projection = SourceRowProjection::new(&initial, &sources.projects, 64, &targets)
            .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &complete, &stale, &types)
                .map_err(|e| e.to_string())?;
        }
        let ledger = projection.take_ledger();
        let rows = projection.finish(Vec::new()).map_err(|e| e.to_string())?;
        // Neither file falls back to structural rows: the stale image still
        // answers, typed stale, and never silently structural.
        for path in [EDITED, UNTOUCHED] {
            if rows
                .iter()
                .any(|row| row.label.contains(path) && !row.label.contains("::semantic::"))
            {
                return Err(format!("{path} was silently answered structurally"));
            }
        }
        if ledger.file_lane(EDITED)
            != Some(FileLane::Semantic {
                freshness: SemanticFreshness::Stale,
            })
        {
            return Err(format!("edited file lane is {:?}", ledger.file_lane(EDITED)));
        }
        if ledger.file_lane(UNTOUCHED)
            != Some(FileLane::Semantic {
                freshness: SemanticFreshness::Fresh,
            })
        {
            return Err(format!(
                "untouched file lane is {:?}",
                ledger.file_lane(UNTOUCHED)
            ));
        }
        Ok(())
    }

    #[test]
    fn an_unavailable_publication_types_the_structural_cause_with_its_reason()
    -> Result<(), String> {
        let frontend = backend_frontend_rust::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(std::path::Path::new(FIXTURE_PATH), FIXTURE_SOURCE.as_bytes())
            .map_err(|e| e.to_string())?;
        let project_key = [7u8; 32];
        let file_key = product_source_file_key(project_key, FIXTURE_PATH);
        let record = super::super::ProductSourceRecord::file(
            project_key,
            FIXTURE_PATH,
            backend_engine::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            analysis.declarations().clone(),
        )?;
        let package = package_key("fixture");
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let types = ProjectTypeIndex::of(&sources);
        let mut targets = super::SemanticTargets::default();
        targets.unavailable.insert(
            (package.to_bytes(), LanguageProfile::Rust(RustEdition::Rust2024)),
            backend_engine::builtin::SemanticUnavailableReason::Toolchain,
        );
        let mut projection = SourceRowProjection::new(&initial, &sources.projects, 64, &targets)
            .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &BTreeSet::new(), &ProfileStalePaths::new(), &types)
                .map_err(|e| e.to_string())?;
        }
        let ledger = projection.take_ledger();
        let rows = projection.finish(Vec::new()).map_err(|e| e.to_string())?;
        if rows
            .iter()
            .find(|row| row.label == "fixture::src/worker.rs:2::Worker")
            .is_none()
        {
            return Err("an unavailable publication lost its structural fallback".to_owned());
        }
        if ledger.file_lane(FIXTURE_PATH)
            != Some(FileLane::Structural {
                cause: StructuralCause::PublicationUnavailable(
                    backend_engine::builtin::SemanticUnavailableReason::Toolchain,
                ),
            })
        {
            return Err(format!(
                "unavailable publication lane is {:?}",
                ledger.file_lane(FIXTURE_PATH)
            ));
        }
        Ok(())
    }

    type SourceId = ContentId<SourceFactDomain>;

    fn fixture_identity_map(path: &str, identity: SourceId) -> BTreeMap<String, SourceId> {
        BTreeMap::from([(path.to_owned(), identity)])
    }

    #[test]
    fn freshness_compares_compiled_paths_against_the_current_scan() -> Result<(), String> {
        let bytes = fixture_semantic_image("src/worker.rs")?;
        let view = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let (path, identity) = super::compiled_source(&view).map_err(|e| e.to_string())?;
        let compiled = BTreeMap::from([(path.clone(), identity)]);
        let current = BTreeSet::from([path.clone()]);
        // The exact bytes the image was compiled from are fresh.
        let fresh = super::freshness_decision(
            &compiled,
            &current,
            Some(&fixture_identity_map(&path, identity)),
        );
        if fresh.stale_paths.contains(&path) || fresh.path_sets_differ {
            return Err("an identical source snapshot was judged stale".to_owned());
        }
        // A file added after the compile is stale; the unchanged file is not.
        let with_extra = BTreeSet::from([path.clone(), "src/extra.rs".to_owned()]);
        let identities = fixture_identity_map(&path, identity);
        let mut with_new_file = identities.clone();
        with_new_file.insert(
            "src/extra.rs".to_owned(),
            ContentId::<SourceFactDomain>::from_canonical_bytes(b"extra source"),
        );
        let expanded = super::freshness_decision(&compiled, &with_extra, Some(&with_new_file));
        if expanded.stale_paths != BTreeSet::from(["src/extra.rs".to_owned()]) {
            return Err(format!("added-file staleness is {:?}", expanded.stale_paths));
        }
        // Without persisted identities the fallback marks the whole scan
        // stale instead of claiming per-file precision it cannot prove.
        let expanded_legacy = super::freshness_decision(&compiled, &with_extra, Some(&identities));
        if expanded_legacy.identity_decisive
            || expanded_legacy.stale_paths != with_extra
        {
            return Err(format!(
                "legacy added-file staleness is {:?}",
                expanded_legacy.stale_paths
            ));
        }
        // A file deleted after the compile leaves a stale orphan image.
        let empty = BTreeSet::new();
        let deleted = super::freshness_decision(&compiled, &empty, None);
        if !deleted.path_sets_differ {
            return Err("a deleted source set was judged matching".to_owned());
        }
        Ok(())
    }

    /// The residual the path-set comparison could never catch: the same path
    /// set with one file edited in place must flip that file's freshness to
    /// stale while every untouched file stays fresh.
    #[test]
    fn an_in_place_edit_is_stale_without_a_path_set_change() -> Result<(), String> {
        let bytes = fixture_semantic_image("src/worker.rs")?;
        let view = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let (path, compiled_identity) = super::compiled_source(&view).map_err(|e| e.to_string())?;
        let compiled = BTreeMap::from([(path.clone(), compiled_identity)]);
        let current = BTreeSet::from([path.clone()]);
        let edited = ContentId::<SourceFactDomain>::from_canonical_bytes(b"edited in place");
        let decision = super::freshness_decision(
            &compiled,
            &current,
            Some(&fixture_identity_map(&path, edited)),
        );
        if decision.stale_paths != BTreeSet::from([path.clone()]) {
            return Err(format!("in-place edit staleness is {:?}", decision.stale_paths));
        }
        if !decision.image_stale(&path, compiled_identity) {
            return Err("the compiled image was not judged stale after an in-place edit".to_owned());
        }
        // The same comparison keeps an untouched file fresh.
        let untouched = super::freshness_decision(
            &compiled,
            &current,
            Some(&fixture_identity_map(&path, compiled_identity)),
        );
        if !untouched.stale_paths.is_empty()
            || untouched.image_stale(&path, compiled_identity)
        {
            return Err("an untouched file was judged stale".to_owned());
        }
        Ok(())
    }

    /// Records persisted before identities existed cannot prove freshness by
    /// content; they keep the coarse path-set comparison, which marks every
    /// current path stale when the path sets disagree and fresh otherwise.
    #[test]
    fn records_without_identities_keep_the_path_set_fallback() -> Result<(), String> {
        let bytes = fixture_semantic_image("src/worker.rs")?;
        let view = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let (path, identity) = super::compiled_source(&view).map_err(|e| e.to_string())?;
        let compiled = BTreeMap::from([(path.clone(), identity)]);
        let current = BTreeSet::from([path.clone()]);
        let fallback = super::freshness_decision(&compiled, &current, None);
        if !fallback.stale_paths.is_empty() || fallback.identity_decisive {
            return Err("an unchanged legacy snapshot was judged stale".to_owned());
        }
        if fallback.image_stale(&path, identity) {
            return Err("a legacy image under an unchanged path set was judged stale".to_owned());
        }
        let shifted = BTreeSet::from([path.clone(), "src/other.rs".to_owned()]);
        let mismatched = super::freshness_decision(&compiled, &shifted, None);
        if mismatched.stale_paths != shifted {
            return Err("a legacy path-set mismatch did not mark the current paths stale".to_owned());
        }
        Ok(())
    }

    #[test]
    fn complete_c_publication_does_not_suppress_cxx_fallback() -> Result<(), String> {
        let package = package_key("mixed-c-cxx");
        let complete = BTreeSet::from([(package.to_bytes(), LanguageProfile::C(CStandard::C23))]);
        if !semantic_profile_is_complete(&complete, Some(package), "source.c")
            .map_err(|error| error.to_string())?
            || semantic_profile_is_complete(&complete, Some(package), "source.cpp")
                .map_err(|error| error.to_string())?
        {
            return Err("C and C++ semantic fallback profiles collapsed".to_owned());
        }
        Ok(())
    }

    #[test]
    fn the_stale_note_is_stable() {
        assert!(STALE_NOTE.contains("stale semantic image"));
    }
}
