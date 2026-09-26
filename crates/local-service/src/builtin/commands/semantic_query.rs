use super::super::read_indexed_sources;
use super::super::view_build;
use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
use super::snapshot::{semantic_confidence, semantic_declaration_identity, semantic_link_kind};
use backend_engine::application::{DocumentationSession, LocalCompilerClient};
use backend_engine::builtin::{ProductSemanticPublicationRecord, SemanticPublicationCoverage};
use backend_semantic::ir::{
    DeclarationIdentity, ExternalTarget, ForeignTargetOrigin, LinkKind, LinkTarget,
    SemanticCoreReader, SemanticReader,
};
use futures_util::StreamExt as _;
use std::collections::{BTreeMap, BTreeSet};
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

pub(super) fn execute_semantic_graph(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
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
    let source = library.view().row(source_id).ok_or_else(|| {
        BuiltinModelError("semantic graph source is absent from the selected view".to_owned())
    })?;
    let semantic_query = query.with_resolved_symbol(source_symbol);
    let Some(package) = source.package else {
        return Ok(None);
    };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_indexed_sources(&snapshot)?;
    let view = library.view();
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic graph relation: {error}")))?;
    let mut activations = Vec::new();
    let mut publication_bindings = Vec::new();
    let mut image_slots = Vec::<(usize, usize)>::new();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("page semantic graph relation: {error}")))?;
        for (key, record) in page.entries() {
            if !key.is_selected() {
                continue;
            }
            let ProductSemanticPublicationRecord::Published {
                coverage: SemanticPublicationCoverage::Complete,
                claim,
            } = record
            else {
                continue;
            };
            if key.package_key() != package {
                continue;
            }
            let binding = claim.binding();
            let activated = activate_semantic_publication(compiler, key, *claim)?;
            let activation_index = activations.len();
            activations.push(activated);
            publication_bindings.push(binding);
            for image_index in 0..activations[activation_index].images().len() {
                image_slots.push((activation_index, image_index));
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    let mut source_binding = None;
    for (activation_index, image_index) in &image_slots {
        let bytes = activations[*activation_index].images()[*image_index].as_ref();
        let image = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
            BuiltinModelError(format!("reopen semantic graph image: {error}"))
        })?;
        if semantic_entity_for_symbol(&image, package, source_symbol)?.is_some() {
            source_binding = Some(publication_bindings[*activation_index]);
            break;
        }
    }
    let Some(binding) = source_binding else {
        return Ok(None);
    };
    let project_paths = project_paths_for_package(&sources, package);
    let mut relations = project_semantic_graph_relations(
        &activations,
        &image_slots,
        view,
        package,
        source_symbol,
        source_id,
        include_incoming,
        &project_paths,
    )?;
    let pairs = if package_indexed_in_sources(&sources, package) {
        view_build::structural_call_coordinate_pairs(&sources, package)?
    } else {
        Vec::new()
    };
    for relation in view_build::structural_call_graph_relations_mapped(
        view,
        &pairs,
        package,
        source_id,
        include_incoming,
    ) {
        relations.insert(relation);
    }
    let relations = relations.into_iter().collect::<Vec<_>>();
    let mut snapshot = library
        .graph_from_semantic_relations(semantic_query, &relations)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let graph_revision = backend_engine::RichGraphRevision::new(
        snapshot.root.root().to_bytes(),
        backend_engine::SemanticGenerationId::new(*binding.identity.as_ref()),
        *binding.generation.pinned_root.as_ref(),
    );
    let rich_graph = backend_engine::RichGraphSnapshot::from_view(
        &snapshot.root,
        source_id,
        &relations,
        graph_revision,
    )
    .map_err(|error| BuiltinModelError(format!("build rich semantic graph: {error}")))?;
    snapshot.rich_graph = Some(rich_graph);
    Ok(Some(snapshot))
}

