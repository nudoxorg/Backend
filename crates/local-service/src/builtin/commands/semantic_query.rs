use super::super::read_package_sources;
use super::super::view_build;
use super::super::view_build::{
    ProjectCallableIndex, foreign_namespace_call_retarget, foreign_package_call_retarget,
    join_native_typescript_target, join_project_field, join_project_mention, join_project_value, project_paths_for_package,
};
use super::super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinModelError, BuiltinSemanticRelation,
    BuiltinValidator, activate_semantic_publication,
};
use super::snapshot::{
    semantic_confidence, semantic_declaration_identity, semantic_link_evidence, semantic_link_kind,
};
use backend_engine::application::{DocumentationSession, LocalCompilerClient};
use backend_engine::builtin::{ProductSemanticPublicationRecord, SemanticPublicationCoverage};
use backend_semantic::ir::{DeclarationIdentity, LinkKind, LinkTarget, SemanticCoreReader, SemanticReader};
use futures_util::StreamExt as _;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread;

pub(super) struct SemanticQueryJob {
    pub(super) query: backend_extension_trustfall::SemanticQueryRequest,
    pub(super) identity: backend_extension_trustfall::SemanticQueryIdentity,
    pub(super) events:
        mpsc::SyncSender<Result<backend_extension_trustfall::SemanticQueryEvent, String>>,
}

static SEMANTIC_QUERY_EXECUTOR: OnceLock<Result<mpsc::SyncSender<SemanticQueryJob>, String>> =
    OnceLock::new();

pub(super) fn semantic_query_executor()
-> Result<&'static mpsc::SyncSender<SemanticQueryJob>, BuiltinModelError> {
    SEMANTIC_QUERY_EXECUTOR
        .get_or_init(|| {
            let (jobs, receiver) = mpsc::sync_channel::<SemanticQueryJob>(1);
            thread::Builder::new()
                .name("backend-trustfall-executor".to_owned())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        execute_semantic_query_job(job);
                    }
                })
                .map(|_| jobs)
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| BuiltinModelError(format!("start semantic query executor: {error}")))
}

fn execute_semantic_query_job(job: SemanticQueryJob) {
    let mut stream = match backend_extension_trustfall::execute_semantic_query(job.query) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = job.events.send(Err(error.to_string()));
            return;
        }
    };
    futures_executor::LocalPool::new().run_until(async move {
        while let Some(event) = stream.next().await {
            let identity_matches = match &event {
                backend_extension_trustfall::SemanticQueryEvent::Row(row) => {
                    job.identity.matches(row.identity())
                }
                backend_extension_trustfall::SemanticQueryEvent::Terminal(terminal) => {
                    job.identity.matches(terminal.identity())
                }
            };
            if !identity_matches {
                let _ = job.events.send(Err(
                    "structured graph query event identity changed during execution".to_owned(),
                ));
                break;
            }
            if job.events.send(Ok(event)).is_err() {
                break;
            }
        }
    });
}

fn package_reference_label(
    sources: &super::super::IndexedSources,
    package: backend_engine::PackageKey,
) -> Option<backend_engine::PackageReference> {
    let project = sources.projects.get(&package.to_bytes())?;
    let reference = backend_engine::PackageReference::parse(project.label.clone()).ok()?;
    if backend_engine::package_key(reference.as_str()) != package {
        return None;
    }
    Some(reference)
}

// An empty persisted file frontier cannot supply structural source identity.
// A missing project also fails closed rather than granting structural authority.
fn typescript_requires_nse(
    sources: &super::super::IndexedSources,
    package: backend_engine::PackageKey,
) -> bool {
    sources
        .projects
        .get(&package.to_bytes())
        .is_none_or(|project| project.files.is_empty())
}

const OBSERVED_POLICY_GAP: &str = "Source selection was captured from observed inputs; current ignore-policy stability is unknown; reindex and inspect package_profile";

fn restrict_typescript_inventory(
    profiles: &mut std::collections::BTreeMap<
        backend_semantic::vocabulary::LanguageProfile,
        view_build::RetargetingScope,
    >,
    profile: backend_semantic::vocabulary::LanguageProfile,
    gaps: &mut BTreeSet<String>,
) {
    profiles.insert(
        super::super::ingest::lane_profile(profile),
        view_build::RetargetingScope::SelectedArtifacts,
    );
    gaps.insert("TypeScript source inventory may be incomplete; inspect package_profile for missing sources".to_owned());
}

/// Visits the selected publications of `package`.
///
/// When the indexed project label is that package reference, the read opens
/// at the package's ordering lower bound and stops at the next package.
/// A label that is not a package reference still pages the whole relation.
pub(super) fn for_package_publications(
    relation: &backend_engine::workspace::WorkspaceRelationHandle<BuiltinSemanticRelation>,
    package: backend_engine::PackageKey,
    sources: &super::super::IndexedSources,
    context: &str,
    mut visit: impl FnMut(
        &backend_engine::ProductSemanticPublicationKey,
        &ProductSemanticPublicationRecord,
    ) -> Result<(), BuiltinModelError>,
) -> Result<(), BuiltinModelError> {
    let reference = package_reference_label(sources, package);
    let mut from = reference
        .clone()
        .map(backend_engine::ProductSemanticPublicationKey::package_lower_bound);
    let mut after = None;
    loop {
        let page = if let Some(start) = from.take() {
            relation.page_from(&start, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        } else {
            relation.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        }
        .map_err(|error| BuiltinModelError(format!("page semantic {context} relation: {error}")))?;
        for (key, record) in page.entries() {
            if let Some(reference) = &reference {
                if key.package() != reference {
                    return Ok(());
                }
            } else if key.package_key() != package {
                continue;
            }
            if !key.is_selected() {
                continue;
            }
            visit(key, record)?;
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    Ok(())
}

pub(super) fn execute_semantic_graph(
    _structural_calls: &mut view_build::StructuralCallResidence,
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut view_build::ImageRowResidence,
    query: backend_engine::GraphNeighborhoodQuery,
    include_incoming: bool,
) -> Result<Option<backend_engine::ViewSnapshot>, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    if !query.basis().matches(library.view().root()) {
        return library
            .graph_from_semantic_ids(query, &[])
            .map(Some)
            .map_err(|error| BuiltinModelError(error.to_string()));
    }
    let source_symbol = query.resolve_symbol(library.view()).ok_or_else(|| {
        BuiltinModelError("semantic graph source is absent from the selected view".to_owned())
    })?;
    let source_id = backend_engine::RowId::Symbol(source_symbol);
    let semantic_query = query.with_resolved_symbol(source_symbol);
    let package = {
        let source = library.view().row_ref(source_id).ok_or_else(|| {
            BuiltinModelError("semantic graph source is absent from the selected view".to_owned())
        })?;
        let Some(package) = source.package else {
            return Ok(None);
        };
        package
    };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_package_sources(&snapshot, package)?;
    let available = view_build::SourceAvailability::of(&sources)?;
    let view = library.view();
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic graph relation: {error}")))?;
    let mut activations = Vec::new();
    let mut profile_scopes = std::collections::BTreeMap::new();
    let mut observed_policy_blocked = false;
    let mut gaps = BTreeSet::from(["Graph relation joins are not certified exhaustive; search source for additional relations".to_owned()]);
    for_package_publications(&relation, package, &sources, "graph", |key, record| {
        view_build::SourceAvailability::record_publication_scope(&mut profile_scopes, key, record);
        match record {
            ProductSemanticPublicationRecord::Unavailable(reason) => {
                gaps.insert(format!("{:?} profile unavailable ({reason}); inspect package_profile", key.profile()));
            }
            ProductSemanticPublicationRecord::Published { coverage: SemanticPublicationCoverage::Partial(partial), .. } => {
                gaps.insert(format!("{:?} profile partial ({}/{}); inspect package_profile", key.profile(), partial.completed(), partial.total()));
            }
            ProductSemanticPublicationRecord::Published { .. } => {}
        }
        let ProductSemanticPublicationRecord::Published { coverage, claim } = record else {
            return Ok(());
        };
        if matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_)) {
            gaps.insert("TypeScript checker diagnostics are not retained with graph relations; check type errors and exports".to_owned());
        }
        let is_typescript = matches!(
            key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_)
        );
        if is_typescript && typescript_requires_nse(&sources, package) {
            restrict_typescript_inventory(&mut profile_scopes, key.profile(), &mut gaps);
            gaps.insert(OBSERVED_POLICY_GAP.to_owned());
            // Public source-bearing edges are held until each can carry its
            // owner-authenticated observed-policy qualification.
            observed_policy_blocked = true;
            return Ok(());
        }
        let activated =
            activate_semantic_publication(compiler, key, *claim, generations, image_rows)?;
        let selected = available.select_current(key, *coverage, activated, image_rows)?;
        let binding = claim.binding();
        if selected.has_excluded_images() {
            profile_scopes.insert(
                super::super::ingest::lane_profile(key.profile()),
                view_build::RetargetingScope::SelectedArtifacts,
            );
            gaps.insert(format!("{:?} profile has stale or unavailable source artifacts; reindex and inspect package_profile", key.profile()));
        }
        if selected.has_program_manifest() && selected.program().is_none() {
            gaps.insert(format!("{:?} native program source or membership proof is unavailable or stale; reindex and inspect package_profile", key.profile()));
        }
        activations.push((selected, binding));
        Ok(())
    })?;
    for profile in available.required_profiles(package) {
        if !profile_scopes.contains_key(&super::super::ingest::lane_profile(profile)) {
            gaps.insert(format!("{profile:?} profile has no selected semantic publication; reindex and inspect package_profile"));
        }
    }
    let mut source_binding = None;
    for (activation, binding) in &activations {
        for image in activation.images() {
            let image = image.reopen().map_err(|error| {
                BuiltinModelError(format!("reopen semantic graph image: {error}"))
            })?;
            if semantic_entity_for_symbol(&image, package, source_symbol)?.is_some() {
                source_binding = Some(*binding);
                break;
            }
        }
        if source_binding.is_some() {
            break;
        }
    }
    let Some(binding) = source_binding else {
        if observed_policy_blocked {
            return Err(BuiltinModelError(OBSERVED_POLICY_GAP.to_owned()));
        }
        return Ok(None);
    };
    let project_paths = project_paths_for_package(&sources, package);
    let relations = project_semantic_graph_relations(
        &activations,
        view,
        package,
        source_symbol,
        source_id,
        include_incoming,
        &project_paths,
        &available.retargeting_scopes(package, &profile_scopes),
    )?;
    let (relations, edges, conflicting) = resolve_graph_edge_origins(relations);
    if conflicting {
        gaps.insert("Conflicting publication evidence for a graph relation; inspect selected publications".to_owned());
    }
    let mut snapshot = library
        .graph_from_semantic_relations(semantic_query, &relations)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let revision = backend_engine::RichGraphRevision::new(
        snapshot.root.root().to_bytes(),
        backend_engine::SemanticGenerationId::new(*binding.identity.as_ref()),
        *binding.generation.pinned_root.as_ref(),
    );
    let mut rich_graph = rich_graph_for_root(&snapshot.root, source_id, revision, edges)?;
    // A selected input publication is not a certificate that graph joins are
    // exhaustive. Keep the positive edges and report the missing proof.
    let backend_engine::ReferenceCoverage::Unknown { reason } = reference_coverage(&gaps, false)? else {
        unreachable!("a graph coverage gap is always present")
    };
    rich_graph.availability = backend_engine::GraphAvailability::Unavailable(reason);
    snapshot.rich_graph = Some(rich_graph);
    Ok(Some(snapshot))
}

/// Keeps the selected semantic fact for an edge, using syntax only for missing edges.
fn extend_structural_graph_relations(
    relations: &mut BTreeSet<backend_engine::GraphRelation>,
    structural: impl IntoIterator<Item = backend_engine::GraphRelation>,
) {
    let mut selected = relations
        .iter()
        .map(|relation| (relation.from, relation.to, relation.relation))
        .collect::<BTreeSet<_>>();
    for relation in structural {
        if selected.insert((relation.from, relation.to, relation.relation)) {
            relations.insert(relation);
        }
    }
}

pub(super) fn execute_structural_call_graph(
    structural_calls: &mut view_build::StructuralCallResidence,
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    query: backend_engine::GraphNeighborhoodQuery,
    include_incoming: bool,
) -> Result<Option<backend_engine::ViewSnapshot>, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    let view = library.view();
    let source_symbol = query.resolve_symbol(view).ok_or_else(|| {
        BuiltinModelError(
            "structural call graph source is absent from the selected view".to_owned(),
        )
    })?;
    let source_id = backend_engine::RowId::Symbol(source_symbol);
    let package = {
        let source = view.row_ref(source_id).ok_or_else(|| {
            BuiltinModelError(
                "structural call graph source is absent from the selected view".to_owned(),
            )
        })?;
        let Some(package) = source.package else {
            return Ok(None);
        };
        package
    };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_package_sources(&snapshot, package)?;
    let relations = view_build::structural_call_graph_relations_resident(
        structural_calls,
        view,
        &sources,
        package,
        source_id,
        include_incoming,
    )?;
    let Some(relations) = relations else {
        return Ok(None);
    };
    library
        .graph_from_semantic_relations(query, &relations)
        .map(Some)
        .map_err(|error| BuiltinModelError(error.to_string()))
}

fn package_indexed_in_sources(
    sources: &super::super::IndexedSources,
    package: backend_engine::PackageKey,
) -> bool {
    sources
        .projects
        .values()
        .any(|project| project.package == package)
}

pub(super) fn semantic_entity_for_symbol(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
) -> Result<Option<backend_semantic::ir::EntityId>, BuiltinModelError> {
    let session = DocumentationSession::new(image);
    session
        .publication_entities()
        .find_map(|entity| match entity {
            Ok(entity)
                if view_build::semantic_symbol(package, entity.entity.version.identity())
                    == symbol =>
            {
                Some(Ok(entity.entity.id))
            }
            Ok(_) => None,
            Err(error) => Some(Err(BuiltinModelError(format!(
                "read semantic graph entity: {error}"
            )))),
        })
        .transpose()
}

fn published_identities_from_views(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
) -> Result<BTreeSet<DeclarationIdentity>, BuiltinModelError> {
    let mut published = BTreeSet::new();
    for image in images {
        let session = DocumentationSession::new(image);
        for entity in session.publication_entities() {
            let entity = entity.map_err(|error| {
                BuiltinModelError(format!("read semantic graph published entity: {error}"))
            })?;
            published.insert(entity.entity.version.identity());
        }
    }
    Ok(published)
}

fn semantic_link_row_id(
    view: &backend_engine::ViewRoot,
    image: &backend_semantic::ir::SemanticImageView<'_>,
    session: &DocumentationSession<'_, backend_semantic::ir::SemanticImageView<'_>>,
    package: backend_engine::PackageKey,
    image_identity: [u8; 32],
    caller_path: &str,
    link_kind: LinkKind,
    target: LinkTarget,
    project_paths: &BTreeSet<String>,
    callable_index: &ProjectCallableIndex,
    published: &BTreeSet<DeclarationIdentity>,
    native_only: bool,
) -> Result<Option<backend_engine::RowId>, BuiltinModelError> {
    let row_id = match target {
        LinkTarget::Local(target) => {
            let target = session.entity(target).map_err(|error| {
                BuiltinModelError(format!("read semantic graph target: {error}"))
            })?;
            backend_engine::RowId::Symbol(view_build::semantic_symbol(
                package,
                target.entity.version.identity(),
            ))
        }
        LinkTarget::External(external) => {
            if let Some(identity) = join_native_typescript_target(image, external, callable_index, published)? {
                let row_id = backend_engine::RowId::Symbol(view_build::semantic_symbol(package, identity));
                return Ok(view.row_ref(row_id).map(|_| row_id));
            }
            if native_only {
                let identity = backend_semantic::ir::ExternalTargetIdentity::capture(image, external)
                    .map_err(|error| BuiltinModelError(format!("identify semantic graph target: {error}")))?;
                let row_id = backend_engine::RowId::Symbol(view_build::external_semantic_symbol(
                    package, image_identity, identity,
                ));
                return Ok(view.row_ref(row_id).map(|_| row_id));
            }
            if matches!(link_kind, LinkKind::Calls | LinkKind::MethodCall) {
                let identity = if let Some(identity) = foreign_package_call_retarget(
                    image,
                    external,
                    caller_path,
                    project_paths,
                    callable_index,
                )? {
                    Some(identity)
                } else {
                    foreign_namespace_call_retarget(image, external, callable_index)?
                };
                if let Some(identity) = identity {
                    let row_id = backend_engine::RowId::Symbol(view_build::semantic_symbol(
                        package, identity,
                    ));
                    return Ok(view.row_ref(row_id).map(|_| row_id));
                }
            } else if matches!(link_kind, LinkKind::TypeReference | LinkKind::Imports)
                && let Some(identity) = join_project_mention(
                    image,
                    link_kind,
                    external,
                    caller_path,
                    project_paths,
                    callable_index,
                    published,
                )?
            {
                let row_id =
                    backend_engine::RowId::Symbol(view_build::semantic_symbol(package, identity));
                return Ok(view.row_ref(row_id).map(|_| row_id));
            } else if matches!(link_kind, LinkKind::Reads) {
                let identity = if let Some(identity) = join_project_field(
                    image,
                    link_kind,
                    external,
                    caller_path,
                    project_paths,
                    callable_index,
                    published,
                )? {
                    Some(identity)
                } else {
                    join_project_value(
                        image,
                        link_kind,
                        external,
                        caller_path,
                        project_paths,
                        callable_index,
                        published,
                    )?
                };
                if let Some(identity) = identity {
                    let row_id = backend_engine::RowId::Symbol(view_build::semantic_symbol(
                        package, identity,
                    ));
                    return Ok(view.row_ref(row_id).map(|_| row_id));
                }
            }
            let identity = backend_semantic::ir::ExternalTargetIdentity::capture(image, external)
                .map_err(|error| {
                BuiltinModelError(format!("identify semantic graph target: {error}"))
            })?;
            backend_engine::RowId::Symbol(view_build::external_semantic_symbol(
                package,
                image_identity,
                identity,
            ))
        }
    };
    if view.row_ref(row_id).is_some() {
        Ok(Some(row_id))
    } else {
        Ok(None)
    }
}

fn project_semantic_graph_relations_from_bytes(
    images: &[&[u8]],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
    source_id: backend_engine::RowId,
    include_incoming: bool,
    project_paths: &BTreeSet<String>,
) -> Result<BTreeSet<backend_engine::GraphRelation>, BuiltinModelError> {
    let mut opened = Vec::with_capacity(images.len());
    for bytes in images {
        opened.push(
            backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
                BuiltinModelError(format!("reopen semantic graph image: {error}"))
            })?,
        );
    }
    project_opened_semantic_graph(
        &opened,
        view,
        package,
        symbol,
        source_id,
        include_incoming,
        project_paths,
        &std::collections::BTreeMap::new(),
        &[],
        true,
        false,
    )
}

fn project_opened_semantic_graph(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
    source_id: backend_engine::RowId,
    include_incoming: bool,
    project_paths: &BTreeSet<String>,
    scope: &std::collections::BTreeMap<
        backend_semantic::vocabulary::Language,
        view_build::RetargetingScope,
    >,
    programs: &[&view_build::CurrentNativeProgram],
    legacy_image_program: bool,
    native_only: bool,
) -> Result<BTreeSet<backend_engine::GraphRelation>, BuiltinModelError> {
    Ok(project_opened_semantic_graph_origins(
        images,
        view,
        package,
        symbol,
        source_id,
        include_incoming,
        project_paths,
        scope,
        programs,
        legacy_image_program,
        native_only,
    )?
    .into_iter()
    .map(|(relation, _)| relation)
    .collect())
}

fn project_opened_semantic_graph_origins(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
    source_id: backend_engine::RowId,
    include_incoming: bool,
    project_paths: &BTreeSet<String>,
    scope: &std::collections::BTreeMap<
        backend_semantic::vocabulary::Language,
        view_build::RetargetingScope,
    >,
    programs: &[&view_build::CurrentNativeProgram],
    legacy_image_program: bool,
    native_only: bool,
) -> Result<BTreeSet<(backend_engine::GraphRelation, usize)>, BuiltinModelError> {
    let image_refs = images.iter().collect::<Vec<_>>();
    let callable_index = ProjectCallableIndex::build_from_views_with_programs(
        &image_refs,
        scope,
        programs,
        legacy_image_program,
    )?;
    project_opened_semantic_graph_origins_with_index(
        images, view, package, symbol, source_id, include_incoming,
        project_paths, &callable_index, native_only,
    )
}

/// The image ordinal remains attached to the projected edge until the exact
/// selected source observation can be bound to that edge's owning image.
fn project_opened_semantic_graph_origins_with_index(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
    source_id: backend_engine::RowId,
    include_incoming: bool,
    project_paths: &BTreeSet<String>,
    callable_index: &ProjectCallableIndex,
    native_only: bool,
) -> Result<BTreeSet<(backend_engine::GraphRelation, usize)>, BuiltinModelError> {
    let published = published_identities_from_views(images)?;
    let mut relations = BTreeSet::new();
    for (image_index, image) in images.iter().enumerate() {
        let caller_path = view_build::compiled_source_path(image)?;
        let image_identity = *blake3::hash(image.as_ref()).as_bytes();
        let session = DocumentationSession::new(image);
        if let Some(source_entity) = semantic_entity_for_symbol(&image, package, symbol)? {
            for (_, link) in image.links_from(source_entity) {
                let Some(target) = semantic_link_row_id(
                    view,
                    &image,
                    &session,
                    package,
                    image_identity,
                    &caller_path,
                    link.kind,
                    link.target,
                    project_paths,
                    callable_index,
                    &published,
                    native_only,
                )?
                else {
                    continue;
                };
                relations.insert((
                    backend_engine::GraphRelation::new(
                        source_id,
                        target,
                        semantic_link_kind(link.kind),
                    )
                    .with_confidence(semantic_confidence(link.confidence)),
                    image_index,
                ));
            }
        }
        if include_incoming {
            for source in session.publication_entities() {
                let source = source.map_err(|error| {
                    BuiltinModelError(format!("read semantic graph source: {error}"))
                })?;
                for (_, link) in image.links_from(source.entity.id) {
                    let Some(target) = semantic_link_row_id(
                        view,
                        &image,
                        &session,
                        package,
                        image_identity,
                        &caller_path,
                        link.kind,
                        link.target,
                        project_paths,
                        callable_index,
                        &published,
                        native_only,
                    )?
                    else {
                        continue;
                    };
                    if target != source_id {
                        continue;
                    }
                    let from = backend_engine::RowId::Symbol(view_build::semantic_symbol(
                        package,
                        source.entity.version.identity(),
                    ));
                    if view.row_ref(from).is_none() {
                        continue;
                    }
                    relations.insert((
                        backend_engine::GraphRelation::new(
                            from,
                            source_id,
                            semantic_link_kind(link.kind),
                        )
                        .with_confidence(semantic_confidence(link.confidence)),
                        image_index,
                    ));
                }
            }
        }
    }
    let mut ids = BTreeSet::from([source_id]);
    for (relation, _) in &relations {
        ids.insert(relation.from);
        ids.insert(relation.to);
    }
    if ids.len() > 1 + usize::from(backend_engine::QueryLimit::MAX)
        || relations.len() > usize::from(backend_engine::QueryLimit::MAX)
    {
        return Err(BuiltinModelError(
            "semantic graph exceeds the bounded result contract".to_owned(),
        ));
    }
    Ok(relations)
}

fn project_semantic_graph_relations(
    activations: &[(
        view_build::SelectedSourceArtifacts,
        backend_engine::publication::binding::CompilationBindingFacts,
    )],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
    source_id: backend_engine::RowId,
    include_incoming: bool,
    project_paths: &BTreeSet<String>,
    scope: &std::collections::BTreeMap<
        backend_semantic::vocabulary::Language,
        view_build::RetargetingScope,
    >,
) -> Result<
    Vec<(
        backend_engine::GraphRelation,
        backend_engine::GraphProvenance,
    )>,
    BuiltinModelError,
> {
    let mut opened = Vec::new();
    let mut origins = Vec::new();
    for (activation, binding) in activations {
        for image in activation.images() {
            origins.push((*binding, image.content_digest()));
            opened.push(image.reopen().map_err(|error| {
                BuiltinModelError(format!("reopen semantic graph image: {error}"))
            })?);
        }
    }
    let projected = project_opened_semantic_graph_origins(
        &opened,
        view,
        package,
        symbol,
        source_id,
        include_incoming,
        project_paths,
        scope,
        &activations
            .iter()
            .filter_map(|(activation, _)| activation.program())
            .collect::<Vec<_>>(),
        !activations
            .iter()
            .any(|(activation, _)| activation.has_program_manifest()),
        true,
    )?;
    Ok(bind_projected_graph_origins(projected, &origins))
}

fn bind_projected_graph_origins(
    projected: BTreeSet<(backend_engine::GraphRelation, usize)>,
    origins: &[(
        backend_engine::publication::binding::CompilationBindingFacts,
        [u8; 32],
    )],
) -> Vec<(
    backend_engine::GraphRelation,
    backend_engine::GraphProvenance,
)> {
    projected
        .into_iter()
        .map(|(relation, index)| {
            let (binding, digest) = origins[index];
            let provenance = backend_engine::GraphProvenance {
                semantic_generation: Some(backend_engine::SemanticGenerationId::new(
                    *binding.identity.as_ref(),
                )),
                semantic_root: Some(*binding.generation.pinned_root.as_ref()),
                authority: backend_engine::GraphAuthority::SemanticGeneration,
                source: digest,
                confidence: relation.confidence,
            };
            (relation, provenance)
        })
        .collect()
}

fn rich_graph_for_root(
    root: &backend_engine::ViewRoot,
    center: backend_engine::RowId,
    revision: backend_engine::RichGraphRevision,
    edges: std::collections::BTreeMap<backend_engine::GraphEdgeId, backend_engine::RichGraphEdge>,
) -> Result<backend_engine::RichGraphSnapshot, BuiltinModelError> {
    let base = backend_engine::RichGraphSnapshot::from_view(root, center, &[], revision)
        .map_err(|error| BuiltinModelError(format!("build graph nodes: {error}")))?;
    let mut builder = backend_engine::RichGraphBuilder::new(
        revision,
        backend_engine::GraphNodeId::for_row(center),
    );
    for node in base.nodes.iter() {
        builder
            .add_node(node.clone())
            .map_err(|error| BuiltinModelError(format!("add graph node: {error}")))?;
    }
    for edge in edges.into_values() {
        builder
            .add_edge(edge)
            .map_err(|error| BuiltinModelError(format!("add graph edge: {error}")))?;
    }
    builder
        .finish()
        .map_err(|error| BuiltinModelError(format!("finish graph: {error}")))
}

fn resolve_graph_edge_origins(
    candidates: Vec<(
        backend_engine::GraphRelation,
        backend_engine::GraphProvenance,
    )>,
) -> (
    Vec<backend_engine::GraphRelation>,
    std::collections::BTreeMap<backend_engine::GraphEdgeId, backend_engine::RichGraphEdge>,
    bool,
) {
    let mut edges = std::collections::BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    for (relation, provenance) in candidates {
        let edge = backend_engine::RichGraphEdge::from_relation(relation, provenance);
        if conflicts.contains(&edge.id) {
            continue;
        }
        if let Some((previous, _)) = edges.get(&edge.id) {
            if previous != &edge {
                edges.remove(&edge.id);
                conflicts.insert(edge.id);
            }
        } else {
            edges.insert(edge.id, (edge, relation));
        }
    }
    let relations = edges.values().map(|(_, relation)| *relation).collect();
    let edges = edges
        .into_iter()
        .map(|(id, (edge, _))| (id, edge))
        .collect();
    (relations, edges, !conflicts.is_empty())
}

/// The evidence of one written site of a link: its own confidence and its
/// own captured span, not the representative of the relation it proves.
fn occurrence_evidence(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    id: backend_semantic::ir::LinkOccurrenceId,
    occurrence: backend_semantic::ir::LinkOccurrence,
) -> Result<backend_engine::SemanticLinkEvidence, BuiltinModelError> {
    use backend_semantic::ir::{FactAvailability, SemanticCoreReader as _};
    let authority = image.occurrence_authority(id).ok_or_else(|| {
        BuiltinModelError("semantic occurrence has no authority record".to_owned())
    })?;
    let source = match (authority.source, occurrence.source) {
        (FactAvailability::Captured, Some(span)) => {
            let file = image.atom(span.file()).ok_or_else(|| {
                BuiltinModelError("semantic occurrence path is absent from its image".to_owned())
            })?;
            let file = std::str::from_utf8(file).map_err(|_| {
                BuiltinModelError("semantic occurrence path is not UTF-8".to_owned())
            })?;
            Some(backend_engine::SemanticSourceSpan {
                file: backend_engine::ProductText::new(file)
                    .map_err(|error| BuiltinModelError(error.to_string()))?,
                start: span.start(),
                end: span.end(),
            })
        }
        _ => None,
    };
    Ok(backend_engine::SemanticLinkEvidence {
        confidence: semantic_confidence(occurrence.confidence),
        source,
    })
}

fn reference_fact_has_site_and_relation(
    facts: &[backend_engine::ReferenceFact],
    site: backend_engine::SymbolKey,
    relation: backend_engine::SemanticLinkKind,
) -> bool {
    facts
        .iter()
        .any(|fact| fact.site == site && fact.relation == relation)
}

#[cfg(test)]
fn project_reference_facts_from_bytes(
    images: &[&[u8]],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    target_symbol: backend_engine::SymbolKey,
    project_paths: &BTreeSet<String>,
    structural_pairs: &[(String, String)],
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    let mut opened = Vec::with_capacity(images.len());
    for bytes in images {
        opened.push(
            backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
                BuiltinModelError(format!("reopen semantic references image: {error}"))
            })?,
        );
    }
    let endpoints = structural_pairs
        .iter()
        .map(|(caller, callee)| {
            (
                view_build::StructuralCallEndpoint::LegacyCoordinate(caller.clone()),
                view_build::StructuralCallEndpoint::LegacyCoordinate(callee.clone()),
            )
        })
        .collect::<Vec<_>>();
    let borrowed_pairs = endpoints
        .iter()
        .map(|(caller, callee)| (caller, callee))
        .collect::<Vec<_>>();
    project_opened_reference_facts(
        &opened,
        view,
        package,
        target_symbol,
        project_paths,
        &borrowed_pairs,
        None,
        &std::collections::BTreeMap::new(),
        &[],
        true,
    )
}

