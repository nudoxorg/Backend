use super::super::read_indexed_sources;
use super::super::view_build;
use super::super::view_build::{
    ProjectCallableIndex, foreign_namespace_call_retarget, foreign_namespace_field_retarget,
    foreign_package_call_retarget, foreign_package_field_retarget, join_project_field,
    join_project_mention, join_project_value, project_paths_for_package,
};
use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
use super::snapshot::{
    semantic_confidence, semantic_declaration_identity, semantic_link_evidence, semantic_link_kind,
};
use backend_engine::application::{DocumentationSession, LocalCompilerClient};
use backend_engine::builtin::{ProductSemanticPublicationRecord, SemanticPublicationCoverage};
use backend_semantic::ir::{
    DeclarationIdentity, ExternalTarget, ForeignTargetOrigin, LinkKind, LinkTarget,
    SemanticReader,
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

fn published_identities_from_images(
    images: &[&[u8]],
) -> Result<BTreeSet<DeclarationIdentity>, BuiltinModelError> {
    let mut published = BTreeSet::new();
    for bytes in images {
        let image = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
            BuiltinModelError(format!("reopen semantic graph published image: {error}"))
        })?;
        let session = DocumentationSession::new(&image);
        for entity in session.canonical_entities() {
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
                    return Ok(view
                        .row(backend_engine::RowId::Symbol(
                            super::super::view_build::semantic_symbol(package, identity),
                        ))
                        .map(|_| backend_engine::RowId::Symbol(
                            super::super::view_build::semantic_symbol(package, identity),
                        )));
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
                return Ok(view
                    .row(backend_engine::RowId::Symbol(
                        super::super::view_build::semantic_symbol(package, identity),
                    ))
                    .map(|_| backend_engine::RowId::Symbol(
                        super::super::view_build::semantic_symbol(package, identity),
                    )));
            } else if matches!(link_kind, LinkKind::Reads) {
                let identity = if let Some(identity) = foreign_package_field_retarget(
                    image,
                    external,
                    caller_path,
                    project_paths,
                    callable_index,
                )? {
                    Some(identity)
                } else {
                    foreign_namespace_field_retarget(image, external, callable_index)?
                };
                if let Some(identity) = identity {
                    return Ok(view
                        .row(backend_engine::RowId::Symbol(
                            super::super::view_build::semantic_symbol(package, identity),
                        ))
                        .map(|_| backend_engine::RowId::Symbol(
                            super::super::view_build::semantic_symbol(package, identity),
                        )));
                }
                if let Some(identity) = join_project_value(
                    image,
                    link_kind,
                    external,
                    caller_path,
                    project_paths,
                    callable_index,
                    published,
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
    let published = published_identities_from_images(images)?;
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
                    &published,
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
                        &published,
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

fn reference_fact_has_site_and_relation(
    facts: &[backend_engine::ReferenceFact],
    site: backend_engine::SymbolKey,
    relation: backend_engine::SemanticLinkKind,
) -> bool {
    facts
        .iter()
        .any(|fact| fact.site == site && fact.relation == relation)
}

fn project_reference_facts_from_bytes(
    images: &[&[u8]],
    view: &backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    target_symbol: backend_engine::SymbolKey,
    project_paths: &BTreeSet<String>,
    structural_pairs: &[(String, String)],
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    let mut facts = Vec::new();
    for bytes in images {
        append_reference_facts(package, bytes, target_symbol, &mut facts)?;
    }
    let callable_index = ProjectCallableIndex::build_from_bytes(images)?;
    let target_row_id = backend_engine::RowId::Symbol(target_symbol);
    let mut published = BTreeSet::new();
    for bytes in images {
        let image = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
            BuiltinModelError(format!("reopen semantic references published image: {error}"))
        })?;
        let session = DocumentationSession::new(&image);
        for entity in session.canonical_entities() {
            let entity = entity.map_err(|error| {
                BuiltinModelError(format!("read semantic references published entity: {error}"))
            })?;
            published.insert(entity.entity.version.identity());
        }
    }
    for bytes in images {
        let image = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
            BuiltinModelError(format!("reopen semantic references image: {error}"))
        })?;
        let caller_path = view_build::compiled_source_path(&image)?;
        let session = DocumentationSession::new(&image);
        for source in session.canonical_entities() {
            let source = source.map_err(|error| {
                BuiltinModelError(format!("read semantic references caller: {error}"))
            })?;
            let caller_symbol = super::super::view_build::semantic_symbol(
                package,
                source.entity.version.identity(),
            );
            if view
                .row(backend_engine::RowId::Symbol(caller_symbol))
                .is_none()
            {
                continue;
            }
            for (_, link) in image.links_from(source.entity.id) {
                let LinkTarget::External(external) = link.target else {
                    continue;
                };
                let identity = if matches!(link.kind, LinkKind::Calls | LinkKind::MethodCall) {
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
                let retargeted_symbol =
                    super::super::view_build::semantic_symbol(package, identity);
                if retargeted_symbol != target_symbol {
                    continue;
                }
                if view
                    .row(backend_engine::RowId::Symbol(retargeted_symbol))
                    .is_none()
                {
                    continue;
                }
                let relation = semantic_link_kind(link.kind);
                if reference_fact_has_site_and_relation(&facts, caller_symbol, relation) {
                    continue;
                }
                facts.push(backend_engine::ReferenceFact {
                    site: caller_symbol,
                    target: backend_engine::SemanticLinkTarget::Local {
                        declaration: semantic_declaration_identity(identity),
                    },
                    relation,
                    evidence: semantic_link_evidence(&image, link)?,
                });
                if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
                    return Err(BuiltinModelError(
                        "semantic references exceed the bounded result contract".to_owned(),
                    ));
                }
            }
        }
    }
    let target_row = view.row(target_row_id).ok_or_else(|| {
        BuiltinModelError("references target is absent from the selected view".to_owned())
    })?;
    let target_name = target_row
        .label
        .rsplit("::")
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            BuiltinModelError("references target has no declaration name".to_owned())
        })?;
    let target_identity = view_build::structural_symbol_identity(target_symbol);
    for (caller_coordinate, callee_coordinate) in structural_pairs {
        let Some(callee_id) =
            view_build::view_row_for_structural_coordinate(view, package, callee_coordinate)
        else {
            continue;
        };
        if callee_id != target_row_id {
            continue;
        }
        let Some(caller_id) =
            view_build::view_row_for_structural_coordinate(view, package, caller_coordinate)
        else {
            continue;
        };
        let caller_row = view.row(caller_id).ok_or_else(|| {
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
        let (start, end) = caller_row
            .excerpt
            .text()
            .and_then(|excerpt| view_build::structural_call_span(excerpt, target_name))
            .map(|(start, end)| {
                (
                    u32::try_from(start).unwrap_or(u32::MAX),
                    u32::try_from(end).unwrap_or(u32::MAX),
                )
            })
            .unwrap_or((0, target_name.len().min(u32::MAX as usize) as u32));
        let source = match caller_row.source.captured() {
            Some(location) => Some(backend_engine::SemanticSourceSpan {
                file: backend_engine::ProductText::new(location.path()).map_err(|error| {
                    BuiltinModelError(format!("structural references path: {error:?}"))
                })?,
                start,
                end,
            }),
            None => None,
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
    if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
        return Err(BuiltinModelError(
            "semantic references exceed the bounded result contract".to_owned(),
        ));
    }
    Ok(facts)
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
    let mut activations = Vec::new();
    let mut image_slots = Vec::<(usize, usize)>::new();
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
            let activation_index = activations.len();
            activations.push(activated);
            for image_index in 0..activations[activation_index].images().len() {
                image_slots.push((activation_index, image_index));
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
    let sources = read_indexed_sources(&snapshot)?;
    let project_paths = project_paths_for_package(&sources, package);
    let bytes = image_slots
        .iter()
        .map(|(activation_index, image_index)| {
            activations[*activation_index].images()[*image_index].as_ref()
        })
        .collect::<Vec<_>>();
    let pairs = if package_indexed_in_sources(&sources, package) {
        view_build::structural_call_coordinate_pairs(&sources, package)?
    } else {
        Vec::new()
    };
    let facts = project_reference_facts_from_bytes(
        &bytes,
        view,
        package,
        target_symbol,
        &project_paths,
        &pairs,
    )?;
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
    use super::project_reference_facts_from_bytes;
    use super::project_semantic_graph_relations_from_bytes;
    use super::super::snapshot::semantic_declaration_identity;
    use super::super::super::view_build::{
        compiled_source_path, foreign_display_name, join_project_call, join_project_field,
        join_project_mention, join_project_value,
        query_semantic_id, semantic_coordinate, semantic_symbol, ProjectCallableIndex,
        structural_call_coordinate_pairs, structural_call_graph_relations_mapped,
    };
    use backend_extension_trustfall::{
        CompilerSemanticEvidence, PackageScopeEvidence, SemanticQueryCancellation,
        SemanticQueryCorpus, SemanticQueryEvent, SemanticQueryEvidence, SemanticQueryFact,
        SemanticQueryPresentation, SemanticQueryRequest, execute_semantic_query,
    };
    use super::super::super::{IndexedProject, IndexedSources, ProductSourceRecord, initial_view};
    use backend_engine::{GraphRelation, Row, RowId, ViewRoot, package_key, product_source_file_key};
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity,
        EntityAuthorityFacts, EntityVersion, ExternalDeclarationIdentity, ExternalId,
        ExternalTarget, FactAvailability, ForeignDeclarationId, ForeignExternalTarget,
        ForeignTargetOrigin, IrBuilder, ItemKind, LinkKind, LinkTarget,
        OccurrenceAuthorityFacts, ParentageAuthority, SemanticImageView, SourceIdentity,
        SourceSpan, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
        VariantAvailability, VariantFingerprint, Visibility, encode_full_semantic_image,
        full_semantic_image_len, SemanticCoreReader, SemanticReader,
    };
    use futures_util::StreamExt as _;
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
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
                    kind: Some(foreign_call.entity_kind),
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
                ("workout/service/__init__.py", 1, "set_note", fixture_version(1)),
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
        let workout_bytes = project_call_image(
            "workout.py",
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
            Some(dotted_foreign_call_fixture_with_path(b"workout..service", 37)),
        )?;
        let weeks_dot_bytes = project_call_image(
            "weeks.py",
            4,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture_with_path(b".workout.service", 38)),
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
            return Err("notworkout/service.go must not satisfy example.com/gymbro/workout".to_owned());
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
            Some(go_import_foreign_call_fixture_with_package(b"example.com//workout", 66)),
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
        let service_bytes = project_call_image(
            "src/service.rs",
            1,
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
        let service_bytes = project_call_image(
            "demo/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
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
        let service_bytes = project_field_image(
            "src/service.rs",
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
        let ecosystem = builder.intern_atom(import.ecosystem).map_err(|e| e.to_string())?;
        let package_atom = builder
            .intern_atom(import.package_specifier)
            .map_err(|e| e.to_string())?;
        let display = builder.intern_atom(import.display).map_err(|e| e.to_string())?;
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
        let service_bytes = project_call_image(
            "src/service.rs",
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
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, set_note_identity, _) =
            rust_set_note_drive_fixture(79)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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

    #[test]
    fn join_project_call_extra_path_stays_unjoined_and_foreign_display_names_call() -> Result<(), String> {
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
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
            return Err("src/service_extra.rs must not satisfy src/service".to_owned());
        }
        let display = foreign_display_name(&caller_image, external).map_err(|error| error.to_string())?;
        if display.as_deref() != Some("set_note") {
            return Err(format!(
                "foreign call display should be set_note, got {display:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn join_project_call_ambiguous_rust_service_paths_returns_none() -> Result<(), String> {
        let service_bytes = project_call_image(
            "src/service.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let (service_bytes, caller_bytes, set_note_identity, _) =
            rust_set_note_drive_fixture(86)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
                return Err(format!("{link_kind:?} must not join through join_project_mention"));
            }
        }
        Ok(())
    }

    #[test]
    fn join_project_mention_does_not_find_functions() -> Result<(), String> {
        let (service_bytes, caller_bytes, set_note_identity, _) =
            rust_set_note_drive_fixture(92)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
    fn join_project_mention_universe_query_corpus_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, workout_identity, drive_identity) =
            java_universe_workout_drive_fixture(102)?;
        let paths = project_paths(&["WorkoutService.java", "Weeks.java"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let paths = project_paths(&["WorkoutService.java", "OtherWorkoutService.java", "Weeks.java"]);
        let images = [&first_bytes[..], &duplicate_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
            fixture_version(2).identity(),
        ]);
        let field_caller = java_namespace_note_field_image(
            "Weeks.java",
            106,
            107,
            108,
            106,
            b"demo.WorkoutService",
        )?;
        let (field_external, field_link_kind, field_caller_path) =
            foreign_namespace_link_from_caller(&field_caller, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let (call_external, call_link_kind, call_caller_path) =
            foreign_namespace_link_from_caller(&method_call_bytes, TreeEntityId::new(2), LinkKind::MethodCall)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let images = [&service_bytes[..], &duplicate_function_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let service_bytes = project_field_image(
            "workout.service.ts",
            1,
            b"note",
            TreeEntityId::new(0),
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
                path_specifier: Some(b"note"),
                display: b"note",
                foreign_key: 125,
                link_kind: LinkKind::Imports,
                foreign_kind: None,
            },
        )?;
        let paths = project_paths(&["workout.service.ts", "drive.ts"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([fixture_version(1).identity(), fixture_version(2).identity()]);
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(1).identity(),
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
            return Err("src/service_extra.rs must not satisfy src/service".to_owned());
        }
        Ok(())
    }

    #[test]
    fn join_project_field_ambiguous_rust_service_paths_returns_none() -> Result<(), String> {
        let service_bytes = project_field_image(
            "src/service.rs",
            1,
            b"note",
            TreeEntityId::new(0),
            None,
        )?;
        let duplicate_service_bytes = project_field_image(
            "src/service/mod.rs",
            3,
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
            Some(rust_field_read_foreign_fixture(96)),
        )?;
        let paths = project_paths(&["src/service.rs", "src/service/mod.rs", "src/lib.rs"]);
        let images = [
            &service_bytes[..],
            &duplicate_service_bytes[..],
            &caller_bytes[..],
        ];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
                return Err(format!("{link_kind:?} must not join through join_project_field"));
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
    fn join_project_value_query_corpus_function_value_referenced_by_names_drive() -> Result<(), String> {
        let package = package_key("fixture");
        let (service_bytes, caller_bytes, set_note_identity, drive_identity) =
            rust_set_note_value_drive_fixture(104)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let service_bytes = project_call_image(
            "src/service.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let service_bytes = project_call_image(
            "demo/service.go",
            1,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
        let duplicate_service_bytes = project_call_image(
            "demo/other.go",
            3,
            b"SetNote",
            TreeEntityId::new(0),
            None,
        )?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
    fn join_project_value_constant_stays_const_not_function_arm() -> Result<(), String> {
        let (service_bytes, caller_bytes, limit_identity, _) = rust_limit_drive_fixture(106)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let (service_bytes, caller_bytes, set_note_identity, _) =
            rust_set_note_drive_fixture(107)?;
        let paths = project_paths(&["src/service.rs", "src/lib.rs"]);
        let images = [&service_bytes[..], &caller_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let value_index =
            ProjectCallableIndex::build_from_bytes(&value_images).map_err(|error| error.to_string())?;
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
        let service_bytes =
            java_namespace_active_variant_image("Status.java", 110, 111, 112, 110)?;
        let active_identity = fixture_version(111).identity();
        let paths = project_paths(&["Status.java"]);
        let images = [&service_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([active_identity, fixture_version(112).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let service_bytes =
            java_namespace_active_variant_image("Status.java", 113, 114, 115, 113)?;
        let active_identity = fixture_version(114).identity();
        let drive_identity = fixture_version(115).identity();
        let paths = project_paths(&["Status.java"]);
        let images = [&service_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([active_identity, drive_identity]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let first_bytes =
            java_namespace_active_variant_image("Status.java", 116, 117, 118, 116)?;
        let duplicate_bytes =
            java_namespace_active_variant_image("Status.java", 119, 120, 121, 119)?;
        let paths = project_paths(&["Status.java"]);
        let images = [&first_bytes[..], &duplicate_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(117).identity(),
            fixture_version(120).identity(),
            fixture_version(118).identity(),
        ]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&first_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(124).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(126).identity(),
            fixture_version(127).identity(),
        ]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(129).identity(),
            fixture_version(131).identity(),
        ]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&caller_bytes, TreeEntityId::new(0), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([token_identity, fixture_version(134).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(3), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(136).identity(),
            fixture_version(137).identity(),
        ]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(3), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(62).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, drive_identity]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(68).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(3), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([
            fixture_version(70).identity(),
            fixture_version(73).identity(),
            fixture_version(71).identity(),
        ]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&first_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let service_bytes = java_namespace_note_field_image(
            "WorkoutService.java",
            75,
            76,
            77,
            75,
            b"demo.Other",
        )?;
        let note_identity = fixture_version(76).identity();
        let paths = project_paths(&["WorkoutService.java"]);
        let images = [&service_bytes[..]];
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(77).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(81).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([note_identity, fixture_version(85).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(88).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
            return Err("Reads of Function kind must not join through join_project_field".to_owned());
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
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
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&method_call_bytes, TreeEntityId::new(2), LinkKind::MethodCall)?;
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
        let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
        let published = BTreeSet::from([set_note_identity, fixture_version(97).identity()]);
        let (external, link_kind, caller_path) =
            foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(2), LinkKind::Calls)?;
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
        let field_index =
            ProjectCallableIndex::build_from_bytes(&field_images).map_err(|error| error.to_string())?;
        let field_published = BTreeSet::from([note_identity, fixture_version(100).identity()]);
        let (field_external, field_link_kind, field_caller_path) =
            foreign_namespace_link_from_caller(&note_field_bytes, TreeEntityId::new(2), LinkKind::Reads)?;
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
        let service_bytes = project_call_image(
            "src/service.rs",
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
    fn project_call_rust_function_mod_rs_retarget_links_callee_semantic_row() -> Result<(), String> {
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
        let service_bytes = project_call_image(
            "src/service.rs",
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
            return Err(format!("rust function retarget relation is {:?}", fact.relation));
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
        let service_bytes = project_call_image(
            "src/service.rs",
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
    fn project_call_rust_module_app_src_suffix_retarget_links_callee_semantic_row() -> Result<(), String> {
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
        let root_service_bytes = project_call_image(
            "src/service.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
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
                ("include/decl.h", 1, "declared_in_header", fixture_version(1)),
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
    fn project_call_c_header_src_include_suffix_retarget_links_callee_semantic_row() -> Result<(), String> {
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
                ("src/include/decl.h", 1, "declared_in_header", fixture_version(1)),
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
                ("include/decl_extra.h", 1, "declared_in_header", fixture_version(1)),
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
                ("include/decl.h", 1, "declared_in_header", fixture_version(1)),
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
                ("include/decl.h", 1, "declared_in_header", fixture_version(1)),
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
            return Err(format!("dotted foreign retarget relation is {:?}", fact.relation));
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
                ("workout/service/__init__.py", 1, "set_note", fixture_version(1)),
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
        let workout_bytes = project_call_image(
            "workout.py",
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
        let other_bytes = project_call_image(
            "other.py",
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
            Some(dotted_foreign_call_fixture_with_path(b"workout..service", 46)),
        )?;
        let weeks_dot_bytes = project_call_image(
            "weeks.py",
            4,
            b"sync_workout",
            TreeEntityId::new(0),
            Some(dotted_foreign_call_fixture_with_path(b".workout.service", 47)),
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
            return Err(format!("go import retarget relation is {:?}", fact.relation));
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
        let service_bytes = project_call_image(
            "src/service.rs",
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
            return Err(format!("rust module retarget relation is {:?}", fact.relation));
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
        let root_service_bytes = project_call_image(
            "src/service.rs",
            1,
            b"set_note",
            TreeEntityId::new(0),
            None,
        )?;
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
        let sync_site_facts = facts.iter().filter(|fact| fact.site == sync_symbol).count();
        if sync_site_facts != 0 {
            return Err(format!(
                "Reads foreign link produced {sync_site_facts} sync-site facts"
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
        let pairs = structural_call_coordinate_pairs(&sources, package).map_err(|e| e.to_string())?;
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
            return Err(format!("expected one structural reference fact, got {}", facts.len()));
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
        let pairs = vec![
            (
                "fixture::local.ts:1::caller".to_owned(),
                "fixture::local.ts:2::callee".to_owned(),
            ),
        ];
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
    fn project_call_namespace_csharp_chain_retarget_links_callee_semantic_row() -> Result<(), String> {
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
    fn project_call_namespace_java_chain_retarget_links_callee_semantic_row() -> Result<(), String> {
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
        encode_full_semantic_image(&ir, &mut universe_bytes)
            .map_err(|error| error.to_string())?;
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
            &[("Box.cs", 1, "SetNote", fixture_version(42)), ("Weeks.cs", 2, "Sync", fixture_version(43))],
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
            &[("Box.cs", 1, "SetNote", fixture_version(52)), ("Weeks.cs", 2, "Sync", fixture_version(53))],
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
            return Err(format!("namespace retarget relation is {:?}", fact.relation));
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
            return Err(format!("namespace retarget relation is {:?}", fact.relation));
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