pub(super) fn execute_structural_call_graph(
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
    let source = view.row(source_id).ok_or_else(|| {
        BuiltinModelError(
            "structural call graph source is absent from the selected view".to_owned(),
        )
    })?;
    let Some(package) = source.package else {
        return Ok(None);
    };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_indexed_sources(&snapshot)?;
    let relations = view_build::structural_call_graph_relations(
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

fn project_paths_for_package(
    sources: &super::super::IndexedSources,
    package: backend_engine::PackageKey,
) -> BTreeSet<String> {
    let Some(project_key) = sources
        .projects
        .values()
        .find(|project| project.package == package)
        .map(|project| project.package.to_bytes())
    else {
        return BTreeSet::new();
    };
    let mut paths = BTreeSet::new();
    for (_, record) in &sources.files {
        let Some(file) = record.file_fields() else {
            continue;
        };
        if file.project == project_key {
            paths.insert(file.path.to_owned());
        }
    }
    paths
}

fn semantic_entity_for_symbol(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
) -> Result<Option<backend_semantic::ir::EntityId>, BuiltinModelError> {
    let session = DocumentationSession::new(image);
    session
        .canonical_entities()
        .find_map(|entity| match entity {
            Ok(entity)
                if super::super::view_build::semantic_symbol(
                    package,
                    entity.entity.version.identity(),
                ) == symbol =>
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

fn semantic_callable(kind: backend_semantic::ir::ItemKind) -> bool {
    matches!(kind, backend_semantic::ir::ItemKind::Function)
}

struct ProjectCallableIndex {
    by_path_name: BTreeMap<(String, String), Vec<DeclarationIdentity>>,
}

impl ProjectCallableIndex {
    fn build_from_bytes(images: &[&[u8]]) -> Result<Self, BuiltinModelError> {
        let mut by_path_name = BTreeMap::<(String, String), Vec<DeclarationIdentity>>::new();
        for bytes in images {
            let image = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
                BuiltinModelError(format!("reopen semantic graph image: {error}"))
            })?;
            let path = view_build::compiled_source_path(&image)?;
            let session = DocumentationSession::new(&image);
            for entity in session.canonical_entities() {
                let entity = entity.map_err(|error| {
                    BuiltinModelError(format!("read semantic graph callable: {error}"))
                })?;
                if !semantic_callable(entity.entity.kind) {
                    continue;
                }
                let name = std::str::from_utf8(entity.name).map_err(|_| {
                    BuiltinModelError("semantic graph callable name is not UTF-8".to_owned())
                })?;
                by_path_name
                    .entry((path.clone(), name.to_owned()))
                    .or_default()
                    .push(entity.entity.version.identity());
            }
        }
        Ok(Self { by_path_name })
    }

    fn resolve(
        &self,
        resolved_paths: &BTreeSet<String>,
        display: &str,
    ) -> Option<DeclarationIdentity> {
        let mut matches = Vec::new();
        for path in resolved_paths {
            if let Some(identities) = self.by_path_name.get(&(path.clone(), display.to_owned())) {
                matches.extend(identities);
            }
        }
        matches.sort();
        matches.dedup();
        if matches.len() == 1 {
            matches.pop()
        } else {
            None
        }
    }
}

fn foreign_package_call_retarget(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    external: backend_semantic::ir::ExternalId,
    caller_path: &str,
    project_paths: &BTreeSet<String>,
    callable_index: &ProjectCallableIndex,
) -> Result<Option<DeclarationIdentity>, BuiltinModelError> {
    let Some(ExternalTarget::Foreign(foreign)) = image.external(external) else {
        return Ok(None);
    };
    let ForeignTargetOrigin::Package { package, .. } = foreign.origin else {
        return Ok(None);
    };
    let package_atom = image
        .atom(package)
        .ok_or_else(|| BuiltinModelError("semantic graph package atom is missing".to_owned()))?;
    let display_atom = image
        .atom(foreign.display)
        .ok_or_else(|| BuiltinModelError("semantic graph display atom is missing".to_owned()))?;
    let specifier = std::str::from_utf8(package_atom).map_err(|_| {
        BuiltinModelError("semantic graph package specifier is not UTF-8".to_owned())
    })?;
    let display = std::str::from_utf8(display_atom).map_err(|_| {
        BuiltinModelError("semantic graph display name is not UTF-8".to_owned())
    })?;
    let resolved_paths =
        view_build::resolve_specifier_paths(specifier, caller_path, project_paths);
    Ok(callable_index.resolve(&resolved_paths, display))
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
) -> Result<Option<backend_engine::RowId>, BuiltinModelError> {
    let row_id = match target {
        LinkTarget::Local(target) => {
            let target = session.entity(target).map_err(|error| {
                BuiltinModelError(format!("read semantic graph target: {error}"))
            })?;
            backend_engine::RowId::Symbol(super::super::view_build::semantic_symbol(
                package,
                target.entity.version.identity(),
            ))
        }
        LinkTarget::External(external) => {
            if matches!(link_kind, LinkKind::Calls | LinkKind::MethodCall) {
                if let Some(identity) = foreign_package_call_retarget(
                    image,
                    external,
                    caller_path,
                    project_paths,
                    callable_index,
                )? {
                    return Ok(view
                        .row(backend_engine::RowId::Symbol(
                            super::super::view_build::semantic_symbol(package, identity),
                        ))
                        .map(|_| backend_engine::RowId::Symbol(
                            super::super::view_build::semantic_symbol(package, identity),
                        )));
                }
            }
            let identity = backend_semantic::ir::ExternalTargetIdentity::capture(image, external)
                .map_err(|error| {
                    BuiltinModelError(format!("identify semantic graph target: {error}"))
                })?;
            backend_engine::RowId::Symbol(super::super::view_build::external_semantic_symbol(
                package,
                image_identity,
                identity,
            ))
        }
    };
    if view.row(row_id).is_some() {
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
    let callable_index = ProjectCallableIndex::build_from_bytes(images)?;
    let mut relations = BTreeSet::new();
    for bytes in images {
        let image = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
            BuiltinModelError(format!("reopen semantic graph image: {error}"))
        })?;
        let caller_path = view_build::compiled_source_path(&image)?;
        let image_identity = *blake3::hash(image.as_ref()).as_bytes();
        let session = DocumentationSession::new(&image);
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
                    &callable_index,
                )?
                else {
                    continue;
                };
                relations.insert(backend_engine::GraphRelation::new(
                    source_id,
                    target,
                    semantic_link_kind(link.kind),
                ));
            }
        }
        if include_incoming {
            for source in session.canonical_entities() {
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
                        &callable_index,
                    )?
                    else {
                        continue;
                    };
                    if target != source_id {
                        continue;
                    }
                    let from = backend_engine::RowId::Symbol(
                        super::super::view_build::semantic_symbol(
                            package,
                            source.entity.version.identity(),
                        ),
                    );
                    if view.row(from).is_none() {
                        continue;
                    }
                    relations.insert(backend_engine::GraphRelation::new(
                        from,
                        source_id,
                        semantic_link_kind(link.kind),
                    ));
                }
            }
        }
    }
    let mut ids = BTreeSet::from([source_id]);
    for relation in &relations {
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
    activations: &[super::super::ActivatedProductSemantics],
    image_slots: &[(usize, usize)],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
    source_id: backend_engine::RowId,
    include_incoming: bool,
    project_paths: &BTreeSet<String>,
) -> Result<BTreeSet<backend_engine::GraphRelation>, BuiltinModelError> {
    let bytes = image_slots
        .iter()
        .map(|(activation_index, image_index)| {
            activations[*activation_index].images()[*image_index].as_ref()
        })
        .collect::<Vec<_>>();
    project_semantic_graph_relations_from_bytes(
        &bytes,
        view,
        package,
        symbol,
        source_id,
        include_incoming,
        project_paths,
    )
}