fn project_opened_reference_facts(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    target_symbol: backend_engine::SymbolKey,
    project_paths: &BTreeSet<String>,
    structural_pairs: &[(
        &view_build::StructuralCallEndpoint,
        &view_build::StructuralCallEndpoint,
    )],
    // The package's source root, where a name-matched call's file is read
    // to place it; `None` places none.
    root: Option<&std::path::Path>,
    scope: &std::collections::BTreeMap<
        backend_semantic::vocabulary::Language,
        view_build::RetargetingScope,
    >,
    programs: &[&view_build::CurrentNativeProgram],
    legacy_image_program: bool,
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    let image_refs = images.iter().collect::<Vec<_>>();
    let mut files = std::collections::BTreeMap::new();
    let mut facts = Vec::new();
    for image in images {
        append_reference_facts(package, image, target_symbol, &mut facts)?;
    }
    let callable_index = ProjectCallableIndex::build_from_views_with_programs(
        &image_refs,
        scope,
        programs,
        legacy_image_program,
    )?;
    let target_row_id = backend_engine::RowId::Symbol(target_symbol);
    let mut published = BTreeSet::new();
    for image in images {
        let session = DocumentationSession::new(image);
        for entity in session.publication_entities() {
            let entity = entity.map_err(|error| {
                BuiltinModelError(format!(
                    "read semantic references published entity: {error}"
                ))
            })?;
            published.insert(entity.entity.version.identity());
        }
    }
    for image in images {
        let caller_path = view_build::compiled_source_path(image)?;
        let mut occurrence_sites = std::collections::BTreeMap::new();
        for (id, occurrence) in image.link_occurrences() {
            occurrence_sites
                .entry(occurrence.link)
                .or_insert_with(Vec::new)
                .push((id, occurrence));
        }
        let session = DocumentationSession::new(image);
        for source in session.publication_entities() {
            let source = source.map_err(|error| {
                BuiltinModelError(format!("read semantic references caller: {error}"))
            })?;
            let caller_symbol =
                view_build::semantic_symbol(package, source.entity.version.identity());
            if view
                .row_ref(backend_engine::RowId::Symbol(caller_symbol))
                .is_none()
            {
                continue;
            }
            for (link_id, link) in image.links_from(source.entity.id) {
                let LinkTarget::External(external) = link.target else {
                    continue;
                };
                let identity = if let Some(identity) = join_native_typescript_target(
                    &image, external, &callable_index, &published,
                )? {
                    Some(identity)
                } else if matches!(link.kind, LinkKind::Calls | LinkKind::MethodCall) {
                    if let Some(identity) = foreign_package_call_retarget(
                        &image,
                        external,
                        &caller_path,
                        project_paths,
                        &callable_index,
                    )? {
                        Some(identity)
                    } else {
                        foreign_namespace_call_retarget(&image, external, &callable_index)?
                    }
                } else if matches!(link.kind, LinkKind::TypeReference | LinkKind::Imports) {
                    join_project_mention(
                        &image,
                        link.kind,
                        external,
                        &caller_path,
                        project_paths,
                        &callable_index,
                        &published,
                    )?
                } else if matches!(link.kind, LinkKind::Reads) {
                    if let Some(identity) = join_project_field(
                        &image,
                        link.kind,
                        external,
                        &caller_path,
                        project_paths,
                        &callable_index,
                        &published,
                    )? {
                        Some(identity)
                    } else {
                        join_project_value(
                            &image,
                            link.kind,
                            external,
                            &caller_path,
                            project_paths,
                            &callable_index,
                            &published,
                        )?
                    }
                } else {
                    None
                };
                let Some(identity) = identity else {
                    continue;
                };
                let retargeted_symbol = view_build::semantic_symbol(package, identity);
                if retargeted_symbol != target_symbol {
                    continue;
                }
                if view
                    .row_ref(backend_engine::RowId::Symbol(retargeted_symbol))
                    .is_none()
                {
                    continue;
                }
                let relation = semantic_link_kind(link.kind);
                let evidence = match occurrence_sites.get(&link_id) {
                    Some(sites) => sites
                        .iter()
                        .map(|(id, occurrence)| occurrence_evidence(image, *id, *occurrence))
                        .collect::<Result<Vec<_>, _>>()?,
                    None => vec![semantic_link_evidence(image, link)?],
                };
                for evidence in evidence {
                    facts.push(backend_engine::ReferenceFact {
                        site: caller_symbol,
                        target: backend_engine::SemanticLinkTarget::Local {
                            declaration: semantic_declaration_identity(identity),
                        },
                        relation,
                        evidence,
                    });
                    if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
                        return Err(BuiltinModelError(
                            "semantic references exceed the bounded result contract".to_owned(),
                        ));
                    }
                }
            }
        }
    }
    let target_name = {
        let target_row = view.row_ref(target_row_id).ok_or_else(|| {
            BuiltinModelError("references target is absent from the selected view".to_owned())
        })?;
        target_row
            .label
            .rsplit("::")
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                BuiltinModelError("references target has no declaration name".to_owned())
            })?
    };
    let target_identity = view_build::structural_symbol_identity(target_symbol);
    let structural_rows = view_build::StructuralCallRows::new(view, package);
    for &(caller_endpoint, callee_endpoint) in structural_pairs {
        let Some(callee_id) = structural_rows.mapped(callee_endpoint) else {
            continue;
        };
        if callee_id != target_row_id {
            continue;
        }
        let Some(caller_id) = structural_rows.mapped(caller_endpoint) else {
            continue;
        };
        let caller_row = view.row_ref(caller_id).ok_or_else(|| {
            BuiltinModelError("structural references site is absent from the view".to_owned())
        })?;
        let backend_engine::RowId::Symbol(caller_symbol) = caller_row.id else {
            return Err(BuiltinModelError(
                "structural references site is not a declaration row".to_owned(),
            ));
        };
        if reference_fact_has_site_and_relation(
            &facts,
            caller_symbol,
            backend_engine::SemanticLinkKind::Calls,
        ) {
            continue;
        }
        // A name-matched call is placed where it is in the file, or not at
        // all (`view_build::structural_file_span`).
        let source = match (
            caller_row.source.captured(),
            caller_row.excerpt.text(),
            root,
        ) {
            (Some(location), Some(excerpt), Some(root)) => {
                view_build::structural_call_span(excerpt, &target_name)
                    .and_then(|span| {
                        view_build::structural_file_span(root, location, excerpt, span, &mut files)
                    })
                    .map(|(start, end)| {
                        Ok::<_, BuiltinModelError>(backend_engine::SemanticSourceSpan {
                            file: backend_engine::ProductText::new(location.path()).map_err(
                                |error| {
                                    BuiltinModelError(format!(
                                        "structural references path: {error:?}"
                                    ))
                                },
                            )?,
                            start,
                            end,
                        })
                    })
                    .transpose()?
            }
            _ => None,
        };
        facts.push(backend_engine::ReferenceFact {
            site: caller_symbol,
            target: backend_engine::SemanticLinkTarget::Local {
                declaration: target_identity,
            },
            relation: backend_engine::SemanticLinkKind::Calls,
            evidence: backend_engine::SemanticLinkEvidence {
                confidence: backend_engine::SemanticConfidence::Syntactic,
                source,
            },
        });
        if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
            return Err(BuiltinModelError(
                "semantic references exceed the bounded result contract".to_owned(),
            ));
        }
    }
    deduplicate_nested_reference_facts(view, &mut facts);
    if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
        return Err(BuiltinModelError(
            "semantic references exceed the bounded result contract".to_owned(),
        ));
    }
    Ok(facts)
}

/// Projects exact native endpoints without consulting structural or name-based joins.
/// Resolves one external link of a legacy-lane image to a declaration the
/// package itself publishes, using the same name joins the graph uses.
///
/// A Roslyn-resolved call to a method declared in another file reaches the image as
/// a foreign key whose namespace is the declaring type's qualified name and whose
/// display is the method name, so it joins by owner chain. Type and field uses join
/// by the project's own declaration inventory. Anything ambiguous stays unjoined.
fn legacy_external_declaration(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    caller_path: &str,
    link_kind: LinkKind,
    external: backend_semantic::ir::ExternalId,
    project_paths: &BTreeSet<String>,
    index: &ProjectCallableIndex,
    published: &BTreeSet<DeclarationIdentity>,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    if matches!(link_kind, LinkKind::Calls | LinkKind::MethodCall) {
        if let Some(identity) =
            foreign_package_call_retarget(image, external, caller_path, project_paths, index)?
        {
            return Ok(Some(identity));
        }
        return foreign_namespace_call_retarget(image, external, index);
    }
    if matches!(link_kind, LinkKind::TypeReference | LinkKind::Imports) {
        return join_project_mention(
            image, link_kind, external, caller_path, project_paths, index, published,
        );
    }
    if matches!(link_kind, LinkKind::Reads) {
        if let Some(identity) = join_project_field(
            image, link_kind, external, caller_path, project_paths, index, published,
        )? {
            return Ok(Some(identity));
        }
        return join_project_value(
            image, link_kind, external, caller_path, project_paths, index, published,
        );
    }
    Ok(None)
}

/// References to `target_symbol` made from other files of the same package.
///
/// Every legacy-lane image describes one source file, so a use in a different file is
/// an external link in that file's image. Join those links through the package's own
/// declaration inventory. C# images are always joined here; every other language is
/// joined only when no native program owns any image, because a native program
/// resolves its own links exactly. The result is a name join over compiler-resolved
/// or written spellings, so the caller still reports the coverage as not certified
/// exhaustive.
fn project_legacy_cross_file_reference_facts(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    target_symbol: backend_engine::SymbolKey,
    project_paths: &BTreeSet<String>,
    scope: &std::collections::BTreeMap<
        backend_semantic::vocabulary::Language,
        view_build::RetargetingScope,
    >,
    every_image_is_legacy: bool,
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    let is_csharp = |image: &backend_semantic::ir::SemanticImageView<'_>| {
        matches!(
            image.image_facts().authority,
            backend_semantic::ir::SemanticImageAuthority::Language(
                backend_semantic::vocabulary::LanguageProfile::CSharp(_)
            )
        )
    };
    let csharp = images
        .iter()
        .filter(|image| every_image_is_legacy || is_csharp(image))
        .collect::<Vec<_>>();
    if csharp.is_empty() {
        return Ok(Vec::new());
    }
    let index = ProjectCallableIndex::build_from_views_with_scope(&csharp, scope)?;
    let mut published = BTreeSet::new();
    for image in &csharp {
        for entity in DocumentationSession::new(*image).publication_entities() {
            published.insert(
                entity
                    .map_err(|error| {
                        BuiltinModelError(format!("read C# references entity: {error}"))
                    })?
                    .entity
                    .version
                    .identity(),
            );
        }
    }
    let mut facts = Vec::new();
    for image in csharp {
        let caller_path = view_build::compiled_source_path(image)?;
        let mut occurrence_sites = std::collections::BTreeMap::new();
        for (id, occurrence) in image.link_occurrences() {
            occurrence_sites
                .entry(occurrence.link)
                .or_insert_with(Vec::new)
                .push((id, occurrence));
        }
        for source in DocumentationSession::new(image).publication_entities() {
            let source = source.map_err(|error| {
                BuiltinModelError(format!("read C# references caller: {error}"))
            })?;
            let caller = view_build::semantic_symbol(package, source.entity.version.identity());
            if view.row_ref(backend_engine::RowId::Symbol(caller)).is_none() {
                continue;
            }
            for (link_id, link) in image.links_from(source.entity.id) {
                let LinkTarget::External(external) = link.target else {
                    continue;
                };
                let joined = legacy_external_declaration(
                    image,
                    &caller_path,
                    link.kind,
                    external,
                    project_paths,
                    &index,
                    &published,
                )?;
                let Some(identity) = joined else {
                    continue;
                };
                if view_build::semantic_symbol(package, identity) != target_symbol {
                    continue;
                }
                let evidence = match occurrence_sites.get(&link_id) {
                    Some(sites) => sites
                        .iter()
                        .map(|(id, occurrence)| occurrence_evidence(image, *id, *occurrence))
                        .collect::<Result<Vec<_>, _>>()?,
                    None => vec![semantic_link_evidence(image, link)?],
                };
                for evidence in evidence {
                    facts.push(backend_engine::ReferenceFact {
                        site: caller,
                        target: backend_engine::SemanticLinkTarget::Local {
                            declaration: semantic_declaration_identity(identity),
                        },
                        relation: semantic_link_kind(link.kind),
                        evidence,
                    });
                    if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
                        return Err(BuiltinModelError(
                            "semantic references exceed the bounded result contract".to_owned(),
                        ));
                    }
                }
            }
        }
    }
    Ok(facts)
}

fn project_native_reference_facts(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    target_symbol: backend_engine::SymbolKey,
    scope: &std::collections::BTreeMap<
        backend_semantic::vocabulary::Language,
        view_build::RetargetingScope,
    >,
    programs: &[&view_build::CurrentNativeProgram],
    legacy_image_program: bool,
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    let image_refs = images.iter().collect::<Vec<_>>();
    let index = ProjectCallableIndex::build_from_views_with_programs(
        &image_refs,
        scope,
        programs,
        legacy_image_program,
    )?;
    let mut facts = project_native_reference_facts_with_origins(
        images, view, package, target_symbol, &index,
    )?
    .into_iter()
    .map(|(fact, _)| fact)
    .collect();
    deduplicate_nested_reference_facts(view, &mut facts);
    Ok(facts)
}

/// Retains the exact source image for every occurrence until the caller can
/// bind a result-specific owner-sealed policy qualification. The ordinal is
/// valid only against this exact `images` slice and is never a wire identity.
fn project_native_reference_facts_with_origins(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    target_symbol: backend_engine::SymbolKey,
    index: &ProjectCallableIndex,
) -> Result<Vec<(backend_engine::ReferenceFact, usize)>, BuiltinModelError> {
    let mut published = BTreeSet::new();
    for image in images {
        for entity in DocumentationSession::new(image).publication_entities() {
            published.insert(
                entity
                    .map_err(|error| {
                        BuiltinModelError(format!("read native references entity: {error}"))
                    })?
                    .entity
                    .version
                    .identity(),
            );
        }
    }
    let mut facts = Vec::new();
    let mut origins = Vec::new();
    for (image_index, image) in images.iter().enumerate() {
        let start = facts.len();
        append_reference_facts(package, image, target_symbol, &mut facts)?;
        let mut occurrence_sites = std::collections::BTreeMap::new();
        for (id, occurrence) in image.link_occurrences() {
            occurrence_sites
                .entry(occurrence.link)
                .or_insert_with(Vec::new)
                .push((id, occurrence));
        }
        for source in DocumentationSession::new(image).publication_entities() {
            let source = source.map_err(|error| {
                BuiltinModelError(format!("read native references caller: {error}"))
            })?;
            let caller = view_build::semantic_symbol(package, source.entity.version.identity());
            if view
                .row_ref(backend_engine::RowId::Symbol(caller))
                .is_none()
            {
                continue;
            }
            for (link_id, link) in image.links_from(source.entity.id) {
                let LinkTarget::External(external) = link.target else {
                    continue;
                };
                let Some(identity) =
                    join_native_typescript_target(image, external, index, &published)?
                else {
                    continue;
                };
                if view_build::semantic_symbol(package, identity) != target_symbol {
                    continue;
                }
                let evidence = match occurrence_sites.get(&link_id) {
                    Some(sites) => sites
                        .iter()
                        .map(|(id, occurrence)| occurrence_evidence(image, *id, *occurrence))
                        .collect::<Result<Vec<_>, _>>()?,
                    None => vec![semantic_link_evidence(image, link)?],
                };
                for evidence in evidence {
                    facts.push(backend_engine::ReferenceFact {
                        site: caller,
                        target: backend_engine::SemanticLinkTarget::Local {
                            declaration: semantic_declaration_identity(identity),
                        },
                        relation: semantic_link_kind(link.kind),
                        evidence,
                    });
                    if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
                        return Err(BuiltinModelError(
                            "semantic references exceed the bounded result contract".to_owned(),
                        ));
                    }
                }
            }
        }
        origins.extend(std::iter::repeat_n(image_index, facts.len() - start));
    }
    Ok(facts.into_iter().zip(origins).collect())
}

/// Image and owning native identity used only to select a corresponding sealed
/// source observation. These fields do not independently prove the relation.
#[derive(Clone)]
struct NativeReferenceOccurrenceOrigin {
    fact: backend_engine::ReferenceFact,
    image_index: usize,
    image_identity: [u8; 32],
    image_byte_len: u32,
    entity_index: u32,
    owner: DeclarationIdentity,
}

/// The issuer compares this untrusted origin with independently admitted
/// selected evidence before reading source bytes or sealing a policy claim.
/// `None` is unknown and must never trigger structural fallback.
fn qualify_native_occurrence_origin(
    snapshot: &backend_engine::WorkspaceSnapshot,
    view: &backend_engine::ViewRoot,
    origin: &NativeReferenceOccurrenceOrigin,
    publication_key: &backend_engine::builtin::ProductSemanticPublicationKey,
    publication_claim: backend_engine::builtin::SemanticPublicationClaim,
    program: &view_build::ObservedNativeProgram<'_>,
    workspace: &std::path::Path,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    authority: &super::super::semantic_authority::SemanticAuthority,
    source_budget: &mut usize,
) -> Result<Option<backend_library::SelectedResultSourcePolicy>, BuiltinModelError> {
    let expected = super::ExpectedNativeSourceOrigin {
        publication_key,
        publication_claim,
        program_identity: program.sources().membership_identity(),
        image_identity: origin.image_identity,
        image_byte_len: origin.image_byte_len,
        entity_index: origin.entity_index,
        declaration_identity: origin.owner,
    };
    let admitted = super::selected_native_policy_for_row(
        snapshot, view, backend_engine::RowId::Symbol(origin.fact.site), &expected,
        workspace, compiler, generations, authority, source_budget,
    ).map_err(|error| BuiltinModelError(format!("qualify native reference source: {error}")))?;
    let Some((selection, qualification)) = admitted else { return Ok(None) };
    let basis = backend_library::Basis { root: view.root(), ..view.basis() };
    backend_library::SelectedResultSourcePolicy::new(origin.fact.site, basis, selection, qualification)
        .map(Some).ok_or_else(|| BuiltinModelError("native reference policy owner or basis mismatch".to_owned()))
}

/// A selected activation and its observed-only artifacts. The factory and
/// source-availability selector own admission; this value cannot mint proof.
struct ObservedQueryPublication {
    key: backend_engine::builtin::ProductSemanticPublicationKey,
    claim: backend_engine::builtin::SemanticPublicationClaim,
    artifacts: view_build::ObservedNativeArtifacts,
}

struct QualifiedNativeReference {
    fact: backend_engine::ReferenceFact,
    source_policy: backend_library::SelectedResultSourcePolicy,
}

/// Owner-side result awaiting the reply-only carrier. No unqualified facts
/// are exposed to the catalog or to the wire through this type.
struct QualifiedNativeReferenceBatch {
    target: backend_engine::ProductText,
    target_symbol: backend_engine::SymbolKey,
    references: Vec<QualifiedNativeReference>,
    coverage: backend_engine::ReferenceCoverage,
}

/// Prove that the requested row occurs in exactly one admitted TypeScript
/// publication. An empty ProductFile frontier does not establish language.
fn observed_typescript_target_present(
    publications: &[ObservedQueryPublication],
    package: backend_engine::PackageKey,
    target: backend_engine::SymbolKey,
) -> Result<bool, BuiltinModelError> {
    let mut found = false;
    for publication in publications {
        for image in publication.artifacts.images() {
            let opened = image.reopen().map_err(|error| BuiltinModelError(format!("reopen observed target image: {error}")))?;
            for entity in DocumentationSession::new(&opened).publication_entities() {
                let entity = entity.map_err(|error| BuiltinModelError(format!("read observed target: {error}")))?;
                if view_build::semantic_symbol(package, entity.entity.version.identity()) == target {
                    if found { return Err(BuiltinModelError("observed target belongs to multiple selected images".to_owned())); }
                    found = true;
                }
            }
        }
    }
    Ok(found)
}

fn execute_observed_native_references(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut view_build::ImageRowResidence,
    workspace: &std::path::Path,
    authority: &super::super::semantic_authority::SemanticAuthority,
    target: &backend_engine::ProductText,
) -> Result<Option<QualifiedNativeReferenceBatch>, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    let view = library.view();
    let row = view_build::resolve_selected_view_address(view, target.as_str())
        .declaration()
        .map_err(|failure| BuiltinModelError(failure.to_string()))?
        .ok_or_else(|| BuiltinModelError("references target is absent from the selected view".to_owned()))?;
    let backend_engine::RowId::Symbol(target_symbol) = row.id else {
        return Err(BuiltinModelError("references target is not a declaration row".to_owned()));
    };
    let package = row.package.ok_or_else(|| BuiltinModelError("references target is not attributed to a package".to_owned()))?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_package_sources(&snapshot, package)?;
    if !typescript_requires_nse(&sources, package) {
        return Ok(None);
    }
    let relation = snapshot.relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open observed semantic scope: {error}")))?;
    let mut has_typescript = false;
    for_package_publications(&relation, package, &sources, "observed scope", |key, _| {
        has_typescript |= matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_));
        Ok(())
    })?;
    if !has_typescript { return Ok(None); }
    let available = view_build::SourceAvailability::of(&sources)?;
    let (publications, publication_gap) = collect_observed_query_publications(
        &snapshot, &sources, package, &available, compiler, generations, image_rows,
    )?;
    if !observed_typescript_target_present(&publications, package, target_symbol)? {
        return Ok(None);
    }
    let scopes = std::collections::BTreeMap::from([(
        backend_semantic::vocabulary::Language::TypeScript,
        view_build::RetargetingScope::SelectedArtifacts,
    )]);
    let mut budget = backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES;
    let (references, result_gap) = qualify_observed_references(
        &publications, &snapshot, view, package, target_symbol, &scopes,
        workspace, compiler, generations, authority, &mut budget,
    )?;
    let mut gaps = BTreeSet::from([
        OBSERVED_POLICY_GAP.to_owned(),
        "Cross-file reference joins are not certified exhaustive; search source text for additional uses".to_owned(),
    ]);
    if publication_gap || result_gap {
        gaps.insert("Some selected native reference evidence was unavailable; reindex and inspect package_profile".to_owned());
    }
    Ok(Some(QualifiedNativeReferenceBatch {
        target: target.clone(),
        target_symbol,
        coverage: reference_coverage(&gaps, !references.is_empty())?,
        references,
    }))
}

/// Convert only individually qualified native occurrences to the live
/// References reply. The wire layer still must authenticate each carrier.
pub(super) fn execute_qualified_observed_references(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut view_build::ImageRowResidence,
    workspace: &std::path::Path,
    authority: &super::super::semantic_authority::SemanticAuthority,
    target: &backend_engine::ProductText,
) -> Result<Option<backend_engine::SurfaceReply>, BuiltinModelError> {
    let Some(batch) = execute_observed_native_references(
        daemon, compiler, generations, image_rows, workspace, authority, target,
    )? else { return Ok(None) };
    let facts = batch.references.iter().map(|item| item.fact.clone()).collect::<Vec<_>>();
    let mut records = daemon.engine().daemon().library()
        .references_for_symbol(batch.target_symbol, &facts)
        .map_err(|error| BuiltinModelError(format!("project qualified references: {error}")))?;
    if records.len() != batch.references.len() {
        return Err(BuiltinModelError("qualified reference result changed during projection".to_owned()));
    }
    for (record, item) in records.iter_mut().zip(batch.references) {
        if record.site.as_str() != item.source_policy.selection().coordinate()
            || item.source_policy.selection().symbol() != item.fact.site
        {
            return Err(BuiltinModelError("qualified reference site does not match selected owner".to_owned()));
        }
        record.site_symbol = Some(item.fact.site.to_bytes());
        record.source_policy = item.source_policy.qualification().clone();
    }
    Ok(Some(backend_engine::SurfaceReply::References {
        target: batch.target, references: records, coverage: batch.coverage,
    }))
}

fn collect_observed_query_publications(
    snapshot: &backend_engine::WorkspaceSnapshot,
    sources: &super::super::IndexedSources,
    package: backend_engine::PackageKey,
    available: &view_build::SourceAvailability,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut view_build::ImageRowResidence,
) -> Result<(Vec<ObservedQueryPublication>, bool), BuiltinModelError> {
    let relation = snapshot.relation::<BuiltinSemanticRelation>().map_err(|error| {
        BuiltinModelError(format!("open observed semantic publications: {error}"))
    })?;
    let mut publications = Vec::new();
    let mut withheld = false;
    for_package_publications(&relation, package, sources, "observed references", |key, record| {
        if !matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_)) {
            return Ok(());
        }
        let ProductSemanticPublicationRecord::Published { coverage, claim } = record else {
            withheld = true;
            return Ok(());
        };
        if matches!(coverage, SemanticPublicationCoverage::Partial(_)) {
            withheld = true;
        }
        let view_build::NseProgramAdmission::Admitted { activation, proof } =
            view_build::load_selected_nse_program(snapshot, sources, compiler, key, generations)?
        else {
            withheld = true;
            return Ok(());
        };
        if proof.claim() != *claim || proof.coverage() != *coverage {
            return Err(BuiltinModelError("observed selected publication changed during admission".to_owned()));
        }
        let artifacts = available.select_observed_with_nse(
            snapshot, key, proof.claim(), proof.coverage(), activation, image_rows, &proof,
        )?;
        publications.push(ObservedQueryPublication { key: key.clone(), claim: *claim, artifacts });
        Ok(())
    })?;
    if publications.is_empty() { withheld = true; }
    Ok((publications, withheld))
}

/// Qualifies each observed occurrence independently. The image ordinal is
/// resolved against the same retained activation that supplied the program;
/// neither a matching name nor a different selected image can fill a gap.
/// `withheld` reports uncertainty without inventing an omitted count.
fn qualify_observed_references(
    publications: &[ObservedQueryPublication],
    snapshot: &backend_engine::WorkspaceSnapshot,
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    target: backend_engine::SymbolKey,
    scopes: &std::collections::BTreeMap<backend_semantic::vocabulary::Language, view_build::RetargetingScope>,
    workspace: &std::path::Path,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    authority: &super::super::semantic_authority::SemanticAuthority,
    source_budget: &mut usize,
) -> Result<(Vec<QualifiedNativeReference>, bool), BuiltinModelError> {
    let mut images = Vec::new();
    let mut image_publications = Vec::new();
    let mut programs = Vec::with_capacity(publications.len());
    for (publication_index, publication) in publications.iter().enumerate() {
        let program = publication.artifacts.program().ok_or_else(|| {
            BuiltinModelError("observed publication has no admitted native program".to_owned())
        })?;
        for image in publication.artifacts.images() {
            images.push(image.reopen().map_err(|error| {
                BuiltinModelError(format!("reopen observed reference image: {error}"))
            })?);
            image_publications.push(publication_index);
        }
        programs.push(program);
    }
    if images.is_empty() { return Ok((Vec::new(), true)); }
    let observed = programs.iter().collect::<Vec<_>>();
    let origins = project_observed_native_reference_origins(
        &images, view, package, target, scopes, &[], &observed,
    )?;
    let mut qualified = Vec::new();
    let mut withheld = publications.iter().any(|publication| publication.artifacts.has_excluded_images());
    for origin in origins {
        let publication_index = *image_publications.get(origin.image_index).ok_or_else(|| {
            BuiltinModelError("observed reference image has no selected publication".to_owned())
        })?;
        let publication = &publications[publication_index];
        let Some(span) = origin.fact.evidence.source.as_ref() else {
            withheld = true;
            continue;
        };
        let image = &images[origin.image_index];
        let backend_semantic::ir::ImageProvenance::Captured { source, .. } = image.image_facts().provenance else {
            withheld = true;
            continue;
        };
        if view_build::compiled_source_path(image)? != span.file.as_str()
            || span.start >= span.end || span.end > source.byte_len
        {
            withheld = true;
            continue;
        }
        let source_policy = qualify_native_occurrence_origin(
            snapshot, view, &origin, &publication.key, publication.claim,
            &programs[publication_index], workspace, compiler, generations, authority, source_budget,
        )?;
        let Some(source_policy) = source_policy else {
            withheld = true;
            continue;
        };
        qualified.push(QualifiedNativeReference { fact: origin.fact, source_policy });
    }
    Ok((qualified, withheld))
}

fn bind_native_reference_occurrence_origins(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    package: backend_engine::PackageKey,
    facts: Vec<(backend_engine::ReferenceFact, usize)>,
) -> Result<Vec<NativeReferenceOccurrenceOrigin>, BuiltinModelError> {
    facts.into_iter().map(|(fact, image_index)| {
        let image = images.get(image_index).ok_or_else(|| {
            BuiltinModelError("reference occurrence image is unavailable".to_owned())
        })?;
        let mut owner = None;
        for entity in DocumentationSession::new(image).publication_entities() {
            let entity = entity.map_err(|error| {
                BuiltinModelError(format!("read reference occurrence owner: {error}"))
            })?;
            let identity = entity.entity.version.identity();
            if view_build::semantic_symbol(package, identity) == fact.site {
                let candidate = (entity.entity.id.raw, identity);
                if owner.replace(candidate).is_some_and(|previous| previous != candidate) {
                    return Err(BuiltinModelError("reference occurrence owner is ambiguous".to_owned()));
                }
            }
        }
        let (entity_index, owner) = owner.ok_or_else(|| {
            BuiltinModelError("reference occurrence owner is not admitted by its image".to_owned())
        })?;
        let image_byte_len = u32::try_from(image.as_ref().len()).map_err(|_| {
            BuiltinModelError("reference occurrence image is too large".to_owned())
        })?;
        Ok(NativeReferenceOccurrenceOrigin {
            fact,
            image_index,
            image_identity: *backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(image.as_ref()).as_ref(),
            image_byte_len,
            entity_index,
            owner,
        })
    }).collect()
}

