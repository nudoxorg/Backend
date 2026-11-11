use super::super::{
    BuiltinModelError, BuiltinSemanticRelation, FileLane, IndexedProject, IndexedSources,
    MAX_REBUILD_BYTES, MAX_REBUILD_PACKAGES, ProjectionLedger, SemanticFreshness, StructuralCause,
    WorkspaceSnapshot,
};
use super::call_join::foreign_display_name;
#[cfg(test)]
use super::identity::semantic_coordinate;
use super::identity::{
    declaration_family, declaration_kind, external_semantic_coordinate, external_semantic_symbol,
    semantic_identity, semantic_symbol,
};
use super::image_rows::{Charge, DuplicatePolicy, ProjectedImage, ProjectedRow};
use super::kinds;
use super::structural::{
    ProfileSourceIdentities, ProfileSourcePaths, StructuralDeclaration, StructuralParent,
    StructuralProjectionPlan, native_frontier_freshness, profile_source_identities,
    profile_source_paths, projected_source_capacity,
};
use super::{
    MAX_SEMANTIC_DOCUMENT_BYTES, MAX_SEMANTIC_SIGNATURE_BYTES, MAX_SEMANTIC_TYPE_DEPTH, STALE_NOTE,
    SourceAvailability, semantic_profile_is_complete,
};
use backend_engine::application::{
    DocumentationFragment, DocumentationSession, LocalCompilerClient,
};
use backend_engine::builtin::{
    ProductSemanticCaptureOutcome, ProductSemanticPublicationRecord, SemanticUnavailableReason,
    semantic_capture_relation,
};
use backend_engine::{Fragment, Row, RowId, ViewRoot, product_source_file_key};
use backend_semantic::ir::{
    DeclarationIdentity, ExternalId, ExternalTargetIdentity, LinkTarget, SemanticCoreReader as _,
    SemanticImageView, SemanticReader as _,
};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::sync::OnceLock;

static C_FRESHNESS_DIAGNOSTICS_ENABLED: OnceLock<bool> = OnceLock::new();

fn c_freshness_diagnostics_enabled() -> bool {
    *C_FRESHNESS_DIAGNOSTICS_ENABLED.get_or_init(|| {
        std::env::var_os("NUDOX_DEBUG_C_FRESHNESS").is_some_and(|value| value == "1")
    })
}