/// Answers "where is this declaration used" from the semantic occurrence
/// plane.
///
/// The queried coordinate resolves against the published view; the selected
/// complete publications of its package are then asked for every occurrence
/// that targets the declaration, and the catalog names each site. Sites,
/// targets, and provenance (relation kind, authority class, and the captured
/// source span) all survive to the reply; nothing is reduced to bare
/// adjacency.
pub(super) fn execute_references(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    target: &backend_engine::ProductText,
) -> Result<backend_engine::SurfaceReply, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    let view = library.view();
    let target_row = view
        .rows()
        .iter()
        .find(|row| row.label == target.as_str())
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
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| {
            BuiltinModelError(format!("open semantic references relation: {error}"))
        })?;
    let mut facts = Vec::new();
    let mut publication_found = false;
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| {
                BuiltinModelError(format!("page semantic references relation: {error}"))
            })?;
        for (key, record) in page.entries() {
            if !key.is_selected() || key.package_key() != package {
                continue;
            }
            let ProductSemanticPublicationRecord::Published {
                coverage: SemanticPublicationCoverage::Complete,
                claim,
            } = record
            else {
                continue;
            };
            publication_found = true;
            let activated = activate_semantic_publication(compiler, key, *claim)?;
            for bytes in activated.images() {
                append_reference_facts(package, bytes.as_ref(), target_symbol, &mut facts)?;
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    if !publication_found {
        return execute_structural_references(daemon, target);
    }
    // Deterministic order by source position, then site; the bound is the
    // same bounded result contract every product reply obeys.
    facts.sort_by(|left, right| {
        left.evidence
            .source
            .as_ref()
            .map(|span| (span.file.as_str(), span.start, span.end))
            .cmp(
                &right
                    .evidence
                    .source
                    .as_ref()
                    .map(|span| (span.file.as_str(), span.start, span.end)),
            )
            .then_with(|| left.site.to_bytes().cmp(&right.site.to_bytes()))
    });
    facts.dedup();
    let references = library.references(target, &facts).map_err(|error| {
        BuiltinModelError(format!("project references through the catalog: {error}"))
    })?;
    Ok(backend_engine::SurfaceReply::References {
        target: target.clone(),
        references,
    })
}

fn execute_structural_references(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    target: &backend_engine::ProductText,
) -> Result<backend_engine::SurfaceReply, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    let view = library.view();
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = read_indexed_sources(&snapshot)?;
    let mut facts = view_build::structural_reference_facts(view, &sources, target.as_str())?;
    facts.sort_by(|left, right| {
        left.evidence
            .source
            .as_ref()
            .map(|span| (span.file.as_str(), span.start, span.end))
            .cmp(
                &right
                    .evidence
                    .source
                    .as_ref()
                    .map(|span| (span.file.as_str(), span.start, span.end)),
            )
            .then_with(|| left.site.to_bytes().cmp(&right.site.to_bytes()))
    });
    let references = library.references(target, &facts).map_err(|error| {
        BuiltinModelError(format!("project references through the catalog: {error}"))
    })?;
    Ok(backend_engine::SurfaceReply::References {
        target: target.clone(),
        references,
    })
}

