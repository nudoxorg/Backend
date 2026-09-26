use super::super::{
    BuiltinModelError, BuiltinSemanticRelation, FileLane, IndexedProject, IndexedSources,
    MAX_REBUILD_BYTES, MAX_REBUILD_PACKAGES, ProjectionLedger, SemanticFreshness, StructuralCause,
    WorkspaceSnapshot, activate_semantic_publication,
};
use super::identity::{
    declaration_coordinate, declaration_family, declaration_kind, external_semantic_symbol,
    semantic_coordinate, semantic_identity, semantic_symbol,
};
use super::structural::{
    FileContainment, ProfileSourceIdentities, ProfileSourcePaths, ProjectTypeIndex,
    StructuralDeclaration, StructuralParent, StructuralProjectionPlan, StructuralSymbol,
    duplicate_declaration_coordinates, is_file_module, profile_source_identities,
    profile_source_paths, projected_source_capacity,
};
use super::{
    MAX_SEMANTIC_DOCUMENT_BYTES, MAX_SEMANTIC_SIGNATURE_BYTES, MAX_SEMANTIC_TYPE_DEPTH, STALE_NOTE,
    semantic_profile_is_complete,
};
use backend_engine::application::{
    DocumentationFragment, DocumentationSession, LocalCompilerClient,
};
use backend_engine::builtin::{ProductSemanticPublicationRecord, SemanticUnavailableReason};
use backend_engine::{DeclarationKind, Fragment, Row, RowId, ViewRoot, product_source_file_key};
use backend_semantic::ir::{
    DeclarationIdentity, ExternalTargetIdentity, LinkTarget, SemanticCoreReader as _,
    SemanticImageView, SemanticReader as _,
};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

fn package_manifest_name(label: &str, workspace: &std::path::Path) -> Result<String, String> {
    super::super::local_manifest::indexed_package_manifest_name(label, workspace)
}

pub(crate) struct ProjectedRows {
    pub(crate) rows: Vec<Row>,
    pub(crate) activated: BTreeSet<(
        backend_engine::PackageKey,
        backend_semantic::vocabulary::LanguageProfile,
    )>,
    /// Typed record of which lane answered for every projected source file.
    /// Diagnostics and tests read it instead of inferring lanes from
    /// coordinates; the published view deliberately does not carry it.
    #[allow(dead_code)]
    pub(crate) ledger: ProjectionLedger,
}

/// Current paths per (project, profile) whose published image is stale.
pub(super) type ProfileStalePaths =
    BTreeMap<([u8; 32], backend_semantic::vocabulary::LanguageProfile), BTreeSet<String>>;
/// How a semantic publication for a package outside `sources` is treated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForeignPublication {
    /// A complete publication whose package frontier is absent is corrupt.
    Reject,
    /// Package-scoped publication leaves every other package's image closed.
    Skip,
}