fn package_row_name(label: &str, workspace: &std::path::Path) -> Result<String, String> {
    super::super::local_manifest::indexed_package_row_name(label, workspace)
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
    /// Rejects an unsourced publication unless an authenticated capture
    /// retains it as historical scope after source-only package removal.
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
    residence: &mut super::image_rows::ImageRowResidence,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
) -> Result<ProjectedRows, BuiltinModelError> {
    if sources.projects.len() > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace package rows exceed the rebuild row bound".to_owned(),
        ));
    }
    let available = SourceAvailability::of(sources)?;
    let current_paths = profile_source_paths(sources)?;
    let mut current_identities = profile_source_identities(sources)?;
    let native_frontier = native_frontier_freshness(sources, &current_paths);
    for (target, identities) in native_frontier.identities {
        current_identities
            .entry(target)
            .or_default()
            .extend(identities);
    }
    let sites = StructuralSites::of(sources)?;
    let semantics = semantic_rows(
        snapshot,
        compiler,
        sources,
        &sources.projects,
        &current_paths,
        &current_identities,
        &native_frontier.complete,
        &sites,
        &available,
        initial,
        MAX_REBUILD_PACKAGES - sources.projects.len(),
        foreign,
        residence,
        generations,
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
    let mut structural_plan = StructuralProjectionPlan::of(sources, &semantics.complete)?;
    structural_plan.unify_exact_sites(sources, &semantics.exact_sites)?;
    let mut projection = SourceRowProjection::new(
        initial,
        sources,
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
    let mut rows = projection.finish(semantics.rows)?;
    super::super::coverage::qualify_package_rows(snapshot, &mut rows, generations)?;
    Ok(ProjectedRows {
        rows,
        activated: semantics.activated,
        ledger,
    })
}

/// Finds the exact selected native declaration owner for raw byte paging.
pub(in crate::builtin) fn admitted_native_source_extent(
    sources: &IndexedSources,
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    package: backend_engine::PackageKey,
    selected: RowId,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
) -> Result<
    Option<(
        String,
        backend_version::ContentId<backend_version::SourceFactDomain>,
        (u32, u32),
    )>,
    BuiltinModelError,
> {
    Ok(admitted_native_source(
        sources,
        snapshot,
        compiler,
        package,
        selected,
        generations,
        true,
    )?
    .and_then(|(path, identity, extent)| extent.map(|extent| (path, identity, extent))))
}

/// Admits the selected native declaration's file for freshness, without
/// requiring that the producer supplied a nonempty pageable owner extent.
pub(in crate::builtin) fn admitted_native_source_file(
    sources: &IndexedSources,
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    package: backend_engine::PackageKey,
    selected: RowId,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
) -> Result<
    Option<(
        String,
        backend_version::ContentId<backend_version::SourceFactDomain>,
    )>,
    BuiltinModelError,
> {
    Ok(admitted_native_source(
        sources,
        snapshot,
        compiler,
        package,
        selected,
        generations,
        false,
    )?
    .map(|(path, identity, _)| (path, identity)))
}

/// Verifies one current publication and native identity; callers choose
/// whether a captured, nonempty owner span is additionally required.
fn admitted_native_source(
    sources: &IndexedSources,
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    package: backend_engine::PackageKey,
    selected: RowId,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    require_extent: bool,
) -> Result<
    Option<(
        String,
        backend_version::ContentId<backend_version::SourceFactDomain>,
        Option<(u32, u32)>,
    )>,
    BuiltinModelError,
> {
    let Some(project) = sources.projects.get(&package.to_bytes()) else {
        return Ok(None);
    };
    let reference = backend_engine::PackageReference::parse(project.label.clone())
        .map_err(|error| BuiltinModelError(format!("native source package: {error}")))?;
    if backend_engine::package_key(reference.as_str()) != package {
        return Ok(None);
    }
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open native source publication: {error}")))?;
    let captures = semantic_capture_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open native source capture: {error}")))?;
    let Some(captures) = captures else {
        return Ok(None);
    };
    let start =
        backend_engine::ProductSemanticPublicationKey::package_lower_bound(reference.clone());
    let mut after = None;
    let mut first = true;
    let mut found = None;
    let mut publication_rows = 0usize;
    let mut remaining_bytes = MAX_REBUILD_BYTES;
    loop {
        let page = if first {
            first = false;
            relation.page_from(&start, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        } else {
            relation.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        }
        .map_err(|error| BuiltinModelError(format!("read native source publication: {error}")))?;
        for (key, record) in page.entries() {
            publication_rows = publication_rows.checked_add(1).ok_or_else(|| {
                BuiltinModelError("native source publication limit exceeded".to_owned())
            })?;
            if publication_rows > MAX_REBUILD_PACKAGES {
                return Err(BuiltinModelError(
                    "native source publication limit exceeded".to_owned(),
                ));
            }
            if key.package() != &reference {
                return Ok(found);
            }
            if !key.is_selected()
                || (require_extent
                    && !matches!(
                        key.profile(),
                        backend_semantic::vocabulary::LanguageProfile::TypeScript(_)
                            | backend_semantic::vocabulary::LanguageProfile::Go(_)
                            | backend_semantic::vocabulary::LanguageProfile::Rust(_)
                            // Roslyn records each declaration's full extent in the image it
                            // publishes. The same gates apply: a selected, published
                            // generation matching the current capture, the image's compiled
                            // source identity equal to the indexed file's, and bytes
                            // verified against that identity when they are read.
                            | backend_semantic::vocabulary::LanguageProfile::CSharp(_)
                    ))
            {
                continue;
            }
            let language = key.profile().language();
            let ProductSemanticPublicationRecord::Published { coverage, claim } = record else {
                continue;
            };
            let Some(capture) = captures.lookup(key).map_err(|error| {
                BuiltinModelError(format!("read native source capture: {error}"))
            })?
            else {
                continue;
            };
            if !matches!(capture.outcome(), ProductSemanticCaptureOutcome::Published { coverage: current_coverage, claim: current_claim }
                if current_coverage == *coverage && current_claim == *claim)
            {
                continue;
            }
            let activated =
                super::super::load_semantic_publication(compiler, key, *claim, generations)?;
            for image in activated.images() {
                remaining_bytes = remaining_bytes
                    .checked_sub(image.as_ref().len())
                    .ok_or_else(|| {
                        BuiltinModelError("native source image budget exceeded".to_owned())
                    })?;
            }
            if !native_program_is_current(sources, package, &activated)? {
                continue;
            }
            for image in activated.images() {
                let image = image.reopen().map_err(|error| {
                    BuiltinModelError(format!("reopen native source image: {error}"))
                })?;
                key.admit_image(&image).map_err(|error| {
                    BuiltinModelError(format!("admit native source image: {error}"))
                })?;
                let backend_semantic::ir::ImageProvenance::Captured { recipe, .. } =
                    image.image_facts().provenance
                else {
                    continue;
                };
                if recipe.tool != language.native_tool() {
                    continue;
                }
                let (image_path, image_source) = compiled_source(&image)?;
                if let Some(extent) =
                    native_file_in_image(&image, package, &image_path, selected, require_extent)?
                {
                    // The selected native entity supplies the path, never a
                    // syntax-projected row location. Admit it against the
                    // current package frontier before using its bytes.
                    if native_frontier_identity(
                        sources,
                        package,
                        &image_path,
                        Some(language),
                        Some(key.profile()),
                    )? != Some(image_source)
                    {
                        return Ok(None);
                    }
                    if found.replace((image_path, image_source, extent)).is_some() {
                        return Ok(None);
                    }
                }
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    Ok(found)
}

/// File evidence admitted from one immutable selected snapshot. Holding this
/// value does not verify current disk bytes or provide declaration inventory.
pub(in crate::builtin) struct AdmittedNativeEvidence<'a> {
    claim: backend_engine::builtin::NativeSourceEvidence,
    image_source: backend_semantic::ir::SourceIdentity,
    primary_extent: Option<(u32, u32)>,
    header_extent: Option<(u32, u32)>,
    selected_row: RowId,
    entity_id: backend_semantic::ir::EntityId,
    declaration_identity: DeclarationIdentity,
    publication_key: backend_engine::builtin::ProductSemanticPublicationKey,
    publication_claim: backend_engine::builtin::SemanticPublicationClaim,
    image_authority: backend_library::interface::SemanticImageAuthority,
    program_identity: [u8; 32],
    source_basis: super::super::native_ts_admission::NativeTsSourceBasis,
    _snapshot: &'a WorkspaceSnapshot,
}
impl AdmittedNativeEvidence<'_> {
    pub(in crate::builtin) fn selected_row(&self) -> RowId {
        self.selected_row
    }
    pub(in crate::builtin) fn entity_id(&self) -> backend_semantic::ir::EntityId {
        self.entity_id
    }
    pub(in crate::builtin) fn declaration_identity(&self) -> DeclarationIdentity {
        self.declaration_identity
    }
    pub(in crate::builtin) fn publication_key(
        &self,
    ) -> &backend_engine::builtin::ProductSemanticPublicationKey {
        &self.publication_key
    }
    pub(in crate::builtin) fn publication_claim(
        &self,
    ) -> backend_engine::builtin::SemanticPublicationClaim {
        self.publication_claim
    }
    pub(in crate::builtin) fn image_authority(
        &self,
    ) -> backend_library::interface::SemanticImageAuthority {
        self.image_authority
    }
    pub(in crate::builtin) fn program_identity(&self) -> [u8; 32] {
        self.program_identity
    }
    pub(in crate::builtin) fn snapshot(&self) -> &WorkspaceSnapshot {
        self._snapshot
    }
    pub(in crate::builtin) fn source_basis(&self) -> super::super::native_ts_admission::NativeTsSourceBasis {
        self.source_basis
    }
    pub(in crate::builtin) fn claim(&self) -> &backend_engine::builtin::NativeSourceEvidence {
        &self.claim
    }
    pub(in crate::builtin) fn image_source(&self) -> backend_semantic::ir::SourceIdentity {
        self.image_source
    }
    pub(in crate::builtin) fn primary_extent(&self) -> Option<(u32, u32)> {
        self.primary_extent
    }
    pub(in crate::builtin) fn header_extent(&self) -> Option<(u32, u32)> {
        self.header_extent
    }
}

/// A bounded routing hint only. Selected TypeScript publication presence does
/// not itself grant source authority or prove current capture state.
pub(in crate::builtin) fn has_selected_typescript_publication(
    snapshot: &WorkspaceSnapshot,
    project_label: &str,
) -> Result<bool, BuiltinModelError> {
    use backend_engine::builtin::ProductSemanticPublicationKey;
    use backend_semantic::vocabulary::LanguageProfile;
    let reference = backend_engine::PackageReference::parse(project_label.to_owned())
        .map_err(|error| BuiltinModelError(format!("native routing package: {error}")))?;
    let publications = snapshot.relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open native routing publication: {error}")))?;
    let start = ProductSemanticPublicationKey::package_lower_bound(reference.clone());
    let mut after = None;
    let mut first = true;
    let mut count = 0usize;
    loop {
        let page = if first {
            first = false;
            publications.page_from(&start, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        } else {
            publications.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        }.map_err(|error| BuiltinModelError(format!("read native routing publication: {error}")))?;
        for (key, _) in page.entries() {
            count = count.checked_add(1).ok_or_else(|| BuiltinModelError("native routing limit".into()))?;
            if count > MAX_REBUILD_PACKAGES { return Err(BuiltinModelError("native routing limit".into())); }
            if key.package() != &reference { return Ok(false); }
            if key.is_selected() && matches!(key.profile(), LanguageProfile::TypeScript(_)) { return Ok(true); }
        }
        let Some(next) = page.next().cloned() else { return Ok(false) };
        after = Some(next);
    }
}

/// Admits selected native-only TS file evidence; the caller must first bind the
/// view to this snapshot and admit the local canonical project root. Registry
/// staging roots and absent Program/image ownership are deliberately refused.
pub(in crate::builtin) fn admitted_native_evidence<'a>(
    sources: &IndexedSources,
    snapshot: &'a WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    package: backend_engine::PackageKey,
    selected: RowId,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
) -> Result<Option<AdmittedNativeEvidence<'a>>, BuiltinModelError> {
    use backend_engine::builtin::{
        ProductSemanticCaptureOutcome, ProductSemanticPublicationRecord,
        native_source_evidence_relation, product_native_frontier_relation,
    };
    use backend_semantic::vocabulary::LanguageProfile;
    let Some(project) = sources.projects.get(&package.to_bytes()) else {
        return Ok(None);
    };
    if project.label.starts_with("pkg:")
        || backend_engine::PackageKey::from_value(&project.label) != package
    {
        return Ok(None);
    }
    super::super::profile::native_evidence_basis_for_snapshot(snapshot)?;
    let Some(evidence) = native_source_evidence_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open selected native evidence: {error}")))?
    else {
        return Ok(None);
    };
    let frontiers = product_native_frontier_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open native frontier: {error}")))?;
    let reference = backend_engine::PackageReference::parse(project.label.clone())
        .map_err(|error| BuiltinModelError(format!("native evidence package: {error}")))?;
    let publications = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open native publication: {error}")))?;
    let Some(captures) = semantic_capture_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open native capture: {error}")))?
    else {
        return Ok(None);
    };
    let structural_files = selected_typescript_structural_files(sources, package)?;
    let start =
        backend_engine::ProductSemanticPublicationKey::package_lower_bound(reference.clone());
    let mut after = None;
    let mut first = true;
    let mut found = None;
    let mut count = 0usize;
    let mut image_budget = MAX_REBUILD_BYTES;
    let mut program_path_work = 0usize;
    loop {
        let page = if first {
            first = false;
            publications.page_from(&start, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        } else {
            publications.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        }
        .map_err(|error| BuiltinModelError(format!("read native publication: {error}")))?;
        for (key, record) in page.entries() {
            count = count
                .checked_add(1)
                .ok_or_else(|| BuiltinModelError("native publication limit".into()))?;
            if count > MAX_REBUILD_PACKAGES {
                return Err(BuiltinModelError("native publication limit".into()));
            }
            if key.package() != &reference {
                return Ok(found);
            }
            if !key.is_selected() || !matches!(key.profile(), LanguageProfile::TypeScript(_)) {
                continue;
            }
            let ProductSemanticPublicationRecord::Published { coverage, claim } = record else {
                continue;
            };
            let Some(capture) = captures
                .lookup(key)
                .map_err(|error| BuiltinModelError(format!("read native capture: {error}")))?
            else {
                continue;
            };
            if !matches!(capture.outcome(), ProductSemanticCaptureOutcome::Published { coverage: actual_coverage, claim: actual_claim } if actual_coverage == *coverage && actual_claim == *claim)
            {
                continue;
            }
            let activated =
                super::super::load_semantic_publication(compiler, key, *claim, generations)?;
            let Some(positive) = selected_nse_program_files(
                snapshot,
                sources,
                key,
                &activated,
                generations,
                &evidence,
                frontiers.as_ref(),
                &structural_files,
                &mut program_path_work,
                &mut image_budget,
            )?
            else {
                continue;
            };
            for image in activated.images() {
                let view = image
                    .reopen()
                    .map_err(|error| BuiltinModelError(format!("reopen native image: {error}")))?;
                let backend_semantic::ir::ImageProvenance::Captured { source, scope, .. } =
                    view.image_facts().provenance
                else {
                    return Ok(None);
                };
                let Some(path) = view
                    .atom(scope.path)
                    .and_then(|value| std::str::from_utf8(value).ok())
                else {
                    return Ok(None);
                };
                let Some((row, expected_source)) = positive.positive.get(path) else {
                    continue;
                };
                if source != *expected_source {
                    return Ok(None);
                }
                let Some(owned) = native_owned_file_in_image(&view, package, path, selected)?
                else {
                    continue;
                };
                let image_identity = *backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(image.as_ref()).as_ref();
                let header_extent = row.headers.as_ref().and_then(|headers| {
                    if headers.image_identity != image_identity
                        || headers.image_byte_len != image.as_ref().len() as u64
                        || headers.producer_recipe != super::super::native_ts_headers::producer_recipe()
                    { return None; }
                    let entry = headers.entries.binary_search_by_key(&owned.entity_id.raw, |entry| entry.entity_id)
                        .ok().and_then(|index| headers.entries.get(index))?;
                    (entry.identity == owned.declaration_identity && Some(entry.compiler_extent) == owned.extent)
                        .then_some(entry.header_extent)
                });
                if found
                    .replace(AdmittedNativeEvidence {
                        claim: row.clone(),
                        image_source: source,
                        primary_extent: owned.extent,
                        header_extent,
                        selected_row: selected,
                        entity_id: owned.entity_id,
                        declaration_identity: owned.declaration_identity,
                        publication_key: key.clone(),
                        publication_claim: *claim,
                        image_authority: image.authority,
                        program_identity: positive.program.membership_identity(),
                        source_basis: positive.source_basis(),
                        _snapshot: snapshot,
                    })
                    .is_some()
                {
                    return Ok(None);
                }
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    Ok(found)
}

/// Bounded selected-root scan used only to decide whether a TS project has
/// native evidence. It does not claim a complete source inventory.
pub(in crate::builtin) fn selected_nse_typescript_owners(
    relation: &backend_engine::workspace::WorkspaceRelationHandle<backend_engine::builtin::NativeSourceEvidenceRelation>,
) -> Result<BTreeSet<[u8; 32]>, BuiltinModelError> {
    let mut owners = BTreeSet::new();
    let mut projects = BTreeMap::<[u8; 32], usize>::new();
    let mut count = 0usize;
    let mut bytes = 0usize;
    let mut previous = None;
    let mut after = None;
    loop {
        let page = relation.page(after.as_ref(), 1)
            .map_err(|error| BuiltinModelError(format!("scan native evidence: {error}")))?;
        for (key, row) in page.entries() {
            count = count.checked_add(1).ok_or_else(|| BuiltinModelError("native evidence row overflow".into()))?;
            if count > backend_engine::builtin::MAX_NATIVE_EVIDENCE_ROWS
                || previous.is_some_and(|prior| prior >= *key) || row.key().ok() != Some(*key) {
                return Err(BuiltinModelError("invalid selected native evidence key or row budget".into()));
            }
            previous = Some(*key);
            bytes = bytes.checked_add(40).and_then(|work| work.checked_add(row.checked_encoded_len()?))
                .ok_or_else(|| BuiltinModelError("native evidence scan overflow".into()))?;
            if bytes > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES {
                return Err(BuiltinModelError("native evidence byte budget exceeded".into()));
            }
            let project_count = projects.entry(row.project).or_default();
            *project_count = project_count.checked_add(1).ok_or_else(|| BuiltinModelError("native evidence project count overflow".into()))?;
            if *project_count > backend_engine::builtin::MAX_NATIVE_EVIDENCE_PROJECT_ROWS {
                return Err(BuiltinModelError("native evidence project budget exceeded".into()));
            }
            if matches!(row.profile, backend_semantic::vocabulary::LanguageProfile::TypeScript(_)) {
                owners.insert(row.project);
            }
        }
        match page.next().copied() { Some(next) => after = Some(next), None => break }
    }
    Ok(owners)
}

/// Index exact admitted structural file rows once, rather than rescanning the
/// entire project for each native Program source.
/// Process-local evidence tied to the exact selected activation. It is never
/// serialized and cannot establish discovery or structural completeness.
pub(in crate::builtin) struct NseProgramProof {
    workspace_root: backend_version::WorkspaceRoot,
    sequence: u64,
    commit: [u8; 32],
    owner_epoch: u64,
    closure: backend_store::ClosureId,
    selected: backend_engine::builtin::ProductSemanticPublicationKey,
    claim: backend_engine::builtin::SemanticPublicationClaim,
    coverage: backend_engine::builtin::SemanticPublicationCoverage,
    images: std::sync::Arc<[backend_library::interface::SemanticImageSnapshot]>,
    program: std::sync::Arc<backend_semantic::ir::NativeProgramSources>,
    projection_safe: bool,
    source_basis: super::super::native_ts_admission::NativeTsSourceBasis,
    positive: BTreeMap<
        String,
        (
            backend_engine::builtin::NativeSourceEvidence,
            backend_semantic::ir::SourceIdentity,
        ),
    >,
}
impl NseProgramProof {
    pub(super) fn projection_safe(&self) -> bool {
        self.projection_safe
    }
    pub(in crate::builtin) fn source_basis(&self) -> super::super::native_ts_admission::NativeTsSourceBasis {
        self.source_basis
    }
    pub(in crate::builtin) fn admits(
        &self,
        snapshot: &WorkspaceSnapshot,
        key: &backend_engine::builtin::ProductSemanticPublicationKey,
        claim: backend_engine::builtin::SemanticPublicationClaim,
        coverage: backend_engine::builtin::SemanticPublicationCoverage,
        activated: &super::super::ActivatedProductSemantics,
    ) -> bool {
        self.projection_safe
            && self.workspace_root == snapshot.root()
            && self.sequence == snapshot.sequence()
            && self.commit == *snapshot.commit().id().as_bytes()
            && self.owner_epoch == snapshot.owner_epoch()
            && self.closure == snapshot.closure().binding().closure()
            && self.selected == *key
            && self.claim == claim
            && self.coverage == coverage
            && std::sync::Arc::ptr_eq(&self.images, activated.image_set())
            && activated
                .native_program_sources
                .as_ref()
                .is_some_and(|program| std::sync::Arc::ptr_eq(&self.program, program))
    }
    pub(in crate::builtin) fn claim(&self) -> backend_engine::builtin::SemanticPublicationClaim {
        self.claim
    }
    pub(in crate::builtin) fn coverage(
        &self,
    ) -> backend_engine::builtin::SemanticPublicationCoverage {
        self.coverage
    }
    pub(in crate::builtin) fn contains(
        &self,
        path: &str,
        identity: backend_version::ContentId<backend_version::SourceFactDomain>,
    ) -> bool {
        self.positive
            .get(path)
            .is_some_and(|(_, source)| source.identity == identity)
    }
}

/// Strictly validates the selected Program and every admitted image, then
/// returns only positive per-file NSE claims. No inventory completeness follows.
pub(super) fn selected_nse_program_files(
    snapshot: &WorkspaceSnapshot,
    sources: &IndexedSources,
    key: &backend_engine::builtin::ProductSemanticPublicationKey,
    activated: &super::super::ActivatedProductSemantics,
    generations: &super::super::generation_residence::SemanticGenerationResidence,
    evidence: &backend_engine::workspace::WorkspaceRelationHandle<
        backend_engine::builtin::NativeSourceEvidenceRelation,
    >,
    frontiers: Option<
        &backend_engine::workspace::WorkspaceRelationHandle<
            backend_engine::builtin::ProductNativeFrontierRelation,
        >,
    >,
    structural_files: &BTreeMap<
        String,
        (
            backend_version::ContentId<backend_version::SourceFactDomain>,
            [u8; 32],
        ),
    >,
    program_path_work: &mut usize,
    image_budget: &mut usize,
) -> Result<Option<NseProgramProof>, BuiltinModelError> {
    use backend_engine::builtin::{
        NativePathDisposition, ProductNativeFrontierRecord, ProductSemanticCaptureOutcome,
        ProductSemanticPublicationRecord, admit_native_frontier,
        product_native_frontier_profile_key,
    };
    use backend_semantic::vocabulary::LanguageProfile;
    if !key.is_selected() || !matches!(key.profile(), LanguageProfile::TypeScript(_)) {
        return Ok(None);
    }
    let package = key.package_key();
    let Some(project) = sources.projects.get(&package.to_bytes()) else {
        return Ok(None);
    };
    if project.label.starts_with("pkg:")
        || backend_engine::PackageKey::from_value(&project.label) != package
    {
        return Ok(None);
    }
    let publications = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open native publication: {error}")))?;
    let Some(ProductSemanticPublicationRecord::Published { coverage, claim }) = publications
        .lookup(key)
        .map_err(|error| BuiltinModelError(format!("read native publication: {error}")))?
    else {
        return Ok(None);
    };
    let Some(captures) = semantic_capture_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open native capture: {error}")))?
    else {
        return Ok(None);
    };
    let Some(capture) = captures
        .lookup(key)
        .map_err(|error| BuiltinModelError(format!("read native capture: {error}")))?
    else {
        return Ok(None);
    };
    if !matches!(capture.outcome(), ProductSemanticCaptureOutcome::Published { coverage: current_coverage, claim: current_claim } if current_coverage == coverage && current_claim == claim)
    {
        return Ok(None);
    }
    let source = snapshot.relation::<super::super::BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open native source project: {error}")))?;
    let Some(current_project) = source.lookup(&package.to_bytes())
        .map_err(|error| BuiltinModelError(format!("read native source project: {error}")))? else { return Ok(None) };
    let Some(current_fields) = current_project.project_fields() else { return Ok(None) };
    if current_fields.label != project.label
        || &current_fields.source_version != capture.capture().source_version()
        || generations.selected_input_digest(key, claim)? != Some(*capture.capture().input_digest())
    {
        return Ok(None);
    }
    let Some(program) = activated.native_program_sources.as_ref() else {
        return Ok(None);
    };
    let mut mappings = BTreeMap::new();
    let mut mapping_inputs = BTreeMap::new();
    let mut profiles = BTreeMap::<LanguageProfile, BTreeSet<String>>::new();
    let mut rejected = false;
    for row in program.sources() {
        let Some(path) = row.package_path else {
            continue;
        };
        let Some(profile) = super::super::ingest::admitted_file_profile(
            backend_compile::SourceLanguage::TypeScript,
            path,
        ) else {
            rejected = true;
            break;
        };
        *program_path_work = program_path_work
            .checked_add(path.len())
            .and_then(|work| work.checked_add(128))
            .ok_or_else(|| BuiltinModelError("native program source work overflow".into()))?;
        if mappings.len() >= backend_library::MAX_SELECTED_PROJECT_FRONTIER_FILES
            || *program_path_work > MAX_REBUILD_BYTES
        {
            return Err(BuiltinModelError(
                "native program source budget exceeded".into(),
            ));
        }
        if mappings.insert(path, row.source).is_some() {
            rejected = true;
            break;
        }
        profiles.entry(profile).or_default().insert(path.to_owned());
        let structural_input = structural_files
            .get(path)
            .and_then(|(identity, input)| (*identity == row.source.identity).then_some(*input));
        let evidence_row = lookup_native_evidence(evidence, package, profile, path, row.source, image_budget)?;
        let evidence_input = evidence_row.as_ref().map(|value| value.input_digest);
        let input = match (structural_input, evidence_input) {
            (Some(a), Some(b)) if a == b => a,
            (Some(a), None) => a,
            (None, Some(b)) => b,
            _ => {
                rejected = true;
                break;
            }
        };
        mapping_inputs.insert(path, input);
    }
    if rejected {
        return Ok(None);
    }
    if let Some(frontiers) = frontiers {
        for (profile, paths) in &profiles {
            let record = frontiers
                .lookup(&product_native_frontier_profile_key(
                    package.to_bytes(),
                    *profile,
                ))
                .map_err(|error| {
                    BuiltinModelError(format!("read native frontier profile: {error}"))
                })?;
            let Some(record) = record else {
                // An orphan path (including a refusal) is never promoted
                // merely because its profile manifest is absent.
                for path in paths {
                    let path_key = backend_engine::builtin::product_native_frontier_path_key(
                        package.to_bytes(),
                        *profile,
                        path,
                    )
                    .map_err(BuiltinModelError)?;
                    if frontiers
                        .lookup(&path_key)
                        .map_err(|error| {
                            BuiltinModelError(format!("read native frontier path: {error}"))
                        })?
                        .is_some()
                    {
                        rejected = true;
                        break;
                    }
                }
                if rejected {
                    break;
                }
                continue;
            };
            let ProductNativeFrontierRecord::Profile {
                package_target: Some(target),
                ..
            } = record
            else {
                rejected = true;
                break;
            };
            if &target != key.coordinate() {
                rejected = true;
                break;
            }
            // TypeScript profiles have no malformed-native candidate
            // lane. Refuse those rows before the frontier verifier can
            // reconstruct their potentially large retained chunks.
            for path in paths {
                let path_key = backend_engine::builtin::product_native_frontier_path_key(
                    package.to_bytes(),
                    *profile,
                    path,
                )
                .map_err(BuiltinModelError)?;
                let path_row = frontiers.lookup(&path_key).map_err(|error| {
                    BuiltinModelError(format!("read native frontier path: {error}"))
                })?;
                let Some(source) = mappings.get(path.as_str()) else {
                    rejected = true;
                    break;
                };
                let Some(input) = mapping_inputs.get(path.as_str()) else {
                    rejected = true;
                    break;
                };
                if !matches!(path_row,
                            Some(ProductNativeFrontierRecord::Path {
                                project, profile: row_profile, path: ref row_path,
                                disposition: NativePathDisposition::Structural { content, source_identity },
                            }) if project == package.to_bytes() && row_profile == *profile && row_path == path
                                && content == *input && source_identity == source.identity)
                {
                    rejected = true;
                    break;
                }
            }
            if rejected {
                break;
            }
            if admit_native_frontier(frontiers, package.to_bytes(), *profile, paths).is_err() {
                rejected = true;
                break;
            }
            if rejected {
                break;
            }
        }
    }
    if rejected {
        return Ok(None);
    }
    let mut positive = BTreeMap::new();
    let mut projection_safe = true;
    for image in activated.images() {
        *image_budget = image_budget
            .checked_sub(image.as_ref().len())
            .ok_or_else(|| BuiltinModelError("native image budget".into()))?;
        let view = image
            .reopen()
            .map_err(|error| BuiltinModelError(format!("reopen native image: {error}")))?;
        key.admit_image(&view)
            .map_err(|error| BuiltinModelError(format!("admit native image: {error}")))?;
        let backend_semantic::ir::ImageProvenance::Captured {
            source,
            recipe,
            scope,
            ..
        } = view.image_facts().provenance
        else {
            return Ok(None);
        };
        let Some(path) = view
            .atom(scope.path)
            .and_then(|value| std::str::from_utf8(value).ok())
        else {
            return Ok(None);
        };
        if recipe.tool != key.profile().language().native_tool()
            || recipe.toolchain != program.toolchain()
            || mappings.get(path) != Some(&source)
            || !program.image_matches(
                path,
                backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(image.as_ref())
                    .as_ref(),
            )
        {
            return Ok(None);
        }
        let Some(profile) = super::super::ingest::admitted_file_profile(
            backend_compile::SourceLanguage::TypeScript,
            path,
        ) else {
            return Ok(None);
        };
        if recipe.profile != profile {
            return Ok(None);
        }
        if let Some(row) = lookup_native_evidence(evidence, package, profile, path, source, image_budget)?
            && row.admits_image()
        {
            // Image-level projection must not make legacy Unavailable entities
            // current merely because one file claim matches. Entity-specific
            // source lookup can independently accept a PrimaryOwned entity.
            for entity in DocumentationSession::new(&view).publication_entities() {
                let entity = entity.map_err(|error| {
                    BuiltinModelError(format!("read native ownership: {error}"))
                })?;
                if entity.entity.authority.publication
                    != backend_semantic::ir::DeclarationPublicationRole::PrimaryOwned
                {
                    projection_safe = false;
                }
            }
            match positive.entry(path.to_owned()) {
                std::collections::btree_map::Entry::Vacant(entry) => { entry.insert((row, source)); }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    if entry.get() != &(row, source) { return Ok(None); }
                }
            }
        }
    }
    if positive.is_empty() {
        return Ok(None);
    }
    Ok(Some(NseProgramProof {
        workspace_root: snapshot.root(),
        sequence: snapshot.sequence(),
        commit: *snapshot.commit().id().as_bytes(),
        owner_epoch: snapshot.owner_epoch(),
        closure: snapshot.closure().binding().closure(),
        selected: key.clone(),
        claim,
        coverage,
        images: std::sync::Arc::clone(activated.image_set()),
        program: std::sync::Arc::clone(program),
        projection_safe,
        source_basis: super::super::native_ts_admission::NativeTsSourceBasis::CapturedObserved {
            policy_stability: super::super::native_ts_admission::NativePolicyStability::Unknown,
        },
        positive,
    }))
}

/// Closed NSE applicability result. Unavailable must never trigger a legacy
/// fallback; structural authority is considered separately.
pub(in crate::builtin) enum NseProgramAdmission {
    /// This language does not use the NSE TypeScript route.
    NotApplicable,
    /// Required selected evidence could not be admitted.
    Unavailable,
    /// Exact selected activation and its process-local, snapshot-bound proof.
    Admitted {
        activation: super::super::ActivatedProductSemantics,
        proof: NseProgramProof,
    },
}

/// Loads the exact selected publication and builds a process-local NSE program
/// proof. A raw caller-provided activation cannot construct this capability.
pub(in crate::builtin) fn load_selected_nse_program(
    snapshot: &WorkspaceSnapshot,
    sources: &IndexedSources,
    compiler: &LocalCompilerClient,
    key: &backend_engine::builtin::ProductSemanticPublicationKey,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
) -> Result<NseProgramAdmission, BuiltinModelError> {
    if !matches!(
        key.profile(),
        backend_semantic::vocabulary::LanguageProfile::TypeScript(_)
    ) {
        return Ok(NseProgramAdmission::NotApplicable);
    }
    if !key.is_selected() {
        return Ok(NseProgramAdmission::Unavailable);
    }
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open selected native publication: {error}")))?;
    let Some(ProductSemanticPublicationRecord::Published { coverage, claim }) = relation
        .lookup(key)
        .map_err(|error| BuiltinModelError(format!("read selected native publication: {error}")))?
    else {
        return Ok(NseProgramAdmission::Unavailable);
    };
    let Some(captures) = semantic_capture_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open selected native capture: {error}")))?
    else {
        return Ok(NseProgramAdmission::Unavailable);
    };
    let Some(capture) = captures
        .lookup(key)
        .map_err(|error| BuiltinModelError(format!("read selected native capture: {error}")))?
    else {
        return Ok(NseProgramAdmission::Unavailable);
    };
    if !matches!(capture.outcome(), ProductSemanticCaptureOutcome::Published { coverage: current_coverage, claim: current_claim } if current_coverage == coverage && current_claim == claim)
    {
        return Ok(NseProgramAdmission::Unavailable);
    }
    super::super::profile::native_evidence_basis_for_snapshot(snapshot)?;
    let Some(evidence) = backend_engine::builtin::native_source_evidence_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open selected native evidence: {error}")))?
    else {
        return Ok(NseProgramAdmission::Unavailable);
    };
    let frontiers = backend_engine::builtin::product_native_frontier_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open native frontier: {error}")))?;
    let files = selected_typescript_structural_files(sources, key.package_key())?;
    let activated = super::super::load_semantic_publication(compiler, key, claim, generations)?;
    let mut path_work = 0;
    let mut image_budget = MAX_REBUILD_BYTES;
    let Some(proof) = selected_nse_program_files(
        snapshot,
        sources,
        key,
        &activated,
        generations,
        &evidence,
        frontiers.as_ref(),
        &files,
        &mut path_work,
        &mut image_budget,
    )?
    else {
        return Ok(NseProgramAdmission::Unavailable);
    };
    if !proof.projection_safe {
        return Ok(NseProgramAdmission::Unavailable);
    }
    Ok(NseProgramAdmission::Admitted {
        activation: activated,
        proof,
    })
}

/// Require explicit compiler ownership for the new native-only route, even
/// when a legacy image carries a span. The old structural path is unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeOwnedEntity {
    entity_id: backend_semantic::ir::EntityId,
    declaration_identity: DeclarationIdentity,
    extent: Option<(u32, u32)>,
}

fn native_owned_file_in_image(
    image: &SemanticImageView<'_>,
    package: backend_engine::PackageKey,
    path: &str,
    selected: RowId,
) -> Result<Option<NativeOwnedEntity>, BuiltinModelError> {
    let session = DocumentationSession::new(image);
    let mut found = None;
    for item in session.publication_entities() {
        let item =
            item.map_err(|error| BuiltinModelError(format!("read native ownership: {error}")))?;
        if RowId::Symbol(semantic_symbol(package, item.entity.version.identity())) != selected {
            continue;
        }
        if found.is_some()
            || item.entity.authority.publication
                != backend_semantic::ir::DeclarationPublicationRole::PrimaryOwned
        {
            return Ok(None);
        }
        found = Some((item.entity.id, item.entity.version.identity()));
    }
    let Some((entity_id, declaration_identity)) = found else {
        return Ok(None);
    };
    let Some(extent) = native_file_in_image(image, package, path, selected, false)? else {
        return Ok(None);
    };
    Ok(Some(NativeOwnedEntity {
        entity_id,
        declaration_identity,
        extent,
    }))
}

pub(super) fn selected_typescript_structural_files(
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
) -> Result<
    BTreeMap<
        String,
        (
            backend_version::ContentId<backend_version::SourceFactDomain>,
            [u8; 32],
        ),
    >,
    BuiltinModelError,
> {
    let mut paths = BTreeSet::new();
    let mut project_count = 0usize;
    let mut result = BTreeMap::new();
    let mut path_work = 0usize;
    for (key, record) in &sources.files {
        let fields = record
            .file_fields()
            .ok_or_else(|| BuiltinModelError("native frontier contains non-file row".into()))?;
        if fields.project != package.to_bytes() {
            continue;
        }
        project_count = project_count
            .checked_add(1)
            .ok_or_else(|| BuiltinModelError("native structural file count overflow".into()))?;
        if project_count > backend_library::MAX_SELECTED_PROJECT_FRONTIER_FILES {
            return Err(BuiltinModelError(
                "native structural file budget exceeded".into(),
            ));
        }
        let owner = sources
            .projects
            .get(&fields.project)
            .ok_or_else(|| BuiltinModelError("native frontier missing project".into()))?;
        if *key != product_source_file_key(fields.project, fields.path)
            || owner.files.binary_search(key).is_err()
            || !paths.insert((fields.project, fields.path))
        {
            return Err(BuiltinModelError(
                "invalid native frontier file membership".into(),
            ));
        }
        if fields.project != package.to_bytes()
            || fields.language != backend_compile::SourceLanguage::TypeScript
        {
            continue;
        }
        if fields
            .admitted_unavailable_reason()
            .map_err(BuiltinModelError)?
            .is_some()
            || fields.content_version == [0; 32]
        {
            continue;
        }
        if let Some(identity) = fields.source_identity {
            path_work = path_work
                .checked_add(fields.path.len())
                .and_then(|work| work.checked_add(128))
                .ok_or_else(|| BuiltinModelError("native structural path work overflow".into()))?;
            if path_work > MAX_REBUILD_BYTES {
                return Err(BuiltinModelError(
                    "native structural path budget exceeded".into(),
                ));
            }
            result.insert(fields.path.to_owned(), (identity, fields.content_version));
        }
    }
    Ok(result)
}

fn lookup_native_evidence(
    relation: &backend_engine::workspace::WorkspaceRelationHandle<
        backend_engine::builtin::NativeSourceEvidenceRelation,
    >,
    package: backend_engine::PackageKey,
    profile: backend_semantic::vocabulary::LanguageProfile,
    path: &str,
    image_source: backend_semantic::ir::SourceIdentity,
    budget: &mut usize,
) -> Result<Option<backend_engine::builtin::NativeSourceEvidence>, BuiltinModelError> {
    let key =
        backend_engine::builtin::native_source_evidence_key(package.to_bytes(), profile, path)
            .map_err(|error| BuiltinModelError(error.into()))?;
    let Some(row) = relation
        .lookup(&key)
        .map_err(|error| BuiltinModelError(format!("read native evidence: {error}")))?
    else {
        return Ok(None);
    };
    // Relation lookup decodes a bounded line map. Charge every decoded
    // allocation, including repeated lookups across profiles, against the
    // same request-wide budget as the retained images.
    let map_bytes = row.checked_heap_bytes()
      .ok_or_else(|| BuiltinModelError("native evidence allocation overflow".into()))?;
    *budget = budget.checked_sub(map_bytes)
        .ok_or_else(|| BuiltinModelError("native evidence budget exceeded".into()))?;
    if row.key().ok() != Some(key)
        || row.project != package.to_bytes()
        || row.profile != profile
        || row.path != path
        || row.byte_length != image_source.byte_len
        || row.source_digest.as_slice() != image_source.identity.as_ref()
    {
        return Err(BuiltinModelError("selected native evidence does not match its key or image".into()));
    }
    Ok(Some(row))
}

/// Reads a single canonical package frontier entry without initializing a
/// syntax frontend. The identity is only a physical source locator here.
fn native_frontier_identity(
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
    path: &str,
    language: Option<backend_compile::SourceLanguage>,
    profile: Option<backend_semantic::vocabulary::LanguageProfile>,
) -> Result<Option<backend_version::ContentId<backend_version::SourceFactDomain>>, BuiltinModelError>
{
    if !sources.projects.contains_key(&package.to_bytes()) {
        return Ok(None);
    }
    let mut found = None;
    let mut paths = BTreeSet::new();
    for (file_key, record) in &sources.files {
        let Some(file) = record.file_fields() else {
            return Err(BuiltinModelError(
                "native frontier contains a non-file row".to_owned(),
            ));
        };
        let Some(owner) = sources.projects.get(&file.project) else {
            return Err(BuiltinModelError(
                "native frontier references a missing project".to_owned(),
            ));
        };
        if *file_key != product_source_file_key(file.project, file.path)
            || owner.files.binary_search(file_key).is_err()
            || !paths.insert((file.project, file.path))
        {
            return Err(BuiltinModelError(
                "native frontier contains a noncanonical or duplicate path".to_owned(),
            ));
        }
        if file.project != package.to_bytes() || file.path != path {
            continue;
        }
        if language.is_some_and(|expected| file.language != expected)
            || profile.is_some_and(|expected| {
                native_file_profile(file).map(super::super::ingest::lane_profile)
                    != Some(super::super::ingest::lane_profile(expected))
            })
        {
            return Ok(None);
        }
        let Some(identity) = file.source_identity else {
            return Ok(None);
        };
        found = Some(identity);
    }
    Ok(found)
}

/// Resolves the stored native lane using admitted language and dialect metadata.
/// This chooses no semantic entity and does not initialize a syntax frontend.
fn native_file_profile(
    file: backend_engine::builtin::ProductFileRef<'_>,
) -> Option<backend_semantic::vocabulary::LanguageProfile> {
    use backend_semantic::vocabulary::{GoVersion, LanguageProfile, PythonVersion};
    match file.language {
        backend_compile::SourceLanguage::Go => Some(LanguageProfile::Go(
            file.go_version.unwrap_or(GoVersion::Go125),
        )),
        backend_compile::SourceLanguage::Python => Some(LanguageProfile::Python(
            file.python_version.unwrap_or(PythonVersion::Python314),
        )),
        language => {
            let extension = std::path::Path::new(file.path)
                .extension()?
                .to_str()?
                .to_ascii_lowercase();
            let known = match language {
                backend_compile::SourceLanguage::Rust => extension == "rs",
                backend_compile::SourceLanguage::TypeScript => matches!(
                    extension.as_str(),
                    "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs"
                ),
                backend_compile::SourceLanguage::Java => extension == "java",
                backend_compile::SourceLanguage::CSharp => extension == "cs",
                backend_compile::SourceLanguage::Clang => true,
                _ => false,
            };
            known
                .then(|| super::super::ingest::dialect(language, &extension))
                .flatten()
        }
    }
}

/// If a publication has a native program manifest, preserve its full source,
/// toolchain and image membership checks without using profile discovery.
fn native_program_is_current(
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
    activated: &super::super::ActivatedProductSemantics,
) -> Result<bool, BuiltinModelError> {
    let Some(program) = activated.native_program_sources.as_ref() else {
        return Ok(true);
    };
    let mut mappings = BTreeMap::new();
    for row in program.sources() {
        let Some(path) = row.package_path else {
            continue;
        };
        if native_frontier_identity(
            sources,
            package,
            path,
            Some(backend_compile::SourceLanguage::TypeScript),
            None,
        )? != Some(row.source.identity)
            || mappings.insert(path, row.source).is_some()
        {
            return Ok(false);
        }
    }
    for image in activated.images() {
        let view = image
            .reopen()
            .map_err(|error| BuiltinModelError(format!("reopen native program image: {error}")))?;
        let backend_semantic::ir::ImageProvenance::Captured {
            source,
            recipe,
            scope,
            ..
        } = view.image_facts().provenance
        else {
            return Ok(false);
        };
        let Some(path) = view
            .atom(scope.path)
            .and_then(|value| std::str::from_utf8(value).ok())
        else {
            return Ok(false);
        };
        if recipe.toolchain != program.toolchain()
            || mappings.get(path) != Some(&source)
            || !program.image_matches(
                path,
                backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(image.as_ref())
                    .as_ref(),
            )
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A compiler identity is authoritative only when its own captured source
/// span is unique, nonempty and names this exact admitted image path.
pub(super) fn native_extent_in_image(
    image: &SemanticImageView<'_>,
    package: backend_engine::PackageKey,
    path: &str,
    selected: RowId,
) -> Result<Option<(u32, u32)>, BuiltinModelError> {
    Ok(native_file_in_image(image, package, path, selected, true)?.flatten())
}

fn native_file_in_image(
    image: &SemanticImageView<'_>,
    package: backend_engine::PackageKey,
    path: &str,
    selected: RowId,
    require_extent: bool,
) -> Result<Option<Option<(u32, u32)>>, BuiltinModelError> {
    let session = DocumentationSession::new(image);
    let mut found = None;
    for item in session.publication_entities() {
        let item =
            item.map_err(|error| BuiltinModelError(format!("read native declaration: {error}")))?;
        if RowId::Symbol(semantic_symbol(package, item.entity.version.identity())) != selected {
            continue;
        }
        let extent = match item.entity.source {
            Some(span) => {
                if item.entity.authority.source_file
                    != backend_semantic::ir::FactAvailability::Captured
                {
                    return Err(BuiltinModelError(
                        "selected native source is not captured".to_owned(),
                    ));
                }
                if image.atom(span.file()) != Some(path.as_bytes()) {
                    return Err(BuiltinModelError(
                        "selected native source path differs".to_owned(),
                    ));
                }
                if item.entity.authority.source == backend_semantic::ir::FactAvailability::Captured
                    && !span.is_empty()
                {
                    Some((span.start(), span.end()))
                } else {
                    None
                }
            }
            None => {
                // PrimaryOwned is a producer-authenticated per-entity statement
                // about the captured image's primary file, not an inferred span.
                // Legacy and reference-only rows cannot provide this witness.
                let backend_semantic::ir::ImageProvenance::Captured { scope, .. } =
                    image.image_facts().provenance
                else {
                    return Err(BuiltinModelError(
                        "selected native source provenance is unavailable".to_owned(),
                    ));
                };
                if require_extent
                    || item.entity.authority.publication
                        != backend_semantic::ir::DeclarationPublicationRole::PrimaryOwned
                    || item.entity.authority.source
                        != backend_semantic::ir::FactAvailability::Unavailable
                    || item.entity.authority.source_file
                        != backend_semantic::ir::FactAvailability::Unavailable
                    || image.atom(scope.path) != Some(path.as_bytes())
                {
                    return Err(BuiltinModelError(
                        "selected native file identity is unavailable".to_owned(),
                    ));
                }
                None
            }
        };
        if require_extent && extent.is_none() {
            return Err(BuiltinModelError(
                "selected native extent is unavailable".to_owned(),
            ));
        }
        if found.replace(extent).is_some() {
            return Err(BuiltinModelError(
                "selected native identity is ambiguous".to_owned(),
            ));
        }
    }
    Ok(found)
}

pub(super) struct SourceRowProjection<'a> {
    initial: &'a ViewRoot,
    sources: &'a IndexedSources,
    rows: Vec<Row>,
    selected_files: BTreeSet<([u8; 32], [u8; 32])>,
    ledger: ProjectionLedger,
    targets: &'a SemanticTargets,
    structural_plan: &'a StructuralProjectionPlan,
}

impl<'a> SourceRowProjection<'a> {
    pub(super) fn new(
        initial: &'a ViewRoot,
        sources: &'a IndexedSources,
        capacity: usize,
        targets: &'a SemanticTargets,
        structural_plan: &'a StructuralProjectionPlan,
        workspace: &'a std::path::Path,
    ) -> Result<Self, BuiltinModelError> {
        let mut rows = Vec::with_capacity(capacity);
        let mut selected_files = BTreeSet::new();
        for (project_key, project) in &sources.projects {
            let row = Row::new(
                RowId::Package(project.package),
                initial.basis(),
                &project.label,
            );
            rows.push(match package_row_name(&project.label, workspace) {
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
            sources,
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
        let project = self.sources.projects.get(&project_key).ok_or_else(|| {
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
            super::super::ingest::source_record_profile(file).map_err(BuiltinModelError)?;
        if semantic_profile_is_complete(
            complete,
            Some(project.package),
            path,
            file.python_version,
            file.go_version,
        )? {
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
        let mut index = 0usize;
        super::structural::visit_complete_declarations(self.sources, record, |declaration| {
            let prepared = self.structural_plan.declaration(file_key, index)?;
            if !prepared.compiler_twin {
                let row = self.declaration_row(
                    declaration,
                    path,
                    file_key,
                    project_key,
                    package,
                    language,
                    prepared,
                )?;
                self.rows.push(row);
            }
            index = index.checked_add(1).ok_or_else(|| {
                BuiltinModelError("structural declaration count overflow".to_owned())
            })?;
            Ok(())
        })?;
        if self
            .structural_plan
            .file(file_key)
            .is_some_and(|file| file.declarations.len() != index)
        {
            return Err(BuiltinModelError(
                "complete source-facts count differs from its structural plan".to_owned(),
            ));
        }
        if profile.is_some_and(|profile| {
            self.targets.partial_artifacts.contains(&(
                project.package.to_bytes(),
                profile,
                path.to_owned(),
            ))
        }) {
            self.ledger.record(path, FileLane::Mixed);
        }
        Ok(())
    }

    /// Builds one declaration's row, including the parent it hangs under.
    fn declaration_row(
        &self,
        declaration: &backend_compile::SourceDeclaration,
        path: &str,
        file_key: [u8; 32],
        project: [u8; 32],
        package: backend_engine::PackageKey,
        language: backend_engine::SourceLanguage,
        prepared: &StructuralDeclaration,
    ) -> Result<Row, BuiltinModelError> {
        structural_declaration_row(
            self.initial,
            self.structural_plan,
            None,
            declaration,
            path,
            file_key,
            project,
            package,
            language,
            prepared,
        )
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

/// Projects structural rows for files the plan retained.
///
/// `resident_labels` maps a coordinate to the symbol already published for
/// this package. A parent planned in this call wins. Otherwise the resident
/// symbol is kept, so a method edited in one file stays attached to a type
/// declared in a file this call did not replan.
pub(super) fn rows_for_changed_structural_files(
    initial: &ViewRoot,
    sources: &IndexedSources,
    plan: &StructuralProjectionPlan,
    resident_labels: &BTreeMap<String, backend_engine::SymbolKey>,
) -> Result<Vec<Row>, BuiltinModelError> {
    let mut rows = Vec::new();
    for (file_key, record) in &sources.files {
        if plan.file(*file_key).is_none() {
            continue;
        }
        let file = record.file_fields().ok_or_else(|| {
            BuiltinModelError("structural file splice received a non-file record".to_owned())
        })?;
        let project = sources.projects.get(&file.project).ok_or_else(|| {
            BuiltinModelError("structural source refers to a missing project".to_owned())
        })?;
        if product_source_file_key(file.project, file.path) != *file_key
            || project.files.binary_search(file_key).is_err()
        {
            return Err(BuiltinModelError(
                "source file is outside its project's canonical frontier".to_owned(),
            ));
        }
        let mut index = 0usize;
        super::structural::visit_complete_declarations(sources, record, |declaration| {
            let prepared = plan.declaration(*file_key, index)?;
            rows.push(structural_declaration_row(
                initial,
                plan,
                Some(resident_labels),
                declaration,
                file.path,
                *file_key,
                file.project,
                project.package,
                file.language,
                prepared,
            )?);
            index = index.checked_add(1).ok_or_else(|| {
                BuiltinModelError("structural declaration count overflow".to_owned())
            })?;
            Ok(())
        })?;
        if plan
            .file(*file_key)
            .is_some_and(|file| file.declarations.len() != index)
        {
            return Err(BuiltinModelError(
                "complete source-facts count differs from its structural plan".to_owned(),
            ));
        }
    }
    Ok(rows)
}

fn structural_declaration_row(
    initial: &ViewRoot,
    plan: &StructuralProjectionPlan,
    resident_labels: Option<&BTreeMap<String, backend_engine::SymbolKey>>,
    declaration: &backend_compile::SourceDeclaration,
    path: &str,
    file_key: [u8; 32],
    project: [u8; 32],
    package: backend_engine::PackageKey,
    language: backend_engine::SourceLanguage,
    prepared: &StructuralDeclaration,
) -> Result<Row, BuiltinModelError> {
    let coordinate = &prepared.coordinate;
    let symbol = prepared.id;
    let document = if prepared.is_file_module {
        vec![Fragment::Text(format!(
            "{} source · {path}",
            language.name()
        ))]
    } else if declaration.documentation().is_empty() {
        Vec::new()
    } else {
        vec![Fragment::Text(declaration.documentation().to_owned())]
    };
    let row = Row::in_package(symbol, initial.basis(), package, coordinate.as_str())
        .with_document(document)
        .with_signature(declaration.signature())
        .with_kind(declaration.kind())
        .with_source(declaration.location().clone())
        .with_excerpt(declaration.source_excerpt().clone())
        .with_facts(declaration.facts().clone());
    let row = match prepared.identity_preimage.clone() {
        Some(preimage) => row.with_identity_preimage(preimage),
        None => row,
    };
    if let Some(binding) = prepared.parent_binding {
        let RowId::Symbol(parent) = plan.binding_parent_id(file_key, binding)? else {
            return Err(BuiltinModelError(
                "exact structural parent is not a symbol".to_owned(),
            ));
        };
        return Ok(row.with_parent(parent));
    }
    Ok(match prepared.parent.as_deref() {
        Some(parent) => {
            match structural_parent_symbol(plan, file_key, project, parent, resident_labels)? {
                Some(parent) => row.with_parent(parent),
                None => row,
            }
        }
        None => row,
    })
}

fn structural_parent_symbol(
    plan: &StructuralProjectionPlan,
    file_key: [u8; 32],
    project: [u8; 32],
    parent: &str,
    resident_labels: Option<&BTreeMap<String, backend_engine::SymbolKey>>,
) -> Result<Option<backend_engine::SymbolKey>, BuiltinModelError> {
    if let Some(id) = plan.symbol_for_coordinate(project, parent) {
        return Ok(match id {
            RowId::Symbol(symbol) => Some(symbol),
            RowId::Package(_) | RowId::Object(_) => None,
        });
    }
    if let Some(labels) = resident_labels
        && let Some(symbol) = labels.get(parent)
    {
        return Ok(Some(*symbol));
    }
    Ok(match plan.parent_id(file_key, parent)? {
        StructuralParent::Symbol(RowId::Symbol(parent)) => Some(parent),
        StructuralParent::Symbol(RowId::Package(_))
        | StructuralParent::Symbol(RowId::Object(_))
        | StructuralParent::Package(_) => None,
    })
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
    exact_sites: super::exact_sites::ExactDeclarationSites,
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
    /// Current artifacts without a complete file inventory; structural rows remain.
    partial_artifacts: BTreeSet<(
        [u8; 32],
        backend_semantic::vocabulary::LanguageProfile,
        String,
    )>,
    /// Targets whose selected publication is a terminal unavailable cause.
    pub(super) unavailable: BTreeMap<
        ([u8; 32], backend_semantic::vocabulary::LanguageProfile),
        SemanticUnavailableReason,
    >,
}

/// Lower bounds for every indexed project, in map order.
///
/// `None` means a label is not the package reference that produced its key, so
/// the caller pages the whole relation. An empty project set seeks nothing.
fn publication_seek_bounds(
    projects: &BTreeMap<[u8; 32], IndexedProject>,
) -> Option<
    Vec<(
        backend_engine::PackageReference,
        backend_engine::ProductSemanticPublicationKey,
    )>,
> {
    let mut bounds = Vec::with_capacity(projects.len());
    for project in projects.values() {
        let reference = backend_engine::PackageReference::parse(project.label.clone()).ok()?;
        if backend_engine::package_key(reference.as_str()) != project.package {
            return None;
        }
        let start =
            backend_engine::ProductSemanticPublicationKey::package_lower_bound(reference.clone());
        bounds.push((reference, start));
    }
    Some(bounds)
}

fn semantic_rows(
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    sources: &IndexedSources,
    projects: &BTreeMap<[u8; 32], IndexedProject>,
    current_paths: &ProfileSourcePaths,
    current_identities: &ProfileSourceIdentities,
    native_frontier_complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
    sites: &StructuralSites<'_>,
    available: &SourceAvailability,
    initial: &ViewRoot,
    row_capacity: usize,
    foreign: ForeignPublication,
    residence: &mut super::image_rows::ImageRowResidence,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
) -> Result<SemanticRows, BuiltinModelError> {
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| {
            BuiltinModelError(format!("open semantic publication relation: {error}"))
        })?;
    // Open the exact auxiliary closure even though this projection only emits
    // publication rows; malformed capture state must still refuse the view.
    let capture_relation = semantic_capture_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open semantic capture relation: {error}")))?;
    let nse_evidence = backend_engine::builtin::native_source_evidence_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open selected native evidence: {error}")))?;
    if nse_evidence.is_some() {
        super::super::profile::native_evidence_basis_for_snapshot(snapshot)?;
    }
    let nse_owners = nse_evidence.as_ref().map(selected_nse_typescript_owners).transpose()?.unwrap_or_default();
    let nse_frontiers = if nse_evidence.is_some() {
        backend_engine::builtin::product_native_frontier_relation(snapshot)
            .map_err(|error| BuiltinModelError(format!("open native frontier: {error}")))?
    } else { None };
    let mut nse_structural_cache = BTreeMap::new();
    let mut nse_structural_work = 0usize;
    let mut nse_program_work = 0usize;
    let mut nse_context_scan_work = 0usize;
    let mut nse_image_budget = MAX_REBUILD_BYTES;
    let mut rows = Vec::new();
    let mut exact_sites = super::exact_sites::ExactDeclarationSites::default();
    let mut complete = BTreeSet::new();
    let mut incomplete_ownership = BTreeSet::new();
    let mut stale_paths = ProfileStalePaths::new();
    let mut activated_publications = BTreeSet::new();
    let mut ledger = ProjectionLedger::default();
    let mut symbols = BTreeSet::new();
    let mut remaining_bytes = MAX_REBUILD_BYTES;
    let mut targets = SemanticTargets::default();
    let mut native_selections = super::native_target_aliases::NativePackageSelections::default();
    let mut native_aliases = super::native_target_aliases::NativeTargetAliases::default();
    // Package publication already named its projects. Seek each one instead of
    // paging every other package's publications and discarding them.
    let bounds = match foreign {
        ForeignPublication::Skip => publication_seek_bounds(projects),
        ForeignPublication::Reject => None,
    };
    let mut bound_index = 0usize;
    let mut from = bounds
        .as_ref()
        .and_then(|bounds| bounds.first().map(|(_, start)| start.clone()));
    let mut after = None;
    loop {
        if let Some(bounds) = &bounds
            && bound_index >= bounds.len()
        {
            break;
        }
        let page = if let Some(start) = from.take() {
            relation.page_from(&start, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        } else {
            relation.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        }
        .map_err(|error| BuiltinModelError(format!("read semantic publication page: {error}")))?;
        let mut package_ended = false;
        for (key, record) in page.entries() {
            if let Some(bounds) = &bounds
                && let Some((reference, _)) = bounds.get(bound_index)
                && key.package() != reference
            {
                package_ended = true;
                break;
            }
            // The relation's typed order groups package references before
            // coordinates/profiles/history, including across page boundaries.
            // Only compact alias keys survive a completed package callback.
            native_selections.advance(key, |package, selected| {
                native_aliases.record_selected(package, selected)
            })?;
            if !key.is_selected() {
                continue;
            }
            if !projects.contains_key(key.package_key().as_bytes())
                && matches!(foreign, ForeignPublication::Skip)
            {
                continue;
            }
            // A source-only removal retains immutable native history and its
            // authenticated capture. That old scope is not a current package
            // and must not be activated or resurrected into workspace rows.
            // An unsourced publication without capture evidence still takes
            // the existing strict missing-frontier refusal below.
            if !projects.contains_key(key.package_key().as_bytes()) {
                let historical_capture = capture_relation
                    .as_ref()
                    .map(|captures| captures.lookup(key))
                    .transpose()
                    .map_err(|error| {
                        BuiltinModelError(format!("read retained semantic capture: {error}"))
                    })?
                    .flatten();
                if historical_capture.is_some() {
                    let source = snapshot
                        .relation::<super::super::BuiltinWorkspaceRelation>()
                        .map_err(|error| {
                            BuiltinModelError(format!("open current package scope: {error}"))
                        })?;
                    if source
                        .lookup(&key.package_key().to_bytes())
                        .map_err(|error| {
                            BuiltinModelError(format!("read removed package scope: {error}"))
                        })?
                        .is_none()
                    {
                        continue;
                    }
                }
            }
            let target = (
                *key.package_key().as_bytes(),
                super::super::ingest::lane_profile(key.profile()),
            );
            let ProductSemanticPublicationRecord::Published { coverage, claim } = record else {
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
            let activated =
                super::super::load_semantic_publication(compiler, key, *claim, generations)?;
            let mut nse_program_needs = false;
            if matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_)) {
                if let Some(program) = activated.native_program_sources.as_ref() {
                    let mut mapped = 0usize;
                    for row in program.sources() {
                        let Some(path) = row.package_path else { continue };
                        mapped = mapped.checked_add(1).ok_or_else(|| BuiltinModelError("native program source count overflow".into()))?;
                        nse_context_scan_work = nse_context_scan_work.checked_add(path.len()).and_then(|work| work.checked_add(128))
                            .ok_or_else(|| BuiltinModelError("native program context work overflow".into()))?;
                        if mapped > backend_library::MAX_SELECTED_PROJECT_FRONTIER_FILES || nse_context_scan_work > MAX_REBUILD_BYTES {
                            return Err(BuiltinModelError("native program context budget exceeded".into()));
                        }
                        if !available.typescript_source_current(key.package_key(), path, row.source.identity) {
                            nse_program_needs = true;
                        }
                    }
                    if mapped == 0 { nse_program_needs = true; }
                }
            }
            let nse_positive = if let Some(evidence) = nse_evidence.as_ref()
                && key.is_selected()
                && matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_))
                && !project.label.starts_with("pkg:")
            {
                let owner = key.package_key().to_bytes();
                if !nse_structural_cache.contains_key(&owner) {
                    // Repeated multi-project scans must have a single bounded work budget.
                    let scan_work = sources.files.len().checked_mul(128)
                        .ok_or_else(|| BuiltinModelError("native structural scan work overflow".into()))?;
                    nse_structural_work = nse_structural_work.checked_add(scan_work)
                        .ok_or_else(|| BuiltinModelError("native structural scan work overflow".into()))?;
                    if nse_structural_work > MAX_REBUILD_BYTES {
                        return Err(BuiltinModelError("native structural scan budget exceeded".into()));
                    }
                    let files = selected_typescript_structural_files(sources, key.package_key())?;
                    let retained = files.keys().try_fold(0usize, |total, path| total.checked_add(path.len() + 128))
                        .ok_or_else(|| BuiltinModelError("native structural index overflow".into()))?;
                    nse_structural_work = nse_structural_work.checked_add(retained)
                        .ok_or_else(|| BuiltinModelError("native structural index overflow".into()))?;
                    if nse_structural_work > MAX_REBUILD_BYTES {
                        return Err(BuiltinModelError("native structural index budget exceeded".into()));
                    }
                    nse_structural_cache.insert(owner, files);
                }
                selected_nse_program_files(snapshot, sources, key, &activated, generations, evidence,
                    nse_frontiers.as_ref(), nse_structural_cache.get(&owner).expect("inserted index"),
                    &mut nse_program_work, &mut nse_image_budget)?.filter(|proof| proof.projection_safe)
            } else { None };
            // Observed policy is a separate capability: it never becomes an
            // unqualified current program or a complete source inventory.
            let (selected, observed) = match nse_positive.as_ref() {
                Some(proof) => (None, Some(available.select_observed_with_nse(
                    snapshot, key, *claim, *coverage, activated, residence, proof,
                )?)),
                None => (Some(available.select(key, *coverage, activated, residence)?), None),
            };
            // An image already admitted for this publication key keeps its
            // path and source identity. Row projection is reused when the
            // freshness overlay is already resident.
            let lineage = key.lineage().map_err(|error| {
                BuiltinModelError(format!("semantic publication lineage: {error}"))
            })?;
            let admission = super::image_rows::publication_admission(
                key.profile(),
                lineage.ecosystem,
                lineage.name,
                key.coordinate().as_str(),
            );
            let mut opened = Vec::new();
            let images: Box<dyn Iterator<Item = &backend_library::interface::SemanticImageSnapshot> + '_> =
                match (&selected, &observed) {
                    (Some(selected), None) => Box::new(selected.images()),
                    (None, Some(observed)) => Box::new(observed.images()),
                    _ => return Err(BuiltinModelError("invalid selected semantic artifacts".into())),
                };
            for image in images {
                match super::image_rows::open_compiled_snapshot(image, admission, residence)? {
                    super::image_rows::CompiledImage::Opened {
                        path,
                        identity,
                        view,
                    } => {
                        super::image_rows::bind_opened_image(
                            image.as_ref(),
                            admission,
                            &view,
                            key,
                            residence,
                        )?;
                        opened.push(super::image_rows::CompiledImage::Opened {
                            path,
                            identity,
                            view,
                        });
                    }
                    resident => opened.push(resident),
                }
            }
            let mut compiled_sources = BTreeMap::new();
            for image in &opened {
                let (path, identity) = match image {
                    super::image_rows::CompiledImage::Resident { path, identity, .. }
                    | super::image_rows::CompiledImage::Opened { path, identity, .. } => {
                        (path, identity)
                    }
                };
                compiled_sources.insert(path.clone(), *identity);
            }
            let nse_required = matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_))
                && (nse_program_needs
                    || nse_owners.contains(key.package_key().as_bytes())
                    || (compiled_sources.is_empty() && current_paths.get(&target).is_none_or(BTreeSet::is_empty))
                    || compiled_sources.iter().any(|(path, identity)| {
                        !available.typescript_source_current(key.package_key(), path, *identity)
                    }));
            let empty = BTreeSet::new();
            let mut decision = freshness_decision(
                &compiled_sources,
                current_paths.get(&target).unwrap_or(&empty),
                current_identities.get(&target),
            );
            let capture = capture_relation
                .as_ref()
                .map(|captures| captures.lookup(key))
                .transpose()
                .map_err(|error| {
                    BuiltinModelError(format!("read selected semantic capture: {error}"))
                })?
                .flatten();
            if nse_required && nse_positive.is_none() {
                // Missing or rejected native evidence is unknown, never a
                // structural complete result or a legacy fallback.
                decision.invalidate_selected_context();
            }
            if capture.as_ref().is_some_and(|capture| {
                matches!(
                    capture.outcome(),
                    ProductSemanticCaptureOutcome::Pending { .. }
                        | ProductSemanticCaptureOutcome::Failed { .. }
                )
            }) {
                // Source bytes alone cannot make the prior generation current:
                // the latest capture has not published its result. This also
                // covers an unselected request replacing an explicit target.
                decision.invalidate_selected_context();
            }
            let native_frontier_is_complete = !matches!(
                key.profile(),
                backend_semantic::vocabulary::LanguageProfile::C(_)
                    | backend_semantic::vocabulary::LanguageProfile::Cxx(_)
                    | backend_semantic::vocabulary::LanguageProfile::CSharp(_)
            ) || native_frontier_complete.contains(&target);
            if !native_frontier_is_complete {
                decision.stale_paths = current_paths.get(&target).cloned().unwrap_or_default();
                decision.path_sets_differ = true;
                decision.identity_decisive = false;
                decision.identities = None;
                if let Some(paths) = current_paths.get(&target) {
                    for path in paths {
                        targets
                            .partial_artifacts
                            .insert((target.0, target.1, path.clone()));
                    }
                }
            }
            if matches!(
                key.profile(),
                backend_semantic::vocabulary::LanguageProfile::C(_)
            ) && !decision.stale_paths.is_empty()
                && c_freshness_diagnostics_enabled()
            {
                let current_paths = current_paths.get(&target).unwrap_or(&empty);
                let current_identities = current_identities.get(&target);
                eprintln!(
                    "NUDOX_C_FRESHNESS coordinate={:?} claim={:?} profile={:?} identity_decisive={} path_sets_differ={} stale_count={} compiled_count={} current_count={}",
                    key.coordinate().as_str(),
                    claim,
                    key.profile(),
                    decision.identity_decisive,
                    decision.path_sets_differ,
                    decision.stale_paths.len(),
                    compiled_sources.len(),
                    current_paths.len(),
                );
                for path in decision.stale_paths.iter().take(64) {
                    let compiled = decision.compiled.get(path);
                    let current = current_identities.and_then(|identities| identities.get(path));
                    eprintln!(
                        "NUDOX_C_FRESHNESS stale path={path:?} compiled={:?} current={:?}",
                        compiled.map(|identity| identity.as_ref()),
                        current.map(|identity| identity.as_ref()),
                    );
                }
                if decision.stale_paths.len() > 64 {
                    eprintln!(
                        "NUDOX_C_FRESHNESS stale_paths_omitted={}",
                        decision.stale_paths.len() - 64
                    );
                }
                for image in opened.iter().take(64) {
                    let (path, identity) = match image {
                        super::image_rows::CompiledImage::Resident { path, identity, .. }
                        | super::image_rows::CompiledImage::Opened { path, identity, .. } => {
                            (path, identity)
                        }
                    };
                    eprintln!(
                        "NUDOX_C_FRESHNESS image path={path:?} identity={:?}",
                        identity.as_ref(),
                    );
                }
                if opened.len() > 64 {
                    eprintln!("NUDOX_C_FRESHNESS images_omitted={}", opened.len() - 64);
                }
                for path in current_paths.iter().take(64) {
                    let current = current_identities.and_then(|identities| identities.get(path));
                    eprintln!(
                        "NUDOX_C_FRESHNESS current path={path:?} identity={:?}",
                        current.map(|identity| identity.as_ref()),
                    );
                }
                if current_paths.len() > 64 {
                    eprintln!(
                        "NUDOX_C_FRESHNESS current_paths_omitted={}",
                        current_paths.len() - 64
                    );
                }
            }
            let checked_native_frontier = if native_frontier_is_complete
                && matches!(
                    key.profile(),
                    backend_semantic::vocabulary::LanguageProfile::C(_)
                        | backend_semantic::vocabulary::LanguageProfile::Cxx(_)
                        | backend_semantic::vocabulary::LanguageProfile::CSharp(_)
                ) {
                sites.sources.source_snapshot.as_ref().and_then(|snapshot| {
                    snapshot
                        .admit_native_frontier(
                            *key.package_key().as_bytes(),
                            key.profile(),
                            current_paths.get(&target).unwrap_or(&empty),
                        )
                        .ok()
                        .flatten()
                })
            } else {
                None
            };
            let mut image_groups = Vec::<(String, Vec<usize>)>::new();
            // NSE proves individual selected files, never a complete inventory.
            let mut ownership_complete = !nse_required && nse_positive.as_ref().is_none_or(|proof| proof.positive.is_empty());
            let mut group_by_path = BTreeMap::<String, usize>::new();
            for (index, image) in opened.iter().enumerate() {
                let path = match image {
                    super::image_rows::CompiledImage::Resident { path, .. }
                    | super::image_rows::CompiledImage::Opened { path, .. } => path,
                };
                let group = match group_by_path.get(path).copied() {
                    Some(group) => group,
                    None => {
                        let group = image_groups.len();
                        group_by_path.insert(path.clone(), group);
                        image_groups.push((path.clone(), Vec::new()));
                        group
                    }
                };
                image_groups[group].1.push(index);
            }
            for (path, image_indices) in image_groups {
                let first_group_row = rows.len();
                // Staleness is per image: this image is stale exactly when the
                // current file compiled from its path no longer hashes to an
                // image's own source identity. A legacy scan without persisted
                // identities falls back to the coarse path-set comparison.
                let nse_current = !decision.selected_context_stale && nse_positive.as_ref()
                    .and_then(|proof| proof.positive.get(&path))
                    .is_some_and(|(_, source)| decision.compiled.get(&path) == Some(&source.identity));
                if nse_current {
                    decision.stale_paths.remove(&path);
                    targets.partial_artifacts.insert((target.0, target.1, path.clone()));
                }
                let stale = !nse_current && matches!(
                    coverage,
                    backend_engine::builtin::SemanticPublicationCoverage::Complete
                ) && match decision.compiled.get(&path) {
                    Some(compiled) => decision.image_stale(&path, *compiled),
                    None => decision.path_sets_differ,
                };
                if matches!(
                    coverage,
                    backend_engine::builtin::SemanticPublicationCoverage::Partial(_)
                ) || !native_frontier_is_complete
                {
                    targets
                        .partial_artifacts
                        .insert((target.0, target.1, path.clone()));
                }
                // Each source path is grouped before projection, so all
                // semantic fragments for this file share one bounded facts
                // visit. The owned declarations are dropped before the next
                // file; this avoids both per-fragment rereads and eager
                // project-wide fact hydration.
                let owned_site_declarations = if stale {
                    Vec::new()
                } else if !matches!(
                    key.profile(),
                    backend_semantic::vocabulary::LanguageProfile::Go(_)
                        | backend_semantic::vocabulary::LanguageProfile::TypeScript(_)
                ) {
                    // Other languages retain their established display-site
                    // fallback. Go and TypeScript require exact source identity.
                    sites.declarations(key.package_key().as_bytes(), &path)?
                } else {
                    match decision.compiled.get(&path) {
                        Some(source)
                            if image_indices.iter().all(|index| match &opened[*index] {
                                super::image_rows::CompiledImage::Opened { identity, .. }
                                | super::image_rows::CompiledImage::Resident { identity, .. } => {
                                    identity == source
                                }
                            }) =>
                        {
                            sites.declarations_of_source(
                                key.package_key().as_bytes(),
                                &path,
                                *source,
                            )?
                        }
                        _ => Vec::new(),
                    }
                };
                let site_declarations = owned_site_declarations.iter().collect::<Vec<_>>();
                let native_source = if stale {
                    None
                } else {
                    if let Some((snapshot, frontier)) = sites
                        .sources
                        .source_snapshot
                        .as_ref()
                        .zip(checked_native_frontier.as_ref())
                    {
                        // Refuse the owned source allocation before hydration,
                        // including when image provenance later yields no sites.
                        if let Ok(index) =
                            frontier.dispositions().binary_search_by(|(candidate, _)| {
                                candidate.as_str().cmp(path.as_str())
                            })
                        {
                            let byte_length = match &frontier.dispositions()[index].1 {
                                backend_engine::builtin::NativePathDisposition::ClangMalformed {
                                    byte_length, ..
                                }
                                | backend_engine::builtin::NativePathDisposition::CSharpMalformed {
                                    byte_length, ..
                                } => Some(*byte_length),
                                _ => None,
                            };
                            if let Some(byte_length) = byte_length {
                                if usize::try_from(byte_length)
                                    .map_or(true, |length| length > remaining_bytes)
                                {
                                    return Err(BuiltinModelError(
                                        "native source exceeds the rebuild byte bound".to_owned(),
                                    ));
                                }
                            }
                        }
                        match key.profile() {
                            backend_semantic::vocabulary::LanguageProfile::CSharp(_) => {
                                snapshot.verified_native_csharp_source(frontier, &path).ok()
                            }
                            _ => snapshot.verified_native_source(frontier, &path).ok(),
                        }
                    } else {
                        None
                    }
                };
                for image_index in image_indices {
                    let image = &opened[image_index];
                    let mut sink = SemanticRowSink {
                        ownership_complete: &mut ownership_complete,
                        exact_sites: &mut exact_sites,
                        initial,
                        symbols: &mut symbols,
                        rows: &mut rows,
                        capacity: row_capacity,
                        remaining_bytes: &mut remaining_bytes,
                        stale,
                        path: &path,
                        site_declarations: &site_declarations,
                        native_source: native_source.as_ref(),
                        native_ts_evidence: nse_current.then(|| nse_positive.as_ref()
                            .and_then(|proof| proof.positive.get(&path))
                            .map(|(claim, _)| claim).filter(|claim| claim.admits_image())).flatten(),
                    };
                    match image {
                        super::image_rows::CompiledImage::Opened { view, .. } => {
                            super::image_rows::append_resident_image_rows(
                                view,
                                project,
                                key.profile(),
                                &mut sink,
                                residence,
                            )?;
                        }
                        super::image_rows::CompiledImage::Resident {
                            bytes,
                            digest,
                            snapshot,
                            ..
                        } => {
                            if !super::image_rows::apply_resident_image(
                                *digest,
                                project,
                                key.profile(),
                                &mut sink,
                                residence,
                            )? {
                                let view = super::image_rows::reopen_resident_image(
                                    bytes, *snapshot, residence,
                                )?;
                                super::image_rows::append_resident_image_rows(
                                    &view,
                                    project,
                                    key.profile(),
                                    &mut sink,
                                    residence,
                                )?;
                            }
                        }
                    }
                }
                if observed.is_some() {
                    const NOTE: &str = " Source was observed at capture; discovery policy stability is unknown.";
                    for row in &mut rows[first_group_row..] {
                        let cost = NOTE.len().checked_add(std::mem::size_of::<Fragment>())
                            .ok_or_else(|| BuiltinModelError("native policy note size overflow".into()))?;
                        remaining_bytes = remaining_bytes.checked_sub(cost)
                            .ok_or_else(|| BuiltinModelError("native policy notes exceed rebuild budget".into()))?;
                        let mut fragments = std::mem::take(&mut row.document).into_vec();
                        fragments.push(Fragment::Text(NOTE.to_owned()));
                        row.document = fragments.into_boxed_slice();
                    }
                }
            }
            if matches!(
                coverage,
                backend_engine::builtin::SemanticPublicationCoverage::Complete
            ) && ownership_complete
                && native_frontier_is_complete
                && !incomplete_ownership.contains(&target)
            {
                complete.insert(target);
            }
            if !ownership_complete {
                incomplete_ownership.insert(target);
                complete.remove(&target);
                for path in compiled_sources.keys() {
                    targets
                        .partial_artifacts
                        .insert((target.0, target.1, path.clone()));
                }
            }
            if matches!(
                coverage,
                backend_engine::builtin::SemanticPublicationCoverage::Complete
            ) && !decision.stale_paths.is_empty()
            {
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
            // Match the query/graph package's entire selected image scope,
            // including a TS import whose local target is in the TSX profile.
            drop(opened);
            if let Some(selected) = selected {
                native_selections.retain(key, selected)?;
            }
        }
        if package_ended || (bounds.is_some() && page.next().is_none()) {
            // Explicit seeks use project-map order (hashed keys), not global
            // reference order. Finish this complete package before any seek.
            native_selections
                .finish(|package, selected| native_aliases.record_selected(package, selected))?;
            bound_index = bound_index.saturating_add(1);
            from = bounds
                .as_ref()
                .and_then(|bounds| bounds.get(bound_index).map(|(_, start)| start.clone()));
            after = None;
            continue;
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    native_selections
        .finish(|package, selected| native_aliases.record_selected(package, selected))?;
    native_aliases.apply(&mut rows);
    Ok(SemanticRows {
        rows,
        exact_sites,
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
    /// A newer source capture has not published a generation yet.
    selected_context_stale: bool,
}

impl FreshnessDecision {
    pub(super) fn invalidate_selected_context(&mut self) {
        self.selected_context_stale = true;
        self.stale_paths.extend(self.compiled.keys().cloned());
    }

    /// Returns whether the image compiled from `path` with `identity`
    /// predates the current file content.
    pub(super) fn image_stale(
        &self,
        path: &str,
        identity: backend_version::ContentId<backend_version::SourceFactDomain>,
    ) -> bool {
        self.selected_context_stale
            || match &self.identities {
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
        selected_context_stale: false,
    }
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
pub(crate) fn compiled_source_path(
    image: &SemanticImageView<'_>,
) -> Result<String, BuiltinModelError> {
    compiled_source(image).map(|(path, _)| path)
}

pub(super) struct SemanticRowSink<'a> {
    pub(super) ownership_complete: &'a mut bool,
    pub(super) exact_sites: &'a mut super::exact_sites::ExactDeclarationSites,
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
    /// Authenticated bytes from the same selected native-only frontier.
    pub(super) native_source: Option<&'a backend_engine::builtin::VerifiedNativeSource>,
    /// Selected image's capture-time, source-bound line offsets.
    pub(super) native_ts_evidence: Option<&'a backend_engine::builtin::NativeSourceEvidence>,
}

impl SemanticRowSink<'_> {
    pub(super) fn native_projection(&self) -> Option<super::native_sites::NativeProjection<'_>> {
        if self.stale {
            return None;
        }
        self.native_source
            .filter(|source| {
                matches!(
                    source.profile(),
                    backend_semantic::vocabulary::LanguageProfile::C(_)
                        | backend_semantic::vocabulary::LanguageProfile::Cxx(_)
                )
            })
            .map(|source| super::native_sites::NativeProjection {
                source,
                max_sites: self.capacity.saturating_sub(self.rows.len()),
                max_working_bytes: *self.remaining_bytes,
            })
    }

    pub(super) fn native_csharp_projection(
        &self,
    ) -> Option<super::csharp_native_sites::NativeCSharpProjection<'_>> {
        if self.stale {
            return None;
        }
        self.native_source
            .filter(|source| {
                matches!(
                    source.profile(),
                    backend_semantic::vocabulary::LanguageProfile::CSharp(_)
                )
            })
            .map(
                |source| super::csharp_native_sites::NativeCSharpProjection {
                    source,
                    max_sites: self.capacity.saturating_sub(self.rows.len()),
                    max_working_bytes: *self.remaining_bytes,
                },
            )
    }
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
    sources: &'a IndexedSources,
    files: BTreeMap<([u8; 32], String), &'a super::super::ProductSourceRecord>,
    cancelled: Option<&'a std::sync::atomic::AtomicBool>,
}

impl<'a> StructuralSites<'a> {
    pub(super) fn of(sources: &'a IndexedSources) -> Result<Self, BuiltinModelError> {
        Self::of_controlled(sources, None)
    }

    pub(super) fn of_controlled(
        sources: &'a IndexedSources,
        cancelled: Option<&'a std::sync::atomic::AtomicBool>,
    ) -> Result<Self, BuiltinModelError> {
        super::super::check_search_preparation(cancelled, "structural-sites")?;
        let mut files = BTreeMap::new();
        for (_, record) in &sources.files {
            super::super::check_search_preparation(cancelled, "structural-sites-file")?;
            let file = record
                .file_fields()
                .ok_or_else(|| BuiltinModelError("expected a source file record".to_owned()))?;
            if files
                .insert((file.project, file.path.to_owned()), record)
                .is_some()
            {
                return Err(BuiltinModelError(
                    "source frontier has duplicate project paths".to_owned(),
                ));
            }
        }
        Ok(Self {
            sources,
            files,
            cancelled,
        })
    }

    fn declarations(
        &self,
        project: &[u8; 32],
        path: &str,
    ) -> Result<Vec<backend_compile::SourceDeclaration>, BuiltinModelError> {
        let Some(record) = self.files.get(&(*project, path.to_owned())) else {
            return Ok(Vec::new());
        };
        super::structural::complete_declarations_for_file_controlled(
            self.sources,
            record,
            self.cancelled,
        )
    }

    /// Returns the declarations of a file that provably hold the same bytes a
    /// compiled image was built from, and none when they might not.
    ///
    /// A file whose persisted identity is absent or differs cannot be paired
    /// without guessing, which is exactly when `project_image_rows` is given
    /// no sites for a stale image.
    pub(super) fn declarations_of_source(
        &self,
        project: &[u8; 32],
        path: &str,
        source: backend_version::ContentId<backend_version::SourceFactDomain>,
    ) -> Result<Vec<backend_compile::SourceDeclaration>, BuiltinModelError> {
        let current = self
            .files
            .get(&(*project, path.to_owned()))
            .and_then(|record| record.file_fields())
            .and_then(|file| file.source_identity);
        if current != Some(source) {
            return Ok(Vec::new());
        }
        self.declarations(project, path)
    }
}
/// The kind of every entity of one image, and the written declaration each
/// is paired with.
///
/// The kind is derived from the image alone (`kinds::image_entity_kind`), so
/// the compiler-backed rows and the structural rows of the same file speak one
/// vocabulary; a paired declaration then settles the spelling the grammar
/// itself decided (`kinds::kind_with_site`).
pub(super) struct EntityKinds<'site> {
    /// The product kind each entity is shown as before its twin is known.
    derived: BTreeMap<DeclarationIdentity, backend_compile::DeclarationKind>,
    /// Each entity's written declaration, when one is known.
    sites: BTreeMap<DeclarationIdentity, &'site backend_compile::SourceDeclaration>,
    pub(super) exact: BTreeMap<DeclarationIdentity, backend_compile::SourceBinding>,
}

impl EntityKinds<'_> {
    /// A singular compiler/syntax extent proves this source location even if
    /// the bounded excerpt cannot recover a larger enclosing source window.
    pub(super) fn exact_location(
        &self,
        profile: backend_semantic::vocabulary::LanguageProfile,
        identity: DeclarationIdentity,
        path: &str,
    ) -> Option<backend_compile::SourceLocation> {
        if !matches!(
            profile,
            backend_semantic::vocabulary::LanguageProfile::TypeScript(_)
        ) {
            return None;
        }
        self.exact.get(&identity)?;
        let site = self.sites.get(&identity)?;
        backend_compile::SourceLocation::new(path, site.line()).ok()
    }

    /// Returns the kind one entity is shown as.
    pub(super) fn kind_of(
        &self,
        identity: DeclarationIdentity,
        item: backend_semantic::ir::ItemKind,
    ) -> backend_compile::DeclarationKind {
        let derived = self
            .derived
            .get(&identity)
            .copied()
            .unwrap_or_else(|| declaration_kind(item));
        match self.sites.get(&identity) {
            Some(site) => kinds::kind_with_site(derived, item, site.kind()),
            None => derived,
        }
    }
}

/// Derives every entity's kind and pairs each with the written declaration of
/// the same file it is (see `kinds::pair_sites`).
///
/// Implementation blocks and parameters are not paired: they have no
/// structural twin. The tag query emits declarations, and only references for
/// implementations, and it does not see parameters at all. Counting them made
/// every type with an `impl` (and every field named like a parameter) count
/// more semantic entities than structural declarations, and the count rule
/// then dropped the type's own site along with theirs.
pub(super) fn entity_kinds<'site, Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    profile: backend_semantic::vocabulary::LanguageProfile,
    reader: &Reader,
    session: &DocumentationSession<'_, Reader>,
    declarations: &[&'site backend_compile::SourceDeclaration],
) -> Result<EntityKinds<'site>, BuiltinModelError> {
    entity_kinds_controlled(profile, reader, session, declarations, None)
}

pub(super) fn entity_kinds_controlled<
    'site,
    Reader: backend_semantic::ir::SemanticReader + ?Sized,
>(
    profile: backend_semantic::vocabulary::LanguageProfile,
    reader: &Reader,
    session: &DocumentationSession<'_, Reader>,
    declarations: &[&'site backend_compile::SourceDeclaration],
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<EntityKinds<'site>, BuiltinModelError> {
    super::super::check_search_preparation(cancelled, "semantic-kinds")?;
    let language = backend_semantic::vocabulary::Language::from(profile);
    let mut derived = BTreeMap::new();
    let mut pairing = Vec::new();
    for entity in session.publication_entities() {
        super::super::check_search_preparation(cancelled, "semantic-kind-entity")?;
        let entity = entity
            .map_err(|error| BuiltinModelError(format!("project semantic declaration: {error}")))?;
        let identity = entity.entity.version.identity();
        let kind = kinds::image_entity_kind(profile, reader, session, &entity)
            .map_err(BuiltinModelError)?;
        derived.insert(identity, kind);
        if matches!(
            entity.entity.kind,
            backend_semantic::ir::ItemKind::Implementation
                | backend_semantic::ir::ItemKind::Parameter
        ) {
            continue;
        }
        let Some(name) = entity.name.named_bytes() else {
            // Structural declarations have no anonymous callable identifier.
            continue;
        };
        let holder = match entity.entity.parent {
            Some(parent) => session
                .entity(parent)
                .map_err(|error| BuiltinModelError(format!("project semantic parent: {error}")))?
                .name
                .named_bytes()
                .unwrap_or_default()
                .to_vec(),
            None => Vec::new(),
        };
        pairing.push(kinds::PairingEntity {
            identity,
            name: semantic_source_name(profile, declaration_family(kind), name),
            holder,
            item: entity.entity.kind,
            kind,
            start: entity.entity.source.map(|span| span.start()),
        });
    }
    super::super::check_search_preparation(cancelled, "semantic-kind-exact-sites")?;
    let exact = match reader.image_facts().provenance {
        backend_semantic::ir::ImageProvenance::Captured { scope, .. } => {
            match reader
                .atom(scope.path)
                .and_then(|path| std::str::from_utf8(path).ok())
            {
                Some(path) => super::exact_sites::declaration_sites(
                    profile,
                    reader,
                    session,
                    path,
                    declarations,
                )?,
                None => BTreeMap::new(),
            }
        }
        _ => BTreeMap::new(),
    };
    super::super::check_search_preparation(cancelled, "semantic-kind-pairing")?;
    let sites = if matches!(
        language,
        backend_semantic::vocabulary::Language::Go
            | backend_semantic::vocabulary::Language::TypeScript
    ) {
        let mut written = BTreeMap::new();
        for site in declarations {
            super::super::check_search_preparation(cancelled, "semantic-kind-written-site")?;
            if let backend_compile::SourceBindingIdentity::Exact(capture) = site.binding_identity()
            {
                written.insert(capture.binding(), *site);
            }
        }
        let mut sites = BTreeMap::new();
        for (identity, binding) in &exact {
            super::super::check_search_preparation(cancelled, "semantic-kind-exact-pair")?;
            if let Some(site) = written.get(binding) {
                sites.insert(*identity, *site);
            }
        }
        if language == backend_semantic::vocabulary::Language::TypeScript {
            // Presentation pairing is not declaration identity. Preserve its
            // existing metadata fallback where extent proof is unavailable,
            // without allowing a fallback to consume an exactly owned site.
            let mut unresolved = Vec::new();
            for entity in pairing {
                super::super::check_search_preparation(cancelled, "semantic-kind-unresolved")?;
                if !exact.contains_key(&entity.identity) {
                    unresolved.push(entity);
                }
            }
            let mut bound = BTreeSet::new();
            for binding in exact.values() {
                super::super::check_search_preparation(cancelled, "semantic-kind-bound")?;
                bound.insert(*binding);
            }
            let mut remaining = Vec::new();
            for site in declarations.iter().copied() {
                super::super::check_search_preparation(cancelled, "semantic-kind-remaining")?;
                if !matches!(site.binding_identity(),
                    backend_compile::SourceBindingIdentity::Exact(capture)
                        if bound.contains(&capture.binding()))
                {
                    remaining.push(site);
                }
            }
            // The existing image-bounded pairing call is atomic; its result is
            // disposable until the next checkpoint admits this selected lifetime.
            for (identity, site) in kinds::pair_sites(language, &unresolved, &remaining) {
                super::super::check_search_preparation(cancelled, "semantic-kind-paired-site")?;
                sites.insert(identity, site);
            }
        }
        sites
    } else {
        kinds::pair_sites(language, &pairing, declarations)
    };
    Ok(EntityKinds {
        derived,
        sites,
        exact,
    })
}

/// Java's semantic type names are qualified while its structural tags capture
/// the declared identifier. Restrict this projection to Java's type family;
/// the existing count and span checks below decide whether the simple name is
/// unique enough to attach a source site.
fn semantic_source_name(
    profile: backend_semantic::vocabulary::LanguageProfile,
    family: u8,
    name: &[u8],
) -> Vec<u8> {
    if profile.language() == backend_semantic::vocabulary::Language::Java
        && family == 1
        && let Ok(name) = std::str::from_utf8(name)
    {
        return name.rsplit('.').next().unwrap_or(name).as_bytes().to_vec();
    }
    name.to_vec()
}

pub(super) struct SemanticRowContent {
    pub(super) document: Vec<Fragment>,
    pub(super) signature: Option<String>,
    encoded_bytes: usize,
    /// The external declarations the documentation links to (`[`f64::NAN`]`),
    /// each with its handle in the image. The view gives each an external
    /// row, so the search corpus must give each an external fact.
    pub(super) documentation_targets: Vec<(ExternalTargetIdentity, ExternalId)>,
}

/// Projects one semantic image into rows whose basis is stamped on admission.
pub(super) fn project_image_rows(
    image: &SemanticImageView<'_>,
    image_identity: [u8; 32],
    project: &IndexedProject,
    profile: backend_semantic::vocabulary::LanguageProfile,
    basis: backend_engine::Basis,
    stale: bool,
    path: &str,
    site_declarations: &[&backend_compile::SourceDeclaration],
    native: Option<super::native_sites::NativeProjection<'_>>,
    native_csharp: Option<super::csharp_native_sites::NativeCSharpProjection<'_>>,
) -> Result<ProjectedImage, BuiltinModelError> {
    project_image_rows_with_native_ts(image, image_identity, project, profile, basis, stale,
        path, site_declarations, native, native_csharp, None)
}

pub(super) fn project_image_rows_with_native_ts(
    image: &SemanticImageView<'_>,
    image_identity: [u8; 32],
    project: &IndexedProject,
    profile: backend_semantic::vocabulary::LanguageProfile,
    basis: backend_engine::Basis,
    stale: bool,
    path: &str,
    site_declarations: &[&backend_compile::SourceDeclaration],
    native: Option<super::native_sites::NativeProjection<'_>>,
    native_csharp: Option<super::csharp_native_sites::NativeCSharpProjection<'_>>,
    native_ts: Option<&backend_engine::builtin::NativeSourceEvidence>,
) -> Result<ProjectedImage, BuiltinModelError> {
    let session = DocumentationSession::new(image);
    let kinds = entity_kinds(profile, image, &session, site_declarations)?;
    let sites = &kinds.sites;
    let native_sites = if stale {
        None
    } else {
        native
            .filter(|proof| {
                proof.source.relative_path() == path && proof.source.profile() == profile
            })
            .map(|proof| {
                super::native_sites::prove_native_sites(
                    image,
                    project.package.to_bytes(),
                    proof.source,
                    proof.max_sites,
                    proof.max_working_bytes,
                )
            })
            .transpose()?
    };
    let native_csharp_sites = if stale {
        None
    } else {
        native_csharp
            .filter(|proof| {
                proof.source.relative_path() == path && proof.source.profile() == profile
            })
            .map(|proof| {
                super::csharp_native_sites::prove_native_csharp_sites(
                    image,
                    project.package.to_bytes(),
                    proof,
                )
            })
            .transpose()?
    };
    let temporary_bytes = native_sites
        .as_ref()
        .map_or(0, |sites| sites.working_bytes)
        .saturating_add(
            native_csharp_sites
                .as_ref()
                .map_or(0, |sites| sites.working_bytes),
        );
    let locations = super::retained_locations::RetainedLocations::for_entities(
        profile,
        image,
        &session,
        site_declarations,
        &kinds.exact,
    )
    .map_err(BuiltinModelError)?;
    let mut placements = BTreeMap::new();
    let mut coordinates = super::identity::semantic_coordinates(
        &project.label,
        session.publication_entities().map(|entity| {
            let entity = entity.map_err(|error| {
                BuiltinModelError(format!("project semantic declaration: {error}"))
            })?;
            let identity = entity.entity.version.identity();
            let placement = locations.place(image, &entity.entity);
            let native_line = native_sites
                .as_ref()
                .and_then(|sites| sites.get(&identity))
                .map(|site| site.line)
                .or_else(|| {
                    native_csharp_sites
                        .as_ref()
                        .and_then(|sites| sites.get(&identity))
                        .map(|site| site.line)
                });
            let location = placement
                .as_ref()
                .map(|placement| placement.location.clone())
                .or_else(|| {
                    native_line.and_then(|line| {
                        backend_compile::SourceLocation::new(path, line).ok()
                    })
                })
                .or_else(|| kinds.exact_location(profile, identity, path));
            placements.insert(identity, placement);
            Ok((identity, entity.name, location))
        }),
        None,
    )?;
    let canonical_identities = coordinates.keys().copied().collect::<BTreeSet<_>>();
    let mut projected = Vec::new();
    for entity in session.publication_entities() {
        let entity = entity
            .map_err(|error| BuiltinModelError(format!("project semantic declaration: {error}")))?;
        let identity = entity.entity.version.identity();
        let placement = placements.remove(&identity).flatten();
        let coordinate = coordinates.remove(&identity).ok_or_else(|| {
            BuiltinModelError("semantic declaration has no exact coordinate".to_owned())
        })?;
        let symbol = semantic_symbol(project.package, identity);
        let content =
            semantic_row_content(profile, image, &entity, project.package, image_identity)?;
        let parent_kind = match entity.entity.parent {
            Some(parent) => Some(
                session
                    .entity(parent)
                    .map_err(|error| {
                        BuiltinModelError(format!("project semantic parent: {error}"))
                    })?
                    .entity
                    .kind,
            ),
            None => None,
        };
        let site_facts = sites
            .get(&identity)
            .map_or(backend_compile::DeclarationFacts::UNOBSERVED, |site| {
                site.facts().clone()
            });
        let facts = semantic_facts(
            backend_semantic::vocabulary::Language::from(profile),
            image,
            &entity.entity,
            parent_kind,
            &content.document,
        )?
        .or(&site_facts);
        let row_bytes = coordinate
            .len()
            .checked_add(content.encoded_bytes)
            .and_then(|bytes| bytes.checked_add(facts.text_bytes()))
            .ok_or_else(|| {
                BuiltinModelError("project semantic row byte count overflow".to_owned())
            })?;
        let mut charges = vec![Charge::Sub(row_bytes)];
        let mut document = content.document;
        if stale {
            charges.push(Charge::Add(STALE_NOTE.len()));
            document.push(Fragment::Text(STALE_NOTE.to_owned()));
        }
        let mut row = Row::in_package(RowId::Symbol(symbol), basis, project.package, coordinate)
            .try_with_identity_preimage(&semantic_identity(project.package, identity))
            .map_err(|error| BuiltinModelError(format!("semantic row identity preimage: {error}")))?
            .with_kind(kinds.kind_of(identity, entity.entity.kind))
            .with_document(document)
            .with_facts(facts);
        if let Some(placement) = placement {
            charges.push(Charge::Sub(placement.excerpt.text().map_or(0, str::len)));
            row = row
                .with_source(placement.location)
                .with_excerpt(placement.excerpt);
        } else if let Some(site) = native_sites
            .as_ref()
            .and_then(|sites| sites.get(&identity))
        {
            let excerpt = site.excerpt();
            charges.push(Charge::Sub(excerpt.text().map_or(0, str::len)));
            let location =
                backend_compile::SourceLocation::new(path, site.line).map_err(|error| {
                    BuiltinModelError(format!("native semantic declaration source site: {error}"))
                })?;
            row = row.with_source(location).with_excerpt(excerpt);
        } else if let Some(site) = native_csharp_sites
            .as_ref()
            .and_then(|sites| sites.get(&identity))
        {
            let excerpt = site.excerpt();
            charges.push(Charge::Sub(excerpt.text().map_or(0, str::len)));
            let location =
                backend_compile::SourceLocation::new(path, site.line).map_err(|error| {
                    BuiltinModelError(format!("native C# semantic declaration source site: {error}"))
                })?;
            row = row.with_source(location).with_excerpt(excerpt);
        } else if let Some(location) = native_ts.filter(|_| !stale).and_then(|claim| {
            use backend_semantic::ir::{DeclarationPublicationRole, FactAvailability};
            let span = entity.entity.source?;
            if !claim.admits_image() || claim.path != path
                || entity.entity.authority.publication != DeclarationPublicationRole::PrimaryOwned
                || entity.entity.authority.source != FactAvailability::Captured
                || entity.entity.authority.source_file != FactAvailability::Captured
                || span.is_empty() || span.end() > claim.byte_length
                || image.atom(span.file()) != Some(path.as_bytes())
            { return None; }
            backend_compile::SourceLocation::new(path, claim.line_at(span.start())?).ok()
        }) {
            row = row.with_source(location);
        } else if let Some(site) = sites.get(&identity) {
            charges.push(Charge::Sub(site.source_excerpt().text().map_or(0, str::len)));
            let location =
                backend_compile::SourceLocation::new(path, site.line()).map_err(|error| {
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
            if session.owns_declaration(&parent.entity) {
                if !canonical_identities.contains(&parent_identity) {
                    return Err(BuiltinModelError(
                        "project semantic parent is outside the canonical entity closure"
                            .to_owned(),
                    ));
                }
                row = row.with_parent(semantic_symbol(project.package, parent_identity));
            }
        }
        projected.push(ProjectedRow {
            exact_binding: kinds.exact.get(&identity).copied(),
            row,
            duplicate: DuplicatePolicy::Error,
            charges,
        });
        let mut external = Vec::new();
        for (_, link) in image.links_from(entity.entity.id) {
            let LinkTarget::External(target) = link.target else {
                continue;
            };
            external.push((
                ExternalTargetIdentity::capture(image, target).map_err(|error| {
                    BuiltinModelError(format!(
                        "identify project semantic external target: {error}"
                    ))
                })?,
                target,
            ));
        }
        // A documentation link to an external declaration (`[`f64::NAN`]`)
        // targets the same external row a graph link would. Without that row
        // the link dangles: its target has no claim in the view's
        // certificate, and the owner could not reopen its own view journal
        // ("snapshot row document: missing producer key commitment").
        external.extend(content.documentation_targets.iter().copied());
        for (identity, target) in external {
            let symbol = external_semantic_symbol(project.package, image_identity, identity);
            // The captured spelling is presentation only. Keep the exact
            // scoped external identity and do not infer a local declaration,
            // source site, kind, or package membership from its name.
            let display = foreign_display_name(image, target)?
                .unwrap_or_else(|| "external semantic target".to_owned());
            let label = external_semantic_coordinate(&project.label, symbol, &display);
            let label_bytes = label.len();
            let preimage = backend_engine::encode_id(
                identity
                    .in_scope(project.package.to_bytes(), image_identity)
                    .as_bytes(),
            );
            projected.push(ProjectedRow {
                exact_binding: None,
                row: Row::new(RowId::Symbol(symbol), basis, label)
                    .with_document(Box::<[Fragment]>::default())
                    .try_with_identity_preimage(&preimage)
                    .map_err(|error| {
                        BuiltinModelError(format!("external row identity preimage: {error}"))
                    })?,
                duplicate: DuplicatePolicy::Skip,
                charges: vec![Charge::Sub(label_bytes)],
            });
        }
    }
    Ok(ProjectedImage {
        rows: projected,
        ownership_complete: session.publication_ownership_available(),
        temporary_bytes,
    })
}

/// Projects one image and admits those rows into `sink`.
pub(super) fn append_image_rows(
    image: &SemanticImageView<'_>,
    project: &IndexedProject,
    profile: backend_semantic::vocabulary::LanguageProfile,
    sink: &mut SemanticRowSink<'_>,
) -> Result<(), BuiltinModelError> {
    let image_identity = *blake3::hash(image.as_ref()).as_bytes();
    let projected = project_image_rows(
        image,
        image_identity,
        project,
        profile,
        sink.initial.basis(),
        sink.stale,
        sink.path,
        sink.site_declarations,
        sink.native_projection(),
        sink.native_csharp_projection(),
    )?;
    super::image_rows::apply_projected_image(&projected, sink)
}

/// The declaration facts the semantic lane itself observed for one entity.
///
/// * Deprecation is observed wherever the authority captured the entity's
///   attribute plane (Rust `#[deprecated]` staged by the lowering, Java
///   annotations, C# attributes, Python decorators), completed by the
///   language's documentation convention (a Javadoc `@deprecated` supplies
///   the words for `@Deprecated(since = …)`). TypeScript and Go state it
///   only in documentation, so a captured documentation plane observes it.
///   C and C++ attributes are not modeled by the Clang authority, so that
///   lane leaves it unobserved.
/// * Obligation is observed for Go, whose interface methods are exactly the
///   functions parented to an interface, and for Python's
///   `@abstractmethod`. Every other obligation is read from the paired
///   structural declaration, which parses bodies and modifiers.
///
/// A fact left unobserved here falls back to the paired structural site.
fn semantic_facts<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    language: backend_semantic::vocabulary::Language,
    reader: &Reader,
    entity: &backend_semantic::ir::SemanticEntity,
    parent_kind: Option<backend_semantic::ir::ItemKind>,
    document: &[Fragment],
) -> Result<backend_compile::DeclarationFacts, BuiltinModelError> {
    use backend_compile::{DeclarationFacts, Fact, Obligation};
    use backend_semantic::ir::{FactAvailability, ItemKind};
    use backend_semantic::vocabulary::Language;
    let mut attributes = Vec::new();
    let attributes_captured = entity.authority.attributes == FactAvailability::Captured;
    if attributes_captured {
        let atoms = reader.atom_list(entity.attributes).ok_or_else(|| {
            BuiltinModelError("project semantic attribute list is dangling".to_owned())
        })?;
        for atom in atoms {
            let bytes = reader.atom(atom).ok_or_else(|| {
                BuiltinModelError("project semantic attribute atom is dangling".to_owned())
            })?;
            attributes.push(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    let documentation_captured = entity.authority.documentation == FactAvailability::Captured;
    let prose = document_text(document);
    let documented = documentation_captured
        .then(|| backend_compile::deprecation_in_documentation(language, &prose))
        .flatten();
    let deprecation = if attributes_captured {
        let written = attributes
            .iter()
            .find_map(|text| backend_compile::deprecation_in_attribute(language, text));
        Fact::observed(match written {
            Some(notice) => Some(notice.completed_by(documented.as_ref())),
            None => documented,
        })
    } else if documentation_captured && matches!(language, Language::TypeScript | Language::Go) {
        Fact::observed(documented)
    } else {
        Fact::Unobserved
    };
    let obligation = match language {
        Language::Go if entity.kind == ItemKind::Function => {
            Fact::observed((parent_kind == Some(ItemKind::Trait)).then_some(Obligation::Required))
        }
        Language::Python
            if attributes_captured
                && attributes
                    .iter()
                    .any(|text| backend_compile::is_abstract_method_decorator(text)) =>
        {
            Fact::Present(Obligation::Required)
        }
        _ => Fact::Unobserved,
    };
    Ok(DeclarationFacts {
        deprecation,
        obligation,
    })
}

/// A document's prose with one `\n` per break, for convention readers.
fn document_text(document: &[Fragment]) -> String {
    let mut text = String::new();
    for fragment in document {
        match fragment {
            Fragment::Text(value) | Fragment::Code(value) => text.push_str(value),
            Fragment::Link { label, .. } => text.push_str(label),
            Fragment::Break => text.push('\n'),
        }
    }
    text
}

/// The signature a compiler-backed declaration shows when no written site
/// supplies one: its semantic type, written in its language's own syntax.
///
/// A callable reads as the declaration header its language writes (`fn
/// name(a: u32) -> bool`), anything else as its type (`&str`, `*Config`,
/// `list[int]`). A type the image cannot name completely is not shown at all
/// rather than shown as the compiler's internal encoding, and a declaration
/// whose type is only itself (a record's nominal type) has no signature to
/// show.
fn semantic_signature<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    profile: backend_semantic::vocabulary::LanguageProfile,
    entity: &backend_engine::application::DocumentationEntity<'_, Reader>,
) -> Result<Option<String>, BuiltinModelError> {
    // These declarations are what their own type names (an enum's variant has
    // the enum for a type, a module has none), so the type says nothing about
    // how to use them.
    if matches!(
        entity.entity.kind,
        backend_semantic::ir::ItemKind::Variant
            | backend_semantic::ir::ItemKind::Implementation
            | backend_semantic::ir::ItemKind::Module
            | backend_semantic::ir::ItemKind::Namespace
            | backend_semantic::ir::ItemKind::Macro
            | backend_semantic::ir::ItemKind::Reexport
    ) {
        return Ok(None);
    }
    let Some(semantic_type) = entity
        .semantic_type()
        .map_err(|error| BuiltinModelError(format!("project semantic type: {error}")))?
    else {
        return Ok(None);
    };
    let type_depth = NonZeroUsize::new(MAX_SEMANTIC_TYPE_DEPTH)
        .ok_or_else(|| BuiltinModelError("semantic type depth bound must be nonzero".to_owned()))?;
    let limits = backend_semantic::ir::CanonicalTypeRenderLimits::new(type_depth);
    let name = entity
        .name
        .named_bytes()
        .and_then(|bytes| std::str::from_utf8(bytes).ok());
    let rendered = match name {
        Some(name) if entity.entity.kind == backend_semantic::ir::ItemKind::Function => {
            semantic_type.render_source_header(profile, name, limits)
        }
        _ => semantic_type.render_source(profile, limits),
    };
    Ok(rendered
        .filter(|text| text.len() <= MAX_SEMANTIC_SIGNATURE_BYTES && Some(text.as_str()) != name))
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
    let signature = semantic_signature(profile, entity)?;
    let encoded_bytes = documentation_bytes
        .checked_add(signature.as_ref().map_or(0, String::len))
        .ok_or_else(|| BuiltinModelError("project semantic row byte count overflow".to_owned()))?;
    // Preparation has already walked and validated every selected semantic
    // fact through the bounded CountWriter and supplied the exact encoded
    // length. Search rows never expose this injective machine encoding: they
    // use the readable signature and declaration documentation below. Writing
    // it into a throwaway allocation repeated the full IR traversal for every
    // declaration while retaining the allocation until this function returned.
    // Keep the same type-depth and byte-size checks without materializing it.
    let mut document = Vec::with_capacity(documentation.len());
    let mut documentation_targets = Vec::new();
    for fragment in documentation {
        let (fragment, external) =
            documentation_fragment(reader, package, image_identity, fragment)?;
        document.push(fragment);
        documentation_targets.extend(external);
    }
    Ok(SemanticRowContent {
        document,
        signature,
        encoded_bytes,
        documentation_targets,
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

/// One documentation fragment as a row fragment, and the external
/// declaration it links to, when it does.
fn documentation_fragment<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &Reader,
    package: backend_engine::PackageKey,
    image: [u8; 32],
    fragment: DocumentationFragment<'_>,
) -> Result<(Fragment, Option<(ExternalTargetIdentity, ExternalId)>), BuiltinModelError> {
    Ok(match fragment {
        DocumentationFragment::Text(text) => (Fragment::Text(text.to_owned()), None),
        DocumentationFragment::Code(code) => (Fragment::Code(code.to_owned()), None),
        DocumentationFragment::SoftBreak | DocumentationFragment::HardBreak => {
            (Fragment::Break, None)
        }
        DocumentationFragment::Link { label, target } => {
            if let backend_engine::application::DocumentationTarget::Local(target) = target {
                if !DocumentationSession::new(reader).owns_declaration(&target.entity) {
                    return Ok((Fragment::Text(label.to_owned()), None));
                }
            }
            let (target, external) = match target {
                backend_engine::application::DocumentationTarget::Local(target) => (
                    semantic_symbol(package, target.entity.version.identity()),
                    None,
                ),
                backend_engine::application::DocumentationTarget::External { id, .. } => {
                    let identity =
                        ExternalTargetIdentity::capture(reader, id).map_err(|error| {
                            BuiltinModelError(format!(
                                "identify documentation external target: {error}"
                            ))
                        })?;
                    (
                        external_semantic_symbol(package, image, identity),
                        Some((identity, id)),
                    )
                }
            };
            (
                Fragment::Link {
                    label: label.to_owned(),
                    target,
                },
                external,
            )
        }
    })
}

#[cfg(test)]
mod tests {
    fn median(mut body: impl FnMut() -> usize) -> u128 {
        for _ in 0..2 {
            let _ = body();
        }
        let mut samples = Vec::new();
        for _ in 0..9 {
            let started = std::time::Instant::now();
            let _ = body();
            samples.push(started.elapsed().as_nanos());
        }
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    use super::super::super::IndexedProject;
    use super::publication_seek_bounds;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[test]
    fn native_frontier_requires_canonical_membership_without_syntax_facts() -> Result<(), String> {
        use backend_compile::{SourceDeclaration, SourceLanguage};
        use backend_engine::{PackageKey, ProductSourceRecord, product_source_file_key};
        use backend_version::{ContentId, SourceFactDomain};

        let package = PackageKey::from_value("/native-fixture");
        let path = "same.ts";
        let key = product_source_file_key(package.to_bytes(), path);
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"source bytes");
        let file = ProductSourceRecord::file(
            package.to_bytes(),
            path,
            SourceLanguage::TypeScript,
            [1; 32],
            [2; 32],
            Vec::<SourceDeclaration>::new(),
        )?
        .with_source_identity(identity)?;
        let project = super::super::super::IndexedProject {
            package,
            label: "/native-fixture".to_owned(),
            files: Arc::from([key]),
        };
        let mut sources = super::super::super::IndexedSources {
            projects: [(package.to_bytes(), project)].into_iter().collect(),
            files: vec![(key, file.clone())],
            cargo_aliases: BTreeMap::new(),
            source_snapshot: None,
        };
        assert_eq!(
            super::native_frontier_identity(
                &sources,
                package,
                path,
                Some(SourceLanguage::TypeScript),
                None,
            )
            .map_err(|e| e.0)?,
            Some(identity)
        );
        assert_eq!(
            super::native_frontier_identity(
                &sources,
                package,
                path,
                Some(SourceLanguage::Go),
                None
            )
            .map_err(|e| e.0)?,
            None
        );
        assert_eq!(
            super::native_frontier_identity(
                &sources,
                package,
                path,
                Some(SourceLanguage::TypeScript),
                Some(backend_semantic::vocabulary::LanguageProfile::TypeScript(
                    backend_semantic::vocabulary::TypeScriptSource::Tsx,
                )),
            )
            .map_err(|e| e.0)?,
            None,
            "a TS file cannot borrow a selected TSX publication",
        );
        let rust_path = "native.rs";
        let rust_key = product_source_file_key(package.to_bytes(), rust_path);
        let rust_file = ProductSourceRecord::file(
            package.to_bytes(),
            rust_path,
            SourceLanguage::Rust,
            [3; 32],
            [4; 32],
            Vec::<SourceDeclaration>::new(),
        )?
        .with_source_identity(identity)?;
        let project = sources
            .projects
            .get_mut(&package.to_bytes())
            .ok_or("missing project")?;
        let mut members = vec![key, rust_key];
        members.sort_unstable();
        project.files = Arc::from(members);
        sources.files.push((rust_key, rust_file));
        assert_eq!(
            super::native_frontier_identity(
                &sources,
                package,
                rust_path,
                Some(SourceLanguage::Rust),
                Some(backend_semantic::vocabulary::LanguageProfile::Rust(
                    backend_semantic::vocabulary::RustEdition::Rust2021,
                )),
            )
            .map_err(|e| e.0)?,
            Some(identity),
            "Rust profile lanes retain admitted file identity without syntax facts",
        );
        let structural = super::selected_typescript_structural_files(&sources, package).map_err(|e| e.0)?;
        assert_eq!(structural.get(path), Some(&(identity, [1; 32])));
        assert!(!structural.contains_key(rust_path));
        sources.files.push((key, file));
        assert!(super::selected_typescript_structural_files(&sources, package).is_err());
        assert!(super::native_frontier_identity(&sources, package, path, None, None).is_err());
        Ok(())
    }

    #[test]
    fn native_ir_owner_extents_bind_selected_identity_without_syntax() -> Result<(), String> {
        use backend_semantic::ir::{
            BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity,
            EntityAuthorityFacts, EntityVersion, FactAvailability, IrBuilder, ItemKind,
            SemanticImageView, SemanticReader as _, SourceSpan, TreeItemInput, VariantFingerprint,
            Visibility,
            encode_full_semantic_image, full_semantic_image_len,
        };
        let path = "native.rs";
        let host = "x".repeat(100);
        let source_identity =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                host.as_bytes(),
            );
        let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
            backend_semantic::vocabulary::RustEdition::Rust2024,
        );
        let coordinate = backend_semantic::vocabulary::PackageUrl::parse(
            "pkg:cargo/native-fixture@1.0.0".to_owned(),
        )
        .map_err(|e| e.to_string())?;
        let recipe = backend_semantic::vocabulary::CompileRecipeFact::derive(
            profile,
            backend_semantic::vocabulary::Stage::LowerIr,
            backend_semantic::vocabulary::NativeTool::Rustc,
            source_identity,
            backend_version::ContentId::<backend_version::ToolchainDomain>::from_canonical_bytes(
                b"fixture compiler",
            ),
        );
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(
                backend_semantic::ir::SourceIdentity {
                    identity: source_identity,
                    byte_len: u32::try_from(host.len()).map_err(|e| e.to_string())?,
                },
                recipe,
                &coordinate,
                path,
            )
            .map_err(|e| e.to_string())?;
        let file = builder
            .intern_atom(path.as_bytes())
            .map_err(|e| e.to_string())?;
        let ids = [1_u8, 2, 3, 4, 5, 6].map(|byte| DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([byte; 16]),
            variant: VariantFingerprint::from_raw([byte; 16]),
        });
        let versions = ids.map(|id| EntityVersion {
            family: id.family,
            variant: id.variant,
            core_payload: CorePayloadHash::from_raw([9; 16]),
        });
        let authority = EntityAuthorityFacts {
            source: FactAvailability::Captured,
            source_file: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"alpha",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: SourceSpan::new(file, 10, 30),
                extension: None,
            },
            TreeItemInput {
                name: b"bravo",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: SourceSpan::new(file, 40, 60),
                extension: None,
            },
            TreeItemInput {
                name: b"charlie",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: SourceSpan::new(file, 80, 80),
                extension: None,
            },
            TreeItemInput {
                name: b"unknown",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: EntityAuthorityFacts {
                    visibility: FactAvailability::Captured,
                    ..EntityAuthorityFacts::default()
                },
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"spanless_owned",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: EntityAuthorityFacts {
                    publication: backend_semantic::ir::DeclarationPublicationRole::PrimaryOwned,
                    visibility: FactAvailability::Captured,
                    ..EntityAuthorityFacts::default()
                },
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"spanless_reference",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: EntityAuthorityFacts {
                    publication: backend_semantic::ir::DeclarationPublicationRole::ReferenceOnly,
                    visibility: FactAvailability::Captured,
                    ..EntityAuthorityFacts::default()
                },
                parent: None,
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
                versions: &versions,
                items: &items,
                links: &[],
            })
            .map_err(|e| e.to_string())?;
        let ir = builder.finish().map_err(|e| e.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|e| e.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|e| e.to_string())?;
        let image = SemanticImageView::reopen(&bytes).map_err(|e| e.to_string())?;
        let key = backend_engine::builtin::ProductSemanticPublicationKey::new(
            backend_engine::PackageReference::parse("/fixture".to_owned())
                .map_err(|e| e.to_string())?,
            coordinate,
            profile,
        )
        .map_err(|e| e.to_string())?;
        key.admit_image(&image).map_err(|e| e.to_string())?;
        let package = key.package_key();
        assert_eq!(
            super::compiled_source(&image).map_err(|e| e.0)?,
            (path.to_owned(), source_identity)
        );
        let first = backend_engine::RowId::Symbol(super::semantic_symbol(package, ids[0]));
        let second = backend_engine::RowId::Symbol(super::semantic_symbol(package, ids[1]));
        assert_eq!(
            super::native_extent_in_image(&image, package, path, first).map_err(|e| e.0)?,
            Some((10, 30))
        );
        assert_eq!(
            super::native_extent_in_image(&image, package, path, second).map_err(|e| e.0)?,
            Some((40, 60))
        );
        let raw_input =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(host.as_bytes())
                .to_bytes();
        backend_compile::VerifiedSourceText::admit_stored(
            host.as_bytes(),
            raw_input,
            source_identity,
        )
        .map_err(|e| e.to_string())?;
        let changed = format!("y{}", &host[1..]);
        assert!(
            backend_compile::VerifiedSourceText::admit_stored(
                changed.as_bytes(),
                raw_input,
                source_identity
            )
            .is_err()
        );
        let wrong_input =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(changed.as_bytes())
                .to_bytes();
        assert_eq!(
            backend_compile::VerifiedSourceText::admit_stored(
                host.as_bytes(),
                wrong_input,
                source_identity
            )
            .unwrap_err(),
            backend_compile::SourceVerificationError::InputRevisionMismatch
        );
        let wrong_source =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                changed.as_bytes(),
            );
        assert_eq!(
            backend_compile::VerifiedSourceText::admit_stored(
                host.as_bytes(),
                raw_input,
                wrong_source
            )
            .unwrap_err(),
            backend_compile::SourceVerificationError::SourceIdentityMismatch
        );
        let file_only = backend_engine::RowId::Symbol(super::semantic_symbol(package, ids[2]));
        assert_eq!(
            super::native_file_in_image(&image, package, path, file_only, false).map_err(|e| e.0)?,
            Some(None),
            "a captured file with an empty primary span can still establish file freshness"
        );
        assert!(super::native_extent_in_image(&image, package, path, file_only).is_err());
        // A span on a legacy Unavailable publication is insufficient for the
        // new native-only capability, while the existing extent path remains intact.
        assert_eq!(
            super::native_owned_file_in_image(&image, package, path, first).map_err(|e| e.0)?,
            None
        );
        let owned = backend_engine::RowId::Symbol(super::semantic_symbol(package, ids[4]));
        let owned_entry = super::native_owned_file_in_image(&image, package, path, owned)
            .map_err(|e| e.0)?
            .ok_or("owned entity missing")?;
        assert_eq!((owned_entry.declaration_identity, owned_entry.extent), (ids[4], None));
        assert_eq!(
            image.entity(owned_entry.entity_id).map(|entity| entity.version.identity()),
            Some(ids[4])
        );
        assert_eq!(
            super::native_file_in_image(&image, package, path, owned, false).map_err(|e| e.0)?,
            Some(None)
        );
        assert!(super::native_extent_in_image(&image, package, path, owned).is_err());
        assert!(super::native_file_in_image(&image, package, "other.rs", owned, false).is_err());
        let reference = backend_engine::RowId::Symbol(super::semantic_symbol(package, ids[5]));
        assert_ne!(
            super::native_file_in_image(&image, package, path, reference, false)
                .ok()
                .flatten(),
            Some(None)
        );
        let mut unproven = IrBuilder::new();
        unproven
            .add_borrowed_tree(BorrowedTree {
                versions: &versions[4..5],
                items: &items[4..5],
                links: &[],
            })
            .map_err(|e| e.to_string())?;
        let unproven = unproven.finish().map_err(|e| e.to_string())?;
        let mut unproven_bytes =
            vec![0; full_semantic_image_len(&unproven).map_err(|e| e.to_string())?];
        encode_full_semantic_image(&unproven, &mut unproven_bytes).map_err(|e| e.to_string())?;
        let unproven_image =
            SemanticImageView::reopen(&unproven_bytes).map_err(|e| e.to_string())?;
        assert!(super::native_file_in_image(&unproven_image, package, path, owned, false).is_err());
        let unknown = backend_engine::RowId::Symbol(super::semantic_symbol(package, ids[3]));
        assert!(super::native_file_in_image(&image, package, path, unknown, false).is_err());
        assert!(super::native_extent_in_image(&image, package, "other.rs", first).is_err());
        assert_eq!(
            super::native_extent_in_image(
                &image,
                backend_engine::PackageKey::from_value("/other"),
                path,
                first
            )
            .map_err(|e| e.0)?,
            None
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires an explicitly selected installed Go compiler"]
    fn actual_go_embedded_field_has_file_evidence_without_a_span() {
        use backend_engine::driver::{
            CompileControl, CompileOutput, CompileRequest, CompileScratch, DeclarationScope,
            ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile_semantic,
        };
        use backend_frontend_go::legacy::GoOracle;
        use backend_frontend_go::legacy::oracle::{
            GoOracleChildEnvironment, GoOracleConfiguration,
        };
        use backend_semantic::ir::{
            DeclarationPublicationRole, ItemKind, SemanticImageView, encode_full_semantic_image,
            full_semantic_image_len,
        };
        use backend_semantic::vocabulary::{
            GoVersion, LanguageProfile, NativeTool, PackageUrl, Stage,
        };
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};

        let go = std::path::PathBuf::from(
            std::env::var_os("COMPILER_GO_COMPILER").expect("selected Go compiler"),
        )
        .canonicalize()
        .expect("canonical Go compiler");
        let discovery = std::process::Command::new(&go)
            .args(["env", "-json", "GOROOT"])
            .env("GOENV", "off")
            .env("GOTOOLCHAIN", "local")
            .output()
            .expect("Go discovery");
        assert!(discovery.status.success());
        let discovery: serde_json::Value =
            serde_json::from_slice(&discovery.stdout).expect("Go discovery JSON");
        let goroot = std::path::PathBuf::from(discovery["GOROOT"].as_str().expect("GOROOT"))
            .canonicalize()
            .expect("canonical GOROOT");
        let root = tempfile::tempdir().expect("fixture root");
        let project = root.path().join("project");
        std::fs::create_dir(&project).expect("fixture project");
        let project = project.canonicalize().expect("canonical project");
        std::fs::write(
            project.join("go.mod"),
            "module example.com/fixture\n\ngo 1.23\n",
        )
        .expect("offline module");
        let path = "fixture.go";
        let primary = project.join(path);
        let source = b"package fixture\ntype Embedded struct{}\ntype Outer struct { Embedded }\n";
        std::fs::write(&primary, source).expect("primary source");
        let modules = root.path().join("modules");
        std::fs::create_dir(&modules).expect("offline module cache");
        let environment = GoOracleChildEnvironment::new(
            go.clone(),
            goroot,
            modules,
            root.path()
                .canonicalize()
                .expect("root")
                .join("build-cache"),
        )
        .expect("Go environment");
        let toolchain_identity = environment.toolchain_identity();
        let oracle = GoOracle::default()
            .with_configuration(
                GoOracleConfiguration::go_toolchain(go.clone()).expect("Go command"),
            )
            .with_child_environment(environment)
            .expect("closed Go environment");
        let witness = oracle
            .package_authority_witness_cancellable(&project, None)
            .expect("package witness");
        assert!(witness.is_complete());
        let native = oracle
            .authority_image_for_package_with_authority_witness_cancellable(
                &primary, &project, &witness, None,
            )
            .expect("native package image");
        let profile = LanguageProfile::Go(GoVersion::Go125);
        let coordinate = PackageUrl::parse("pkg:golang/example.com/fixture@1.0.0".to_owned())
            .expect("coordinate");
        let resolved =
            ResolvedToolchain::from_version(NativeTool::GoCompiler, &go, &toolchain_identity)
                .expect("toolchain identity");
        let cancelled = AtomicBool::new(false);
        let mut diagnostics = [];
        let mut output = vec![0; 1024 * 1024];
        let compiled = compile_semantic(
            CompileRequest {
                profile,
                stage: Stage::LowerIr,
                source,
                declaration_scope: DeclarationScope::for_package(&coordinate, path).expect("scope"),
                toolchain: ToolchainSelection::ResolvedNative(resolved),
                authority: SemanticAuthorityInput::Go { image: &native },
                control: CompileControl {
                    deadline: Instant::now() + Duration::from_secs(60),
                    cancelled: &cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut diagnostics,
                native_work: root.path(),
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        )
        .expect("actual native Go projection");
        let original_session = super::DocumentationSession::new(&compiled.ir);
        let original_fields: Vec<_> = original_session
            .publication_entities()
            .map(|item| item.expect("original entity"))
            .filter(|item| {
                item.entity.kind == ItemKind::Field
                    && item.name.named_bytes() == Some(b"Embedded".as_slice())
            })
            .collect();
        assert_eq!(original_fields.len(), 1);
        assert_eq!(original_fields[0].entity.source, None);
        assert_eq!(
            original_fields[0].entity.authority.publication,
            DeclarationPublicationRole::PrimaryOwned
        );
        let original_identity = original_fields[0].entity.version.identity();
        let mut bytes = vec![0; full_semantic_image_len(&compiled.ir).expect("image size")];
        encode_full_semantic_image(&compiled.ir, &mut bytes).expect("encode image");
        let image = SemanticImageView::reopen(&bytes).expect("reopen image");
        let key = backend_engine::builtin::ProductSemanticPublicationKey::new(
            backend_engine::PackageReference::parse("/fixture".to_owned()).expect("package"),
            coordinate,
            profile,
        )
        .expect("publication key");
        key.admit_image(&image).expect("admitted image");
        let session = super::DocumentationSession::new(&image);
        let fields: Vec<_> = session
            .publication_entities()
            .map(|item| item.expect("entity"))
            .filter(|item| {
                item.entity.kind == ItemKind::Field && item.name.named_bytes() == Some(b"Embedded".as_slice())
            })
            .collect();
        assert_eq!(fields.len(), 1, "select the actual embedded field");
        let field = &fields[0].entity;
        assert_eq!(field.version.identity(), original_identity);
        assert_eq!(field.source, None, "native producer omitted the field span");
        assert_eq!(
            field.authority.publication,
            DeclarationPublicationRole::PrimaryOwned
        );
        let package = key.package_key();
        let selected = backend_engine::RowId::Symbol(super::semantic_symbol(
            package,
            field.version.identity(),
        ));
        assert_eq!(
            super::native_file_in_image(&image, package, path, selected, false)
                .expect("file evidence"),
            Some(None)
        );
        assert!(super::native_extent_in_image(&image, package, path, selected).is_err());
        let (_, source_identity) = super::compiled_source(&image).expect("captured source");
        let input =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(source).to_bytes();
        backend_compile::VerifiedSourceText::admit_stored(source, input, source_identity)
            .expect("actual bytes match both domains");
    }

    #[test]
    fn java_qualified_semantic_type_pairs_to_its_authored_simple_name() -> Result<(), String> {
        use super::semantic_source_name;
        use backend_compile::{DeclarationKind, SourceDeclaration};
        use backend_semantic::ir::{
            DeclarationFamilyId, DeclarationIdentity, ItemKind, VariantFingerprint,
        };
        use backend_semantic::vocabulary::{JavaRelease, Language, LanguageProfile};

        let profile = LanguageProfile::Java(JavaRelease::Java25);
        let name = semantic_source_name(profile, 1, b"javax.annotation.Nonnull");
        let entity = super::kinds::PairingEntity {
            identity: DeclarationIdentity {
                family: DeclarationFamilyId::from_raw([1; 16]),
                variant: VariantFingerprint::from_raw([1; 16]),
            },
            name,
            holder: Vec::new(),
            item: ItemKind::Record,
            kind: DeclarationKind::Interface,
            start: Some(10),
        };
        let declaration = SourceDeclaration::at_path(
            "javax/annotation/Nonnull.java",
            "Nonnull",
            DeclarationKind::Interface,
            10,
            "public @interface Nonnull",
            "",
        )?;
        let sites = super::kinds::pair_sites(Language::Java, &[entity], &[&declaration]);

        assert_eq!(sites.len(), 1);
        assert_eq!(
            sites.values().next().map(|site| site.location().path()),
            Some("javax/annotation/Nonnull.java")
        );
        Ok(())
    }

    #[test]
    fn partial_artifacts_retain_structural_inventory_and_distinct_identities() -> Result<(), String>
    {
        use super::*;
        use backend_compile::SourceDeclaration;
        let package = backend_engine::package_key("fixture");
        let project_key = package.to_bytes();
        let path = "src/partial.rs";
        let file_key = product_source_file_key(project_key, path);
        // The artifact need not contain every structural declaration. Keeping
        // both rows prevents available positive facts from claiming absence.
        let declarations = [
            SourceDeclaration::at_path(
                path,
                "Same",
                backend_compile::DeclarationKind::Function,
                1,
                "fn Same()",
                "",
            )?,
            SourceDeclaration::at_path(
                path,
                "Unlowered",
                backend_compile::DeclarationKind::Function,
                2,
                "fn Unlowered()",
                "",
            )?,
        ];
        let record = super::super::super::ProductSourceRecord::file(
            project_key,
            path,
            backend_engine::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            Vec::from(declarations),
        )?;
        let sources = IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
            cargo_aliases: BTreeMap::new(),
            source_snapshot: None,
        };
        let (initial, _) =
            super::super::super::initial_view().map_err(|error| error.to_string())?;
        let mut targets = SemanticTargets::default();
        targets.partial_artifacts.insert((
            project_key,
            backend_semantic::vocabulary::LanguageProfile::Rust(
                backend_semantic::vocabulary::RustEdition::Rust2024,
            ),
            path.to_owned(),
        ));
        let complete = BTreeSet::new();
        let plan =
            StructuralProjectionPlan::of(&sources, &complete).map_err(|error| error.to_string())?;
        let mut projection = SourceRowProjection::new(
            &initial,
            &sources,
            16,
            &targets,
            &plan,
            std::path::Path::new("/tmp"),
        )
        .map_err(|error| error.to_string())?;
        projection
            .append_file(
                file_key,
                &sources.files[0].1,
                &complete,
                &ProfileStalePaths::new(),
            )
            .map_err(|error| error.to_string())?;
        let ledger = projection.take_ledger();
        let identity = DeclarationIdentity {
            family: backend_semantic::ir::DeclarationFamilyId::from_raw([7; 16]),
            variant: backend_semantic::ir::VariantFingerprint::from_raw([8; 16]),
        };
        let native_id = RowId::Symbol(semantic_symbol(package, identity));
        let native_label = semantic_coordinate("fixture", identity, "Same");
        let rows = projection
            .finish(vec![Row::in_package(
                native_id,
                initial.basis(),
                package,
                native_label.clone(),
            )])
            .map_err(|error| error.to_string())?;
        assert_eq!(ledger.file_lane(path), Some(FileLane::Mixed));
        assert!(!ledger.all_semantic_fresh());
        let structural = rows
            .iter()
            .find(|row| row.label == "fixture::src/partial.rs:1::Same")
            .ok_or("same-named structural row was suppressed")?;
        assert_ne!(structural.id, native_id);
        assert!(
            rows.iter()
                .any(|row| row.label == native_label && row.id == native_id)
        );
        assert!(
            rows.iter()
                .any(|row| row.label == "fixture::src/partial.rs:2::Unlowered")
        );
        assert!(
            complete.is_empty(),
            "mixed rows never promote publication coverage"
        );
        Ok(())
    }

    #[test]
    fn skip_seeks_one_package_inside_a_full_publication_relation() {
        const PACKAGES: usize = 4_096;
        let value = backend_engine::builtin::ProductSemanticPublicationRecord::Unavailable(
            backend_engine::builtin::SemanticUnavailableReason::Toolchain,
        );
        let mut entries = Vec::with_capacity(PACKAGES);
        for index in 0..PACKAGES {
            let name = format!("pkg:cargo/p{index:04}@1.0.0");
            let key = backend_engine::ProductSemanticPublicationKey::new(
                backend_engine::PackageReference::parse(name.clone()).expect("package"),
                backend_semantic::vocabulary::PackageUrl::parse(name).expect("coordinate"),
                backend_semantic::vocabulary::LanguageProfile::Rust(
                    backend_semantic::vocabulary::RustEdition::Rust2015,
                ),
            )
            .expect("key");
            entries.push((key, value.clone()));
        }
        let target = entries[PACKAGES - 1].0.clone();
        let relation = backend_version::RelationState::<
            super::super::super::BuiltinSemanticRelation,
        >::from_entries(
            entries,
            super::super::super::admitted_coverage().expect("coverage"),
        )
        .expect("relation");
        let mut projects = BTreeMap::new();
        projects.insert(
            target.package_key().to_bytes(),
            IndexedProject {
                package: target.package_key(),
                label: target.package().as_str().to_owned(),
                files: Arc::from([]),
            },
        );
        let bounds = publication_seek_bounds(&projects).expect("seek bounds");
        assert_eq!(bounds.len(), 1);
        assert_eq!(&bounds[0].0, target.package());
        let start = &bounds[0].1;
        let ranged = relation
            .range(start..)
            .take_while(|(key, _)| key.package() == target.package())
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        assert_eq!(ranged, vec![target.clone()]);

        let page_median = median(|| relation.iter().count());
        let range_median = median(|| {
            relation
                .range(start..)
                .take_while(|(key, _)| key.package() == target.package())
                .count()
        });
        eprintln!(
            "semantic_rebuild_seek packages={PACKAGES} page_median_ns={page_median} \
             range_median_ns={range_median}"
        );
        assert!(
            range_median.saturating_mul(8) < page_median,
            "package seek {range_median} ns was not 8× cheaper than a full publication walk \
             {page_median} ns"
        );
    }

    fn cpp_image_with_documentation(documentation: &str) -> Result<Vec<u8>, String> {
        use backend_semantic::ir::{
            BorrowedTree, ClangFacts, ClangLayout, ClangQualifiers, ClangStorageClass,
            CorePayloadHash, DeclarationFamilyId, DocInput, EntityAuthorityFacts, EntityVersion,
            FactAvailability, IrBuilder, ItemKind, LanguageExtensionInput, ParentageAuthority,
            TreeItemInput, VariantFingerprint, Visibility, encode_full_semantic_image,
            full_semantic_image_len,
        };
        use backend_semantic::vocabulary::{CxxStandard, LanguageProfile};

        let profile = LanguageProfile::Cxx(CxxStandard::Cxx23);
        let mut builder = IrBuilder::new();
        builder
            .set_language_profile(profile)
            .map_err(|error| error.to_string())?;
        let templates = builder
            .intern_type_parameters(&[])
            .map_err(|error| error.to_string())?;
        let includes = builder
            .intern_attributes(&[])
            .map_err(|error| error.to_string())?;
        let docs = [DocInput::Text(documentation)];
        let item = [TreeItemInput {
            name: b"xml_document",
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                documentation: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                language_extension: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &docs,
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::Clang(&ClangFacts {
                qualifiers: ClangQualifiers {
                    is_const: false,
                    is_volatile: false,
                    is_restrict: false,
                },
                storage: ClangStorageClass::None,
                layout: ClangLayout {
                    size_bits: None,
                    align_bits: None,
                },
                declared_name: None,
                templates,
                includes,
            })),
        }];
        let versions = [EntityVersion {
            family: DeclarationFamilyId::from_raw([1; 16]),
            variant: VariantFingerprint::from_raw([2; 16]),
            core_payload: CorePayloadHash::from_raw([3; 16]),
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &item,
                links: &[],
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    #[test]
    fn cpp_search_row_checks_document_bounds_without_changing_visible_content() -> Result<(), String>
    {
        use super::*;
        use backend_engine::application::DocumentationSession;
        use backend_semantic::ir::EntityId;
        use backend_semantic::vocabulary::{CxxStandard, LanguageProfile};

        let profile = LanguageProfile::Cxx(CxxStandard::Cxx23);
        let bytes = cpp_image_with_documentation("Pugixml declaration documentation")?;
        let image = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let session = DocumentationSession::new(&image);
        let entity = session
            .entity(EntityId::new(0))
            .map_err(|error| error.to_string())?;
        let content = semantic_row_content(
            profile,
            &image,
            &entity,
            backend_engine::package_key("cpp-pugixml"),
            [7; 32],
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            document_text(&content.document),
            "Pugixml declaration documentation"
        );
        assert_eq!(content.signature, None);

        let oversized = "x".repeat(MAX_SEMANTIC_DOCUMENT_BYTES);
        let bytes = cpp_image_with_documentation(&oversized)?;
        let image = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let session = DocumentationSession::new(&image);
        let entity = session
            .entity(EntityId::new(0))
            .map_err(|error| error.to_string())?;
        let result = semantic_row_content(
            profile,
            &image,
            &entity,
            backend_engine::package_key("cpp-pugixml"),
            [7; 32],
        );
        let error = match result {
            Ok(_) => {
                return Err("the existing canonical-document output bound was bypassed".to_owned());
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("exceeds its output bound"));
        Ok(())
    }
}

#[cfg(test)]
mod search_cancellation_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn cancelled_entity_kind_walk_preserves_the_complete_image_classification() -> Result<(), String>
    {
        let encoded = super::super::image_reopen::fixture_semantic_image("src/worker.rs")?;
        let view = SemanticImageView::reopen(&encoded).map_err(|error| error.to_string())?;
        let session = DocumentationSession::new(&view);
        let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
            backend_semantic::vocabulary::RustEdition::Rust2024,
        );
        let baseline =
            entity_kinds(profile, &view, &session, &[]).map_err(|error| error.to_string())?;
        assert_eq!(baseline.derived.len(), 4);
        let token = Arc::new(AtomicBool::new(true));
        assert!(entity_kinds_controlled(profile, &view, &session, &[], Some(&token)).is_err());
        token.store(false, Ordering::Release);
        let cancel = crate::builtin::cancel_search_preparation_after(
            "semantic-kind-entity",
            2,
            Arc::clone(&token),
        );
        let error = entity_kinds_controlled(profile, &view, &session, &[], Some(&token))
            .err()
            .ok_or("cancelled entity walk returned partial classification")?;
        assert!(error.to_string().contains("search preparation cancelled"));
        assert!(token.load(Ordering::Acquire));
        drop(cancel);
        token.store(false, Ordering::Release);
        let resumed = entity_kinds_controlled(profile, &view, &session, &[], Some(&token))
            .map_err(|error| error.to_string())?;
        assert_eq!(resumed.derived, baseline.derived);
        assert_eq!(resumed.exact, baseline.exact);
        assert_eq!(resumed.sites, baseline.sites);
        Ok(())
    }
}