/// Internal-only projection. The returned origins are not public evidence:
/// every item still requires an owner-issued, per-result policy qualifier.
fn project_observed_native_reference_origins(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    target: backend_engine::SymbolKey,
    scopes: &std::collections::BTreeMap<backend_semantic::vocabulary::Language, view_build::RetargetingScope>,
    current: &[&view_build::CurrentNativeProgram],
    observed: &[&view_build::ObservedNativeProgram<'_>],
) -> Result<Vec<NativeReferenceOccurrenceOrigin>, BuiltinModelError> {
    if observed.is_empty() {
        return Err(BuiltinModelError("observed reference projection requires an observed program".to_owned()));
    }
    let image_refs = images.iter().collect::<Vec<_>>();
    let index = ProjectCallableIndex::build_from_views_with_observed_programs(
        &image_refs, scopes, current, observed, false,
    )?;
    let facts = project_native_reference_facts_with_origins(images, view, package, target, &index)?;
    let mut origins = bind_native_reference_occurrence_origins(images, package, facts)?;
    deduplicate_observed_reference_origins(view, &mut origins);
    Ok(origins)
}

/// Deduplicate within one retained image only. Equal facts from distinct
/// images remain separate until each has been independently qualified.
fn deduplicate_observed_reference_origins(
    view: &backend_engine::ViewRoot,
    origins: &mut Vec<NativeReferenceOccurrenceOrigin>,
) {
    fn ancestor(view: &backend_engine::ViewRoot, parent: backend_engine::SymbolKey, child: backend_engine::SymbolKey) -> bool {
        let mut current = Some(child);
        for _ in 0..backend_engine::MAX_PRODUCT_ROWS {
            let Some(symbol) = current else { return false };
            if symbol == parent { return true; }
            current = view.row_ref(backend_engine::RowId::Symbol(symbol)).and_then(|row| row.parent);
        }
        false
    }
    origins.sort_by(|left, right| {
        let a = &left.fact;
        let b = &right.fact;
        a.evidence.source.as_ref().map(|s| (s.file.as_str(), s.start, s.end))
            .cmp(&b.evidence.source.as_ref().map(|s| (s.file.as_str(), s.start, s.end)))
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.site.to_bytes().cmp(&b.site.to_bytes()))
            .then_with(|| a.evidence.confidence.cmp(&b.evidence.confidence))
            .then_with(|| left.image_identity.cmp(&right.image_identity))
            .then_with(|| left.image_index.cmp(&right.image_index))
            .then_with(|| left.entity_index.cmp(&right.entity_index))
    });
    origins.dedup_by(|left, right| left.image_index == right.image_index && left.fact == right.fact && left.owner == right.owner && left.entity_index == right.entity_index);
    let mut keep = vec![true; origins.len()];
    for (index, origin) in origins.iter().enumerate() {
        let Some(source) = origin.fact.evidence.source.as_ref() else { continue };
        for (other_index, other) in origins.iter().enumerate() {
            if index == other_index || origin.image_index != other.image_index
                || other.fact.evidence.source.as_ref() != Some(source)
                || other.fact.relation != origin.fact.relation || other.fact.target != origin.fact.target { continue; }
            if (origin.fact.site == other.fact.site && other.fact.evidence.confidence > origin.fact.evidence.confidence)
                || (origin.fact.site != other.fact.site && ancestor(view, origin.fact.site, other.fact.site) && !ancestor(view, other.fact.site, origin.fact.site)) {
                keep[index] = false;
                break;
            }
        }
    }
    let mut index = 0;
    origins.retain(|_| { let retained = keep[index]; index += 1; retained });
}

/// Internal-only counterpart for graph edges. Edge ordinals retain their
/// originating image; they are not independent proof of the edge itself.
fn project_observed_native_graph_origins(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
    source_id: backend_engine::RowId,
    include_incoming: bool,
    project_paths: &BTreeSet<String>,
    scopes: &std::collections::BTreeMap<backend_semantic::vocabulary::Language, view_build::RetargetingScope>,
    current: &[&view_build::CurrentNativeProgram],
    observed: &[&view_build::ObservedNativeProgram<'_>],
) -> Result<BTreeSet<(backend_engine::GraphRelation, usize)>, BuiltinModelError> {
    if observed.is_empty() {
        return Err(BuiltinModelError("observed graph projection requires an observed program".to_owned()));
    }
    let image_refs = images.iter().collect::<Vec<_>>();
    let index = ProjectCallableIndex::build_from_views_with_observed_programs(
        &image_refs, scopes, current, observed, false,
    )?;
    project_opened_semantic_graph_origins_with_index(
        images, view, package, symbol, source_id, include_incoming, project_paths, &index, true,
    )
}

struct NativeGraphEdgeSourceOrigin {
    relation: backend_engine::GraphRelation,
    image_index: usize,
    image_identity: [u8; 32],
    image_byte_len: u32,
    entity_index: u32,
    owner: DeclarationIdentity,
}

struct QualifiedNativeGraphEdge {
    relation: backend_engine::GraphRelation,
    provenance: backend_engine::GraphProvenance,
    source_policy: backend_library::SelectedResultSourcePolicy,
}

struct QualifiedNativeGraphNode {
    row: backend_engine::RowId,
    source_policy: backend_library::SelectedResultSourcePolicy,
    binding: backend_engine::publication::binding::CompilationBindingFacts,
}

struct QualifiedNativeGraphBatch {
    nodes: Vec<QualifiedNativeGraphNode>,
    edges: Vec<QualifiedNativeGraphEdge>,
    availability: backend_engine::GraphAvailability,
}

/// Find exactly one admitted native image owner for this node. Multiple
/// selected images remain ambiguous even if their bytes happen to match.
fn qualify_observed_graph_node(
    publications: &[ObservedQueryPublication],
    snapshot: &backend_engine::WorkspaceSnapshot,
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    row: backend_engine::RowId,
    workspace: &std::path::Path,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    authority: &super::super::semantic_authority::SemanticAuthority,
    source_budget: &mut usize,
) -> Result<Option<QualifiedNativeGraphNode>, BuiltinModelError> {
    let backend_engine::RowId::Symbol(symbol) = row else { return Ok(None) };
    let mut candidate = None;
    for (publication_index, publication) in publications.iter().enumerate() {
        for image in publication.artifacts.images() {
            let opened = image.reopen().map_err(|error| BuiltinModelError(format!("reopen observed graph node image: {error}")))?;
            for entity in DocumentationSession::new(&opened).publication_entities() {
                let entity = entity.map_err(|error| BuiltinModelError(format!("read observed graph node: {error}")))?;
                let identity = entity.entity.version.identity();
                if view_build::semantic_symbol(package, identity) != symbol { continue; }
                let image_identity = *backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(opened.as_ref()).as_ref();
                let image_byte_len = u32::try_from(opened.as_ref().len()).map_err(|_| BuiltinModelError("observed graph node image is too large".to_owned()))?;
                let found = (publication_index, image_identity, image_byte_len, entity.entity.id.raw, identity);
                if candidate.replace(found).is_some() { return Ok(None); }
            }
        }
    }
    let Some((publication_index, image_identity, image_byte_len, entity_index, owner)) = candidate else { return Ok(None) };
    let publication = &publications[publication_index];
    let program = publication.artifacts.program().ok_or_else(|| BuiltinModelError("observed graph node program is unavailable".to_owned()))?;
    let expected = super::ExpectedNativeSourceOrigin {
        publication_key: &publication.key,
        publication_claim: publication.claim,
        program_identity: program.sources().membership_identity(),
        image_identity, image_byte_len, entity_index, declaration_identity: owner,
    };
    let admitted = super::selected_native_policy_for_row(
        snapshot, view, row, &expected, workspace, compiler, generations, authority, source_budget,
    ).map_err(|error| BuiltinModelError(format!("qualify native graph node: {error}")))?;
    let Some((selection, qualification)) = admitted else { return Ok(None) };
    let basis = backend_library::Basis { root: view.root(), ..view.basis() };
    let source_policy = backend_library::SelectedResultSourcePolicy::new(symbol, basis, selection, qualification)
        .ok_or_else(|| BuiltinModelError("native graph node policy owner or basis mismatch".to_owned()))?;
    Ok(Some(QualifiedNativeGraphNode { row, source_policy, binding: publication.claim.binding() }))
}

fn qualify_native_graph_edge_origin(
    snapshot: &backend_engine::WorkspaceSnapshot,
    view: &backend_engine::ViewRoot,
    origin: &NativeGraphEdgeSourceOrigin,
    publication_key: &backend_engine::builtin::ProductSemanticPublicationKey,
    publication_claim: backend_engine::builtin::SemanticPublicationClaim,
    program: &view_build::ObservedNativeProgram<'_>,
    workspace: &std::path::Path,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    authority: &super::super::semantic_authority::SemanticAuthority,
    source_budget: &mut usize,
) -> Result<Option<backend_library::SelectedResultSourcePolicy>, BuiltinModelError> {
    let expected = super::ExpectedNativeSourceOrigin {
        publication_key,
        publication_claim,
        program_identity: program.sources().membership_identity(),
        image_identity: origin.image_identity,
        image_byte_len: origin.image_byte_len,
        entity_index: origin.entity_index,
        declaration_identity: origin.owner,
    };
    let admitted = super::selected_native_policy_for_row(
        snapshot, view, origin.relation.from, &expected, workspace, compiler,
        generations, authority, source_budget,
    ).map_err(|error| BuiltinModelError(format!("qualify native graph source: {error}")))?;
    let Some((selection, qualification)) = admitted else { return Ok(None) };
    let backend_engine::RowId::Symbol(owner) = origin.relation.from else {
        return Err(BuiltinModelError("native graph policy source is not a declaration".to_owned()));
    };
    let basis = backend_library::Basis { root: view.root(), ..view.basis() };
    backend_library::SelectedResultSourcePolicy::new(owner, basis, selection, qualification)
        .map(Some).ok_or_else(|| BuiltinModelError("native graph policy owner or basis mismatch".to_owned()))
}

/// Binds the originating caller of every incoming or outgoing edge to its own
/// image. The retained tuple selects an issuer capability; it is not an edge
/// proof and cannot be used as a qualifier for the other endpoint.
fn bind_native_graph_edge_source_origins(
    images: &[backend_semantic::ir::SemanticImageView<'_>],
    package: backend_engine::PackageKey,
    edges: BTreeSet<(backend_engine::GraphRelation, usize)>,
) -> Result<Vec<NativeGraphEdgeSourceOrigin>, BuiltinModelError> {
    edges.into_iter().map(|(relation, image_index)| {
        let backend_engine::RowId::Symbol(source) = relation.from else {
            return Err(BuiltinModelError("native graph edge source is not a declaration".to_owned()));
        };
        let image = images.get(image_index).ok_or_else(|| BuiltinModelError("native graph edge image is unavailable".to_owned()))?;
        let mut owner = None;
        for entity in DocumentationSession::new(image).publication_entities() {
            let entity = entity.map_err(|error| BuiltinModelError(format!("read native graph edge owner: {error}")))?;
            let identity = entity.entity.version.identity();
            if view_build::semantic_symbol(package, identity) == source {
                let candidate = (entity.entity.id.raw, identity);
                if owner.replace(candidate).is_some_and(|previous| previous != candidate) {
                    return Err(BuiltinModelError("native graph edge owner is ambiguous".to_owned()));
                }
            }
        }
        let (entity_index, owner) = owner.ok_or_else(|| BuiltinModelError("native graph edge owner is not admitted by its image".to_owned()))?;
        let image_byte_len = u32::try_from(image.as_ref().len()).map_err(|_| BuiltinModelError("native graph edge image is too large".to_owned()))?;
        Ok(NativeGraphEdgeSourceOrigin {
            relation, image_index,
            image_identity: *backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(image.as_ref()).as_ref(),
            image_byte_len, entity_index, owner,
        })
    }).collect()
}

/// Qualify edges from their own caller image, including incoming edges.
/// Node policies and the live page carrier are separate required admissions.
fn qualify_observed_graph_edges(
    publications: &[ObservedQueryPublication],
    snapshot: &backend_engine::WorkspaceSnapshot,
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    center: backend_engine::SymbolKey,
    include_incoming: bool,
    project_paths: &BTreeSet<String>,
    scopes: &std::collections::BTreeMap<backend_semantic::vocabulary::Language, view_build::RetargetingScope>,
    workspace: &std::path::Path,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    authority: &super::super::semantic_authority::SemanticAuthority,
    source_budget: &mut usize,
) -> Result<(Vec<QualifiedNativeGraphEdge>, bool), BuiltinModelError> {
    let mut images = Vec::new();
    let mut image_publications = Vec::new();
    let mut programs = Vec::with_capacity(publications.len());
    for (publication_index, publication) in publications.iter().enumerate() {
        let program = publication.artifacts.program().ok_or_else(|| {
            BuiltinModelError("observed graph publication has no admitted native program".to_owned())
        })?;
        for image in publication.artifacts.images() {
            images.push(image.reopen().map_err(|error| BuiltinModelError(format!("reopen observed graph image: {error}")))?);
            image_publications.push(publication_index);
        }
        programs.push(program);
    }
    if images.is_empty() { return Ok((Vec::new(), true)); }
    let observed = programs.iter().collect::<Vec<_>>();
    let projected = project_observed_native_graph_origins(
        &images, view, package, center, backend_engine::RowId::Symbol(center),
        include_incoming, project_paths, scopes, &[], &observed,
    )?;
    let origins = bind_native_graph_edge_source_origins(&images, package, projected)?;
    let mut qualified = std::collections::BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    let mut withheld = publications.iter().any(|publication| publication.artifacts.has_excluded_images());
    for origin in origins {
        let publication_index = *image_publications.get(origin.image_index).ok_or_else(|| {
            BuiltinModelError("observed graph image has no selected publication".to_owned())
        })?;
        let publication = &publications[publication_index];
        let source_policy = qualify_native_graph_edge_origin(
            snapshot, view, &origin, &publication.key, publication.claim,
            &programs[publication_index], workspace, compiler, generations, authority, source_budget,
        )?;
        let Some(source_policy) = source_policy else {
            withheld = true;
            continue;
        };
        let binding = publication.claim.binding();
        let edge = QualifiedNativeGraphEdge {
            relation: origin.relation,
            provenance: backend_engine::GraphProvenance {
                semantic_generation: Some(backend_engine::SemanticGenerationId::new(*binding.identity.as_ref())),
                semantic_root: Some(*binding.generation.pinned_root.as_ref()),
                authority: backend_engine::GraphAuthority::SemanticGeneration,
                // Graph provenance uses the raw content digest; the
                // domain-separated image identity remains in the issuer tuple.
                source: *blake3::hash(images[origin.image_index].as_ref()).as_bytes(),
                confidence: origin.relation.confidence,
            },
            source_policy,
        };
        let id = backend_engine::RichGraphEdge::from_relation(edge.relation, edge.provenance).id;
        if conflicts.contains(&id) { continue; }
        if let Some(previous) = qualified.get(&id) {
            let previous: &QualifiedNativeGraphEdge = previous;
            if previous.provenance != edge.provenance || previous.source_policy != edge.source_policy {
                qualified.remove(&id);
                conflicts.insert(id);
                withheld = true;
            }
        } else {
            qualified.insert(id, edge);
        }
    }
    Ok((qualified.into_values().collect(), withheld))
}

/// Build a qualified in-memory graph batch for the future reply-only carrier.
/// Nothing from this batch is emitted until the wire/page owner admits each
/// node and edge against the authenticated local peer and selected basis.
fn execute_observed_native_graph(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut view_build::ImageRowResidence,
    workspace: &std::path::Path,
    authority: &super::super::semantic_authority::SemanticAuthority,
    query: backend_engine::GraphNeighborhoodQuery,
    include_incoming: bool,
) -> Result<Option<QualifiedNativeGraphBatch>, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    let view = library.view();
    if !query.basis().matches(view.root()) {
        return Err(BuiltinModelError("observed graph basis is stale".to_owned()));
    }
    let center = query.resolve_symbol(view).ok_or_else(|| BuiltinModelError("observed graph source is absent".to_owned()))?;
    let row = view.row_ref(backend_engine::RowId::Symbol(center)).ok_or_else(|| BuiltinModelError("observed graph source is absent".to_owned()))?;
    let Some(package) = row.package else { return Ok(None) };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_package_sources(&snapshot, package)?;
    if !typescript_requires_nse(&sources, package) { return Ok(None) }
    let available = view_build::SourceAvailability::of(&sources)?;
    let (publications, publication_gap) = collect_observed_query_publications(
        &snapshot, &sources, package, &available, compiler, generations, image_rows,
    )?;
    if !observed_typescript_target_present(&publications, package, center)? {
        return Ok(None);
    }
    let scopes = std::collections::BTreeMap::from([(
        backend_semantic::vocabulary::Language::TypeScript,
        view_build::RetargetingScope::SelectedArtifacts,
    )]);
    let mut budget = backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES;
    let (mut edges, edge_gap) = qualify_observed_graph_edges(
        &publications, &snapshot, view, package, center, include_incoming,
        &project_paths_for_package(&sources, package), &scopes, workspace, compiler,
        generations, authority, &mut budget,
    )?;
    let mut wanted = BTreeSet::from([backend_engine::RowId::Symbol(center)]);
    for edge in &edges { wanted.insert(edge.relation.from); wanted.insert(edge.relation.to); }
    let mut nodes = Vec::new();
    let mut qualified_rows = BTreeSet::new();
    let mut node_gap = false;
    for row in wanted {
        match qualify_observed_graph_node(
            &publications, &snapshot, view, package, row, workspace, compiler,
            generations, authority, &mut budget,
        )? {
            Some(node) => { qualified_rows.insert(node.row); nodes.push(node); }
            None => node_gap = true,
        }
    }
    edges.retain(|edge| qualified_rows.contains(&edge.relation.from) && qualified_rows.contains(&edge.relation.to));
    let mut gaps = BTreeSet::from([
        OBSERVED_POLICY_GAP.to_owned(),
        "Graph relation joins are not certified exhaustive; search source for additional relations".to_owned(),
    ]);
    if publication_gap || edge_gap || node_gap {
        gaps.insert("Some selected native graph evidence was unavailable; reindex and inspect package_profile".to_owned());
    }
    let backend_engine::ReferenceCoverage::Unknown { reason } = reference_coverage(&gaps, false)? else {
        unreachable!("observed graph always carries a policy gap")
    };
    Ok(Some(QualifiedNativeGraphBatch {
        nodes, edges, availability: backend_engine::GraphAvailability::Unavailable(reason),
    }))
}

/// Construct a live graph sidecar only after every retained node and edge has
/// its own independently issued selected-source claim.
pub(super) fn execute_qualified_observed_graph(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut view_build::ImageRowResidence,
    workspace: &std::path::Path,
    authority: &super::super::semantic_authority::SemanticAuthority,
    query: backend_engine::GraphNeighborhoodQuery,
    include_incoming: bool,
) -> Result<Option<backend_engine::ViewSnapshot>, BuiltinModelError> {
    let Some(batch) = execute_observed_native_graph(
        daemon, compiler, generations, image_rows, workspace, authority, query, include_incoming,
    )? else { return Ok(None) };
    let library = daemon.engine().daemon().library();
    let center = query.resolve_symbol(library.view()).ok_or_else(|| BuiltinModelError("qualified graph center is absent".to_owned()))?;
    let center_row = backend_engine::RowId::Symbol(center);
    let center_binding = batch.nodes.iter().find(|node| node.row == center_row)
        .map(|node| node.binding)
        .ok_or_else(|| BuiltinModelError("qualified graph center source policy is unavailable".to_owned()))?;
    let relations = batch.edges.iter().map(|edge| edge.relation).collect::<Vec<_>>();
    let mut snapshot = library.graph_from_semantic_relations(query.with_resolved_symbol(center), &relations)
        .map_err(|error| BuiltinModelError(format!("project qualified graph: {error}")))?;
    let revision = backend_engine::RichGraphRevision::new(
        snapshot.root.root().to_bytes(),
        backend_engine::SemanticGenerationId::new(*center_binding.identity.as_ref()),
        *center_binding.generation.pinned_root.as_ref(),
    );
    let base = backend_engine::RichGraphSnapshot::from_view(&snapshot.root, center_row, &[], revision)
        .map_err(|error| BuiltinModelError(format!("build qualified graph nodes: {error}")))?;
    let mut policies = std::collections::BTreeMap::new();
    for node in batch.nodes {
        let id = backend_engine::GraphNodeId::for_row(node.row);
        if policies.insert(id, node.source_policy).is_some() {
            return Err(BuiltinModelError("qualified graph node owner is ambiguous".to_owned()));
        }
    }
    let mut builder = backend_engine::RichGraphBuilder::new(revision, backend_engine::GraphNodeId::for_row(center_row));
    for node in base.nodes.iter() {
        let policy = policies.get(&node.id).ok_or_else(|| BuiltinModelError("qualified graph node source is unavailable".to_owned()))?;
        if node.coordinate.as_ref().map(|coordinate| coordinate.as_str()) != Some(policy.selection().coordinate()) {
            return Err(BuiltinModelError("qualified graph node coordinate mismatch".to_owned()));
        }
        let mut node = node.clone();
        node.source_policy = policy.qualification().clone();
        builder.add_node(node).map_err(|error| BuiltinModelError(format!("add qualified graph node: {error}")))?;
    }
    for item in batch.edges {
        let mut edge = backend_engine::RichGraphEdge::from_relation(item.relation, item.provenance);
        if edge.from != backend_engine::GraphNodeId::for_symbol(item.source_policy.selection().symbol()) {
            return Err(BuiltinModelError("qualified graph edge owner mismatch".to_owned()));
        }
        edge.source_policy = item.source_policy.qualification().clone();
        builder.add_edge(edge).map_err(|error| BuiltinModelError(format!("add qualified graph edge: {error}")))?;
    }
    let graph = builder.finish_with_availability(batch.availability)
        .map_err(|error| BuiltinModelError(format!("finish qualified graph: {error}")))?;
    snapshot.rich_graph = Some(graph);
    Ok(Some(snapshot))
}

/// Preserve bounded graph metadata, including a qualified center witness when
/// the center is not itself a page row. Receiver admission must correlate that
/// raw observed witness with the original request and authenticated peer.
pub(super) fn propagate_qualified_graph_page(
    page: &mut backend_library::ProjectionPage,
    full: &backend_engine::ViewSnapshot,
) -> Result<(), backend_library::LibraryError> {
    let Some(graph) = full.rich_graph.as_ref() else { return Ok(()) };
    if !graph.nodes.iter().any(|node| !node.source_policy.is_unknown()) { return Ok(()) }
    let rows = page.snapshot.root.rows().iter().map(|row| row.id).collect::<BTreeSet<_>>();
    let mut ids = rows.iter().map(|row| backend_engine::GraphNodeId::for_row(*row)).collect::<BTreeSet<_>>();
    let center_is_witness = !ids.contains(&graph.center);
    if center_is_witness {
        let center = graph.nodes.iter().find(|node| node.id == graph.center)
            .ok_or_else(|| backend_library::LibraryError::InvalidQuery("qualified graph center is absent".to_owned()))?;
        if center.source_policy.is_unknown() || center.coordinate.is_none() {
            return Err(backend_library::LibraryError::InvalidQuery("qualified graph center witness is unavailable".to_owned()));
        }
        ids.insert(graph.center);
    }
    let revision = backend_engine::RichGraphRevision::new(
        page.snapshot.root.root().to_bytes(), graph.revision.semantic_generation, graph.revision.semantic_root,
    );
    let mut builder = backend_engine::RichGraphBuilder::new(revision, graph.center);
    for node in graph.nodes.iter().filter(|node| ids.contains(&node.id)) {
        builder.add_node(node.clone()).map_err(|error| backend_library::LibraryError::InvalidQuery(error.to_string()))?;
    }
    for edge in graph.edges.iter().filter(|edge| {
        ids.contains(&edge.from) && ids.contains(&edge.to)
            && (!center_is_witness || edge.from != graph.center || edge.to != graph.center)
    }) {
        builder.add_edge(edge.clone()).map_err(|error| backend_library::LibraryError::InvalidQuery(error.to_string()))?;
    }
    let bounded = builder.finish_with_availability(graph.availability.clone())
        .map_err(|error| backend_library::LibraryError::InvalidQuery(error.to_string()))?;
    page.snapshot.graph_relations = if center_is_witness { None } else {
        full.graph_relations.as_ref().map(|relations| {
            relations.iter().filter(|relation| rows.contains(&relation.from) && rows.contains(&relation.to))
                .copied().collect::<Vec<_>>().into_boxed_slice()
        })
    };
    page.snapshot.rich_graph = Some(bounded);
    Ok(())
}


/// The most specific containing owner owns a repeated occurrence. An exact
/// span, relation and target must agree before parentage can collapse sites;
/// unrelated owners and sites without a span remain separate. Evidence is
/// retained from the selected owner, not transferred from its ancestors.
fn deduplicate_nested_reference_facts(view: &backend_engine::ViewRoot, facts: &mut Vec<backend_engine::ReferenceFact>) {
    fn ancestor(view: &backend_engine::ViewRoot, parent: backend_engine::SymbolKey, child: backend_engine::SymbolKey) -> bool {
        let mut current = Some(child);
        for _ in 0..backend_engine::MAX_PRODUCT_ROWS {
            let Some(symbol) = current else { return false };
            if symbol == parent { return true; }
            current = view.row_ref(backend_engine::RowId::Symbol(symbol)).and_then(|row| row.parent);
        }
        false
    }
    facts.sort_by(|left, right| {
        left.evidence.source.as_ref().map(|s| (s.file.as_str(), s.start, s.end))
            .cmp(&right.evidence.source.as_ref().map(|s| (s.file.as_str(), s.start, s.end)))
            .then_with(|| left.relation.cmp(&right.relation))
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| left.site.to_bytes().cmp(&right.site.to_bytes()))
            .then_with(|| left.evidence.confidence.cmp(&right.evidence.confidence))
    });
    facts.dedup();
    let mut keep = vec![true; facts.len()];
    for (index, fact) in facts.iter().enumerate() {
        let Some(source) = fact.evidence.source.as_ref() else { continue };
        for (other_index, other) in facts.iter().enumerate() {
            if index == other_index || other.evidence.source.as_ref() != Some(source)
                || other.relation != fact.relation || other.target != fact.target { continue; }
            if (fact.site == other.site && other.evidence.confidence > fact.evidence.confidence)
                || (fact.site != other.site && ancestor(view, fact.site, other.site) && !ancestor(view, other.site, fact.site)) {
                keep[index] = false;
                break;
            }
        }
    }
    let mut index = 0;
    facts.retain(|_| { let retained = keep[index]; index += 1; retained });
}

/// Answers references from selected artifacts, preserving each fact's target,
/// authority and source span. Artifacts must match the current source;
/// unrelated unavailable sources do not suppress their positive facts. Exact
/// cross-source native coordinates retain their program/source/declaration
/// proof. A missing publication or uncertified join stays unknown; a
/// nonempty result retains partial coverage and its causes.
pub(super) fn execute_references(
    _structural_calls: &mut view_build::StructuralCallResidence,
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut view_build::ImageRowResidence,
    target: &backend_engine::ProductText,
) -> Result<backend_engine::SurfaceReply, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    let view = library.view();
    let (target_symbol, package) = {
        let target_row = view_build::resolve_selected_view_address(view, target.as_str())
            .declaration()
            .map_err(|failure| {
                BuiltinModelError(match failure {
                    backend_library::CommandFailure::InvalidQuery(message) => message,
                    other => other.to_string(),
                })
            })?
            .ok_or_else(|| {
                BuiltinModelError("references target is absent from the selected view".to_owned())
            })?;
        let target_symbol = match target_row.id {
            backend_engine::RowId::Symbol(symbol) => symbol,
            _ => {
                return Err(BuiltinModelError(
                    "references target is not a declaration row".to_owned(),
                ));
            }
        };
        let Some(package) = target_row.package else {
            return Err(BuiltinModelError(
                "references target is not attributed to a package".to_owned(),
            ));
        };
        (target_symbol, package)
    };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_package_sources(&snapshot, package)?;
    if let Some(row) = view.row_ref(backend_engine::RowId::Symbol(target_symbol)) {
        if let Some(reply) = super::vue_references::references(&sources, view, row, target)? {
            return Ok(reply);
        }
    }
    let available = view_build::SourceAvailability::of(&sources)?;
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| {
            BuiltinModelError(format!("open semantic references relation: {error}"))
        })?;
    let mut activations = Vec::new();
    let mut publication_found = false;
    let mut typescript_publication_found = false;
    let mut profile_scopes = std::collections::BTreeMap::new();
    let mut coverage_gaps = BTreeSet::new();
    for_package_publications(&relation, package, &sources, "references", |key, record| {
        view_build::SourceAvailability::record_publication_scope(&mut profile_scopes, key, record);
        match record {
            ProductSemanticPublicationRecord::Unavailable(reason) => {
                let guidance = if matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::CSharp(_))
                    && matches!(reason, backend_engine::builtin::SemanticUnavailableReason::Toolchain | backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority) {
                    "; check dotnet and NUDOX_ROSLYN_HELPER"
                } else { "" };
                coverage_gaps.insert(format!("{:?} profile unavailable ({reason}); inspect package_profile{guidance}", key.profile()));
            }
            ProductSemanticPublicationRecord::Published { coverage: SemanticPublicationCoverage::Partial(partial), .. } => {
                coverage_gaps.insert(format!("{:?} profile partial ({}/{}); inspect package_profile", key.profile(), partial.completed(), partial.total()));
            }
            ProductSemanticPublicationRecord::Published { .. } => {}
        }
        let ProductSemanticPublicationRecord::Published { coverage, claim } = record else {
            return Ok(());
        };
        publication_found = true;
        typescript_publication_found |= matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_));
        let is_typescript = matches!(
            key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_)
        );
        if is_typescript && typescript_requires_nse(&sources, package) {
            restrict_typescript_inventory(&mut profile_scopes, key.profile(), &mut coverage_gaps);
            coverage_gaps.insert(OBSERVED_POLICY_GAP.to_owned());
            // No public observed source site may escape without its own typed
            // selected native identity and policy qualification.
            return Ok(());
        }
        let activated =
            activate_semantic_publication(compiler, key, *claim, generations, image_rows)?;
        let selected = available.select_current(key, *coverage, activated, image_rows)?;
        if selected.has_excluded_images() {
            profile_scopes.insert(
                super::super::ingest::lane_profile(key.profile()),
                view_build::RetargetingScope::SelectedArtifacts,
            );
            coverage_gaps.insert(format!("{:?} profile has stale or unavailable source artifacts; reindex and inspect package_profile", key.profile()));
        }
        activations.push(selected);
        Ok(())
    })?;
    for profile in available.required_profiles(package) {
        if !profile_scopes.contains_key(&super::super::ingest::lane_profile(profile)) {
            coverage_gaps.insert(format!("{profile:?} profile has no selected semantic publication; reindex and inspect package_profile"));
        }
    }
    if view.row_refs().any(|row| row.package == Some(package) && matches!(row.source, backend_library::SourceAvailability::StaleFile { .. })) {
        coverage_gaps.insert("This package retains stale semantic rows; reindex and inspect package_profile".to_owned());
    }
    // Publication coverage is input coverage, not an exhaustive cross-file
    // reference-join certificate. Do not infer a complete negative from it.
    coverage_gaps.insert("Cross-file reference joins are not certified exhaustive; search source text for additional uses".to_owned());
    if typescript_publication_found {
        coverage_gaps.insert("TypeScript checker diagnostics are not retained with these references; check type errors and exports before relying on resolved targets".to_owned());
    }
    if !publication_found {
        coverage_gaps.insert(
            "No selected semantic publication is available; index the package and inspect package_profile".to_owned(),
        );
        return Ok(backend_engine::SurfaceReply::References {
            target: target.clone(),
            references: Box::new([]),
            coverage: reference_coverage(&coverage_gaps, false)?,
        });
    }
    let mut opened = Vec::new();
    for activation in &activations {
        for image in activation.images() {
            opened.push(image.reopen().map_err(|error| {
                BuiltinModelError(format!("reopen semantic references image: {error}"))
            })?);
        }
    }
    let retargeting_scopes = available.retargeting_scopes(package, &profile_scopes);
    let mut facts = project_native_reference_facts(
        &opened,
        view,
        package,
        target_symbol,
        &retargeting_scopes,
        &activations
            .iter()
            .filter_map(|activation| activation.program())
            .collect::<Vec<_>>(),
        !activations
            .iter()
            .any(|activation| activation.has_program_manifest()),
    )?;
    facts.extend(project_legacy_cross_file_reference_facts(
        &opened,
        view,
        package,
        target_symbol,
        &project_paths_for_package(&sources, package),
        &retargeting_scopes,
        !activations.iter().any(|activation| activation.has_program_manifest()),
    )?);
    deduplicate_nested_reference_facts(view, &mut facts);
    let references = library
        .references_for_symbol(target_symbol, &facts)
        .map_err(|error| {
            BuiltinModelError(format!("project references through the catalog: {error}"))
        })?;
    Ok(backend_engine::SurfaceReply::References {
        target: target.clone(),
        coverage: reference_coverage(&coverage_gaps, !references.is_empty())?,
        references,
    })
}