pub(crate) fn rows_for_indexed_sources(
    initial: &ViewRoot,
    sources: &IndexedSources,
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    workspace: &std::path::Path,
    foreign: ForeignPublication,
) -> Result<ProjectedRows, BuiltinModelError> {
    if sources.projects.len() > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace package rows exceed the rebuild row bound".to_owned(),
        ));
    }
    let current_paths = profile_source_paths(sources)?;
    let current_identities = profile_source_identities(sources)?;
    let sites = StructuralSites::of(sources)?;
    let semantics = semantic_rows(
        snapshot,
        compiler,
        &sources.projects,
        &current_paths,
        &current_identities,
        &sites,
        initial,
        MAX_REBUILD_PACKAGES - sources.projects.len(),
        foreign,
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
    let structural_plan = StructuralProjectionPlan::of(sources, &semantics.complete)?;
    let mut projection = SourceRowProjection::new(
        initial,
        &sources.projects,
        total_capacity,
        &semantics.targets,
        &structural_plan,
        workspace,
    )?;
    for (file_key, record) in &sources.files {
        projection.append_file(
            *file_key,
            record,
            &semantics.complete,
            &semantics.stale_paths,
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

pub(super) struct SourceRowProjection<'a> {
    initial: &'a ViewRoot,
    projects: &'a BTreeMap<[u8; 32], IndexedProject>,
    rows: Vec<Row>,
    selected_files: BTreeSet<([u8; 32], [u8; 32])>,
    ledger: ProjectionLedger,
    targets: &'a SemanticTargets,
    structural_plan: &'a StructuralProjectionPlan,
}

impl<'a> SourceRowProjection<'a> {
    pub(super) fn new(
        initial: &'a ViewRoot,
        projects: &'a BTreeMap<[u8; 32], IndexedProject>,
        capacity: usize,
        targets: &'a SemanticTargets,
        structural_plan: &'a StructuralProjectionPlan,
        workspace: &'a std::path::Path,
    ) -> Result<Self, BuiltinModelError> {
        let mut rows = Vec::with_capacity(capacity);
        let mut selected_files = BTreeSet::new();
        for (project_key, project) in projects {
            let row = Row::new(
                RowId::Package(project.package),
                initial.basis(),
                &project.label,
            );
            rows.push(match package_manifest_name(&project.label, workspace) {
                Ok(name) => row.with_signature(name),
                Err(error) => {
                    if project.label.starts_with("pkg:")
                        || std::path::Path::new(&project.label)
                            .join("Cargo.toml")
                            .is_file()
                    {
                        return Err(BuiltinModelError(error));
                    }
                    row
                }
            });
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
            structural_plan,
        })
    }

    /// Takes the structural lane decisions recorded during projection.
    pub(super) fn take_ledger(&mut self) -> ProjectionLedger {
        std::mem::take(&mut self.ledger)
    }

    pub(super) fn append_file(
        &mut self,
        file_key: [u8; 32],
        record: &super::super::ProductSourceRecord,
        complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
        stale_paths: &ProfileStalePaths,
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
        let profile = super::super::ingest::source_profile(std::path::Path::new(path))
            .map_err(BuiltinModelError)?;
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
            self.ledger.record(path, FileLane::Semantic { freshness });
            return Ok(());
        }
        self.ledger.record(
            path,
            FileLane::Structural {
                cause: match profile {
                    None => StructuralCause::NoSemanticProfile,
                    Some(profile) => {
                        targets_unavailable_cause(self.targets, project.package, profile)
                    }
                },
            },
        );
        let package = project.package;
        if self.structural_plan.file(file_key).is_none() {
            return Err(BuiltinModelError(
                "structural source is missing its emitted declaration plan".to_owned(),
            ));
        }
        for (index, declaration) in declarations.iter().enumerate() {
            let prepared = self.structural_plan.declaration(file_key, index)?;
            let row =
                self.declaration_row(declaration, path, file_key, package, language, prepared)?;
            self.rows.push(row);
        }
        Ok(())
    }

    /// Builds one declaration's row, including the parent it hangs under.
    fn declaration_row(
        &self,
        declaration: &backend_compile::SourceDeclaration,
        path: &str,
        file_key: [u8; 32],
        package: backend_engine::PackageKey,
        language: backend_engine::SourceLanguage,
        prepared: &StructuralDeclaration,
    ) -> Result<Row, BuiltinModelError> {
        let coordinate = &prepared.coordinate;
        let symbol = prepared.id;
        let prose = if prepared.is_file_module {
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
        let row = Row::in_package(symbol, self.initial.basis(), package, coordinate.as_str())
            .with_document(vec![Fragment::Text(prose)])
            .with_signature(declaration.signature())
            .with_kind(declaration.kind())
            .with_source(declaration.location().clone())
            .with_excerpt(declaration.source_excerpt().clone());
        let row = match prepared.identity_preimage.clone() {
            Some(preimage) => row.with_identity_preimage(preimage),
            None => row,
        };
        Ok(match prepared.parent.as_deref() {
            Some(parent) => match self.structural_plan.parent_id(file_key, parent)? {
                StructuralParent::Symbol(RowId::Symbol(parent)) => row.with_parent(parent),
                StructuralParent::Symbol(RowId::Package(_))
                | StructuralParent::Symbol(RowId::Object(_))
                | StructuralParent::Package(_) => row,
            },
            None => row,
        })
    }

    pub(super) fn finish(mut self, semantic_rows: Vec<Row>) -> Result<Vec<Row>, BuiltinModelError> {
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
fn targets_unavailable_cause(
    targets: &SemanticTargets,
    package: backend_engine::PackageKey,
    profile: backend_semantic::vocabulary::LanguageProfile,
) -> StructuralCause {
    targets
        .unavailable
        .get(&(package.to_bytes(), profile))
        .map_or(StructuralCause::NoCompletePublication, |reason| {
            StructuralCause::PublicationUnavailable(*reason)
        })
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
pub(super) struct SemanticTargets {
    /// Targets whose selected publication is a terminal unavailable cause.
    pub(super) unavailable: BTreeMap<
        ([u8; 32], backend_semantic::vocabulary::LanguageProfile),
        SemanticUnavailableReason,
    >,
}

fn semantic_rows(
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    projects: &BTreeMap<[u8; 32], IndexedProject>,
    current_paths: &ProfileSourcePaths,
    current_identities: &ProfileSourceIdentities,
    sites: &StructuralSites<'_>,
    initial: &ViewRoot,
    row_capacity: usize,
    foreign: ForeignPublication,
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
            if !projects.contains_key(key.package_key().as_bytes())
                && matches!(foreign, ForeignPublication::Skip)
            {
                continue;
            }
            let target = (
                *key.package_key().as_bytes(),
                super::super::ingest::lane_profile(key.profile()),
            );
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
                // A stale image was compiled from other bytes than the
                // file's current structural declarations, so their lines
                // cannot be trusted for it.
                let site_declarations = if stale {
                    &[][..]
                } else {
                    sites.declarations(key.package_key().as_bytes(), &path)
                };
                let mut sink = SemanticRowSink {
                    initial,
                    symbols: &mut symbols,
                    rows: &mut rows,
                    capacity: row_capacity,
                    remaining_bytes: &mut remaining_bytes,
                    stale,
                    path: &path,
                    site_declarations,
                };
                append_image_rows(&view, project, key.profile(), &mut sink)?;
            }
            complete.insert((
                project.package.to_bytes(),
                super::super::ingest::lane_profile(key.profile()),
            ));
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
pub(super) struct FreshnessDecision {
    /// Each image's compiled path and its source content identity.
    compiled: CompiledSources,
    /// The persisted identities behind this decision, present exactly when
    /// the comparison was decisive.
    identities:
        Option<BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>>,
    /// Current paths of this profile whose content identity mismatches the
    /// image compiled from that path, or that no image compiled at all.
    pub(super) stale_paths: BTreeSet<String>,
    /// Whether every current file of the profile carries a persisted
    /// identity, making the content comparison decisive.
    pub(super) identity_decisive: bool,
    /// Whether the compiled path set already differs from the current scan.
    pub(super) path_sets_differ: bool,
}

impl FreshnessDecision {
    /// Returns whether the image compiled from `path` with `identity`
    /// predates the current file content.
    pub(super) fn image_stale(
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
pub(super) fn freshness_decision(
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
        let current = current_paths
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        compiled_paths != current
    };
    let mut stale_paths = BTreeSet::new();
    if identity_decisive {
        let identities = current_identities.expect("decisive above");
        for path in current_paths {
            let stale = compiled.get(path).is_none_or(|identity| {
                identities
                    .get(path)
                    .is_none_or(|current| current != identity)
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
    activated: &super::super::ActivatedProductSemantics,
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
pub(super) fn compiled_source(
    image: &SemanticImageView<'_>,
) -> Result<
    (
        String,
        backend_version::ContentId<backend_version::SourceFactDomain>,
    ),
    BuiltinModelError,
> {
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
pub(crate) fn compiled_source_path(image: &SemanticImageView<'_>) -> Result<String, BuiltinModelError> {
    compiled_source(image).map(|(path, _)| path)
}

pub(super) struct SemanticRowSink<'a> {
    pub(super) initial: &'a ViewRoot,
    pub(super) symbols: &'a mut BTreeSet<RowId>,
    pub(super) rows: &'a mut Vec<Row>,
    pub(super) capacity: usize,
    pub(super) remaining_bytes: &'a mut usize,
    /// Whether the image behind this sink predates the current scan. Stale
    /// rows stay semantic (never silently structural) and carry an explicit
    /// staleness note in their document.
    pub(super) stale: bool,
    /// The project-relative path this image was compiled from.
    pub(super) path: &'a str,
    /// The structural declarations extracted from the same current bytes of
    /// [`Self::path`]; empty when the image is stale.
    pub(super) site_declarations: &'a [&'a backend_compile::SourceDeclaration],
}

/// Structural declarations of every indexed file, keyed by owning project
/// and project-relative path.
///
/// A semantic image records each declaration's byte span but not its line,
/// and the view does not retain whole source files. The structural lane,
/// however, extracted every declaration of the same file bytes with its
/// exact line and bounded excerpt. Those sites let a semantic row carry the
/// source location and text a reader needs, instead of rendering every
/// compiler-backed row without a path, language, or source.
pub(crate) struct StructuralSites<'a> {
    files: BTreeMap<([u8; 32], String), Vec<&'a backend_compile::SourceDeclaration>>,
}

impl<'a> StructuralSites<'a> {
    fn of(sources: &'a IndexedSources) -> Result<Self, BuiltinModelError> {
        let mut files = BTreeMap::new();
        for (_, record) in &sources.files {
            let file = record
                .file_fields()
                .ok_or_else(|| BuiltinModelError("expected a source file record".to_owned()))?;
            files.insert(
                (file.project, file.path.to_owned()),
                file.declarations.iter().collect::<Vec<_>>(),
            );
        }
        Ok(Self { files })
    }

    fn declarations(
        &self,
        project: &[u8; 32],
        path: &str,
    ) -> &[&'a backend_compile::SourceDeclaration] {
        self.files
            .get(&(*project, path.to_owned()))
            .map_or(&[][..], Vec::as_slice)
    }
}
/// Pairs each semantic declaration with the structural declaration of the
/// same name and declaration family in the same file bytes.
///
/// Same-keyed declarations are paired by source order: the k-th semantic
/// entity by span start with the k-th structural declaration by line. A key
/// whose two extractions disagree on the count, or whose semantic spans are
/// not all captured, is ambiguous and keeps no site rather than a guess.
fn semantic_sites<'a, Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    session: &DocumentationSession<'_, Reader>,
    declarations: &[&'a backend_compile::SourceDeclaration],
) -> Result<BTreeMap<DeclarationIdentity, &'a backend_compile::SourceDeclaration>, BuiltinModelError>
{
    let mut sites = BTreeMap::new();
    if declarations.is_empty() {
        return Ok(sites);
    }
    let mut structural = BTreeMap::<(&str, u8), Vec<&'a backend_compile::SourceDeclaration>>::new();
    for declaration in declarations {
        structural
            .entry((declaration.name(), declaration_family(declaration.kind())))
            .or_default()
            .push(declaration);
    }
    for same_name in structural.values_mut() {
        same_name.sort_by_key(|declaration| declaration.line());
    }
    let mut semantic = BTreeMap::<(Vec<u8>, u8), Vec<(Option<u32>, DeclarationIdentity)>>::new();
    for entity in session.canonical_entities() {
        let entity = entity
            .map_err(|error| BuiltinModelError(format!("project semantic declaration: {error}")))?;
        let family = declaration_family(declaration_kind(entity.entity.kind));
        semantic
            .entry((entity.name.to_vec(), family))
            .or_default()
            .push((
                entity.entity.source.map(|span| span.start()),
                entity.entity.version.identity(),
            ));
    }
    for ((name, family), mut entities) in semantic {
        let Ok(name) = std::str::from_utf8(&name) else {
            continue;
        };
        let Some(candidates) = structural.get(&(name, family)) else {
            continue;
        };
        if candidates.len() != entities.len() || entities.iter().any(|(start, _)| start.is_none()) {
            continue;
        }
        entities.sort_by_key(|(start, _)| *start);
        for ((_, identity), declaration) in entities.into_iter().zip(candidates) {
            sites.insert(identity, *declaration);
        }
    }
    Ok(sites)
}

pub(super) struct SemanticRowContent {
    pub(super) document: Vec<Fragment>,
    pub(super) signature: Option<String>,
    encoded_bytes: usize,
}

pub(super) fn append_image_rows(
    image: &SemanticImageView<'_>,
    project: &IndexedProject,
    profile: backend_semantic::vocabulary::LanguageProfile,
    sink: &mut SemanticRowSink<'_>,
) -> Result<(), BuiltinModelError> {
    let session = DocumentationSession::new(image);
    let image_identity = *blake3::hash(image.as_ref()).as_bytes();
    let sites = semantic_sites(&session, sink.site_declarations)?;
    let canonical_identities = session
        .canonical_entities()
        .map(|entity| {
            entity
                .map(|entity| entity.entity.version.identity())
                .map_err(|error| {
                    BuiltinModelError(format!("project semantic declaration: {error}"))
                })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
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
        if let Some(site) = sites.get(&identity) {
            let excerpt_bytes = site.source_excerpt().text().map_or(0, str::len);
            *sink.remaining_bytes =
                sink.remaining_bytes
                    .checked_sub(excerpt_bytes)
                    .ok_or_else(|| {
                        BuiltinModelError(
                            "workspace semantic declarations exceed the rebuild byte bound"
                                .to_owned(),
                        )
                    })?;
            let location =
                backend_compile::SourceLocation::new(sink.path, site.line()).map_err(|error| {
                    BuiltinModelError(format!("semantic declaration source site: {error}"))
                })?;
            row = row
                .with_source(location)
                .with_excerpt(site.source_excerpt().clone());
        }
        // A reader's signature is the declaration as written. The semantic
        // type renders as canonical IR text (`function(parameters=[element(
        // kind=required,label=x"6C6576656C",type=builtin(u64))],...)`, the
        // parameter name hex-encoded), which stays in the document's code
        // fragment as the compiler's typed answer; the paired source site
        // supplies the written text when there is one.
        let written = sites
            .get(&identity)
            .map(|site| site.signature())
            .filter(|signature| !signature.trim().is_empty());
        if let Some(signature) = written.map(str::to_owned).or(content.signature) {
            row = row.with_signature(signature);
        }
        if let Some(parent) = entity.entity.parent {
            let parent = session
                .entity(parent)
                .map_err(|error| BuiltinModelError(format!("project semantic parent: {error}")))?;
            let parent_identity = parent.entity.version.identity();
            if !canonical_identities.contains(&parent_identity) {
                return Err(BuiltinModelError(
                    "project semantic parent is outside the canonical entity closure".to_owned(),
                ));
            }
            row = row.with_parent(semantic_symbol(project.package, parent_identity));
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
        .prepare_canonical(backend_semantic::ir::CanonicalTypeRenderLimits::new(
            type_depth,
        ))
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

pub(super) fn semantic_row_content<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
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
    let encoded_bytes = documentation_bytes
        .checked_add(signature.as_ref().map_or(0, String::len))
        .ok_or_else(|| BuiltinModelError("project semantic row byte count overflow".to_owned()))?;
    // The canonical semantic document is rendered (and so validated against
    // its type-depth and size bounds) but not shown. It is an injective
    // machine encoding that spells every name as hex (`name=x"626561636F6E
    // 5F656E747279"`) and every type as canonical IR; placing it first in
    // the document put that dump at the top of every compiler-backed page and
    // into every documentation fact. A reader gets the written signature and
    // the declaration's own documentation instead.
    let mut output = vec![0; prepared.encoded_len];
    prepared
        .write_into(&mut output)
        .map_err(|error| BuiltinModelError(format!("write project semantic document: {error}")))?;
    let mut document = Vec::with_capacity(documentation.len());
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
