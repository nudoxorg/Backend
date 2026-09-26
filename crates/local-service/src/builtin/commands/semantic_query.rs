use super::super::read_indexed_sources;
use super::super::view_build;
use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
use super::snapshot::semantic_link_kind;
use backend_engine::application::{DocumentationSession, LocalCompilerClient};
use backend_engine::builtin::{ProductSemanticPublicationRecord, SemanticPublicationCoverage};
use backend_semantic::ir::{LinkTarget, SemanticReader};
use futures_util::StreamExt as _;
use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread;

mod references;

pub(super) use references::execute_references;

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
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic graph relation: {error}")))?;
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
            let activated = activate_semantic_publication(compiler, key, *claim)?;
            let binding = claim.binding();
            for bytes in activated.images() {
                let image = backend_semantic::ir::SemanticImageView::reopen(bytes.as_ref())
                    .map_err(|error| {
                        BuiltinModelError(format!("reopen semantic graph image: {error}"))
                    })?;
                if let Some(relations) = semantic_graph_relations(
                    &image,
                    package,
                    source_symbol,
                    source_id,
                    include_incoming,
                )? {
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
                    .map_err(|error| {
                        BuiltinModelError(format!("build rich semantic graph: {error}"))
                    })?;
                    snapshot.rich_graph = Some(rich_graph);
                    return Ok(Some(snapshot));
                }
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    Ok(None)
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

fn semantic_graph_relations(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    package: backend_engine::PackageKey,
    symbol: backend_engine::SymbolKey,
    source_id: backend_engine::RowId,
    include_incoming: bool,
) -> Result<Option<Vec<backend_engine::GraphRelation>>, BuiltinModelError> {
    let session = backend_engine::application::DocumentationSession::new(image);
    let source_entity = session
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
        .transpose()?;
    let Some(source_entity) = source_entity else {
        return Ok(None);
    };
    let image_identity = *blake3::hash(image.as_ref()).as_bytes();
    let mut relations = BTreeSet::new();
    for (_, link) in image.links_from(source_entity) {
        let target = semantic_link_row_id(image, &session, package, image_identity, link.target)?;
        relations.insert(backend_engine::GraphRelation::new(
            source_id,
            target,
            semantic_link_kind(link.kind),
        ));
    }
    if include_incoming {
        for source in session.canonical_entities() {
            let source = source.map_err(|error| {
                BuiltinModelError(format!("read semantic graph source: {error}"))
            })?;
            for (_, link) in image.links_from(source.entity.id) {
                if link.target != LinkTarget::Local(source_entity) {
                    continue;
                }
                let from =
                    backend_engine::RowId::Symbol(super::super::view_build::semantic_symbol(
                        package,
                        source.entity.version.identity(),
                    ));
                relations.insert(backend_engine::GraphRelation::new(
                    from,
                    source_id,
                    semantic_link_kind(link.kind),
                ));
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
    Ok(Some(relations.into_iter().collect()))
}

fn semantic_link_row_id(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    session: &DocumentationSession<'_, backend_semantic::ir::SemanticImageView<'_>>,
    package: backend_engine::PackageKey,
    image_identity: [u8; 32],
    target: LinkTarget,
) -> Result<backend_engine::RowId, BuiltinModelError> {
    match target {
        LinkTarget::Local(target) => {
            let target = session.entity(target).map_err(|error| {
                BuiltinModelError(format!("read semantic graph target: {error}"))
            })?;
            Ok(backend_engine::RowId::Symbol(
                super::super::view_build::semantic_symbol(
                    package,
                    target.entity.version.identity(),
                ),
            ))
        }
        LinkTarget::External(target) => {
            let identity = backend_semantic::ir::ExternalTargetIdentity::capture(image, target)
                .map_err(|error| {
                    BuiltinModelError(format!("identify semantic graph target: {error}"))
                })?;
            Ok(backend_engine::RowId::Symbol(
                super::super::view_build::external_semantic_symbol(
                    package,
                    image_identity,
                    identity,
                ),
            ))
        }
    }
}