fn reference_coverage(gaps: &BTreeSet<String>, found: bool) -> Result<backend_engine::ReferenceCoverage, BuiltinModelError> {
    // A selected publication does not certify exhaustive cross-file reference joins,
    // so neither an empty result nor a nonempty one establishes Complete coverage.
    let mut message = String::new();
    // Keep this qualification even if other diagnostics exceed the bounded
    // product text: compiler confidence is not live-policy eligibility.
    for gap in gaps.get(OBSERVED_POLICY_GAP).into_iter().chain(
        gaps.iter().filter(|gap| gap.as_str() != OBSERVED_POLICY_GAP),
    ) {
        let separator = if message.is_empty() { "" } else { "; " };
        if message.len() + separator.len() + gap.len() > backend_library::MAX_PRODUCT_TEXT_BYTES - 64 {
            message.push_str("; additional coverage gaps omitted; inspect package_profile");
            break;
        }
        message.push_str(separator);
        message.push_str(gap);
    }
    let reason = backend_engine::ProductText::new(message)
        .map_err(|error| BuiltinModelError(format!("reference coverage reason: {error}")))?;
    Ok(if found { backend_engine::ReferenceCoverage::Partial { reason } } else { backend_engine::ReferenceCoverage::Unknown { reason } })
}

/// Appends every occurrence of one image that targets `target_symbol`.
fn append_reference_facts(
    package: backend_engine::PackageKey,
    image: &backend_semantic::ir::SemanticImageView<'_>,
    target_symbol: backend_engine::SymbolKey,
    facts: &mut Vec<backend_engine::ReferenceFact>,
) -> Result<(), BuiltinModelError> {
    let session = DocumentationSession::new(image);
    let target_entity = session
        .canonical_entities()
        .find_map(|entity| match entity {
            Ok(entity)
                if view_build::semantic_symbol(package, entity.entity.version.identity())
                    == target_symbol =>
            {
                Some(Ok(entity.entity.id))
            }
            Ok(_) => None,
            Err(error) => Some(Err(BuiltinModelError(format!(
                "read semantic references declaration: {error}"
            )))),
        })
        .transpose()?;
    let Some(target_entity) = target_entity else {
        // This image did not compile the queried declaration.
        return Ok(());
    };
    let target_identity = session
        .entity(target_entity)
        .map_err(|error| BuiltinModelError(format!("read semantic references target: {error}")))?
        .entity
        .version
        .identity();
    let cancellation = backend_semantic::graph_vector::Cancellation::new();
    let graph =
        backend_extension_trustfall::server::SemanticTrustfallGraph::new(&image, &cancellation);
    futures_executor::block_on(async {
        let mut incoming = graph
            .incoming_occurrence_neighbors(target_entity)
            .map_err(|error| error.to_string())?;
        while let Some(hit) = incoming.next().await {
            let hit = hit.map_err(|error| error.to_string())?;
            let site = session
                .entity(hit.entity())
                .map_err(|error| error.to_string())?;
            if !session.owns_declaration(&site.entity) { continue; }
            let site = site.entity.version.identity();
            let evidence = backend_engine::SemanticLinkEvidence {
                confidence: semantic_confidence(hit.confidence()),
                source: hit
                    .provenance()
                    .source()
                    .zip(hit.provenance().path())
                    .map(|(span, path)| {
                        let file = std::str::from_utf8(path)
                            .map_err(|_| "semantic references site path is not UTF-8".to_owned())?;
                        Ok::<_, String>(backend_engine::SemanticSourceSpan {
                            file: backend_engine::ProductText::new(file)
                                .map_err(|error| error.to_string())?,
                            start: span.start(),
                            end: span.end(),
                        })
                    })
                    .transpose()?,
            };
            facts.push(backend_engine::ReferenceFact {
                site: view_build::semantic_symbol(package, site),
                target: backend_engine::SemanticLinkTarget::Local {
                    declaration: semantic_declaration_identity(target_identity),
                },
                relation: semantic_link_kind(hit.kind()),
                evidence,
            });
            if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
                return Err("semantic references exceed the bounded result contract".to_owned());
            }
        }
        Ok::<_, String>(())
    })
    .map_err(|error| BuiltinModelError(error.to_string()))
}
#[cfg(test)]
mod project_call_tests {
    use super::super::super::view_build::{
        ProjectCallableIndex, compiled_source_path, foreign_display_name, join_project_call,
        join_project_field, join_project_mention, join_project_value, query_semantic_id,
        semantic_coordinate, semantic_symbol, structural_call_coordinate_pairs,
        structural_call_graph_relations_mapped,
    };
    use super::super::super::{IndexedProject, IndexedSources, ProductSourceRecord, initial_view};
    use super::super::snapshot::semantic_declaration_identity;
    use super::project_reference_facts_from_bytes;
    use super::project_semantic_graph_relations_from_bytes;
    use super::{OBSERVED_POLICY_GAP, project_opened_reference_facts, reference_coverage, restrict_typescript_inventory};
    use backend_engine::{
        GraphRelation, Row, RowId, ViewRoot, package_key, product_source_file_key,
    };
    use backend_extension_trustfall::{
        CompilerSemanticEvidence, PackageScopeEvidence, SemanticQueryCancellation,
        SemanticQueryCorpus, SemanticQueryEvent, SemanticQueryEvidence, SemanticQueryFact,
        SemanticQueryPresentation, SemanticQueryRequest, execute_semantic_query,
    };
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity,
        EntityAuthorityFacts, EntityVersion, ExternalDeclarationIdentity, ExternalId,
        ExternalTarget, FactAvailability, ForeignDeclarationId, ForeignExternalTarget,
        ForeignTargetOrigin, IrBuilder, ItemKind, LinkKind, LinkTarget, OccurrenceAuthorityFacts,
        ParentageAuthority, SemanticCoreReader, SemanticImageView, SemanticReader, SourceIdentity,
        SourceSpan, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
        VariantAvailability, VariantFingerprint, Visibility, encode_full_semantic_image,
        full_semantic_image_len,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use futures_util::StreamExt as _;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    struct ForeignCallFixture {
        package_specifier: &'static [u8],
        path_specifier: Option<&'static [u8]>,
        display: &'static [u8],
        foreign_key: u8,
        link_kind: LinkKind,
        entity_kind: ItemKind,
    }

    struct NamespaceCallFixture {
        ecosystem: &'static [u8],
        namespace: &'static [u8],
        display: &'static [u8],
        foreign_key: u8,
        link_kind: LinkKind,
    }

    struct UniverseMentionFixture {
        ecosystem: &'static [u8],
        spelling: &'static [u8],
        foreign_key: u8,
        entity_kind: ItemKind,
    }

    struct AncestorFixture {
        name: &'static [u8],
        version_byte: u8,
        entity_id: TreeEntityId,
        parent_id: Option<TreeEntityId>,
        parent_version_byte: Option<u8>,
        kind: ItemKind,
    }