/// Appends every occurrence of one image that targets `target_symbol`.
fn append_reference_facts(
    package: backend_engine::PackageKey,
    image_bytes: &[u8],
    target_symbol: backend_engine::SymbolKey,
    facts: &mut Vec<backend_engine::ReferenceFact>,
) -> Result<(), BuiltinModelError> {
    let image = backend_semantic::ir::SemanticImageView::reopen(image_bytes)
        .map_err(|error| BuiltinModelError(format!("reopen semantic references image: {error}")))?;
    let session = backend_engine::application::DocumentationSession::new(&image);
    let target_entity = session
        .canonical_entities()
        .find_map(|entity| match entity {
            Ok(entity)
                if super::super::view_build::semantic_symbol(
                    package,
                    entity.entity.version.identity(),
                ) == target_symbol =>
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
                .map_err(|error| error.to_string())?
                .entity
                .version
                .identity();
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
                site: super::super::view_build::semantic_symbol(package, site),
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
    use super::project_semantic_graph_relations_from_bytes;
    use super::super::super::view_build::{
        semantic_coordinate, semantic_symbol, structural_call_coordinate_pairs,
        structural_call_graph_relations_mapped,
    };
    use super::super::super::{IndexedProject, IndexedSources, ProductSourceRecord, initial_view};
    use backend_engine::{GraphRelation, Row, RowId, ViewRoot, package_key, product_source_file_key};
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
        ExternalDeclarationIdentity, ExternalTarget, FactAvailability, ForeignDeclarationId,
        ForeignExternalTarget, ForeignTargetOrigin, IrBuilder, ItemKind, LinkKind,
        OccurrenceAuthorityFacts, ParentageAuthority, SourceIdentity, TreeEntityId, TreeItemInput,
        TreeLinkInput, TreeLinkTarget, VariantAvailability, VariantFingerprint, Visibility,
        encode_full_semantic_image, full_semantic_image_len,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    struct ForeignCallFixture {
        package_specifier: &'static [u8],
        display: &'static [u8],
        foreign_key: u8,
    }

    fn fixture_version(identity: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([identity; 16]),
            variant: VariantFingerprint::from_raw([identity; 16]),
            core_payload: CorePayloadHash::from_raw([identity; 16]),
        }
    }

    fn project_call_image(
        path: &str,
        source_identity_byte: u8,
        function_name: &[u8],
        entity_id: TreeEntityId,
        foreign_call: Option<ForeignCallFixture>,
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
            name: function_name,
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
        if let Some(foreign_call) = foreign_call {
            let ecosystem = builder.intern_atom(b"npm").map_err(|e| e.to_string())?;
            let package_atom = builder
                .intern_atom(foreign_call.package_specifier)
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
                    origin: ForeignTargetOrigin::Package {
                        ecosystem,
                        package: package_atom,
                    },
                    path: path_atom,
                    display,
                    kind: Some(ItemKind::Function),
                }))
                .map_err(|e| e.to_string())?;
            links.push(TreeLinkInput {
                from: entity_id,
                target: TreeLinkTarget::External(external),
                kind: LinkKind::Calls,
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

    fn semantic_view(
        rows: Vec<Row>,
    ) -> Result<ViewRoot, String> {
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

    fn project_paths(files: &[&str]) -> BTreeSet<String> {
        files.iter().map(|path| (*path).to_owned()).collect()
    }

    fn relation_targets(
        relations: &BTreeSet<GraphRelation>,
        from: RowId,
    ) -> Vec<RowId> {
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
                display: b"entriesFromItems",
                foreign_key: 9,
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
                display: b"entriesFromItems",
                foreign_key: 9,
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
                display: b"entriesFromItems",
                foreign_key: 9,
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
                display: b"entriesFromItems",
                foreign_key: 10,
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

    fn analyze_source(path: &str, source: &str) -> Result<Arc<[backend_compile::SourceDeclaration]>, String> {
        Ok(
            backend_frontend_typescript::syntax_frontend()
                .map_err(|error| error.to_string())?
                .analyze(std::path::Path::new(path), source.as_bytes())
                .map_err(|error| error.to_string())?
                .declarations()
                .clone(),
        )
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
        let other_set_note_id = RowId::Symbol(semantic_symbol(package, fixture_version(6).identity()));
        let pairs = structural_call_coordinate_pairs(&sources, package).map_err(|e| e.to_string())?;
        let relations = structural_call_graph_relations_mapped(
            &view,
            &pairs,
            package,
            sync_id,
            false,
        );
        let targets = relations.iter().map(|relation| relation.to).collect::<Vec<_>>();
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
        let authority = |parentage| EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let items = [
            TreeItemInput {
                name: b"Caller",
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
                name: b"Callee",
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
    fn a_call_occurrence_answers_as_a_reference_with_its_source_span() -> Result<(), String> {
        let bytes = fixture_image()?;
        let package = backend_engine::package_key("fixture");
        let callee_symbol = super::super::super::view_build::semantic_symbol(
            package,
            fixture_version(2).identity(),
        );
        let mut facts = Vec::new();
        append_reference_facts(package, &bytes, callee_symbol, &mut facts)
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
        append_reference_facts(package, &bytes, absent, &mut facts)
            .map_err(|error| error.to_string())?;
        if !facts.is_empty() {
            return Err(format!("absent target produced {} facts", facts.len()));
        }
        Ok(())
    }
}