    fn fixture_version(identity: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([identity; 16]),
            variant: VariantFingerprint::from_raw([identity; 16]),
            core_payload: CorePayloadHash::from_raw([identity; 16]),
        }
    }

    fn project_namespace_call_image(
        path: &str,
        source_identity_byte: u8,
        ancestors: &[AncestorFixture],
        callee_name: &'static [u8],
        callee_version_byte: u8,
        _callee_id: TreeEntityId,
        caller_name: &'static [u8],
        caller_version_byte: u8,
        caller_id: TreeEntityId,
        foreign_call: NamespaceCallFixture,
        callee_kind: ItemKind,
        foreign_kind: Option<ItemKind>,
    ) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_identity_byte]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let mut versions = Vec::new();
        let mut items = Vec::new();
        for ancestor in ancestors {
            let version = fixture_version(ancestor.version_byte);
            versions.push(version);
            let parentage = match ancestor.parent_version_byte {
                None => ParentageAuthority::Root,
                Some(parent_version_byte) => {
                    ParentageAuthority::Bound(fixture_version(parent_version_byte).identity())
                }
            };
            items.push(TreeItemInput {
                name: ancestor.name,
                anonymous_callable_anchor: None,
                kind: ancestor.kind,
                visibility: Visibility::Public,
                authority: authority(parentage),
                parent: ancestor.parent_id,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            });
        }
        let callee_version = fixture_version(callee_version_byte);
        versions.push(callee_version);
        let callee_parent = ancestors.last().map(|ancestor| ancestor.entity_id);
        let callee_parentage = ancestors
            .last()
            .and_then(|ancestor| Some(fixture_version(ancestor.version_byte).identity()))
            .map(ParentageAuthority::Bound)
            .unwrap_or(ParentageAuthority::Root);
        items.push(TreeItemInput {
            name: callee_name,
            anonymous_callable_anchor: None,
            kind: callee_kind,
            visibility: Visibility::Public,
            authority: authority(callee_parentage),
            parent: callee_parent,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        });
        let caller_version = fixture_version(caller_version_byte);
        versions.push(caller_version);
        items.push(TreeItemInput {
            name: caller_name,
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        });
        let ecosystem = builder
            .intern_atom(foreign_call.ecosystem)
            .map_err(|e| e.to_string())?;
        let namespace_atom = builder
            .intern_atom(foreign_call.namespace)
            .map_err(|e| e.to_string())?;
        let display = builder
            .intern_atom(foreign_call.display)
            .map_err(|e| e.to_string())?;
        let path_atom = builder
            .intern_atom(foreign_call.display)
            .map_err(|e| e.to_string())?;
        let external = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([foreign_call.foreign_key; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Namespace {
                    ecosystem,
                    namespace: namespace_atom,
                },
                path: path_atom,
                display,
                kind: foreign_kind,
            }))
            .map_err(|e| e.to_string())?;
        let links = [TreeLinkInput {
            from: caller_id,
            target: TreeLinkTarget::External(external),
            kind: foreign_call.link_kind,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn project_universe_mention_image(
        path: &str,
        source_identity_byte: u8,
        item_name: &[u8],
        entity_id: TreeEntityId,
        item_kind: ItemKind,
        foreign: Option<UniverseMentionFixture>,
    ) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_identity_byte]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [TreeItemInput {
            name: item_name,
            anonymous_callable_anchor: None,
            kind: item_kind,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        let mut links = Vec::new();
        if let Some(foreign) = foreign {
            let ecosystem = builder
                .intern_atom(foreign.ecosystem)
                .map_err(|e| e.to_string())?;
            let spelling = builder
                .intern_atom(foreign.spelling)
                .map_err(|e| e.to_string())?;
            let external = builder
                .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                    identity: ExternalDeclarationIdentity {
                        foreign: ForeignDeclarationId::from_raw([foreign.foreign_key; 16]),
                        variant: VariantAvailability::Unavailable,
                    },
                    origin: ForeignTargetOrigin::Universe { ecosystem },
                    path: spelling,
                    display: spelling,
                    kind: Some(foreign.entity_kind),
                }))
                .map_err(|e| e.to_string())?;
            links.push(TreeLinkInput {
                from: entity_id,
                target: TreeLinkTarget::External(external),
                kind: LinkKind::TypeReference,
                confidence: backend_semantic::ir::Confidence::Compiler,
                authority: OccurrenceAuthorityFacts {
                    source: FactAvailability::Unavailable,
                },
                source: None,
            });
        }
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[fixture_version(source_identity_byte)],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn project_call_image(
        path: &str,
        source_identity_byte: u8,
        function_name: &[u8],
        entity_id: TreeEntityId,
        foreign_call: Option<ForeignCallFixture>,
    ) -> Result<Vec<u8>, String> {
        project_item_image(
            path,
            source_identity_byte,
            function_name,
            entity_id,
            ItemKind::Function,
            foreign_call,
        )
    }

    fn project_item_image(
        path: &str,
        source_identity_byte: u8,
        item_name: &[u8],
        entity_id: TreeEntityId,
        item_kind: ItemKind,
        foreign_call: Option<ForeignCallFixture>,
    ) -> Result<Vec<u8>, String> {
        project_item_image_inner(
            path,
            source_identity_byte,
            item_name,
            entity_id,
            item_kind,
            foreign_call,
            false,
        )
    }

    fn project_item_image_without_foreign_kind(
        path: &str,
        source_identity_byte: u8,
        item_name: &[u8],
        entity_id: TreeEntityId,
        item_kind: ItemKind,
        foreign_call: ForeignCallFixture,
    ) -> Result<Vec<u8>, String> {
        project_item_image_inner(
            path,
            source_identity_byte,
            item_name,
            entity_id,
            item_kind,
            Some(foreign_call),
            true,
        )
    }

    fn project_item_image_inner(
        path: &str,
        source_identity_byte: u8,
        item_name: &[u8],
        entity_id: TreeEntityId,
        item_kind: ItemKind,
        foreign_call: Option<ForeignCallFixture>,
        omit_foreign_kind: bool,
    ) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_identity_byte]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [TreeItemInput {
            name: item_name,
            anonymous_callable_anchor: None,
            kind: item_kind,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        let mut links = Vec::new();
        if let Some(foreign_call) = foreign_call {
            let ecosystem = builder.intern_atom(b"npm").map_err(|e| e.to_string())?;
            let package_atom = builder
                .intern_atom(foreign_call.package_specifier)
                .map_err(|e| e.to_string())?;
            let display = builder
                .intern_atom(foreign_call.display)
                .map_err(|e| e.to_string())?;
            let path_atom = builder
                .intern_atom(foreign_call.path_specifier.unwrap_or(foreign_call.display))
                .map_err(|e| e.to_string())?;
            let external = builder
                .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                    identity: ExternalDeclarationIdentity {
                        foreign: ForeignDeclarationId::from_raw([foreign_call.foreign_key; 16]),
                        variant: VariantAvailability::Unavailable,
                    },
                    origin: ForeignTargetOrigin::Package {
                        ecosystem,
                        package: package_atom,
                    },
                    path: path_atom,
                    display,
                    kind: if omit_foreign_kind {
                        None
                    } else {
                        Some(foreign_call.entity_kind)
                    },
                }))
                .map_err(|e| e.to_string())?;
            links.push(TreeLinkInput {
                from: entity_id,
                target: TreeLinkTarget::External(external),
                kind: foreign_call.link_kind,
                confidence: backend_semantic::ir::Confidence::Compiler,
                authority: OccurrenceAuthorityFacts {
                    source: FactAvailability::Unavailable,
                },
                source: None,
            });
        }
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[fixture_version(source_identity_byte)],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn same_image_local_call() -> Result<(Vec<u8>, EntityVersion, EntityVersion), String> {
        let caller_version = fixture_version(11);
        let callee_version = fixture_version(12);
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[11]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, "local.ts")
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"caller",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"callee",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        let links = [TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[caller_version, callee_version],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok((bytes, caller_version, callee_version))
    }

    fn semantic_view_rows(
        package: backend_engine::PackageKey,
        declarations: &[(&str, u32, &str, EntityVersion)],
    ) -> Result<Vec<Row>, String> {
        let (initial, _) = initial_view().map_err(|error| error.to_string())?;
        let mut rows = Vec::new();
        for (path, line, name, version) in declarations {
            let identity = version.identity();
            let label = semantic_coordinate("fixture", identity, name);
            let symbol = semantic_symbol(package, identity);
            let location =
                backend_compile::SourceLocation::new(*path, *line).map_err(|error| error)?;
            rows.push(
                Row::in_package(RowId::Symbol(symbol), initial.basis(), package, &label)
                    .with_kind(backend_engine::DeclarationKind::Function)
                    .with_source(location),
            );
        }
        Ok(rows)
    }

    fn semantic_view(rows: Vec<Row>) -> Result<ViewRoot, String> {
        let (initial, _) = initial_view().map_err(|error| error.to_string())?;
        let capability = super::super::super::test_builtin_view_capability()
            .map_err(|error| error.to_string())?;
        ViewRoot::new_checked(
            initial.recipe(),
            initial.basis(),
            initial.frontier(),
            rows,
            vec![backend_engine::ViewCoverage::Complete],
            capability,
        )
        .map_err(|error| format!("{error:?}"))
    }

    #[test]
    fn nested_occurrence_keeps_selected_owner_evidence_and_distinct_sites() -> Result<(), String> {
        let package = package_key("fixture");
        let outer = semantic_symbol(package, fixture_version(1).identity());
        let inner = semantic_symbol(package, fixture_version(2).identity());
        let mut rows = semantic_view_rows(package, &[("a.ts", 1, "outer", fixture_version(1)), ("a.ts", 2, "inner", fixture_version(2))])?;
        rows[1] = rows[1].clone().with_parent(outer);
        let view = semantic_view(rows)?;
        let target = backend_engine::SemanticLinkTarget::Local { declaration: semantic_declaration_identity(fixture_version(3).identity()) };
        let fact = |site, start, confidence| backend_engine::ReferenceFact {
            site, target: target.clone(), relation: backend_engine::SemanticLinkKind::Calls,
            evidence: backend_engine::SemanticLinkEvidence { confidence, source: Some(backend_engine::SemanticSourceSpan { file: backend_engine::ProductText::from_static("a.ts"), start, end: start + 3 }) },
        };
        let mut facts = vec![fact(outer, 20, backend_engine::SemanticConfidence::Compiler), fact(inner, 20, backend_engine::SemanticConfidence::Syntactic), fact(inner, 40, backend_engine::SemanticConfidence::Compiler)];
        super::deduplicate_nested_reference_facts(&view, &mut facts);
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].site, inner);
        assert_eq!(facts[0].evidence.confidence, backend_engine::SemanticConfidence::Syntactic);
        assert_eq!(facts[1].evidence.source.as_ref().map(|source| source.start), Some(40));
        Ok(())
    }

    #[test]
    fn nested_occurrences_are_deterministic_and_only_collapse_proven_duplicates() -> Result<(), String> {
        let package = package_key("fixture");
        let outer = semantic_symbol(package, fixture_version(1).identity());
        let inner = semantic_symbol(package, fixture_version(2).identity());
        let sibling = semantic_symbol(package, fixture_version(3).identity());
        let mut rows = semantic_view_rows(package, &[("a.ts", 1, "outer", fixture_version(1)), ("a.ts", 2, "inner", fixture_version(2)), ("a.ts", 3, "sibling", fixture_version(3))])?;
        rows[1] = rows[1].clone().with_parent(outer);
        rows[2] = rows[2].clone().with_parent(outer);
        let view = semantic_view(rows)?;
        let target = backend_engine::SemanticLinkTarget::Local { declaration: semantic_declaration_identity(fixture_version(4).identity()) };
        let fact = |site, start: Option<u32>| backend_engine::ReferenceFact {
            site, target: target.clone(), relation: backend_engine::SemanticLinkKind::Calls,
            evidence: backend_engine::SemanticLinkEvidence { confidence: backend_engine::SemanticConfidence::Syntactic, source: start.map(|start| backend_engine::SemanticSourceSpan { file: backend_engine::ProductText::from_static("a.ts"), start, end: start + 3 }) },
        };
        let none = fact(inner, None);
        let at20 = fact(inner, Some(20));
        let sibling20 = fact(sibling, Some(20));
        let mut relation = at20.clone();
        relation.relation = backend_engine::SemanticLinkKind::Reads;
        let mut other_target = at20.clone();
        other_target.target = backend_engine::SemanticLinkTarget::Local { declaration: semantic_declaration_identity(fixture_version(5).identity()) };
        let input = vec![none.clone(), none.clone(), fact(outer, Some(20)), at20.clone(), sibling20.clone(), fact(inner, Some(40)), relation.clone(), other_target.clone()];
        let mut forward = input.clone();
        let mut reverse = input;
        reverse.reverse();
        super::deduplicate_nested_reference_facts(&view, &mut forward);
        super::deduplicate_nested_reference_facts(&view, &mut reverse);
        assert_eq!(forward, reverse);
        assert_eq!(forward.len(), 6);
        for expected in [none, at20, sibling20, fact(inner, Some(40)), relation, other_target] {
            assert!(forward.contains(&expected));
        }
        Ok(())
    }

    fn project_paths(files: &[&str]) -> BTreeSet<String> {
        files.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn structural_graph_union_keeps_selected_semantic_confidence_and_adds_only_missing_edges()
    -> Result<(), String> {
        use crate::test_support::TestValue as _;
        let from = RowId::Symbol(backend_engine::symbol_key("pkg::from"));
        let to = RowId::Symbol(backend_engine::symbol_key("pkg::to"));
        let missing = RowId::Symbol(backend_engine::symbol_key("pkg::missing"));
        for confidence in [
            None,
            Some(backend_engine::SemanticConfidence::Compiler),
            Some(backend_engine::SemanticConfidence::Heuristic),
        ] {
            let selected = GraphRelation {
                confidence,
                ..GraphRelation::new(from, to, backend_engine::SemanticLinkKind::Calls)
            };
            let mut relations = BTreeSet::from([selected]);
            super::extend_structural_graph_relations(
                &mut relations,
                [
                    GraphRelation::new(from, to, backend_engine::SemanticLinkKind::Calls)
                        .with_confidence(backend_engine::SemanticConfidence::Syntactic),
                    GraphRelation::new(from, missing, backend_engine::SemanticLinkKind::Calls)
                        .with_confidence(backend_engine::SemanticConfidence::Syntactic),
                ],
            );
            assert_eq!(relations.len(), 2);
            assert!(relations.contains(&selected));
            assert_eq!(
                relations
                    .iter()
                    .find(|relation| relation.to == missing)
                    .test_value()?
                    .confidence,
                Some(backend_engine::SemanticConfidence::Syntactic)
            );
        }
        Ok(())
    }

    fn relation_targets(relations: &BTreeSet<GraphRelation>, from: RowId) -> Vec<RowId> {
        relations
            .iter()
            .filter(|relation| relation.from == from)
            .map(|relation| relation.to)
            .collect()
    }

    #[test]
    fn project_call_semantic_neighborhood_without_indexed_sources() -> Result<(), String> {
        let package = package_key("fixture");
        let (bytes, caller_version, callee_version) = same_image_local_call()?;
        let rows = semantic_view_rows(
            package,
            &[
                ("local.ts", 1, "caller", caller_version),
                ("local.ts", 2, "callee", callee_version),
            ],
        )?;
        let view = semantic_view(rows)?;
        let caller_id = RowId::Symbol(semantic_symbol(package, caller_version.identity()));
        let callee_id = RowId::Symbol(semantic_symbol(package, callee_version.identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&bytes],
            &view,
            package,
            semantic_symbol(package, caller_version.identity()),
            caller_id,
            false,
            &BTreeSet::new(),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, caller_id);
        if targets != vec![callee_id] {
            return Err(format!(
                "expected local Calls edge without indexed sources, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_foreign_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"./apply-set",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 9,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Calls,
            }),
        )?;
        let entries_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let entries_id = RowId::Symbol(semantic_symbol(package, entries_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["apply-set.ts", "weeks.ts"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![entries_id] {
            return Err(format!(
                "expected syncWorkout to call entriesFromItems semantic row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_foreign_retarget_incoming_from_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"./apply-set",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 9,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Calls,
            }),
        )?;
        let entries_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let entries_id = RowId::Symbol(semantic_symbol(package, entries_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, entries_identity),
            entries_id,
            true,
            &project_paths(&["apply-set.ts", "weeks.ts"]),
        )
        .map_err(|error| error.to_string())?;
        if !relations
            .iter()
            .any(|relation| relation.from == sync_id && relation.to == entries_id)
        {
            return Err(format!(
                "expected incoming edge from syncWorkout, got {relations:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_foreign_ambiguous_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let duplicate_apply_bytes = project_call_image(
            "apply-set.ts",
            3,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"./apply-set",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 9,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Calls,
            }),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let entries_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&apply_bytes, &duplicate_apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["apply-set.ts", "weeks.ts"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&entries_id) {
            return Err("ambiguous foreign target must not retarget to apply-set.ts".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_foreign_unresolved_package_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"lodash",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 10,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Calls,
            }),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let entries_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["apply-set.ts", "weeks.ts"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&entries_id) {
            return Err("lodash import must not retarget to entriesFromItems".to_owned());
        }
        Ok(())
    }

    fn dotted_foreign_call_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"workout",
            path_specifier: Some(b"workout.service"),
            display: b"set_note",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Calls,
        }
    }

    fn dotted_foreign_call_fixture_with_path(
        path_specifier: &'static [u8],
        foreign_key: u8,
    ) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"workout",
            path_specifier: Some(path_specifier),
            display: b"set_note",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Calls,
        }
    }

    #[test]
    fn project_call_dotted_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "workout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(30)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["workout/service.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected sync_workout to call set_note semantic row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_dotted_src_suffix_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/workout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(31)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/workout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["src/workout/service.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected sync_workout to call src/workout/service.py set_note row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_dotted_init_package_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "workout/service/__init__.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(32)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                (
                    "workout/service/__init__.py",
                    1,
                    "set_note",
                    fixture_version(1),
                ),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["workout/service/__init__.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected sync_workout to call workout/service/__init__.py set_note row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_dotted_retarget_incoming_from_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "workout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(33)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            set_note_id,
            true,
            &project_paths(&["workout/service.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        if !relations
            .iter()
            .any(|relation| relation.from == sync_id && relation.to == set_note_id)
        {
            return Err(format!(
                "expected incoming edge from sync_workout, got {relations:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_dotted_package_head_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let workout_bytes =
            project_call_image("workout.py", 1, b"set_note", TreeEntityId::new(0), None)?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(34)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&workout_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["workout.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("dotted import must not fall back to workout.py".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_dotted_ambiguous_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let root_service_bytes = project_call_image(
            "workout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let src_service_bytes = project_call_image(
            "src/workout/service.py",
            3,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(35)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&root_service_bytes, &src_service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["workout/service.py", "src/workout/service.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("ambiguous dotted module target must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_dotted_wrong_prefix_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "notworkout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(36)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("notworkout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["notworkout/service.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("notworkout/service.py must not satisfy workout.service".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_dotted_invalid_path_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture_with_path(
                b"workout..service",
                37,
            )),
        )?;
        let weeks_dot_bytes = project_call_image(
            "weeks.py",
            4,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture_with_path(
                b".workout.service",
                38,
            )),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[("weeks.py", 2, "sync_workout", fixture_version(2))],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        for weeks_image in [&weeks_bytes, &weeks_dot_bytes] {
            let relations = project_semantic_graph_relations_from_bytes(
                &[weeks_image],
                &view,
                package,
                semantic_symbol(package, sync_identity),
                sync_id,
                false,
                &project_paths(&["weeks.py"]),
            )
            .map_err(|error| error.to_string())?;
            if !relation_targets(&relations, sync_id).is_empty() {
                return Err("invalid dotted path must stay external".to_owned());
            }
        }
        Ok(())
    }

    fn go_import_foreign_call_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"example.com/gymbro/workout",
            path_specifier: None,
            display: b"SetNote",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Calls,
        }
    }

    fn go_import_foreign_call_fixture_with_package(
        package_specifier: &'static [u8],
        foreign_key: u8,
    ) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier,
            path_specifier: None,
            display: b"SetNote",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Calls,
        }
    }

    #[test]
    fn project_call_go_import_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/workout/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.go",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(go_import_foreign_call_fixture(60)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/workout/service.go", 1, "SetNote", fixture_version(1)),
                ("weeks.go", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["src/workout/service.go", "weeks.go"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected syncWorkout to call SetNote semantic row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_go_import_retarget_incoming_from_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/workout/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.go",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(go_import_foreign_call_fixture(61)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/workout/service.go", 1, "SetNote", fixture_version(1)),
                ("weeks.go", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            set_note_id,
            true,
            &project_paths(&["src/workout/service.go", "weeks.go"]),
        )
        .map_err(|error| error.to_string())?;
        if !relations
            .iter()
            .any(|relation| relation.from == sync_id && relation.to == set_note_id)
        {
            return Err(format!(
                "expected incoming edge from syncWorkout, got {relations:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_go_import_longest_suffix_wins() -> Result<(), String> {
        let package = package_key("fixture");
        let real_bytes = project_call_image(
            "example.com/gymbro/workout/real.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let decoy_bytes = project_call_image(
            "workout/decoy.go",
            3,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.go",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(go_import_foreign_call_fixture(62)),
        )?;
        let real_identity = fixture_version(1).identity();
        let decoy_identity = fixture_version(3).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                (
                    "example.com/gymbro/workout/real.go",
                    1,
                    "SetNote",
                    fixture_version(1),
                ),
                ("weeks.go", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let real_id = RowId::Symbol(semantic_symbol(package, real_identity));
        let decoy_id = RowId::Symbol(semantic_symbol(package, decoy_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&real_bytes, &decoy_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&[
                "example.com/gymbro/workout/real.go",
                "workout/decoy.go",
                "weeks.go",
            ]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![real_id] {
            return Err(format!(
                "expected syncWorkout to call real.go SetNote row, got {targets:?}"
            ));
        }
        if targets.contains(&decoy_id) {
            return Err("longest import suffix must not retarget to workout/decoy.go".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_go_import_notworkout_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "notworkout/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.go",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(go_import_foreign_call_fixture(63)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("notworkout/service.go", 1, "SetNote", fixture_version(1)),
                ("weeks.go", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["notworkout/service.go", "weeks.go"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err(
                "notworkout/service.go must not satisfy example.com/gymbro/workout".to_owned(),
            );
        }
        Ok(())
    }

    #[test]
    fn project_call_go_import_subdir_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "workout/sub/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.go",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(go_import_foreign_call_fixture(64)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/sub/service.go", 1, "SetNote", fixture_version(1)),
                ("weeks.go", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["workout/sub/service.go", "weeks.go"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("workout/sub/service.go must not satisfy suffix workout".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_go_import_ambiguous_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let root_service_bytes = project_call_image(
            "workout/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let other_service_bytes = project_call_image(
            "other/workout/service.go",
            3,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.go",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(go_import_foreign_call_fixture(65)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/service.go", 1, "SetNote", fixture_version(1)),
                ("weeks.go", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&root_service_bytes, &other_service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["workout/service.go", "other/workout/service.go", "weeks.go"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("ambiguous go import suffix must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_go_import_empty_segment_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/workout/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.go",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(go_import_foreign_call_fixture_with_package(
                b"example.com//workout",
                66,
            )),
        )?;
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/workout/service.go", 1, "SetNote", fixture_version(1)),
                ("weeks.go", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["src/workout/service.go", "weeks.go"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("empty import segment must not retarget to SetNote".to_owned());
        }
        Ok(())
    }

    fn rust_module_foreign_call_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"src/service",
            path_specifier: None,
            display: b"set_note",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::MethodCall,
        }
    }

    fn rust_function_foreign_call_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"src/service",
            path_specifier: None,
            display: b"set_note",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Calls,
        }
    }

    fn rust_type_reference_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"src/service",
            path_specifier: None,
            display: b"Workout",
            foreign_key,
            entity_kind: ItemKind::Record,
            link_kind: LinkKind::TypeReference,
        }
    }

    fn rust_field_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"src/service",
            path_specifier: None,
            display: b"note",
            foreign_key,
            entity_kind: ItemKind::Field,
            link_kind: LinkKind::Reads,
        }
    }

    fn rust_value_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"src/service",
            path_specifier: None,
            display: b"LIMIT",
            foreign_key,
            entity_kind: ItemKind::Constant,
            link_kind: LinkKind::Reads,
        }
    }

    fn rust_function_value_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"src/service",
            path_specifier: None,
            display: b"set_note",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Reads,
        }
    }

    fn go_function_value_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"example.com/demo",
            path_specifier: None,
            display: b"SetNote",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Reads,
        }
    }

    fn python_field_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"workout",
            path_specifier: Some(b"workout.service"),
            display: b"note",
            foreign_key,
            entity_kind: ItemKind::Field,
            link_kind: LinkKind::Reads,
        }
    }

    fn python_set_note_field_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"workout",
            path_specifier: Some(b"workout.service"),
            display: b"set_note",
            foreign_key,
            entity_kind: ItemKind::Field,
            link_kind: LinkKind::Reads,
        }
    }

    fn python_note_static_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "workout/service.py",
            1,
            b"note",
            TreeEntityId::new(0),
            ItemKind::Static,
            None,
        )?;
        let caller_bytes = project_item_image(
            "weeks.py",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(python_field_read_foreign_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn python_set_note_function_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "workout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            ItemKind::Function,
            None,
        )?;
        let caller_bytes = project_item_image(
            "weeks.py",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(python_set_note_field_read_foreign_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn ts_method_value_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"./workout.service",
            path_specifier: None,
            display: b"setNote",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Reads,
        }
    }

    fn ts_const_value_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"./demo",
            path_specifier: None,
            display: b"note",
            foreign_key,
            entity_kind: ItemKind::Constant,
            link_kind: LinkKind::Reads,
        }
    }

    fn ts_static_value_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"./demo",
            path_specifier: None,
            display: b"count",
            foreign_key,
            entity_kind: ItemKind::Static,
            link_kind: LinkKind::Reads,
        }
    }

    fn ts_function_named_note_read_foreign_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"./demo",
            path_specifier: None,
            display: b"note",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Reads,
        }
    }

    fn ts_demo_const_note_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "demo.ts",
            1,
            b"note",
            TreeEntityId::new(0),
            ItemKind::Constant,
            None,
        )?;
        let caller_bytes = project_item_image(
            "read.ts",
            2,
            b"read",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(ts_const_value_read_foreign_fixture(foreign_key)),
        )?;
        Ok((service_bytes, caller_bytes, fixture_version(1).identity()))
    }

    fn ts_demo_static_count_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "demo.ts",
            1,
            b"count",
            TreeEntityId::new(0),
            ItemKind::Static,
            None,
        )?;
        let caller_bytes = project_item_image(
            "read.ts",
            2,
            b"read",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(ts_static_value_read_foreign_fixture(foreign_key)),
        )?;
        Ok((service_bytes, caller_bytes, fixture_version(1).identity()))
    }

    fn ts_set_note_value_group_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = project_call_image(
            "workout.service.ts",
            1,
            b"setNote",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_item_image(
            "weeks.ts",
            2,
            b"group",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(ts_method_value_read_foreign_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn rust_limit_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "src/service.rs",
            1,
            b"LIMIT",
            TreeEntityId::new(0),
            ItemKind::Constant,
            None,
        )?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_value_read_foreign_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn rust_set_note_value_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_function_value_read_foreign_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn go_set_note_value_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes =
            project_call_image("demo/service.go", 1, b"SetNote", TreeEntityId::new(0), None)?;
        let caller_bytes = project_item_image(
            "demo/lib.go",
            2,
            b"Drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(go_function_value_read_foreign_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn foreign_value_read_from_caller(
        caller_bytes: &[u8],
    ) -> Result<(ExternalId, LinkKind, String), String> {
        let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
        let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
        for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(0)) {
            if matches!(link.kind, LinkKind::Reads) {
                if let LinkTarget::External(external) = link.target {
                    return Ok((external, link.kind, caller_path));
                }
            }
        }
        Err("caller fixture has no foreign value read".to_owned())
    }

    fn project_field_image(
        path: &str,
        source_identity_byte: u8,
        field_name: &[u8],
        entity_id: TreeEntityId,
        foreign_read: Option<ForeignCallFixture>,
    ) -> Result<Vec<u8>, String> {
        project_item_image(
            path,
            source_identity_byte,
            field_name,
            entity_id,
            ItemKind::Field,
            foreign_read,
        )
    }

    fn rust_note_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes =
            project_field_image("src/service.rs", 1, b"note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_field_read_foreign_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn foreign_field_read_from_caller(
        caller_bytes: &[u8],
    ) -> Result<(ExternalId, LinkKind, String), String> {
        let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
        let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
        for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(0)) {
            if matches!(link.kind, LinkKind::Reads) {
                if let LinkTarget::External(external) = link.target {
                    return Ok((external, link.kind, caller_path));
                }
            }
        }
        Err("caller fixture has no foreign field read".to_owned())
    }

    fn java_universe_workout_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "WorkoutService.java",
            1,
            b"demo.WorkoutService",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_universe_mention_image(
            "Weeks.java",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(UniverseMentionFixture {
                ecosystem: b"maven",
                spelling: b"demo.WorkoutService",
                foreign_key,
                entity_kind: ItemKind::Record,
            }),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn rust_workout_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "src/service.rs",
            1,
            b"Workout",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_type_reference_foreign_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn foreign_mention_from_caller(
        caller_bytes: &[u8],
    ) -> Result<(ExternalId, LinkKind, String), String> {
        let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
        let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
        for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(0)) {
            if matches!(link.kind, LinkKind::TypeReference | LinkKind::Imports) {
                if let LinkTarget::External(external) = link.target {
                    return Ok((external, link.kind, caller_path));
                }
            }
        }
        Err("caller fixture has no foreign type mention".to_owned())
    }

    struct ImportMentionFixture {
        ecosystem: &'static [u8],
        package_specifier: &'static [u8],
        path_specifier: Option<&'static [u8]>,
        display: &'static [u8],
        foreign_key: u8,
        link_kind: LinkKind,
        foreign_kind: Option<ItemKind>,
    }

    fn project_import_mention_image(
        path: &str,
        source_identity_byte: u8,
        caller_name: &[u8],
        entity_id: TreeEntityId,
        import: ImportMentionFixture,
    ) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_identity_byte]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [TreeItemInput {
            name: caller_name,
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        let mut links = Vec::new();
        let ecosystem = builder
            .intern_atom(import.ecosystem)
            .map_err(|e| e.to_string())?;
        let package_atom = builder
            .intern_atom(import.package_specifier)
            .map_err(|e| e.to_string())?;
        let display = builder
            .intern_atom(import.display)
            .map_err(|e| e.to_string())?;
        let path_atom = builder
            .intern_atom(import.path_specifier.unwrap_or(import.display))
            .map_err(|e| e.to_string())?;
        let external = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([import.foreign_key; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Package {
                    ecosystem,
                    package: package_atom,
                },
                path: path_atom,
                display,
                kind: import.foreign_kind,
            }))
            .map_err(|e| e.to_string())?;
        links.push(TreeLinkInput {
            from: entity_id,
            target: TreeLinkTarget::External(external),
            kind: import.link_kind,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        });
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[fixture_version(source_identity_byte)],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn ts_workout_service_import_fixture(
        foreign_key: u8,
        link_kind: LinkKind,
    ) -> ImportMentionFixture {
        ImportMentionFixture {
            ecosystem: b"npm",
            package_specifier: b"./workout.service",
            path_specifier: Some(b"WorkoutService"),
            display: b"WorkoutService",
            foreign_key,
            link_kind,
            foreign_kind: Some(ItemKind::Reexport),
        }
    }

    fn python_workout_import_fixture(foreign_key: u8) -> ImportMentionFixture {
        ImportMentionFixture {
            ecosystem: b"pypi",
            package_specifier: b"workout",
            path_specifier: Some(b"workout.service"),
            display: b"Workout",
            foreign_key,
            link_kind: LinkKind::Imports,
            foreign_kind: None,
        }
    }

    fn ts_workout_service_drive_fixture(
        foreign_key: u8,
        link_kind: LinkKind,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "workout.service.ts",
            1,
            b"WorkoutService",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_import_mention_image(
            "drive.ts",
            2,
            b"drive",
            TreeEntityId::new(0),
            ts_workout_service_import_fixture(foreign_key, link_kind),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn python_workout_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = project_item_image(
            "workout/service.py",
            1,
            b"Workout",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_import_mention_image(
            "weeks.py",
            2,
            b"drive",
            TreeEntityId::new(0),
            python_workout_import_fixture(foreign_key),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn rust_set_note_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_function_foreign_call_fixture(foreign_key)),
        )?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ))
    }

    fn foreign_call_from_caller(
        caller_bytes: &[u8],
    ) -> Result<(ExternalId, LinkKind, String), String> {
        let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
        let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
        for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(0)) {
            if let LinkTarget::External(external) = link.target {
                return Ok((external, link.kind, caller_path));
            }
        }
        Err("caller fixture has no foreign call".to_owned())
    }

    fn query_corpus_coordinate() -> Result<PackageUrl, String> {
        PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))
    }

    fn compiler_query_presentation(
        package: backend_engine::PackageKey,
        label: &str,
        image_bytes: &[u8],
        identity: DeclarationIdentity,
        name: &str,
        related: Box<[String]>,
    ) -> Result<(SemanticQueryFact, String), String> {
        let image = SemanticImageView::reopen(image_bytes).map_err(|error| error.to_string())?;
        let coordinate = query_corpus_coordinate()?;
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let image_digest = *blake3::hash(image_bytes).as_bytes();
        let evidence = CompilerSemanticEvidence::new(
            package,
            coordinate,
            profile,
            identity,
            image_digest,
            image.image_facts(),
        );
        let id = evidence.row_id();
        Ok((
            SemanticQueryFact::new(
                SemanticQueryEvidence::Compiler(evidence),
                SemanticQueryPresentation {
                    id: id.clone(),
                    kind: "function".to_owned(),
                    coordinate: semantic_coordinate(label, identity, name),
                    name: name.to_owned(),
                    signature: None,
                    documentation: String::new(),
                    score: None,
                    project: Some(RowId::Package(package).stable_key()),
                    parent: None,
                    related,
                },
            ),
            id,
        ))
    }

    #[test]
    fn join_project_call_rust_function_retargets_set_note() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) = rust_set_note_drive_fixture(79)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_call_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(set_note_identity) {
            return Err(format!(
                "join_project_call should retarget to set_note, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_call_query_corpus_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, set_note_identity, drive_identity) =
            rust_set_note_drive_fixture(83)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_call_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_call returned None".to_owned())?;
        let set_note_id = query_semantic_id(package, joined);
        let (set_note_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            set_note_identity,
            "set_note",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "drive",
            vec![set_note_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                set_note_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "set_note".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on set_note should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    fn go_workout_set_note_service_image(
        path: &str,
        source_identity_byte: u8,
        workout_version_byte: u8,
        set_note_version_byte: u8,
    ) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_identity_byte]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let workout_version = fixture_version(workout_version_byte);
        let set_note_version = fixture_version(set_note_version_byte);
        let items = [
            TreeItemInput {
                name: b"Workout",
                anonymous_callable_anchor: None,
                kind: ItemKind::Record,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"SetNote",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Bound(workout_version.identity())),
                parent: Some(TreeEntityId::new(0)),
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
                versions: &[workout_version, set_note_version],
                items: &items,
                links: &[],
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn go_workout_and_session_set_note_service_image(
        path: &str,
        source_identity_byte: u8,
        workout_version_byte: u8,
        workout_set_note_version_byte: u8,
        session_version_byte: u8,
        session_set_note_version_byte: u8,
    ) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_identity_byte]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let workout_version = fixture_version(workout_version_byte);
        let workout_set_note_version = fixture_version(workout_set_note_version_byte);
        let session_version = fixture_version(session_version_byte);
        let session_set_note_version = fixture_version(session_set_note_version_byte);
        let items = [
            TreeItemInput {
                name: b"Workout",
                anonymous_callable_anchor: None,
                kind: ItemKind::Record,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"SetNote",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Bound(workout_version.identity())),
                parent: Some(TreeEntityId::new(0)),
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"Session",
                anonymous_callable_anchor: None,
                kind: ItemKind::Record,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"SetNote",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Bound(session_version.identity())),
                parent: Some(TreeEntityId::new(2)),
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
                versions: &[
                    workout_version,
                    workout_set_note_version,
                    session_version,
                    session_set_note_version,
                ],
                items: &items,
                links: &[],
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn go_drive_namespace_method_call_image(
        path: &str,
        source_identity_byte: u8,
        drive_version_byte: u8,
        foreign_key: u8,
    ) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_identity_byte]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let drive_version = fixture_version(drive_version_byte);
        let items = [TreeItemInput {
            name: b"Drive",
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        let ecosystem = builder.intern_atom(b"go").map_err(|e| e.to_string())?;
        let namespace_atom = builder.intern_atom(b"Workout").map_err(|e| e.to_string())?;
        let display = builder.intern_atom(b"SetNote").map_err(|e| e.to_string())?;
        let path_atom = builder.intern_atom(b"SetNote").map_err(|e| e.to_string())?;
        let external = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([foreign_key; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Namespace {
                    ecosystem,
                    namespace: namespace_atom,
                },
                path: path_atom,
                display,
                kind: Some(ItemKind::Function),
            }))
            .map_err(|e| e.to_string())?;
        let links = [TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::External(external),
            kind: LinkKind::MethodCall,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[drive_version],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn go_set_note_drive_fixture(
        foreign_key: u8,
    ) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
        let service_bytes = go_workout_set_note_service_image("demo/service.go", 140, 141, 142)?;
        let caller_bytes =
            go_drive_namespace_method_call_image("demo/lib.go", 143, 144, foreign_key)?;
        Ok((
            service_bytes,
            caller_bytes,
            fixture_version(142).identity(),
            fixture_version(144).identity(),
        ))
    }

    #[test]
    fn join_project_call_go_method_retargets_set_note() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) = go_set_note_drive_fixture(145)?;
        let paths = project_paths(&["demo/service.go", "demo/lib.go"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(144).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &caller_bytes,
            TreeEntityId::new(0),
            LinkKind::MethodCall,
        )?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(set_note_identity) {
            return Err(format!(
                "join_project_call should retarget to SetNote, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_call_go_method_query_corpus_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, set_note_identity, drive_identity) =
            go_set_note_drive_fixture(146)?;
        let paths = project_paths(&["demo/service.go", "demo/lib.go"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &caller_bytes,
            TreeEntityId::new(0),
            LinkKind::MethodCall,
        )?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_call returned None".to_owned())?;
        let set_note_id = query_semantic_id(package, joined);
        let (set_note_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            set_note_identity,
            "SetNote",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "Drive",
            vec![set_note_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                set_note_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "SetNote".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["Drive".into()] {
            return Err(format!(
                "referencedBy on SetNote should name only Drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_call_go_method_session_set_note_not_chosen() -> Result<(), String> {
        let service_bytes = go_workout_and_session_set_note_service_image(
            "demo/service.go",
            147,
            148,
            149,
            150,
            151,
        )?;
        let caller_bytes = go_drive_namespace_method_call_image("demo/lib.go", 152, 153, 147)?;
        let workout_set_note_identity = fixture_version(149).identity();
        let session_set_note_identity = fixture_version(151).identity();
        let paths = project_paths(&["demo/service.go", "demo/lib.go"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            workout_set_note_identity,
            session_set_note_identity,
            fixture_version(153).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &caller_bytes,
            TreeEntityId::new(0),
            LinkKind::MethodCall,
        )?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(workout_set_note_identity) {
            return Err(format!(
                "Workout namespace key must join Workout SetNote, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_call_go_method_ambiguous_set_note_returns_none() -> Result<(), String> {
        let first_service = go_workout_set_note_service_image("demo/service_a.go", 154, 155, 156)?;
        let second_service = go_workout_set_note_service_image("demo/service_b.go", 157, 158, 159)?;
        let caller_bytes = go_drive_namespace_method_call_image("demo/lib.go", 160, 161, 154)?;
        let paths = project_paths(&["demo/service_a.go", "demo/service_b.go", "demo/lib.go"]);
        let images = [&first_service[..], &second_service[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(156).identity(),
            fixture_version(159).identity(),
            fixture_version(161).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &caller_bytes,
            TreeEntityId::new(0),
            LinkKind::MethodCall,
        )?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != None {
            return Err(format!(
                "ambiguous Workout SetNote methods must not join, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_call_extra_path_stays_unjoined_and_foreign_display_names_call()
    -> Result<(), String> {
        let service_bytes = project_call_image(
            "src/service_extra.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_function_foreign_call_fixture(84)),
        )?;
        let paths = project_paths(&["src/service_extra.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_call_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("src/service_extra.rs must not satisfy src/service".to_owned());
        }
        let display =
            foreign_display_name(&caller_image, external).map_err(|error| error.to_string())?;
        if display.as_deref() != Some("set_note") {
            return Err(format!(
                "foreign call display should be set_note, got {display:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_call_ambiguous_rust_service_paths_returns_none() -> Result<(), String> {
        let service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let duplicate_service_bytes = project_call_image(
            "src/service/mod.rs",
            3,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_function_foreign_call_fixture(85)),
        )?;
        let paths = project_paths(&["src/service.rs", "src/service/mod.rs", "src/lib.rs"]);
        let images = [
            &service_bytes[..],
            &duplicate_service_bytes[..],
            &caller_bytes[..],
        ];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_call_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous src/service matches must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_call_non_call_link_returns_none() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) = rust_set_note_drive_fixture(86)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(2).identity()]);
        let (external, _, caller_path) = foreign_call_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_call(
            &caller_image,
            LinkKind::Reads,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Reads must not join even when the display would match".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_rust_record_retargets_workout() -> Result<(), String> {
        let (service_bytes, caller_bytes, workout_identity, _) = rust_workout_drive_fixture(87)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(workout_identity) {
            return Err(format!(
                "join_project_mention should retarget to Workout, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_query_corpus_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, workout_identity, drive_identity) =
            rust_workout_drive_fixture(88)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_mention returned None".to_owned())?;
        let workout_id = query_semantic_id(package, joined);
        let (workout_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            workout_identity,
            "Workout",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "drive",
            vec![workout_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                workout_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "Workout".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on Workout should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_extra_path_stays_unjoined() -> Result<(), String> {
        let service_bytes = project_item_image(
            "src/service_extra.rs",
            1,
            b"Workout",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_type_reference_foreign_fixture(89)),
        )?;
        let paths = project_paths(&["src/service_extra.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("src/service_extra.rs must not satisfy src/service".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_ambiguous_rust_service_paths_returns_none() -> Result<(), String> {
        let service_bytes = project_item_image(
            "src/service.rs",
            1,
            b"Workout",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let duplicate_service_bytes = project_item_image(
            "src/service/mod.rs",
            3,
            b"Workout",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_type_reference_foreign_fixture(90)),
        )?;
        let paths = project_paths(&["src/service.rs", "src/service/mod.rs", "src/lib.rs"]);
        let images = [
            &service_bytes[..],
            &duplicate_service_bytes[..],
            &caller_bytes[..],
        ];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous src/service matches must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_reads_or_calls_returns_none() -> Result<(), String> {
        let (service_bytes, caller_bytes, workout_identity, _) = rust_workout_drive_fixture(91)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, fixture_version(2).identity()]);
        let (external, _, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        for link_kind in [LinkKind::Reads, LinkKind::Calls] {
            if join_project_mention(
                &caller_image,
                link_kind,
                external,
                &caller_path,
                &paths,
                &index,
                &published,
            )
            .map_err(|error| error.to_string())?
            .is_some()
            {
                return Err(format!(
                    "{link_kind:?} must not join through join_project_mention"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_does_not_find_functions() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) = rust_set_note_drive_fixture(92)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_call_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("functions must not be found by join_project_mention".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_universe_java_record_retargets_workout_service() -> Result<(), String> {
        let (service_bytes, caller_bytes, workout_identity, _) =
            java_universe_workout_drive_fixture(101)?;
        let paths = project_paths(&["WorkoutService.java", "Weeks.java"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(workout_identity) {
            return Err(format!(
                "join_project_mention should retarget to demo.WorkoutService, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_universe_query_corpus_referenced_by_names_drive() -> Result<(), String>
    {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, workout_identity, drive_identity) =
            java_universe_workout_drive_fixture(102)?;
        let paths = project_paths(&["WorkoutService.java", "Weeks.java"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_mention returned None".to_owned())?;
        let workout_id = query_semantic_id(package, joined);
        let (workout_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            workout_identity,
            "demo.WorkoutService",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "drive",
            vec![workout_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                workout_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "demo.WorkoutService".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on demo.WorkoutService should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_universe_ambiguous_same_name_returns_none() -> Result<(), String> {
        let first_bytes = project_item_image(
            "WorkoutService.java",
            1,
            b"demo.WorkoutService",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let duplicate_bytes = project_item_image(
            "OtherWorkoutService.java",
            3,
            b"demo.WorkoutService",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_universe_mention_image(
            "Weeks.java",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(UniverseMentionFixture {
                ecosystem: b"maven",
                spelling: b"demo.WorkoutService",
                foreign_key: 103,
                entity_kind: ItemKind::Record,
            }),
        )?;
        let paths = project_paths(&[
            "WorkoutService.java",
            "OtherWorkoutService.java",
            "Weeks.java",
        ]);
        let images = [&first_bytes[..], &duplicate_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous demo.WorkoutService records must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_universe_suffix_display_returns_none() -> Result<(), String> {
        let service_bytes = project_item_image(
            "WorkoutService.java",
            1,
            b"demo.WorkoutService",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_universe_mention_image(
            "Weeks.java",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(UniverseMentionFixture {
                ecosystem: b"maven",
                spelling: b"Service",
                foreign_key: 104,
                entity_kind: ItemKind::Record,
            }),
        )?;
        let paths = project_paths(&["WorkoutService.java", "Weeks.java"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Service must not match demo.WorkoutService".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_universe_java_lang_string_returns_none() -> Result<(), String> {
        let caller_bytes = project_universe_mention_image(
            "Weeks.java",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(UniverseMentionFixture {
                ecosystem: b"maven",
                spelling: b"java.lang.String",
                foreign_key: 105,
                entity_kind: ItemKind::Record,
            }),
        )?;
        let paths = project_paths(&["Weeks.java"]);
        let images = [&caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("java.lang.String must not join without a published entity".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_universe_reads_or_method_call_returns_none() -> Result<(), String> {
        let service_bytes = project_item_image(
            "WorkoutService.java",
            1,
            b"demo.WorkoutService",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let paths = project_paths(&["WorkoutService.java", "Weeks.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let field_caller = java_namespace_note_field_image(
            "Weeks.java",
            106,
            107,
            108,
            106,
            b"demo.WorkoutService",
        )?;
        let (field_external, field_link_kind, field_caller_path) =
            foreign_namespace_link_from_caller(
                &field_caller,
                TreeEntityId::new(2),
                LinkKind::Reads,
            )?;
        let field_image =
            SemanticImageView::reopen(&field_caller).map_err(|error| error.to_string())?;
        if join_project_mention(
            &field_image,
            field_link_kind,
            field_external,
            &field_caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Reads must not join through join_project_mention".to_owned());
        }
        let method_call_bytes = project_namespace_call_image(
            "Weeks.java",
            109,
            &java_owner_chain(),
            b"setNote",
            110,
            TreeEntityId::new(1),
            b"drive",
            111,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.WorkoutService",
                display: b"setNote",
                foreign_key: 109,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let (call_external, call_link_kind, call_caller_path) = foreign_namespace_link_from_caller(
            &method_call_bytes,
            TreeEntityId::new(2),
            LinkKind::MethodCall,
        )?;
        let call_image =
            SemanticImageView::reopen(&method_call_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &call_image,
            call_link_kind,
            call_external,
            &call_caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("MethodCall must not join through join_project_mention".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_import_reexport_joins_workout_service() -> Result<(), String> {
        let (service_bytes, caller_bytes, workout_identity, _) =
            ts_workout_service_drive_fixture(120, LinkKind::Imports)?;
        let paths = project_paths(&["workout.service.ts", "drive.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(workout_identity) {
            return Err(format!(
                "import mention should retarget to WorkoutService, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_import_query_corpus_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, workout_identity, drive_identity) =
            ts_workout_service_drive_fixture(121, LinkKind::Imports)?;
        let paths = project_paths(&["workout.service.ts", "drive.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_mention returned None".to_owned())?;
        let workout_id = query_semantic_id(package, joined);
        let (workout_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            workout_identity,
            "WorkoutService",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "drive",
            vec![workout_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                workout_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "WorkoutService".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on WorkoutService should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_import_type_reference_joins_workout_service() -> Result<(), String> {
        let (service_bytes, caller_bytes, workout_identity, _) =
            ts_workout_service_drive_fixture(122, LinkKind::TypeReference)?;
        let paths = project_paths(&["workout.service.ts", "drive.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(workout_identity) {
            return Err(format!(
                "type mention of imported name should retarget to WorkoutService, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_python_import_joins_workout() -> Result<(), String> {
        let (service_bytes, caller_bytes, workout_identity, _) = python_workout_drive_fixture(123)?;
        let paths = project_paths(&["workout/service.py", "weeks.py"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([workout_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(workout_identity) {
            return Err(format!(
                "python import mention should retarget to Workout, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_import_ambiguous_same_name_returns_none() -> Result<(), String> {
        let service_bytes = project_item_image(
            "workout.service.ts",
            1,
            b"WorkoutService",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let duplicate_function_bytes = project_call_image(
            "workout.service.ts",
            3,
            b"WorkoutService",
            TreeEntityId::new(1),
            None,
        )?;
        let caller_bytes = project_import_mention_image(
            "drive.ts",
            2,
            b"drive",
            TreeEntityId::new(0),
            ts_workout_service_import_fixture(124, LinkKind::Imports),
        )?;
        let paths = project_paths(&["workout.service.ts", "drive.ts"]);
        let images = [
            &service_bytes[..],
            &duplicate_function_bytes[..],
            &caller_bytes[..],
        ];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous WorkoutService entities must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_import_field_named_note_returns_none() -> Result<(), String> {
        let service_bytes =
            project_field_image("workout.service.ts", 1, b"note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_import_mention_image(
            "drive.ts",
            2,
            b"drive",
            TreeEntityId::new(0),
            ImportMentionFixture {
                ecosystem: b"npm",
                package_specifier: b"./workout.service",
                path_specifier: Some(b"note"),
                display: b"note",
                foreign_key: 125,
                link_kind: LinkKind::Imports,
                foreign_kind: None,
            },
        )?;
        let paths = project_paths(&["workout.service.ts", "drive.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_mention(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Field named note must not satisfy import mention".to_owned());
        }
        let reexport_caller = project_import_mention_image(
            "drive.ts",
            3,
            b"drive",
            TreeEntityId::new(0),
            ImportMentionFixture {
                ecosystem: b"npm",
                package_specifier: b"./workout.service",
                path_specifier: Some(b"note"),
                display: b"note",
                foreign_key: 126,
                link_kind: LinkKind::Imports,
                foreign_kind: Some(ItemKind::Reexport),
            },
        )?;
        let (reexport_external, reexport_link_kind, reexport_caller_path) =
            foreign_mention_from_caller(&reexport_caller)?;
        let reexport_image =
            SemanticImageView::reopen(&reexport_caller).map_err(|error| error.to_string())?;
        if join_project_mention(
            &reexport_image,
            reexport_link_kind,
            reexport_external,
            &reexport_caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Field named note must not satisfy Reexport import mention".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_import_reads_returns_none() -> Result<(), String> {
        let service_bytes = project_item_image(
            "workout.service.ts",
            1,
            b"WorkoutService",
            TreeEntityId::new(0),
            ItemKind::Record,
            None,
        )?;
        let caller_bytes = project_import_mention_image(
            "drive.ts",
            2,
            b"drive",
            TreeEntityId::new(0),
            ImportMentionFixture {
                ecosystem: b"npm",
                package_specifier: b"./workout.service",
                path_specifier: Some(b"WorkoutService"),
                display: b"WorkoutService",
                foreign_key: 127,
                link_kind: LinkKind::Reads,
                foreign_kind: Some(ItemKind::Reexport),
            },
        )?;
        let paths = project_paths(&["workout.service.ts", "drive.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let image = SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
        let mut external = None;
        for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(0)) {
            if link.kind == LinkKind::Reads {
                if let LinkTarget::External(found) = link.target {
                    external = Some(found);
                    break;
                }
            }
        }
        let external = external.ok_or_else(|| "caller fixture has no foreign read".to_owned())?;
        if join_project_mention(
            &image,
            LinkKind::Reads,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Reads must not join through join_project_mention".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_field_rust_field_retargets_note() -> Result<(), String> {
        let (service_bytes, caller_bytes, note_identity, _) = rust_note_drive_fixture(93)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_field(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(note_identity) {
            return Err(format!(
                "join_project_field should retarget to note, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_field_query_corpus_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, note_identity, drive_identity) =
            rust_note_drive_fixture(94)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_field(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_field returned None".to_owned())?;
        let note_id = query_semantic_id(package, joined);
        let (note_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            note_identity,
            "note",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "drive",
            vec![note_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                note_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "note".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on note should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_field_python_static_semantic_link_row_id_referenced_by_drive()
    -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, note_identity, drive_identity) =
            python_note_static_drive_fixture(147)?;
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/service.py", 1, "note", fixture_version(1)),
                ("weeks.py", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let note_id = RowId::Symbol(semantic_symbol(package, note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, note_identity),
            note_id,
            true,
            &project_paths(&["workout/service.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        let incoming = relations
            .iter()
            .filter(|relation| relation.to == note_id)
            .map(|relation| relation.from)
            .collect::<Vec<_>>();
        if incoming != vec![drive_id] {
            return Err(format!(
                "referencedBy on note static should name only drive, got {incoming:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_field_python_function_semantic_link_row_id_referenced_by_drive()
    -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, set_note_identity, drive_identity) =
            python_set_note_function_drive_fixture(148)?;
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            set_note_id,
            true,
            &project_paths(&["workout/service.py", "weeks.py"]),
        )
        .map_err(|error| error.to_string())?;
        let incoming = relations
            .iter()
            .filter(|relation| relation.to == set_note_id)
            .map(|relation| relation.from)
            .collect::<Vec<_>>();
        if incoming != vec![drive_id] {
            return Err(format!(
                "referencedBy on set_note function should name only drive, got {incoming:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_field_extra_path_stays_unjoined() -> Result<(), String> {
        let service_bytes = project_field_image(
            "src/service_extra.rs",
            1,
            b"note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_field_read_foreign_fixture(95)),
        )?;
        let paths = project_paths(&["src/service_extra.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_field(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("src/service_extra.rs must not satisfy src/service".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_field_ambiguous_rust_service_paths_returns_none() -> Result<(), String> {
        let service_bytes =
            project_field_image("src/service.rs", 1, b"note", TreeEntityId::new(0), None)?;
        let duplicate_service_bytes =
            project_field_image("src/service/mod.rs", 3, b"note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_field_read_foreign_fixture(96)),
        )?;
        let paths = project_paths(&["src/service.rs", "src/service/mod.rs", "src/lib.rs"]);
        let images = [
            &service_bytes[..],
            &duplicate_service_bytes[..],
            &caller_bytes[..],
        ];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_field(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous src/service matches must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_field_calls_or_record_kind_returns_none() -> Result<(), String> {
        let (service_bytes, caller_bytes, note_identity, _) = rust_note_drive_fixture(97)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(2).identity()]);
        let (external, _, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        for link_kind in [LinkKind::Calls, LinkKind::TypeReference] {
            if join_project_field(
                &caller_image,
                link_kind,
                external,
                &caller_path,
                &paths,
                &index,
                &published,
            )
            .map_err(|error| error.to_string())?
            .is_some()
            {
                return Err(format!(
                    "{link_kind:?} must not join through join_project_field"
                ));
            }
        }
        let image = SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
            return Err("foreign field read absent".to_owned());
        };
        if foreign.kind != Some(ItemKind::Field) {
            return Err("fixture foreign kind must be Field".to_owned());
        }
        let record_fixture = ForeignCallFixture {
            package_specifier: b"src/service",
            path_specifier: None,
            display: b"note",
            foreign_key: 97,
            entity_kind: ItemKind::Record,
            link_kind: LinkKind::Reads,
        };
        let record_caller = project_item_image(
            "src/lib.rs",
            4,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(record_fixture),
        )?;
        let record_image =
            SemanticImageView::reopen(&record_caller).map_err(|error| error.to_string())?;
        let (record_external, record_link_kind, record_caller_path) =
            foreign_field_read_from_caller(&record_caller)?;
        if join_project_field(
            &record_image,
            record_link_kind,
            record_external,
            &record_caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Record kind must not join through join_project_field".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_call_does_not_find_fields() -> Result<(), String> {
        let (service_bytes, caller_bytes, note_identity, _) = rust_note_drive_fixture(98)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("fields must not be found by join_project_call".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_value_rust_const_retargets_limit() -> Result<(), String> {
        let (service_bytes, caller_bytes, limit_identity, _) = rust_limit_drive_fixture(99)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([limit_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(limit_identity) {
            return Err(format!(
                "join_project_value should retarget to LIMIT, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_query_corpus_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, limit_identity, drive_identity) =
            rust_limit_drive_fixture(100)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([limit_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_value returned None".to_owned())?;
        let limit_id = query_semantic_id(package, joined);
        let (limit_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            limit_identity,
            "LIMIT",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "drive",
            vec![limit_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                limit_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "LIMIT".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on LIMIT should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ambiguous_rust_service_paths_returns_none() -> Result<(), String> {
        let service_bytes = project_item_image(
            "src/service.rs",
            1,
            b"LIMIT",
            TreeEntityId::new(0),
            ItemKind::Constant,
            None,
        )?;
        let duplicate_service_bytes = project_item_image(
            "src/service/mod.rs",
            3,
            b"LIMIT",
            TreeEntityId::new(0),
            ItemKind::Constant,
            None,
        )?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_value_read_foreign_fixture(101)),
        )?;
        let paths = project_paths(&["src/service.rs", "src/service/mod.rs", "src/lib.rs"]);
        let images = [
            &service_bytes[..],
            &duplicate_service_bytes[..],
            &caller_bytes[..],
        ];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous src/service LIMIT matches must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_value_field_kind_returns_none() -> Result<(), String> {
        let (service_bytes, caller_bytes, note_identity, _) = rust_note_drive_fixture(102)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Field kind must not join through join_project_value".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_value_rust_function_value_retargets_set_note() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) =
            rust_set_note_value_drive_fixture(103)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(set_note_identity) {
            return Err(format!(
                "join_project_value should retarget to set_note, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_query_corpus_function_value_referenced_by_names_drive()
    -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, set_note_identity, drive_identity) =
            rust_set_note_value_drive_fixture(104)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_value returned None".to_owned())?;
        let set_note_id = query_semantic_id(package, joined);
        let (set_note_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            set_note_identity,
            "set_note",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "drive",
            vec![set_note_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                set_note_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "set_note".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on set_note should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ambiguous_rust_function_paths_returns_none() -> Result<(), String> {
        let service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let duplicate_service_bytes = project_call_image(
            "src/service/mod.rs",
            3,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_item_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(rust_function_value_read_foreign_fixture(105)),
        )?;
        let paths = project_paths(&["src/service.rs", "src/service/mod.rs", "src/lib.rs"]);
        let images = [
            &service_bytes[..],
            &duplicate_service_bytes[..],
            &caller_bytes[..],
        ];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous src/service set_note matches must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_value_go_function_value_retargets_set_note() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) =
            go_set_note_value_drive_fixture(109)?;
        let paths = project_paths(&["demo/service.go", "demo/lib.go"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(set_note_identity) {
            return Err(format!(
                "join_project_value should retarget to SetNote, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_query_corpus_go_function_value_referenced_by_names_drive()
    -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, set_note_identity, drive_identity) =
            go_set_note_value_drive_fixture(110)?;
        let paths = project_paths(&["demo/service.go", "demo/lib.go"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_value returned None".to_owned())?;
        let set_note_id = query_semantic_id(package, joined);
        let (set_note_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            set_note_identity,
            "SetNote",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            drive_identity,
            "Drive",
            vec![set_note_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                set_note_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "SetNote".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["Drive".into()] {
            return Err(format!(
                "referencedBy on SetNote should name only Drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ambiguous_go_set_note_paths_returns_none() -> Result<(), String> {
        let service_bytes =
            project_call_image("demo/service.go", 1, b"SetNote", TreeEntityId::new(0), None)?;
        let duplicate_service_bytes =
            project_call_image("demo/other.go", 3, b"SetNote", TreeEntityId::new(0), None)?;
        let caller_bytes = project_item_image(
            "demo/lib.go",
            2,
            b"Drive",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(go_function_value_read_foreign_fixture(111)),
        )?;
        let paths = project_paths(&["demo/service.go", "demo/other.go", "demo/lib.go"]);
        let images = [
            &service_bytes[..],
            &duplicate_service_bytes[..],
            &caller_bytes[..],
        ];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous demo SetNote matches must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ts_method_value_retargets_set_note() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) =
            ts_set_note_value_group_fixture(110)?;
        let paths = project_paths(&["workout.service.ts", "weeks.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(set_note_identity) {
            return Err(format!(
                "join_project_value should retarget to setNote, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ts_method_value_none_kind_returns_none() -> Result<(), String> {
        let service_bytes = project_call_image(
            "workout.service.ts",
            1,
            b"setNote",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_item_image_without_foreign_kind(
            "weeks.ts",
            2,
            b"group",
            TreeEntityId::new(0),
            ItemKind::Function,
            ts_method_value_read_foreign_fixture(111),
        )?;
        let paths = project_paths(&["workout.service.ts", "weeks.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != None {
            return Err(format!(
                "join_project_value with kind None must not retarget, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ts_const_value_retargets_note() -> Result<(), String> {
        let (service_bytes, caller_bytes, note_identity) = ts_demo_const_note_fixture(114)?;
        let paths = project_paths(&["demo.ts", "read.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(note_identity) {
            return Err(format!(
                "join_project_value should retarget to note const, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ts_const_value_none_kind_returns_none() -> Result<(), String> {
        let service_bytes = project_item_image(
            "demo.ts",
            1,
            b"note",
            TreeEntityId::new(0),
            ItemKind::Constant,
            None,
        )?;
        let caller_bytes = project_item_image_without_foreign_kind(
            "read.ts",
            2,
            b"read",
            TreeEntityId::new(0),
            ItemKind::Function,
            ts_const_value_read_foreign_fixture(115),
        )?;
        let paths = project_paths(&["demo.ts", "read.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published =
            BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != None {
            return Err(format!(
                "join_project_value with kind None must not retarget note const, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ts_static_value_retargets_count() -> Result<(), String> {
        let (service_bytes, caller_bytes, count_identity) = ts_demo_static_count_fixture(116)?;
        let paths = project_paths(&["demo.ts", "read.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([count_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(count_identity) {
            return Err(format!(
                "join_project_value should retarget to count static, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ts_function_named_note_does_not_retarget_const() -> Result<(), String> {
        let note_identity = fixture_version(1).identity();
        let service_bytes = project_item_image(
            "demo.ts",
            1,
            b"note",
            TreeEntityId::new(0),
            ItemKind::Constant,
            None,
        )?;
        let caller_bytes = project_item_image(
            "read.ts",
            2,
            b"read",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(ts_function_named_note_read_foreign_fixture(117)),
        )?;
        let paths = project_paths(&["demo.ts", "read.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != None {
            return Err(format!(
                "join_project_value with Function kind must not retarget to const note, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ts_method_value_referenced_by_names_group() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, set_note_identity, group_identity) =
            ts_set_note_value_group_fixture(112)?;
        let paths = project_paths(&["workout.service.ts", "weeks.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, group_identity]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_value returned None".to_owned())?;
        let set_note_id = query_semantic_id(package, joined);
        let (set_note_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            set_note_identity,
            "setNote",
            Box::new([]),
        )?;
        let (group_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &caller_bytes,
            group_identity,
            "group",
            vec![set_note_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                set_note_fact,
                group_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "setNote".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["group".into()] {
            return Err(format!(
                "referencedBy on setNote should name only group, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_ts_method_value_ambiguous_paths_returns_none() -> Result<(), String> {
        let service_bytes = project_call_image(
            "workout.service.ts",
            1,
            b"setNote",
            TreeEntityId::new(0),
            None,
        )?;
        let duplicate_service_bytes = project_call_image(
            "workout.service/index.ts",
            3,
            b"setNote",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_item_image(
            "weeks.ts",
            2,
            b"group",
            TreeEntityId::new(0),
            ItemKind::Function,
            Some(ts_method_value_read_foreign_fixture(113)),
        )?;
        let paths = project_paths(&["workout.service.ts", "workout.service/index.ts", "weeks.ts"]);
        let images = [
            &service_bytes[..],
            &duplicate_service_bytes[..],
            &caller_bytes[..],
        ];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(3).identity(),
            fixture_version(2).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != None {
            return Err(format!(
                "ambiguous workout.service setNote matches must not retarget, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_constant_stays_const_not_function_arm() -> Result<(), String> {
        let (service_bytes, caller_bytes, limit_identity, _) = rust_limit_drive_fixture(106)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([limit_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(limit_identity) {
            return Err(format!(
                "join_project_value should retarget LIMIT as const, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_call_still_joins_function_call_not_value_arm() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) = rust_set_note_drive_fixture(107)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(2).identity()]);
        let (external, link_kind, caller_path) = foreign_call_from_caller(&caller_bytes)?;
        let caller_image =
            SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_call(
            &caller_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(set_note_identity) {
            return Err(format!(
                "join_project_call should retarget to set_note, got {joined:?}"
            ));
        }
        let (value_service_bytes, value_caller_bytes, value_set_note_identity, _) =
            rust_set_note_value_drive_fixture(108)?;
        let value_images = [&value_service_bytes[..], &value_caller_bytes[..]];
        let value_index = ProjectCallableIndex::build_from_bytes(&value_images)
            .map_err(|error| error.to_string())?;
        let (value_external, value_link_kind, value_caller_path) =
            foreign_value_read_from_caller(&value_caller_bytes)?;
        let value_caller_image =
            SemanticImageView::reopen(&value_caller_bytes).map_err(|error| error.to_string())?;
        if join_project_call(
            &value_caller_image,
            value_link_kind,
            value_external,
            &value_caller_path,
            &paths,
            &value_index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("function value read must not join through join_project_call".to_owned());
        }
        if join_project_value(
            &value_caller_image,
            value_link_kind,
            value_external,
            &value_caller_path,
            &paths,
            &value_index,
            &published,
        )
        .map_err(|error| error.to_string())?
            != Some(value_set_note_identity)
        {
            return Err("function value read must join through join_project_value".to_owned());
        }
        Ok(())
    }

    fn java_status_owner_chain() -> [AncestorFixture; 1] {
        [AncestorFixture {
            name: b"demo.Status",
            version_byte: 20,
            entity_id: TreeEntityId::new(0),
            parent_id: None,
            parent_version_byte: None,
            kind: ItemKind::Enum,
        }]
    }

    fn java_namespace_active_variant_image(
        path: &str,
        source_identity_byte: u8,
        active_version_byte: u8,
        drive_version_byte: u8,
        foreign_key: u8,
    ) -> Result<Vec<u8>, String> {
        project_namespace_call_image(
            path,
            source_identity_byte,
            &java_status_owner_chain(),
            b"Active",
            active_version_byte,
            TreeEntityId::new(1),
            b"drive",
            drive_version_byte,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.Status",
                display: b"Active",
                foreign_key,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Variant,
            Some(ItemKind::Variant),
        )
    }

    fn demo_box_owner_chain() -> [AncestorFixture; 2] {
        [
            AncestorFixture {
                name: b"Demo",
                version_byte: 40,
                entity_id: TreeEntityId::new(0),
                parent_id: None,
                parent_version_byte: None,
                kind: ItemKind::Module,
            },
            AncestorFixture {
                name: b"Box",
                version_byte: 41,
                entity_id: TreeEntityId::new(1),
                parent_id: Some(TreeEntityId::new(0)),
                parent_version_byte: Some(40),
                kind: ItemKind::Record,
            },
        ]
    }

    #[test]
    fn join_project_value_namespace_variant_retargets_active() -> Result<(), String> {
        let service_bytes = java_namespace_active_variant_image("Status.java", 110, 111, 112, 110)?;
        let active_identity = fixture_version(111).identity();
        let paths = project_paths(&["Status.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([active_identity, fixture_version(112).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(active_identity) {
            return Err(format!(
                "namespace Variant read should retarget to Active, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_namespace_variant_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = java_namespace_active_variant_image("Status.java", 113, 114, 115, 113)?;
        let active_identity = fixture_version(114).identity();
        let drive_identity = fixture_version(115).identity();
        let paths = project_paths(&["Status.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([active_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_value returned None".to_owned())?;
        let active_id = query_semantic_id(package, joined);
        let (active_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            active_identity,
            "Active",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            drive_identity,
            "drive",
            vec![active_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                active_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "Active".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on Active should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_namespace_variant_ambiguous_returns_none() -> Result<(), String> {
        let first_bytes = java_namespace_active_variant_image("Status.java", 116, 117, 118, 116)?;
        let duplicate_bytes =
            java_namespace_active_variant_image("Status.java", 119, 120, 121, 119)?;
        let paths = project_paths(&["Status.java"]);
        let images = [&first_bytes[..], &duplicate_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(117).identity(),
            fixture_version(120).identity(),
            fixture_version(118).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &first_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&first_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous namespace Active variants must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_value_namespace_field_stays_on_field_join() -> Result<(), String> {
        let service_bytes = java_namespace_note_field_image(
            "WorkoutService.java",
            122,
            123,
            124,
            122,
            b"demo.WorkoutService",
        )?;
        let note_identity = fixture_version(123).identity();
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(124).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("namespace Field read must not join through join_project_value".to_owned());
        }
        let joined = join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(note_identity) {
            return Err(format!(
                "namespace Field read should retarget through join_project_field, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_namespace_kind_none_returns_none() -> Result<(), String> {
        let service_bytes = project_namespace_call_image(
            "Status.java",
            125,
            &java_status_owner_chain(),
            b"Active",
            126,
            TreeEntityId::new(1),
            b"drive",
            127,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.Status",
                display: b"Active",
                foreign_key: 125,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Variant,
            None,
        )?;
        let paths = project_paths(&["Status.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(126).identity(),
            fixture_version(127).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("namespace foreign kind None must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_value_namespace_universe_returns_none() -> Result<(), String> {
        let service_bytes = project_namespace_call_image(
            "Status.java",
            128,
            &java_status_owner_chain(),
            b"Active",
            129,
            TreeEntityId::new(1),
            b"drive",
            130,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.Status",
                display: b"Active",
                foreign_key: 128,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Variant,
            Some(ItemKind::Variant),
        )?;
        let mut builder = IrBuilder::new();
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[131]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, "Caller.java")
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [TreeItemInput {
            name: b"drive",
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        let ecosystem = builder.intern_atom(b"maven").map_err(|e| e.to_string())?;
        let spelling = builder.intern_atom(b"Active").map_err(|e| e.to_string())?;
        let external = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([131; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Universe { ecosystem },
                path: spelling,
                display: spelling,
                kind: Some(ItemKind::Variant),
            }))
            .map_err(|e| e.to_string())?;
        let links = [TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::External(external),
            kind: LinkKind::Reads,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[fixture_version(131)],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut caller_bytes =
            vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut caller_bytes).map_err(|error| error.to_string())?;
        let paths = project_paths(&["Status.java", "Caller.java"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(129).identity(),
            fixture_version(131).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &caller_bytes,
            TreeEntityId::new(0),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("Universe origin must not retarget through join_project_value".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_value_namespace_generic_fallback_retargets() -> Result<(), String> {
        let service_bytes = project_namespace_call_image(
            "Box.java",
            132,
            &demo_box_owner_chain(),
            b"TOKEN",
            133,
            TreeEntityId::new(2),
            b"drive",
            134,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"Demo.Box<T>",
                display: b"TOKEN",
                foreign_key: 132,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Constant,
            Some(ItemKind::Constant),
        )?;
        let token_identity = fixture_version(133).identity();
        let paths = project_paths(&["Box.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([token_identity, fixture_version(134).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(3),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_value(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(token_identity) {
            return Err(format!(
                "generic namespace fallback should retarget to TOKEN, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_value_namespace_unbalanced_generic_stays_none() -> Result<(), String> {
        let service_bytes = project_namespace_call_image(
            "Box.java",
            135,
            &demo_box_owner_chain(),
            b"TOKEN",
            136,
            TreeEntityId::new(2),
            b"drive",
            137,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"Demo.Box<T",
                display: b"TOKEN",
                foreign_key: 135,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Constant,
            Some(ItemKind::Constant),
        )?;
        let paths = project_paths(&["Box.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(136).identity(),
            fixture_version(137).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(3),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        if join_project_value(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("unbalanced generic namespace must stay external".to_owned());
        }
        Ok(())
    }

    fn foreign_namespace_link_from_caller(
        caller_bytes: &[u8],
        caller_entity: TreeEntityId,
        expected_kind: LinkKind,
    ) -> Result<(ExternalId, LinkKind, String), String> {
        let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
        let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
        for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(caller_entity.raw)) {
            if link.kind == expected_kind {
                if let LinkTarget::External(external) = link.target {
                    return Ok((external, link.kind, caller_path));
                }
            }
        }
        Err(format!(
            "caller fixture has no foreign {:?} link",
            expected_kind
        ))
    }

    fn java_namespace_note_field_image(
        path: &str,
        source_identity_byte: u8,
        note_version_byte: u8,
        drive_version_byte: u8,
        foreign_key: u8,
        namespace: &'static [u8],
    ) -> Result<Vec<u8>, String> {
        project_namespace_call_image(
            path,
            source_identity_byte,
            &java_owner_chain(),
            b"note",
            note_version_byte,
            TreeEntityId::new(1),
            b"drive",
            drive_version_byte,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace,
                display: b"note",
                foreign_key,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Field,
            Some(ItemKind::Field),
        )
    }

    #[test]
    fn join_project_field_namespace_java_chain_retargets_note() -> Result<(), String> {
        let service_bytes = java_namespace_note_field_image(
            "WorkoutService.java",
            60,
            61,
            62,
            60,
            b"demo.WorkoutService",
        )?;
        let note_identity = fixture_version(61).identity();
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(62).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(note_identity) {
            return Err(format!(
                "join_project_field should retarget to note, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_field_namespace_java_chain_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = java_namespace_note_field_image(
            "WorkoutService.java",
            63,
            64,
            65,
            63,
            b"demo.WorkoutService",
        )?;
        let note_identity = fixture_version(64).identity();
        let drive_identity = fixture_version(65).identity();
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, drive_identity]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "join_project_field returned None".to_owned())?;
        let note_id = query_semantic_id(package, joined);
        let (note_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            note_identity,
            "note",
            Box::new([]),
        )?;
        let (drive_fact, _) = compiler_query_presentation(
            package,
            "fixture",
            &service_bytes,
            drive_identity,
            "drive",
            vec![note_id.clone()].into_boxed_slice(),
        )?;
        let workspace = super::super::super::genesis().map_err(|error| error.to_string())?;
        let corpus = SemanticQueryCorpus::admit(
            workspace.root(),
            vec![
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                    SemanticQueryPresentation {
                        id: RowId::Package(package).stable_key(),
                        kind: "project".to_owned(),
                        coordinate: "fixture".to_owned(),
                        name: "fixture".to_owned(),
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: None,
                        parent: None,
                        related: Box::new([]),
                    },
                ),
                note_fact,
                drive_fact,
            ],
        )
        .map_err(|error| error.to_string())?;
        let (cancellation, _) = SemanticQueryCancellation::new();
        let request = SemanticQueryRequest::admit_page(
            corpus,
            "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
            BTreeMap::from([("name".to_owned(), "note".into())]),
            0,
            8,
            cancellation,
        )
        .map_err(|error| error.to_string())?;
        let events = futures_executor::block_on(
            execute_semantic_query(request)
                .map_err(|error| error.to_string())?
                .collect::<Vec<_>>(),
        );
        let callers = events
            .iter()
            .filter_map(|event| match event {
                SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
                SemanticQueryEvent::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        if callers != ["drive".into()] {
            return Err(format!(
                "referencedBy on note should name only drive, got {callers:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_field_namespace_owner_chain_retargets_note() -> Result<(), String> {
        let service_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            66,
            &csharp_owner_chain(),
            b"note",
            67,
            TreeEntityId::new(2),
            b"drive",
            68,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"note",
                foreign_key: 66,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Field,
            Some(ItemKind::Field),
        )?;
        let note_identity = fixture_version(67).identity();
        let paths = project_paths(&["WorkoutService.cs"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(68).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(3),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(note_identity) {
            return Err(format!(
                "owner-chain namespace field read should retarget to note, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_field_namespace_ambiguous_returns_none() -> Result<(), String> {
        let first_bytes = java_namespace_note_field_image(
            "WorkoutService.java",
            69,
            70,
            71,
            69,
            b"demo.WorkoutService",
        )?;
        let duplicate_bytes = java_namespace_note_field_image(
            "WorkoutService.java",
            72,
            73,
            74,
            72,
            b"demo.WorkoutService",
        )?;
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&first_bytes[..], &duplicate_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(70).identity(),
            fixture_version(73).identity(),
            fixture_version(71).identity(),
        ]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &first_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&first_bytes).map_err(|error| error.to_string())?;
        if join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("ambiguous namespace field matches must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_field_namespace_wrong_namespace_returns_none() -> Result<(), String> {
        let service_bytes =
            java_namespace_note_field_image("WorkoutService.java", 75, 76, 77, 75, b"demo.Other")?;
        let note_identity = fixture_version(76).identity();
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(77).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        if join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("wrong namespace must not retarget to note".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_field_namespace_generic_fallback_retargets_note() -> Result<(), String> {
        let box_chain = [AncestorFixture {
            name: b"demo.Box",
            version_byte: 78,
            entity_id: TreeEntityId::new(0),
            parent_id: None,
            parent_version_byte: None,
            kind: ItemKind::Record,
        }];
        let service_bytes = project_namespace_call_image(
            "Box.java",
            79,
            &box_chain,
            b"note",
            80,
            TreeEntityId::new(1),
            b"drive",
            81,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.Box<T>",
                display: b"note",
                foreign_key: 79,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Field,
            Some(ItemKind::Field),
        )?;
        let note_identity = fixture_version(80).identity();
        let paths = project_paths(&["Box.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(81).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(note_identity) {
            return Err(format!(
                "generic namespace fallback should retarget to note, got {joined:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_field_namespace_unbalanced_generic_returns_none() -> Result<(), String> {
        let box_chain = [AncestorFixture {
            name: b"demo.Box",
            version_byte: 82,
            entity_id: TreeEntityId::new(0),
            parent_id: None,
            parent_version_byte: None,
            kind: ItemKind::Record,
        }];
        let service_bytes = project_namespace_call_image(
            "Box.java",
            83,
            &box_chain,
            b"note",
            84,
            TreeEntityId::new(1),
            b"drive",
            85,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.Box<T",
                display: b"note",
                foreign_key: 83,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Field,
            Some(ItemKind::Field),
        )?;
        let note_identity = fixture_version(84).identity();
        let paths = project_paths(&["Box.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(85).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        if join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("unbalanced generic namespace must not retarget to note".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_field_namespace_function_kind_returns_none() -> Result<(), String> {
        let service_bytes = project_namespace_call_image(
            "WorkoutService.java",
            86,
            &java_owner_chain(),
            b"setNote",
            87,
            TreeEntityId::new(1),
            b"drive",
            88,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.WorkoutService",
                display: b"setNote",
                foreign_key: 86,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let set_note_identity = fixture_version(87).identity();
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(88).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Reads,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        if join_project_field(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err(
                "Reads of Function kind must not join through join_project_field".to_owned(),
            );
        }
        Ok(())
    }

    #[test]
    fn join_project_field_namespace_method_call_returns_none() -> Result<(), String> {
        let service_bytes = java_namespace_note_field_image(
            "WorkoutService.java",
            89,
            90,
            91,
            89,
            b"demo.WorkoutService",
        )?;
        let note_identity = fixture_version(90).identity();
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(91).identity()]);
        let method_call_bytes = project_namespace_call_image(
            "WorkoutService.java",
            92,
            &java_owner_chain(),
            b"note",
            93,
            TreeEntityId::new(1),
            b"drive",
            94,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.WorkoutService",
                display: b"note",
                foreign_key: 92,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Field,
            Some(ItemKind::Field),
        )?;
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &method_call_bytes,
            TreeEntityId::new(2),
            LinkKind::MethodCall,
        )?;
        let method_image =
            SemanticImageView::reopen(&method_call_bytes).map_err(|error| error.to_string())?;
        if join_project_field(
            &method_image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?
        .is_some()
        {
            return Err("MethodCall must not join through join_project_field".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_call_namespace_java_still_retargets_function_not_field() -> Result<(), String> {
        let service_bytes = project_namespace_call_image(
            "WorkoutService.java",
            95,
            &java_owner_chain(),
            b"setNote",
            96,
            TreeEntityId::new(1),
            b"sync",
            97,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.WorkoutService",
                display: b"setNote",
                foreign_key: 95,
                link_kind: LinkKind::Calls,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let set_note_identity = fixture_version(96).identity();
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&service_bytes[..]];
        let index =
            ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(97).identity()]);
        let (external, link_kind, caller_path) = foreign_namespace_link_from_caller(
            &service_bytes,
            TreeEntityId::new(2),
            LinkKind::Calls,
        )?;
        let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
        let joined = join_project_call(
            &image,
            link_kind,
            external,
            &caller_path,
            &paths,
            &index,
            &published,
        )
        .map_err(|error| error.to_string())?;
        if joined != Some(set_note_identity) {
            return Err(format!(
                "join_project_call should still retarget to setNote function, got {joined:?}"
            ));
        }
        let note_field_bytes = java_namespace_note_field_image(
            "WorkoutService.java",
            98,
            99,
            100,
            98,
            b"demo.WorkoutService",
        )?;
        let note_identity = fixture_version(99).identity();
        let field_images = [&note_field_bytes[..]];
        let field_index = ProjectCallableIndex::build_from_bytes(&field_images)
            .map_err(|error| error.to_string())?;
        let field_published = BTreeSet::from([note_identity, fixture_version(100).identity()]);
        let (field_external, field_link_kind, field_caller_path) =
            foreign_namespace_link_from_caller(
                &note_field_bytes,
                TreeEntityId::new(2),
                LinkKind::Reads,
            )?;
        let field_image =
            SemanticImageView::reopen(&note_field_bytes).map_err(|error| error.to_string())?;
        let field_joined = join_project_field(
            &field_image,
            field_link_kind,
            field_external,
            &field_caller_path,
            &paths,
            &field_index,
            &field_published,
        )
        .map_err(|error| error.to_string())?;
        if field_joined != Some(note_identity) {
            return Err(format!(
                "join_project_field should retarget to note field, got {field_joined:?}"
            ));
        }
        if field_joined == joined {
            return Err("field join must not return the function identity".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_rust_function_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_function_foreign_call_fixture(79)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, drive_identity),
            drive_id,
            false,
            &project_paths(&["src/service.rs", "src/lib.rs"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, drive_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected drive to call set_note semantic row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_rust_function_mod_rs_retarget_links_callee_semantic_row() -> Result<(), String>
    {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/service/mod.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_function_foreign_call_fixture(80)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service/mod.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, drive_identity),
            drive_id,
            false,
            &project_paths(&["src/service/mod.rs", "src/lib.rs"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, drive_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected drive to call src/service/mod.rs set_note row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_rust_function_extra_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/service_extra.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_function_foreign_call_fixture(81)),
        )?;
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service_extra.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, drive_identity),
            drive_id,
            false,
            &project_paths(&["src/service_extra.rs", "src/lib.rs"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, drive_id).contains(&set_note_id) {
            return Err("src/service_extra.rs must not satisfy src/service".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_rust_function_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_function_foreign_call_fixture(82)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["src/service.rs", "src/lib.rs"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, drive_identity) {
            return Err("rust function retarget site is not drive".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::Calls {
            return Err(format!(
                "rust function retarget relation is {:?}",
                fact.relation
            ));
        }
        let expected_target = semantic_declaration_identity(set_note_identity);
        if !matches!(
            &fact.target,
            backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == expected_target
        ) {
            return Err("rust function retarget target is not set_note".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_rust_module_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(70)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, drive_identity),
            drive_id,
            false,
            &project_paths(&["src/service.rs", "src/lib.rs"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, drive_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected drive to call set_note semantic row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_rust_module_app_src_suffix_retarget_links_callee_semantic_row()
    -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "app/src/service.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(71)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("app/src/service.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, drive_identity),
            drive_id,
            false,
            &project_paths(&["app/src/service.rs", "src/lib.rs"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, drive_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected drive to call app/src/service.rs set_note row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_rust_module_mod_rs_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/service/mod.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(72)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service/mod.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, drive_identity),
            drive_id,
            false,
            &project_paths(&["src/service/mod.rs", "src/lib.rs"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, drive_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected drive to call src/service/mod.rs set_note row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_rust_module_extra_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/service_extra.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(73)),
        )?;
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service_extra.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, drive_identity),
            drive_id,
            false,
            &project_paths(&["src/service_extra.rs", "src/lib.rs"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, drive_id).contains(&set_note_id) {
            return Err("src/service_extra.rs must not satisfy src/service".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_rust_module_ambiguous_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let root_service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let app_service_bytes = project_call_image(
            "app/src/service.rs",
            3,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(74)),
        )?;
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let drive_id = RowId::Symbol(semantic_symbol(package, drive_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&root_service_bytes, &app_service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, drive_identity),
            drive_id,
            false,
            &project_paths(&["src/service.rs", "app/src/service.rs", "src/lib.rs"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, drive_id).contains(&set_note_id) {
            return Err("ambiguous src/service path must not retarget".to_owned());
        }
        Ok(())
    }

    fn c_header_foreign_call_fixture(foreign_key: u8) -> ForeignCallFixture {
        ForeignCallFixture {
            package_specifier: b"include/decl.h",
            path_specifier: Some(b"declared_in_header"),
            display: b"declared_in_header",
            foreign_key,
            entity_kind: ItemKind::Function,
            link_kind: LinkKind::Calls,
        }
    }

    #[test]
    fn project_call_c_header_retarget_links_callee_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let header_bytes = project_call_image(
            "include/decl.h",
            1,
            b"declared_in_header",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/main.c",
            2,
            b"use",
            TreeEntityId::new(0),
            Some(c_header_foreign_call_fixture(90)),
        )?;
        let callee_identity = fixture_version(1).identity();
        let caller_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                (
                    "include/decl.h",
                    1,
                    "declared_in_header",
                    fixture_version(1),
                ),
                ("src/main.c", 2, "use", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let caller_id = RowId::Symbol(semantic_symbol(package, caller_identity));
        let callee_id = RowId::Symbol(semantic_symbol(package, callee_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&header_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, caller_identity),
            caller_id,
            false,
            &project_paths(&["include/decl.h", "src/main.c"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, caller_id);
        if targets != vec![callee_id] {
            return Err(format!(
                "expected use to call declared_in_header semantic row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_c_header_src_include_suffix_retarget_links_callee_semantic_row()
    -> Result<(), String> {
        let package = package_key("fixture");
        let header_bytes = project_call_image(
            "src/include/decl.h",
            1,
            b"declared_in_header",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/main.c",
            2,
            b"use",
            TreeEntityId::new(0),
            Some(c_header_foreign_call_fixture(91)),
        )?;
        let callee_identity = fixture_version(1).identity();
        let caller_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                (
                    "src/include/decl.h",
                    1,
                    "declared_in_header",
                    fixture_version(1),
                ),
                ("src/main.c", 2, "use", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let caller_id = RowId::Symbol(semantic_symbol(package, caller_identity));
        let callee_id = RowId::Symbol(semantic_symbol(package, callee_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&header_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, caller_identity),
            caller_id,
            false,
            &project_paths(&["src/include/decl.h", "src/main.c"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, caller_id);
        if targets != vec![callee_id] {
            return Err(format!(
                "expected use to call src/include/decl.h declared_in_header row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_c_header_extra_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let header_bytes = project_call_image(
            "include/decl_extra.h",
            1,
            b"declared_in_header",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/main.c",
            2,
            b"use",
            TreeEntityId::new(0),
            Some(c_header_foreign_call_fixture(92)),
        )?;
        let caller_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                (
                    "include/decl_extra.h",
                    1,
                    "declared_in_header",
                    fixture_version(1),
                ),
                ("src/main.c", 2, "use", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let caller_id = RowId::Symbol(semantic_symbol(package, caller_identity));
        let callee_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&header_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, caller_identity),
            caller_id,
            false,
            &project_paths(&["include/decl_extra.h", "src/main.c"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, caller_id).contains(&callee_id) {
            return Err("include/decl_extra.h must not satisfy include/decl.h".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_c_header_ambiguous_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let root_header_bytes = project_call_image(
            "include/decl.h",
            1,
            b"declared_in_header",
            TreeEntityId::new(0),
            None,
        )?;
        let app_header_bytes = project_call_image(
            "app/include/decl.h",
            3,
            b"declared_in_header",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/main.c",
            2,
            b"use",
            TreeEntityId::new(0),
            Some(c_header_foreign_call_fixture(93)),
        )?;
        let caller_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                (
                    "include/decl.h",
                    1,
                    "declared_in_header",
                    fixture_version(1),
                ),
                ("src/main.c", 2, "use", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let caller_id = RowId::Symbol(semantic_symbol(package, caller_identity));
        let callee_id = RowId::Symbol(semantic_symbol(package, fixture_version(1).identity()));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&root_header_bytes, &app_header_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, caller_identity),
            caller_id,
            false,
            &project_paths(&["include/decl.h", "app/include/decl.h", "src/main.c"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, caller_id).contains(&callee_id) {
            return Err("ambiguous include/decl.h path must not retarget".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_c_header_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let header_bytes = project_call_image(
            "include/decl.h",
            1,
            b"declared_in_header",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/main.c",
            2,
            b"use",
            TreeEntityId::new(0),
            Some(c_header_foreign_call_fixture(94)),
        )?;
        let callee_identity = fixture_version(1).identity();
        let caller_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                (
                    "include/decl.h",
                    1,
                    "declared_in_header",
                    fixture_version(1),
                ),
                ("src/main.c", 2, "use", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&header_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, callee_identity),
            &project_paths(&["include/decl.h", "src/main.c"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, caller_identity) {
            return Err("c header retarget site is not use".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::Calls {
            return Err(format!("c header retarget relation is {:?}", fact.relation));
        }
        let expected_target = semantic_declaration_identity(callee_identity);
        if !matches!(
            &fact.target,
            backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == expected_target
        ) {
            return Err("c header retarget target is not declared_in_header".to_owned());
        }
        Ok(())
    }

    #[test]
    fn typed_structural_reference_endpoints_keep_shadowed_worker_sites_distinct()
    -> Result<(), String> {
        let path = "workers.ts";
        let text = "const left = () => { function worker() { leftTarget(); } function leftTarget() {} }, right = () => { function worker() { rightTarget(); } function rightTarget() {} };";
        let sources = indexed_sources(&[(path, analyze_source(path, text)?)])?;
        let package = package_key("fixture");
        let (view, rows) = super::view_build::structural_view_for_test(&sources)?;
        let mut residence = super::view_build::StructuralCallResidence::default();
        let pairs = residence
            .endpoint_pairs(&sources, package)
            .map_err(|error| error.to_string())?;
        let mut sites = BTreeSet::new();
        for (owner_name, target_name) in [("left", "leftTarget"), ("right", "rightTarget")] {
            let owner = rows
                .iter()
                .find(|row| row.label == format!("fixture::{path}:1::{owner_name}"))
                .ok_or("owner")?
                .id;
            let caller = rows
                .iter()
                .find(|row| {
                    row.label == "fixture::workers.ts:1::worker"
                        && row.parent.map(RowId::Symbol) == Some(owner)
                })
                .ok_or("exact caller")?
                .id;
            let RowId::Symbol(target) = rows
                .iter()
                .find(|row| row.label == format!("fixture::{path}:1::{target_name}"))
                .ok_or("target")?
                .id
            else {
                return Err("target symbol".to_owned());
            };
            let facts = super::project_opened_reference_facts(
                &[],
                &view,
                package,
                target,
                &project_paths(&[path]),
                &pairs,
                None,
                &BTreeMap::new(),
                &[],
                true,
            )
            .map_err(|error| error.to_string())?;
            let RowId::Symbol(caller) = caller else {
                return Err("caller symbol".to_owned());
            };
            assert_eq!(facts.len(), 1);
            assert_eq!(facts[0].site, caller);
            assert!(sites.insert(caller));
        }
        assert_eq!(
            sites.len(),
            2,
            "the production opened-image reference path preserves both exact call sites"
        );
        Ok(())
    }

    fn analyze_source(
        path: &str,
        source: &str,
    ) -> Result<Arc<[backend_compile::SourceDeclaration]>, String> {
        Ok(backend_frontend_typescript::syntax_frontend()
            .map_err(|error| error.to_string())?
            .analyze(std::path::Path::new(path), source.as_bytes())
            .map_err(|error| error.to_string())?
            .declarations()
            .clone())
    }

    fn indexed_sources(
        files: &[(&str, Arc<[backend_compile::SourceDeclaration]>)],
    ) -> Result<IndexedSources, String> {
        let package = package_key("fixture");
        let project_key = package.to_bytes();
        let mut file_records = Vec::new();
        for (path, declarations) in files {
            let file_key = product_source_file_key(project_key, *path);
            let record = ProductSourceRecord::file(
                project_key,
                *path,
                backend_engine::SourceLanguage::TypeScript,
                [1; 32],
                [2; 32],
                declarations.clone(),
            )?;
            file_records.push((file_key, record));
        }
        file_records.sort_by_key(|(file_key, _)| *file_key);
        Ok(IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from(file_records.iter().map(|(key, _)| *key).collect::<Vec<_>>()),
                },
            )]),
            files: file_records,
            cargo_aliases: BTreeMap::new(),
            source_snapshot: None,
        })
    }

    #[test]
    fn project_call_structural_union_lands_on_semantic_row() -> Result<(), String> {
        let package = package_key("fixture");
        let workout = analyze_source(
            "workout.service.ts",
            "export class WorkoutService { setNote() {} }\n",
        )?;
        let other = analyze_source(
            "other.service.ts",
            "export class OtherService { setNote() {} }\n",
        )?;
        let weeks = analyze_source(
            "weeks.ts",
            "import { WorkoutService } from \"./workout.service\";\nexport class Weeks { service!: WorkoutService; sync() { this.service.setNote(); } }\n",
        )?;
        let sources = indexed_sources(&[
            ("workout.service.ts", workout),
            ("other.service.ts", other),
            ("weeks.ts", weeks),
        ])?;
        let set_note_identity = fixture_version(4).identity();
        let sync_identity = fixture_version(5).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout.service.ts", 1, "setNote", fixture_version(4)),
                ("other.service.ts", 1, "setNote", fixture_version(6)),
                ("weeks.ts", 2, "sync", fixture_version(5)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let other_set_note_id =
            RowId::Symbol(semantic_symbol(package, fixture_version(6).identity()));
        let pairs =
            structural_call_coordinate_pairs(&sources, package).map_err(|e| e.to_string())?;
        let relations =
            structural_call_graph_relations_mapped(&view, &pairs, package, sync_id, false);
        if relations.iter().any(|relation| relation.confidence != Some(backend_engine::SemanticConfidence::Syntactic)) {
            return Err("structural graph relations must retain syntactic confidence".to_owned());
        }
        let targets = relations
            .iter()
            .map(|relation| relation.to)
            .collect::<Vec<_>>();
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected structural union onto workout setNote semantic row, got {targets:?}"
            ));
        }
        if targets.contains(&other_set_note_id) {
            return Err("unimported OtherService.setNote must not appear".to_owned());
        }
        Ok(())
    }

    fn local_call_image_with_span() -> Result<(Vec<u8>, EntityVersion, EntityVersion), String> {
        const CALL_PATH: &str = "local.ts";
        let caller_version = fixture_version(11);
        let callee_version = fixture_version(12);
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[11]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, CALL_PATH)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"caller",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"callee",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let path_atom = (0..64)
            .map(backend_semantic::ir::AtomId::new)
            .find(|id| {
                ir.atom(*id)
                    .is_some_and(|bytes| bytes == CALL_PATH.as_bytes())
            })
            .ok_or("the fixture source path is absent from its own atom table")?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, CALL_PATH)
            .map_err(|error| error.to_string())?;
        let links = [TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(path_atom, 12, 24),
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[caller_version, callee_version],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok((bytes, caller_version, callee_version))
    }

    #[test]
    fn project_references_foreign_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"./apply-set",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 9,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Calls,
            }),
        )?;
        let entries_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, entries_identity),
            &project_paths(&["apply-set.ts", "weeks.ts"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, sync_identity) {
            return Err("foreign retarget site is not syncWorkout".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::Calls {
            return Err(format!("foreign retarget relation is {:?}", fact.relation));
        }
        let expected_target = semantic_declaration_identity(entries_identity);
        if !matches!(
            &fact.target,
            backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == expected_target
        ) {
            return Err("foreign retarget target is not entriesFromItems".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_foreign_ambiguous_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let duplicate_apply_bytes = project_call_image(
            "apply-set.ts",
            3,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"./apply-set",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 9,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Calls,
            }),
        )?;
        let sync_identity = fixture_version(2).identity();
        let entries_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&apply_bytes, &duplicate_apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, entries_identity),
            &project_paths(&["apply-set.ts", "weeks.ts"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        let sync_symbol = semantic_symbol(package, sync_identity);
        let sync_site_facts = facts.iter().filter(|fact| fact.site == sync_symbol).count();
        if sync_site_facts != 0 {
            return Err(format!(
                "ambiguous foreign target produced {sync_site_facts} sync-site facts"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_foreign_lodash_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"lodash",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 10,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Calls,
            }),
        )?;
        let sync_identity = fixture_version(2).identity();
        let entries_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, entries_identity),
            &project_paths(&["apply-set.ts", "weeks.ts"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        let sync_symbol = semantic_symbol(package, sync_identity);
        let sync_site_facts = facts.iter().filter(|fact| fact.site == sync_symbol).count();
        if sync_site_facts != 0 {
            return Err(format!(
                "lodash import produced {sync_site_facts} sync-site facts"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_dotted_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "workout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(40)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["workout/service.py", "weeks.py"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, sync_identity) {
            return Err("dotted foreign retarget site is not sync_workout".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::Calls {
            return Err(format!(
                "dotted foreign retarget relation is {:?}",
                fact.relation
            ));
        }
        let expected_target = semantic_declaration_identity(set_note_identity);
        if !matches!(
            &fact.target,
            backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == expected_target
        ) {
            return Err("dotted foreign retarget target is not set_note".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_dotted_src_suffix_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/workout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(41)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/workout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["src/workout/service.py", "weeks.py"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        if facts[0].site != semantic_symbol(package, sync_identity) {
            return Err("dotted src suffix retarget site is not sync_workout".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_dotted_init_package_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "workout/service/__init__.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(42)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                (
                    "workout/service/__init__.py",
                    1,
                    "set_note",
                    fixture_version(1),
                ),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["workout/service/__init__.py", "weeks.py"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        if facts[0].site != semantic_symbol(package, sync_identity) {
            return Err("dotted init package retarget site is not sync_workout".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_dotted_package_head_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let workout_bytes =
            project_call_image("workout.py", 1, b"set_note", TreeEntityId::new(0), None)?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(43)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let set_note_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&workout_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["workout.py", "weeks.py"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        let sync_symbol = semantic_symbol(package, sync_identity);
        let sync_site_facts = facts.iter().filter(|fact| fact.site == sync_symbol).count();
        if sync_site_facts != 0 {
            return Err(format!(
                "dotted import must not fall back to workout.py, got {sync_site_facts} sync-site facts"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_dotted_ambiguous_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let root_service_bytes = project_call_image(
            "workout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let src_service_bytes = project_call_image(
            "src/workout/service.py",
            3,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(44)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let set_note_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("workout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&root_service_bytes, &src_service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["workout/service.py", "src/workout/service.py", "weeks.py"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        let sync_symbol = semantic_symbol(package, sync_identity);
        let sync_site_facts = facts.iter().filter(|fact| fact.site == sync_symbol).count();
        if sync_site_facts != 0 {
            return Err(format!(
                "ambiguous dotted module target produced {sync_site_facts} sync-site facts"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_dotted_wrong_prefix_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "notworkout/service.py",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture(45)),
        )?;
        let sync_identity = fixture_version(2).identity();
        let set_note_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("notworkout/service.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["notworkout/service.py", "weeks.py"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        let sync_symbol = semantic_symbol(package, sync_identity);
        let sync_site_facts = facts.iter().filter(|fact| fact.site == sync_symbol).count();
        if sync_site_facts != 0 {
            return Err(format!(
                "notworkout/service.py produced {sync_site_facts} sync-site facts"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_dotted_invalid_path_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let other_bytes =
            project_call_image("other.py", 1, b"set_note", TreeEntityId::new(0), None)?;
        let weeks_bytes = project_call_image(
            "weeks.py",
            2,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture_with_path(
                b"workout..service",
                46,
            )),
        )?;
        let weeks_dot_bytes = project_call_image(
            "weeks.py",
            4,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture_with_path(
                b".workout.service",
                47,
            )),
        )?;
        let sync_identity = fixture_version(2).identity();
        let set_note_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("other.py", 1, "set_note", fixture_version(1)),
                ("weeks.py", 2, "sync_workout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        for weeks_image in [&weeks_bytes, &weeks_dot_bytes] {
            let facts = project_reference_facts_from_bytes(
                &[&other_bytes, weeks_image],
                &view,
                package,
                semantic_symbol(package, set_note_identity),
                &project_paths(&["other.py", "weeks.py"]),
                &[],
            )
            .map_err(|error| error.to_string())?;
            let sync_symbol = semantic_symbol(package, sync_identity);
            let sync_site_facts = facts.iter().filter(|fact| fact.site == sync_symbol).count();
            if sync_site_facts != 0 {
                return Err(format!(
                    "invalid dotted path produced {sync_site_facts} sync-site facts"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn project_references_go_import_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/workout/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.go",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(go_import_foreign_call_fixture(67)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/workout/service.go", 1, "SetNote", fixture_version(1)),
                ("weeks.go", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["src/workout/service.go", "weeks.go"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, sync_identity) {
            return Err("go import retarget site is not syncWorkout".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::Calls {
            return Err(format!(
                "go import retarget relation is {:?}",
                fact.relation
            ));
        }
        let expected_target = semantic_declaration_identity(set_note_identity);
        if !matches!(
            &fact.target,
            backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == expected_target
        ) {
            return Err("go import retarget target is not SetNote".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_rust_module_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(75)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["src/service.rs", "src/lib.rs"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, drive_identity) {
            return Err("rust module retarget site is not drive".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::MethodCall {
            return Err(format!(
                "rust module retarget relation is {:?}",
                fact.relation
            ));
        }
        let expected_target = semantic_declaration_identity(set_note_identity);
        if !matches!(
            &fact.target,
            backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == expected_target
        ) {
            return Err("rust module retarget target is not set_note".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_rust_module_mod_rs_retarget_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/service/mod.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(76)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let drive_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service/mod.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["src/service/mod.rs", "src/lib.rs"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        if facts[0].site != semantic_symbol(package, drive_identity) {
            return Err("mod.rs retarget site is not drive".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_rust_module_extra_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_call_image(
            "src/service_extra.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(77)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service_extra.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["src/service_extra.rs", "src/lib.rs"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if !facts.is_empty() {
            return Err(format!(
                "src/service_extra.rs must stay external, got {facts:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_rust_module_ambiguous_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let root_service_bytes =
            project_call_image("src/service.rs", 1, b"set_note", TreeEntityId::new(0), None)?;
        let app_service_bytes = project_call_image(
            "app/src/service.rs",
            3,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
        let caller_bytes = project_call_image(
            "src/lib.rs",
            2,
            b"drive",
            TreeEntityId::new(0),
            Some(rust_module_foreign_call_fixture(78)),
        )?;
        let set_note_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("src/service.rs", 1, "set_note", fixture_version(1)),
                ("src/lib.rs", 2, "drive", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&root_service_bytes, &app_service_bytes, &caller_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["src/service.rs", "app/src/service.rs", "src/lib.rs"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if !facts.is_empty() {
            return Err(format!(
                "ambiguous src/service path must stay external, got {facts:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_reads_are_not_callers() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"./apply-set",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 9,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Reads,
            }),
        )?;
        let sync_identity = fixture_version(2).identity();
        let entries_identity = fixture_version(1).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, entries_identity),
            &project_paths(&["apply-set.ts", "weeks.ts"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        let sync_symbol = semantic_symbol(package, sync_identity);
        let sync_site_facts: Vec<_> = facts
            .iter()
            .filter(|fact| fact.site == sync_symbol)
            .collect();
        if sync_site_facts.len() != 1 {
            return Err(format!(
                "resolved function read produced {} sync-site facts",
                sync_site_facts.len()
            ));
        }
        if sync_site_facts[0].relation != backend_engine::SemanticLinkKind::Reads {
            return Err(format!(
                "function read was recorded as {:?}",
                sync_site_facts[0].relation
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_structural_union_names_the_importer() -> Result<(), String> {
        let package = package_key("fixture");
        let workout = analyze_source(
            "workout.service.ts",
            "export class WorkoutService { setNote() {} }\n",
        )?;
        let other = analyze_source(
            "other.service.ts",
            "export class OtherService { setNote() {} }\n",
        )?;
        let weeks = analyze_source(
            "weeks.ts",
            "import { WorkoutService } from \"./workout.service\";\nexport class Weeks { service!: WorkoutService; sync() { this.service.setNote(); } }\n",
        )?;
        let sources = indexed_sources(&[
            ("workout.service.ts", workout),
            ("other.service.ts", other),
            ("weeks.ts", weeks),
        ])?;
        let set_note_identity = fixture_version(4).identity();
        let sync_identity = fixture_version(5).identity();
        let other_set_note_symbol = semantic_symbol(package, fixture_version(6).identity());
        let rows = semantic_view_rows(
            package,
            &[
                ("workout.service.ts", 1, "setNote", fixture_version(4)),
                ("other.service.ts", 1, "setNote", fixture_version(6)),
                ("weeks.ts", 2, "sync", fixture_version(5)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let pairs =
            structural_call_coordinate_pairs(&sources, package).map_err(|e| e.to_string())?;
        let facts = project_reference_facts_from_bytes(
            &[],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["workout.service.ts", "other.service.ts", "weeks.ts"]),
            &pairs,
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!(
                "expected one structural reference fact, got {}",
                facts.len()
            ));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, sync_identity) {
            return Err("structural union site is not sync".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::Calls {
            return Err(format!("structural union relation is {:?}", fact.relation));
        }
        if fact.evidence.confidence != backend_engine::SemanticConfidence::Syntactic {
            return Err(format!(
                "structural union confidence is {:?}",
                fact.evidence.confidence
            ));
        }
        if fact.site == other_set_note_symbol {
            return Err("unimported OtherService.setNote must not be the site".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_structural_does_not_duplicate_semantic_site() -> Result<(), String> {
        let package = package_key("fixture");
        let (bytes, caller_version, callee_version) = local_call_image_with_span()?;
        let rows = semantic_view_rows(
            package,
            &[
                ("local.ts", 1, "caller", caller_version),
                ("local.ts", 2, "callee", callee_version),
            ],
        )?;
        let view = semantic_view(rows)?;
        let pairs = vec![(
            "fixture::local.ts:1::caller".to_owned(),
            "fixture::local.ts:2::callee".to_owned(),
        )];
        let facts = project_reference_facts_from_bytes(
            &[&bytes],
            &view,
            package,
            semantic_symbol(package, callee_version.identity()),
            &project_paths(&["local.ts"]),
            &pairs,
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!(
                "semantic and structural union must emit one fact, got {}",
                facts.len()
            ));
        }
        let fact = &facts[0];
        if fact.evidence.confidence != backend_engine::SemanticConfidence::Compiler {
            return Err(format!(
                "deduplicated reference confidence is {:?}",
                fact.evidence.confidence
            ));
        }
        let span = fact
            .evidence
            .source
            .as_ref()
            .ok_or("semantic occurrence lost its captured span")?;
        if (span.start, span.end) != (12, 24) {
            return Err(format!("span is {}..{}", span.start, span.end));
        }
        Ok(())
    }

    fn csharp_owner_chain() -> [AncestorFixture; 2] {
        [
            AncestorFixture {
                name: b"Demo",
                version_byte: 10,
                entity_id: TreeEntityId::new(0),
                parent_id: None,
                parent_version_byte: None,
                kind: ItemKind::Module,
            },
            AncestorFixture {
                name: b"WorkoutService",
                version_byte: 11,
                entity_id: TreeEntityId::new(1),
                parent_id: Some(TreeEntityId::new(0)),
                parent_version_byte: Some(10),
                kind: ItemKind::Record,
            },
        ]
    }

    fn java_owner_chain() -> [AncestorFixture; 1] {
        [AncestorFixture {
            name: b"demo.WorkoutService",
            version_byte: 20,
            entity_id: TreeEntityId::new(0),
            parent_id: None,
            parent_version_byte: None,
            kind: ItemKind::Record,
        }]
    }

    #[test]
    fn project_call_namespace_csharp_chain_retarget_links_callee_semantic_row() -> Result<(), String>
    {
        let package = package_key("fixture");
        let service_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            1,
            &csharp_owner_chain(),
            b"SetNote",
            12,
            TreeEntityId::new(2),
            b"Sync",
            13,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"SetNote",
                foreign_key: 21,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let set_note_identity = fixture_version(12).identity();
        let sync_identity = fixture_version(13).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.cs", 1, "SetNote", fixture_version(12)),
                ("Weeks.cs", 2, "Sync", fixture_version(13)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["WorkoutService.cs", "Weeks.cs"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected Sync to call SetNote semantic row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_namespace_java_chain_retarget_links_callee_semantic_row() -> Result<(), String>
    {
        let package = package_key("fixture");
        let service_bytes = project_namespace_call_image(
            "WorkoutService.java",
            1,
            &java_owner_chain(),
            b"setNote",
            22,
            TreeEntityId::new(1),
            b"sync",
            23,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.WorkoutService",
                display: b"setNote",
                foreign_key: 22,
                link_kind: LinkKind::Calls,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let set_note_identity = fixture_version(22).identity();
        let sync_identity = fixture_version(23).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.java", 1, "setNote", fixture_version(22)),
                ("Weeks.java", 2, "sync", fixture_version(23)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["WorkoutService.java", "Weeks.java"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected sync to call setNote semantic row, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_namespace_retarget_incoming_from_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            1,
            &csharp_owner_chain(),
            b"SetNote",
            12,
            TreeEntityId::new(2),
            b"Sync",
            13,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"SetNote",
                foreign_key: 21,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let set_note_identity = fixture_version(12).identity();
        let sync_identity = fixture_version(13).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.cs", 1, "SetNote", fixture_version(12)),
                ("Weeks.cs", 2, "Sync", fixture_version(13)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            set_note_id,
            true,
            &project_paths(&["WorkoutService.cs", "Weeks.cs"]),
        )
        .map_err(|error| error.to_string())?;
        if !relations
            .iter()
            .any(|relation| relation.from == sync_id && relation.to == set_note_id)
        {
            return Err(format!(
                "expected incoming edge from Sync, got {relations:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_namespace_ambiguous_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let first_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            1,
            &csharp_owner_chain(),
            b"SetNote",
            12,
            TreeEntityId::new(2),
            b"Sync",
            13,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"SetNote",
                foreign_key: 21,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let duplicate_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            4,
            &csharp_owner_chain(),
            b"SetNote",
            14,
            TreeEntityId::new(2),
            b"Sync",
            15,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"SetNote",
                foreign_key: 24,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let sync_identity = fixture_version(13).identity();
        let set_note_identity = fixture_version(12).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.cs", 1, "SetNote", fixture_version(12)),
                ("Weeks.cs", 2, "Sync", fixture_version(13)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&first_bytes, &duplicate_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["WorkoutService.cs", "Weeks.cs"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("ambiguous namespace target must not retarget to SetNote".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_namespace_wrong_namespace_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            1,
            &csharp_owner_chain(),
            b"SetNote",
            12,
            TreeEntityId::new(2),
            b"Sync",
            13,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Other.WorkoutService",
                display: b"SetNote",
                foreign_key: 21,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let sync_identity = fixture_version(13).identity();
        let set_note_identity = fixture_version(12).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.cs", 1, "SetNote", fixture_version(12)),
                ("Weeks.cs", 2, "Sync", fixture_version(13)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["WorkoutService.cs", "Weeks.cs"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("wrong namespace must not retarget to SetNote".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_namespace_reads_stay_external() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            1,
            &csharp_owner_chain(),
            b"SetNote",
            12,
            TreeEntityId::new(2),
            b"Sync",
            13,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"SetNote",
                foreign_key: 21,
                link_kind: LinkKind::Reads,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let sync_identity = fixture_version(13).identity();
        let set_note_identity = fixture_version(12).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.cs", 1, "SetNote", fixture_version(12)),
                ("Weeks.cs", 2, "Sync", fixture_version(13)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["WorkoutService.cs", "Weeks.cs"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("Reads namespace link must not retarget to SetNote".to_owned());
        }
        Ok(())
    }

    fn callee_only_image(path: &str, identity_byte: u8) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[identity_byte]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let versions = [
            fixture_version(10),
            fixture_version(11),
            fixture_version(12),
        ];
        let items = [
            TreeItemInput {
                name: b"Demo",
                anonymous_callable_anchor: None,
                kind: ItemKind::Module,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"WorkoutService",
                anonymous_callable_anchor: None,
                kind: ItemKind::Record,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Bound(versions[0].identity())),
                parent: Some(TreeEntityId::new(0)),
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"SetNote",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Bound(versions[1].identity())),
                parent: Some(TreeEntityId::new(1)),
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
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    #[test]
    fn project_call_namespace_universe_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let callee_bytes = callee_only_image("WorkoutService.cs", 1)?;
        let mut builder = IrBuilder::new();
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[30]),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, "Weeks.cs")
            .map_err(|error| error.to_string())?;
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [TreeItemInput {
            name: b"Sync",
            anonymous_callable_anchor: None,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        let ecosystem = builder.intern_atom(b"nuget").map_err(|e| e.to_string())?;
        let spelling = builder
            .intern_atom(b"this.service.SetNote")
            .map_err(|e| e.to_string())?;
        let external = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([30; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Universe { ecosystem },
                path: spelling,
                display: spelling,
                kind: Some(ItemKind::Function),
            }))
            .map_err(|e| e.to_string())?;
        let links = [TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::External(external),
            kind: LinkKind::MethodCall,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[fixture_version(13)],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut universe_bytes =
            vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut universe_bytes).map_err(|error| error.to_string())?;
        let sync_identity = fixture_version(13).identity();
        let set_note_identity = fixture_version(12).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.cs", 1, "SetNote", fixture_version(12)),
                ("Weeks.cs", 2, "Sync", fixture_version(13)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&callee_bytes, &universe_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["WorkoutService.cs", "Weeks.cs"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("Universe foreign call must not retarget to SetNote".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_call_namespace_generic_fallback_retargets() -> Result<(), String> {
        let package = package_key("fixture");
        let chain = [
            AncestorFixture {
                name: b"Demo",
                version_byte: 40,
                entity_id: TreeEntityId::new(0),
                parent_id: None,
                parent_version_byte: None,
                kind: ItemKind::Module,
            },
            AncestorFixture {
                name: b"Box",
                version_byte: 41,
                entity_id: TreeEntityId::new(1),
                parent_id: Some(TreeEntityId::new(0)),
                parent_version_byte: Some(40),
                kind: ItemKind::Record,
            },
        ];
        let service_bytes = project_namespace_call_image(
            "Box.cs",
            1,
            &chain,
            b"SetNote",
            42,
            TreeEntityId::new(2),
            b"Sync",
            43,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.Box<T>",
                display: b"SetNote",
                foreign_key: 42,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let set_note_identity = fixture_version(42).identity();
        let sync_identity = fixture_version(43).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("Box.cs", 1, "SetNote", fixture_version(42)),
                ("Weeks.cs", 2, "Sync", fixture_version(43)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["Box.cs", "Weeks.cs"]),
        )
        .map_err(|error| error.to_string())?;
        let targets = relation_targets(&relations, sync_id);
        if targets != vec![set_note_id] {
            return Err(format!(
                "expected generic namespace fallback onto SetNote, got {targets:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_call_namespace_unbalanced_generic_stays_external() -> Result<(), String> {
        let package = package_key("fixture");
        let chain = [
            AncestorFixture {
                name: b"Demo",
                version_byte: 50,
                entity_id: TreeEntityId::new(0),
                parent_id: None,
                parent_version_byte: None,
                kind: ItemKind::Module,
            },
            AncestorFixture {
                name: b"Box",
                version_byte: 51,
                entity_id: TreeEntityId::new(1),
                parent_id: Some(TreeEntityId::new(0)),
                parent_version_byte: Some(50),
                kind: ItemKind::Record,
            },
        ];
        let service_bytes = project_namespace_call_image(
            "Box.cs",
            1,
            &chain,
            b"SetNote",
            52,
            TreeEntityId::new(2),
            b"Sync",
            53,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.Box<T",
                display: b"SetNote",
                foreign_key: 52,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let sync_identity = fixture_version(53).identity();
        let set_note_identity = fixture_version(52).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("Box.cs", 1, "SetNote", fixture_version(52)),
                ("Weeks.cs", 2, "Sync", fixture_version(53)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let sync_id = RowId::Symbol(semantic_symbol(package, sync_identity));
        let set_note_id = RowId::Symbol(semantic_symbol(package, set_note_identity));
        let relations = project_semantic_graph_relations_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, sync_identity),
            sync_id,
            false,
            &project_paths(&["Box.cs", "Weeks.cs"]),
        )
        .map_err(|error| error.to_string())?;
        if relation_targets(&relations, sync_id).contains(&set_note_id) {
            return Err("unbalanced generic namespace must stay external".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_namespace_csharp_chain_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            1,
            &csharp_owner_chain(),
            b"SetNote",
            12,
            TreeEntityId::new(2),
            b"Sync",
            13,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"SetNote",
                foreign_key: 21,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let set_note_identity = fixture_version(12).identity();
        let sync_identity = fixture_version(13).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.cs", 1, "SetNote", fixture_version(12)),
                ("Weeks.cs", 2, "Sync", fixture_version(13)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["WorkoutService.cs", "Weeks.cs"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, sync_identity) {
            return Err("namespace retarget site is not Sync".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::MethodCall {
            return Err(format!(
                "namespace retarget relation is {:?}",
                fact.relation
            ));
        }
        let expected_target = semantic_declaration_identity(set_note_identity);
        if !matches!(
            &fact.target,
            backend_engine::SemanticLinkTarget::Local { declaration }
                if *declaration == expected_target
        ) {
            return Err("namespace retarget target is not SetNote".to_owned());
        }
        Ok(())
    }

    #[test]
    fn project_references_namespace_java_chain_names_the_caller() -> Result<(), String> {
        let package = package_key("fixture");
        let service_bytes = project_namespace_call_image(
            "WorkoutService.java",
            1,
            &java_owner_chain(),
            b"setNote",
            22,
            TreeEntityId::new(1),
            b"sync",
            23,
            TreeEntityId::new(2),
            NamespaceCallFixture {
                ecosystem: b"maven",
                namespace: b"demo.WorkoutService",
                display: b"setNote",
                foreign_key: 22,
                link_kind: LinkKind::Calls,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let set_note_identity = fixture_version(22).identity();
        let sync_identity = fixture_version(23).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.java", 1, "setNote", fixture_version(22)),
                ("Weeks.java", 2, "sync", fixture_version(23)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&service_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["WorkoutService.java", "Weeks.java"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site != semantic_symbol(package, sync_identity) {
            return Err("namespace retarget site is not sync".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::Calls {
            return Err(format!(
                "namespace retarget relation is {:?}",
                fact.relation
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_namespace_ambiguous_emits_nothing() -> Result<(), String> {
        let package = package_key("fixture");
        let first_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            1,
            &csharp_owner_chain(),
            b"SetNote",
            12,
            TreeEntityId::new(2),
            b"Sync",
            13,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"SetNote",
                foreign_key: 21,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let duplicate_bytes = project_namespace_call_image(
            "WorkoutService.cs",
            4,
            &csharp_owner_chain(),
            b"SetNote",
            14,
            TreeEntityId::new(2),
            b"Sync",
            15,
            TreeEntityId::new(3),
            NamespaceCallFixture {
                ecosystem: b"nuget",
                namespace: b"Demo.WorkoutService",
                display: b"SetNote",
                foreign_key: 24,
                link_kind: LinkKind::MethodCall,
            },
            ItemKind::Function,
            Some(ItemKind::Function),
        )?;
        let sync_identity = fixture_version(13).identity();
        let set_note_identity = fixture_version(12).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("WorkoutService.cs", 1, "SetNote", fixture_version(12)),
                ("Weeks.cs", 2, "Sync", fixture_version(13)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&first_bytes, &duplicate_bytes],
            &view,
            package,
            semantic_symbol(package, set_note_identity),
            &project_paths(&["WorkoutService.cs", "Weeks.cs"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        let sync_symbol = semantic_symbol(package, sync_identity);
        let sync_site_facts = facts.iter().filter(|fact| fact.site == sync_symbol).count();
        if sync_site_facts != 0 {
            return Err(format!(
                "ambiguous namespace target produced {sync_site_facts} sync-site facts"
            ));
        }
        Ok(())
    }

    #[test]
    fn project_references_unindexed_package_does_not_error() -> Result<(), String> {
        let package = package_key("fixture");
        let apply_bytes = project_call_image(
            "apply-set.ts",
            1,
            b"entriesFromItems",
            TreeEntityId::new(0),
            None,
        )?;
        let weeks_bytes = project_call_image(
            "weeks.ts",
            2,
            b"syncWorkout",
            TreeEntityId::new(0),
            Some(ForeignCallFixture {
                package_specifier: b"./apply-set",
                path_specifier: None,
                display: b"entriesFromItems",
                foreign_key: 9,
                entity_kind: ItemKind::Function,
                link_kind: LinkKind::Calls,
            }),
        )?;
        let entries_identity = fixture_version(1).identity();
        let sync_identity = fixture_version(2).identity();
        let rows = semantic_view_rows(
            package,
            &[
                ("apply-set.ts", 1, "entriesFromItems", fixture_version(1)),
                ("weeks.ts", 2, "syncWorkout", fixture_version(2)),
            ],
        )?;
        let view = semantic_view(rows)?;
        let facts = project_reference_facts_from_bytes(
            &[&apply_bytes, &weeks_bytes],
            &view,
            package,
            semantic_symbol(package, entries_identity),
            &project_paths(&["apply-set.ts", "weeks.ts"]),
            &[],
        )
        .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        if facts[0].site != semantic_symbol(package, sync_identity) {
            return Err("unindexed foreign retarget site is not syncWorkout".to_owned());
        }

        // This legacy image can join by name under the old unrestricted
        // scope. The current mixed-inventory policy must withhold that join
        // and surface a gap, rather than silently claim identical coverage.
        let mut profiles = std::collections::BTreeMap::new();
        let mut gaps = BTreeSet::new();
        restrict_typescript_inventory(
            &mut profiles,
            backend_semantic::vocabulary::LanguageProfile::TypeScript(
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
            ),
            &mut gaps,
        );
        let scope = std::collections::BTreeMap::from([(
            backend_semantic::vocabulary::Language::TypeScript,
            *profiles.values().next().ok_or("missing TypeScript scope")?,
        )]);
        let opened = [&apply_bytes, &weeks_bytes]
            .into_iter()
            .map(|bytes| backend_semantic::ir::SemanticImageView::reopen(bytes))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let restricted = project_opened_reference_facts(
            &opened,
            &view,
            package,
            semantic_symbol(package, entries_identity),
            &project_paths(&["apply-set.ts", "weeks.ts"]),
            &[],
            None,
            &scope,
            &[],
            true,
        )
        .map_err(|error| error.to_string())?;
        assert!(restricted.iter().all(|fact| fact.site != semantic_symbol(package, sync_identity)));
        assert!(matches!(
            reference_coverage(&gaps, true).map_err(|error| error.to_string())?,
            backend_engine::ReferenceCoverage::Partial { reason }
                if reason.as_str().contains("TypeScript source inventory may be incomplete")
        ));
        assert!(matches!(
            reference_coverage(&gaps, false).map_err(|error| error.to_string())?,
            backend_engine::ReferenceCoverage::Unknown { .. }
        ));
        gaps.insert(OBSERVED_POLICY_GAP.to_owned());
        gaps.insert("A".repeat(backend_library::MAX_PRODUCT_TEXT_BYTES));
        assert!(matches!(
            reference_coverage(&gaps, true).map_err(|error| error.to_string())?,
            backend_engine::ReferenceCoverage::Partial { reason }
                if reason.as_str().contains(OBSERVED_POLICY_GAP)
        ));
        Ok(())
    }
}

/// A compiled-image fixture for the references lane: one declaration calling
/// another, with the call's occurrence site captured in source coordinates.
#[cfg(test)]
mod references_tests {
    use super::*;
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
        FactAvailability, IrBuilder, ItemKind, OccurrenceAuthorityFacts, ParentageAuthority,
        SourceIdentity, SourceSpan, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
        VariantFingerprint, Visibility, encode_full_semantic_image, full_semantic_image_len,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    const CALL_PATH: &str = "src/call.rs";

    fn fixture_version(identity: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([identity; 16]),
            variant: VariantFingerprint::from_raw([identity; 16]),
            core_payload: CorePayloadHash::from_raw([identity; 16]),
        }
    }

    /// Encodes one Rust image whose `Caller` entity calls `Callee`, with the
    /// call occurrence carrying a captured source span.
    fn fixture_image() -> Result<Vec<u8>, String> {
        fixture_image_with_roles([backend_semantic::ir::DeclarationPublicationRole::Unavailable; 2])
    }

    fn fixture_image_with_roles(
        roles: [backend_semantic::ir::DeclarationPublicationRole; 2],
    ) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"call fixture"),
            byte_len: 12,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, CALL_PATH)
            .map_err(|error| error.to_string())?;
        let calling_site = TreeEntityId::new(0);
        let called_target = TreeEntityId::new(1);
        let authority = |parentage, publication| EntityAuthorityFacts {
            publication,
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"Caller",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root, roles[0]),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"Callee",
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: authority(ParentageAuthority::Root, roles[1]),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        // The provenance path atom is interned by the builder before the tree;
        // resolve its exact dense coordinate instead of guessing it.
        let links = [TreeLinkInput {
            from: calling_site,
            target: TreeLinkTarget::Local(called_target),
            kind: backend_semantic::ir::LinkKind::Calls,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[fixture_version(1), fixture_version(2)],
                items: &items,
                links: &links,
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let path_atom = (0..64)
            .map(backend_semantic::ir::AtomId::new)
            .find(|id| {
                ir.atom(*id)
                    .is_some_and(|bytes| bytes == CALL_PATH.as_bytes())
            })
            .ok_or("the fixture source path is absent from its own atom table")?;
        // Attach the captured call site now that the path atom is known by
        // re-adding one identified image-level occurrence lane entry.
        let ir = {
            let mut builder = IrBuilder::new();
            let source = SourceIdentity {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"call fixture"),
                byte_len: 12,
            };
            builder
                .set_image_provenance_for_package(source, recipe, &coordinate, CALL_PATH)
                .map_err(|error| error.to_string())?;
            let links = [TreeLinkInput {
                from: calling_site,
                target: TreeLinkTarget::Local(called_target),
                kind: backend_semantic::ir::LinkKind::Calls,
                confidence: backend_semantic::ir::Confidence::Compiler,
                authority: OccurrenceAuthorityFacts {
                    source: FactAvailability::Captured,
                },
                source: SourceSpan::new(path_atom, 12, 24),
            }];
            builder
                .add_borrowed_tree(BorrowedTree {
                    versions: &[fixture_version(1), fixture_version(2)],
                    items: &items,
                    links: &links,
                })
                .map_err(|error| error.to_string())?;
            builder.finish().map_err(|error| error.to_string())?
        };
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    #[test]
    fn reference_only_targets_can_anchor_owned_sites_but_support_sites_do_not_publish()
    -> Result<(), String> {
        use backend_semantic::ir::DeclarationPublicationRole as Role;
        let package = backend_engine::package_key("fixture");
        let callee = super::super::super::view_build::semantic_symbol(
            package,
            fixture_version(2).identity(),
        );
        for (roles, expected) in [
            ([Role::PrimaryOwned, Role::ReferenceOnly], 1),
            ([Role::ReferenceOnly, Role::PrimaryOwned], 0),
        ] {
            let bytes = fixture_image_with_roles(roles)?;
            let image = backend_semantic::ir::SemanticImageView::reopen(&bytes)
                .map_err(|error| error.to_string())?;
            let mut facts = Vec::new();
            append_reference_facts(package, &image, callee, &mut facts)
                .map_err(|error| error.to_string())?;
            assert_eq!(facts.len(), expected);
        }
        Ok(())
    }

    #[test]
    fn native_reference_occurrences_keep_their_image_origins() -> Result<(), String> {
        let package = backend_engine::package_key("fixture");
        let caller = view_build::semantic_symbol(package, fixture_version(1).identity());
        let callee = view_build::semantic_symbol(package, fixture_version(2).identity());
        let (initial, _) = super::super::super::initial_view().map_err(|error| error.to_string())?;
        let capability = super::super::super::test_builtin_view_capability()
            .map_err(|error| error.to_string())?;
        let rows = vec![
            backend_engine::Row::in_package(backend_engine::RowId::Symbol(caller), initial.basis(), package, "fixture::Caller"),
            backend_engine::Row::in_package(backend_engine::RowId::Symbol(callee), initial.basis(), package, "fixture::Callee"),
        ];
        let view = backend_engine::ViewRoot::new_checked(
            initial.recipe(), initial.basis(), initial.frontier(), rows,
            vec![backend_engine::ViewCoverage::Complete], capability,
        ).map_err(|error| format!("{error:?}"))?;
        let first = fixture_image()?;
        let second = fixture_image()?;
        let images = [&first, &second].into_iter()
            .map(|bytes| backend_semantic::ir::SemanticImageView::reopen(bytes))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let image_refs = images.iter().collect::<Vec<_>>();
        let index = ProjectCallableIndex::build_from_views(&image_refs)
            .map_err(|error| error.to_string())?;
        let facts = project_native_reference_facts_with_origins(
            &images, &view, package, callee, &index,
        ).map_err(|error| error.to_string())?;
        assert_eq!(facts.len(), 2);
        assert_eq!((facts[0].1, facts[1].1), (0, 1));
        assert_eq!(facts[0].0, facts[1].0);
        assert_eq!(facts[0].0.site, caller);
        assert!(facts[0].0.evidence.source.is_some());
        let bound = bind_native_reference_occurrence_origins(&images, package, facts)
            .map_err(|error| error.to_string())?;
        assert_eq!(bound.len(), 2);
        assert_eq!((bound[0].image_index, bound[1].image_index), (0, 1));
        assert_eq!(bound[0].owner, fixture_version(1).identity());
        assert_eq!(bound[1].owner, fixture_version(1).identity());
        assert_eq!(bound[0].entity_index, bound[1].entity_index);
        assert_eq!(bound[0].image_identity, bound[1].image_identity);
        assert_eq!(bound[0].image_byte_len as usize, first.len());
        assert_eq!(bound[0].fact, bound[1].fact);
        let mut repeated = vec![bound[1].clone(), bound[0].clone(), bound[0].clone()];
        deduplicate_observed_reference_origins(&view, &mut repeated);
        assert_eq!(repeated.len(), 2);
        assert_eq!((repeated[0].image_index, repeated[1].image_index), (0, 1));
        let relation = backend_engine::GraphRelation::new(
            backend_engine::RowId::Symbol(caller),
            backend_engine::RowId::Symbol(callee),
            backend_engine::SemanticLinkKind::Calls,
        );
        let graph = bind_native_graph_edge_source_origins(
            &images, package, BTreeSet::from([(relation, 0), (relation, 1)]),
        ).map_err(|error| error.to_string())?;
        assert_eq!(graph.len(), 2);
        assert_eq!((graph[0].image_index, graph[1].image_index), (0, 1));
        assert_eq!(graph[0].owner, fixture_version(1).identity());
        assert_eq!(graph[1].owner, fixture_version(1).identity());
        assert_eq!(graph[0].entity_index, graph[1].entity_index);
        assert_eq!(graph[0].relation.from, backend_engine::RowId::Symbol(caller));
        assert!(bind_native_graph_edge_source_origins(
            &images, package, BTreeSet::from([(relation, 2)]),
        ).is_err());
        Ok(())
    }

    #[test]
    fn qualified_graph_continuation_keeps_center_witness_and_edge() -> Result<(), String> {
        use backend_library::{GraphAvailability, SourcePolicyObservation, SourcePolicyQualification};
        let package = backend_engine::package_key("fixture");
        let (initial, _) = super::super::super::initial_view().map_err(|error| error.to_string())?;
        let capability = super::super::super::test_builtin_view_capability().map_err(|error| error.to_string())?;
        let center = backend_engine::symbol_key("fixture::center");
        let neighbor = backend_engine::symbol_key("fixture::neighbor");
        let rows = vec![
            backend_engine::Row::in_package(backend_engine::RowId::Symbol(center), initial.basis(), package, "fixture::center"),
            backend_engine::Row::in_package(backend_engine::RowId::Symbol(neighbor), initial.basis(), package, "fixture::neighbor"),
        ];
        let make_root = |rows| backend_engine::ViewRoot::new_checked(
            initial.recipe(), initial.basis(), initial.frontier(), rows,
            vec![backend_engine::ViewCoverage::Complete], capability.clone(),
        ).map_err(|error| format!("{error:?}"));
        let full_root = make_root(rows.clone())?;
        let page_root = make_root(vec![rows[1].clone()])?;
        let policy = |row: &backend_engine::Row| -> Result<SourcePolicyQualification, String> {
            let selection = backend_library::DocumentSelection::from_row(row, page_root.basis()).ok_or("selection")?;
            let profile = backend_library::SemanticLanguageProfile::from_name("tsx").ok_or("profile")?;
            SourcePolicyQualification::issue_observed(&[7; 32], &selection, SourcePolicyObservation {
                workspace_root: [1; 32], evidence_root: [2; 32], project: [3; 32],
                profile, publication_profile: profile, publication_coordinate: "pkg:npm/fixture@1.0.0".into(),
                source_path: "page.tsx".into(), source_fact: [4; 32], input_digest: [5; 32], byte_len: 50,
                publication_binding: [6; 32], image_identity: [7; 32], image_byte_len: 128,
                program_membership: [8; 32], entity_index: 1, declaration_family: [9; 16], declaration_variant: [10; 16],
            }).map_err(|error| error.to_string())
        };
        let relation = backend_engine::GraphRelation::new(
            backend_engine::RowId::Symbol(center), backend_engine::RowId::Symbol(neighbor), backend_engine::SemanticLinkKind::Calls,
        );
        let generation = backend_engine::SemanticGenerationId::new([11; 32]);
        let revision = backend_engine::RichGraphRevision::new(full_root.root().to_bytes(), generation, [12; 32]);
        let mut builder = backend_engine::RichGraphBuilder::new(revision, backend_engine::GraphNodeId::for_symbol(center));
        for row in &rows {
            let mut node = backend_engine::RichGraphNode::new(
                backend_engine::GraphNodeId::for_row(row.id), row.label.clone(), Some(row.label.clone()), None,
                GraphAvailability::Ready, None,
            ).map_err(|error| error.to_string())?;
            node.source_policy = policy(row)?;
            builder.add_node(node).map_err(|error| error.to_string())?;
        }
        let mut edge = backend_engine::RichGraphEdge::from_relation(
            relation, backend_engine::GraphProvenance::semantic(generation, [12; 32], [13; 32], backend_engine::SemanticConfidence::Compiler),
        );
        edge.source_policy = policy(&rows[0])?;
        builder.add_edge(edge).map_err(|error| error.to_string())?;
        let gap = GraphAvailability::Unavailable(backend_engine::ProductText::new("observed policy unknown").map_err(|error| error.to_string())?);
        let graph = builder.finish_with_availability(gap.clone()).map_err(|error| error.to_string())?;
        let full = backend_engine::ViewSnapshot {
            root: full_root, freshness: backend_engine::Freshness::Current, next: None,
            graph_relations: Some(vec![relation].into_boxed_slice()), rich_graph: Some(graph),
        };
        let mut page = backend_library::ProjectionPage {
            snapshot: backend_engine::ViewSnapshot { root: page_root, freshness: backend_engine::Freshness::Current, next: None, graph_relations: None, rich_graph: None },
            terminal: backend_library::PageTerminal::Complete, outline_order: None,
        };
        propagate_qualified_graph_page(&mut page, &full).map_err(|error| error.to_string())?;
        let graph = page.snapshot.rich_graph.as_ref().ok_or("missing page graph")?;
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.center, backend_engine::GraphNodeId::for_symbol(center));
        assert_eq!(graph.availability, gap);
        assert!(graph.nodes.iter().all(|node| !node.source_policy.is_unknown()));
        assert!(!graph.edges[0].source_policy.is_unknown());
        assert!(page.snapshot.graph_relations.is_none());
        let mut missing_center_claim = full.clone();
        let source = missing_center_claim.rich_graph.as_mut().ok_or("missing full graph")?;
        let center_id = source.center;
        let center_node = std::sync::Arc::make_mut(&mut source.nodes).iter_mut()
            .find(|node| node.id == center_id).ok_or("missing center node")?;
        center_node.source_policy = SourcePolicyQualification::Unknown;
        let mut rejected = page.clone();
        rejected.snapshot.rich_graph = None;
        assert!(propagate_qualified_graph_page(&mut rejected, &missing_center_claim).is_err());
        Ok(())
    }

    #[test]
    fn a_call_occurrence_answers_as_a_reference_with_its_source_span() -> Result<(), String> {
        let bytes = fixture_image()?;
        let package = backend_engine::package_key("fixture");
        let callee_symbol = super::super::super::view_build::semantic_symbol(
            package,
            fixture_version(2).identity(),
        );
        let mut facts = Vec::new();
        let image = backend_semantic::ir::SemanticImageView::reopen(&bytes)
            .map_err(|error| error.to_string())?;
        append_reference_facts(package, &image, callee_symbol, &mut facts)
            .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site
            != super::super::super::view_build::semantic_symbol(
                package,
                fixture_version(1).identity(),
            )
        {
            return Err("the reference site is not the calling declaration".to_owned());
        }
        if fact.relation != backend_engine::SemanticLinkKind::Calls {
            return Err(format!("reference relation is {:?}", fact.relation));
        }
        if fact.evidence.confidence != backend_engine::SemanticConfidence::Compiler {
            return Err(format!(
                "reference confidence is {:?}",
                fact.evidence.confidence
            ));
        }
        if !matches!(
            fact.target,
            backend_engine::SemanticLinkTarget::Local { .. }
        ) {
            return Err("a same-image call must resolve as a local target".to_owned());
        }
        let span = fact
            .evidence
            .source
            .as_ref()
            .ok_or("the call occurrence lost its captured span")?;
        if (span.start, span.end) != (12, 24) {
            return Err(format!("span is {}..{}", span.start, span.end));
        }
        if span.file.as_str() != CALL_PATH {
            return Err(format!("span file is {}", span.file.as_str()));
        }
        Ok(())
    }

    #[test]
    fn an_image_without_the_target_contributes_no_reference_facts() -> Result<(), String> {
        let bytes = fixture_image()?;
        let package = backend_engine::package_key("fixture");
        // A declaration the fixture never compiled.
        let absent = super::super::super::view_build::semantic_symbol(
            package,
            fixture_version(9).identity(),
        );
        let mut facts = Vec::new();
        let image = backend_semantic::ir::SemanticImageView::reopen(&bytes)
            .map_err(|error| error.to_string())?;
        append_reference_facts(package, &image, absent, &mut facts)
            .map_err(|error| error.to_string())?;
        if !facts.is_empty() {
            return Err(format!("absent target produced {} facts", facts.len()));
        }
        Ok(())
    }

    #[test]
    fn borrowed_reference_target_matches_the_first_cloned_row() -> Result<(), String> {
        const ROWS: usize = 4096;
        const SAMPLES: usize = 9;
        let (initial, _) =
            super::super::super::initial_view().map_err(|error| error.to_string())?;
        let capability = super::super::super::test_builtin_view_capability()
            .map_err(|error| error.to_string())?;
        let package = backend_engine::package_key("fixture");
        let body = "d".repeat(4096);
        let mut built = Vec::with_capacity(ROWS);
        for index in 0..ROWS {
            let label = if index == 7 || index == 4000 {
                "pkg::shared".to_owned()
            } else {
                format!("pkg::decl-{index}")
            };
            let symbol = backend_engine::symbol_key(&format!("reference-symbol-{index}"));
            built.push(
                backend_engine::Row::in_package(
                    backend_engine::RowId::Symbol(symbol),
                    initial.basis(),
                    package,
                    label,
                )
                .with_document(vec![backend_engine::Fragment::Text(body.clone())]),
            );
        }
        let view = backend_engine::ViewRoot::new_checked(
            initial.recipe(),
            initial.basis(),
            initial.frontier(),
            built,
            vec![backend_engine::ViewCoverage::Complete],
            capability,
        )
        .map_err(|error| format!("{error:?}"))?;
        let owned_first = view
            .row_refs()
            .cloned()
            .find(|row| row.label == "pkg::shared")
            .map(|row| row.id);
        let borrowed_first = view
            .row_refs()
            .find(|row| row.label == "pkg::shared")
            .map(|row| row.id);
        if owned_first != borrowed_first {
            return Err(format!(
                "borrowed target {borrowed_first:?} differs from cloned target {owned_first:?}"
            ));
        }
        let duplicate_ids: Vec<_> = view
            .row_refs()
            .filter(|row| row.label == "pkg::shared")
            .map(|row| row.id)
            .collect();
        if duplicate_ids.len() != 2 {
            return Err(format!(
                "expected two shared labels, got {}",
                duplicate_ids.len()
            ));
        }
        let earliest = duplicate_ids.iter().copied().min();
        if borrowed_first != earliest {
            return Err(format!(
                "first label match {borrowed_first:?} is not the earliest row {earliest:?}"
            ));
        }
        let absent = backend_engine::RowId::Symbol(backend_engine::symbol_key("missing-symbol"));
        if view.row(absent).is_none() != view.row_ref(absent).is_none() {
            return Err("absent presence check diverged".to_owned());
        }
        let present = borrowed_first.ok_or("shared label is absent")?;
        if view.row(present).is_none() || view.row_ref(present).is_none() {
            return Err("present target was reported absent".to_owned());
        }
        let ids: Vec<_> = view.row_refs().map(|row| row.id).collect();
        let mut owned_find = [0_u128; SAMPLES];
        let mut borrowed_find = [0_u128; SAMPLES];
        let mut owned_presence = [0_u128; SAMPLES];
        let mut borrowed_presence = [0_u128; SAMPLES];
        for sample in 0..SAMPLES {
            let started = std::time::Instant::now();
            let owned: Vec<_> = view.row_refs().cloned().collect();
            let hit = owned
                .iter()
                .find(|row| row.label == "pkg::shared")
                .map(|row| row.document.len());
            owned_find[sample] = started.elapsed().as_nanos();
            std::hint::black_box(hit);
            let started = std::time::Instant::now();
            let hit = view
                .row_refs()
                .find(|row| row.label == "pkg::shared")
                .map(|row| row.id);
            borrowed_find[sample] = started.elapsed().as_nanos();
            std::hint::black_box(hit);
            let started = std::time::Instant::now();
            let mut bytes = 0_usize;
            for id in &ids {
                if let Some(row) = view.row(*id) {
                    bytes += row.document.len();
                }
            }
            owned_presence[sample] = started.elapsed().as_nanos();
            std::hint::black_box(bytes);
            let started = std::time::Instant::now();
            let mut absent_count = 0_usize;
            for id in &ids {
                if view.row_ref(*id).is_none() {
                    absent_count += 1;
                }
            }
            borrowed_presence[sample] = started.elapsed().as_nanos();
            std::hint::black_box(absent_count);
        }
        let materialized_first = view
            .rows()
            .iter()
            .find(|row| row.label == "pkg::shared")
            .map(|row| row.id);
        if materialized_first != borrowed_first {
            return Err(format!(
                "borrowed target {borrowed_first:?} differs from rows() target \
                 {materialized_first:?}"
            ));
        }
        owned_find.sort_unstable();
        borrowed_find.sort_unstable();
        owned_presence.sort_unstable();
        borrowed_presence.sort_unstable();
        let owned_find_median = owned_find[SAMPLES / 2];
        let borrowed_find_median = borrowed_find[SAMPLES / 2];
        let owned_presence_median = owned_presence[SAMPLES / 2];
        let borrowed_presence_median = borrowed_presence[SAMPLES / 2];
        eprintln!(
            "borrowed_reference_target rows={ROWS} \
             find_owned_median_ns={owned_find_median} \
             find_borrowed_median_ns={borrowed_find_median} \
             presence_owned_median_ns={owned_presence_median} \
             presence_borrowed_median_ns={borrowed_presence_median}"
        );
        if borrowed_find_median.saturating_mul(2) >= owned_find_median {
            return Err(format!(
                "borrowed label find {borrowed_find_median} ns was not twice as fast as cloning \
                 every row {owned_find_median} ns"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "selected_source_references_tests.rs"]
mod selected_source_references_tests;

#[cfg(test)]
#[path = "python_alias_publication_tests.rs"]
mod python_alias_publication_tests;
