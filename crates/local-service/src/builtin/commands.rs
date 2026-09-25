//! Product command admission and the atomic intent/view/projection pipeline.

use super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
use backend_engine::application::{
    DocumentationSession, LocalCompilerClient, OwnedPackageSource, OwnedPackageSourceSet,
    PackageSemanticError, PackageSemanticRuntimeError,
};
use backend_engine::builtin::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord, SemanticPublicationClaim,
    SemanticPublicationCoverage, SemanticPublicationSelection, SemanticUnavailableReason,
};
use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
};
use backend_semantic::ir::{
    LinkTarget, SemanticReader as _, SemanticSnapshot, SemanticStableLinks, StableLinkKey,
};
use backend_semantic::vocabulary::{Language, LanguageProfile};
use futures_util::StreamExt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread;

struct SemanticQueryJob {
    query: backend_extension_trustfall::SemanticQueryRequest,
    identity: backend_extension_trustfall::SemanticQueryIdentity,
    events: mpsc::SyncSender<Result<backend_extension_trustfall::SemanticQueryEvent, String>>,
}

static SEMANTIC_QUERY_EXECUTOR: OnceLock<Result<mpsc::SyncSender<SemanticQueryJob>, String>> =
    OnceLock::new();

fn semantic_query_executor()
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

fn execute_semantic_graph(
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

fn execute_structural_call_graph(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    query: backend_engine::GraphNeighborhoodQuery,
    include_incoming: bool,
) -> Result<Option<backend_engine::ViewSnapshot>, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    let view = library.view();
    let source_symbol = query.resolve_symbol(view).ok_or_else(|| {
        BuiltinModelError("structural call graph source is absent from the selected view".to_owned())
    })?;
    let source_id = backend_engine::RowId::Symbol(source_symbol);
    let source = view.row(source_id).ok_or_else(|| {
        BuiltinModelError("structural call graph source is absent from the selected view".to_owned())
    })?;
    let Some(package) = source.package else {
        return Ok(None);
    };
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = super::read_indexed_sources(&snapshot)?;
    let relations = super::view_build::structural_call_graph_relations(
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
                if super::view_build::semantic_symbol(
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
                let from = backend_engine::RowId::Symbol(super::view_build::semantic_symbol(
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
                super::view_build::semantic_symbol(package, target.entity.version.identity()),
            ))
        }
        LinkTarget::External(target) => {
            let identity = backend_semantic::ir::ExternalTargetIdentity::capture(image, target)
                .map_err(|error| {
                    BuiltinModelError(format!("identify semantic graph target: {error}"))
                })?;
            Ok(backend_engine::RowId::Symbol(
                super::view_build::external_semantic_symbol(package, image_identity, identity),
            ))
        }
    }
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
fn execute_references(
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
    let sources = super::read_indexed_sources(&snapshot)?;
    let mut facts =
        super::view_build::structural_reference_facts(view, &sources, target.as_str())?;
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
                if super::view_build::semantic_symbol(
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
                site: super::view_build::semantic_symbol(package, site),
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

#[derive(Clone, Copy)]
struct SemanticDeclaration<'view> {
    label: &'view str,
    version: backend_semantic::ir::EntityVersion,
    parent: Option<backend_semantic::ir::DeclarationIdentity>,
}

struct SemanticPackageSnapshot<'view> {
    declarations: Vec<(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'view>,
    )>,
    links: Vec<SemanticLinkSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticLinkSummary {
    key: StableLinkKey,
    evidence: backend_engine::SemanticLinkEvidence,
}

const MAX_DIFF_DECLARATIONS: usize = (super::MAX_REBUILD_BYTES / 4)
    / size_of::<(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'static>,
    )>();
const MAX_DIFF_LINKS: usize = (super::MAX_REBUILD_BYTES / 4) / size_of::<SemanticLinkSummary>();

fn semantic_package_snapshot<'view>(
    daemon: &'view crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    package: &backend_engine::PackageReference,
) -> Result<Option<SemanticPackageSnapshot<'view>>, BuiltinModelError> {
    let view = daemon.engine().daemon().library().view();
    let package_key = backend_engine::package_key(package.as_str());
    let package = view
        .row_ref(backend_engine::RowId::Package(package_key))
        .filter(|row| row.label == package.as_str())
        .map(|_| package_key)
        .ok_or_else(|| BuiltinModelError("diff package is not indexed".to_owned()))?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic diff relation: {error}")))?;
    let mut found_publication = false;
    let mut declarations = Vec::new();
    let mut links = Vec::new();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("page semantic diff relation: {error}")))?;
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
            found_publication = true;
            let activated = activate_semantic_publication(compiler, key, *claim)?;
            for bytes in activated.images() {
                append_semantic_image(
                    view,
                    package,
                    bytes.as_ref(),
                    &mut declarations,
                    &mut links,
                )?;
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    finish_semantic_snapshot(found_publication, declarations, links)
}

fn append_semantic_image<'view>(
    view: &'view backend_engine::ViewRoot,
    package: backend_engine::PackageKey,
    bytes: &[u8],
    declarations: &mut Vec<(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'view>,
    )>,
    links: &mut Vec<SemanticLinkSummary>,
) -> Result<(), BuiltinModelError> {
    let image = backend_semantic::ir::SemanticImageView::reopen(bytes)
        .map_err(|error| BuiltinModelError(format!("reopen semantic diff image: {error}")))?;
    for entity in image.canonical_entities() {
        let identity = entity.version.identity();
        let symbol = super::view_build::semantic_symbol(package, identity);
        let row = view
            .row_ref(backend_engine::RowId::Symbol(symbol))
            .filter(|row| row.package == Some(package))
            .ok_or_else(|| {
                BuiltinModelError(
                    "semantic diff declaration is absent from its activated product view"
                        .to_owned(),
                )
            })?;
        let parent = entity
            .parent
            .and_then(|parent| image.entity(parent))
            .map(|parent| parent.version.identity());
        if declarations.len() == MAX_DIFF_DECLARATIONS {
            return Err(BuiltinModelError(
                "semantic diff declaration index exceeds its memory budget".to_owned(),
            ));
        }
        declarations.try_reserve(1).map_err(|error| {
            BuiltinModelError(format!("reserve semantic diff declaration: {error}"))
        })?;
        declarations.push((
            identity,
            SemanticDeclaration {
                label: &row.label,
                version: entity.version,
                parent,
            },
        ));
    }
    let snapshot = SemanticSnapshot {
        generation: backend_semantic::ir::GenerationId::from_canonical_bytes(bytes),
        reader: &image,
    };
    for link in SemanticStableLinks::new(snapshot) {
        if links.len() == MAX_DIFF_LINKS {
            return Err(BuiltinModelError(
                "semantic diff graph index exceeds its memory budget".to_owned(),
            ));
        }
        links.try_reserve(1).map_err(|error| {
            BuiltinModelError(format!("reserve semantic diff graph relation: {error}"))
        })?;
        links.push(SemanticLinkSummary {
            key: link.key,
            evidence: semantic_link_evidence(&image, link.evidence)?,
        });
    }
    Ok(())
}

fn finish_semantic_snapshot(
    found_publication: bool,
    mut declarations: Vec<(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )>,
    mut links: Vec<SemanticLinkSummary>,
) -> Result<Option<SemanticPackageSnapshot<'_>>, BuiltinModelError> {
    declarations.sort_unstable_by_key(|(identity, _)| *identity);
    if declarations.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(BuiltinModelError(
            "semantic diff publication contains a duplicate declaration identity".to_owned(),
        ));
    }
    links.sort_unstable_by_key(|link| link.key);
    if links.windows(2).any(|pair| pair[0].key == pair[1].key) {
        return Err(BuiltinModelError(
            "semantic diff publication contains a duplicate graph relation".to_owned(),
        ));
    }
    Ok(found_publication.then_some(SemanticPackageSnapshot {
        declarations,
        links,
    }))
}

fn semantic_link_evidence<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
    reader: &Reader,
    link: backend_semantic::ir::Link,
) -> Result<backend_engine::SemanticLinkEvidence, BuiltinModelError> {
    let source = link
        .source
        .map(|source| {
            let file = reader.atom(source.file()).ok_or_else(|| {
                BuiltinModelError("semantic link source path is absent from its image".to_owned())
            })?;
            let file = std::str::from_utf8(file).map_err(|_| {
                BuiltinModelError("semantic link source path is not UTF-8".to_owned())
            })?;
            Ok(backend_engine::SemanticSourceSpan {
                file: backend_engine::ProductText::new(file)
                    .map_err(|error| BuiltinModelError(error.to_string()))?,
                start: source.start(),
                end: source.end(),
            })
        })
        .transpose()?;
    Ok(backend_engine::SemanticLinkEvidence {
        confidence: semantic_confidence(link.confidence),
        source,
    })
}

const fn semantic_confidence(
    confidence: backend_semantic::ir::Confidence,
) -> backend_engine::SemanticConfidence {
    match confidence {
        backend_semantic::ir::Confidence::Syntactic => {
            backend_engine::SemanticConfidence::Syntactic
        }
        backend_semantic::ir::Confidence::Heuristic => {
            backend_engine::SemanticConfidence::Heuristic
        }
        backend_semantic::ir::Confidence::Indexed => backend_engine::SemanticConfidence::Indexed,
        backend_semantic::ir::Confidence::Imported => backend_engine::SemanticConfidence::Imported,
        backend_semantic::ir::Confidence::Compiler => backend_engine::SemanticConfidence::Compiler,
    }
}

const fn semantic_declaration_identity(
    identity: backend_semantic::ir::DeclarationIdentity,
) -> backend_engine::SemanticDeclarationIdentity {
    backend_engine::SemanticDeclarationIdentity {
        family: *identity.family.as_bytes(),
        variant: *identity.variant.as_bytes(),
    }
}

const fn semantic_link_kind(
    kind: backend_semantic::ir::LinkKind,
) -> backend_engine::SemanticLinkKind {
    match kind {
        backend_semantic::ir::LinkKind::Calls => backend_engine::SemanticLinkKind::Calls,
        backend_semantic::ir::LinkKind::MethodCall => backend_engine::SemanticLinkKind::MethodCall,
        backend_semantic::ir::LinkKind::TypeReference => {
            backend_engine::SemanticLinkKind::TypeReference
        }
        backend_semantic::ir::LinkKind::Reads => backend_engine::SemanticLinkKind::Reads,
        backend_semantic::ir::LinkKind::Writes => backend_engine::SemanticLinkKind::Writes,
        backend_semantic::ir::LinkKind::Imports => backend_engine::SemanticLinkKind::Imports,
        backend_semantic::ir::LinkKind::Implements => backend_engine::SemanticLinkKind::Implements,
        backend_semantic::ir::LinkKind::Overrides => backend_engine::SemanticLinkKind::Overrides,
        backend_semantic::ir::LinkKind::Reexports => backend_engine::SemanticLinkKind::Reexports,
        backend_semantic::ir::LinkKind::Inherits => backend_engine::SemanticLinkKind::Inherits,
        backend_semantic::ir::LinkKind::Documents => backend_engine::SemanticLinkKind::Documents,
    }
}

fn semantic_link_target(
    target: backend_semantic::ir::DeclarationLinkTarget,
) -> backend_engine::SemanticLinkTarget {
    match target {
        backend_semantic::ir::DeclarationLinkTarget::Local(declaration) => {
            backend_engine::SemanticLinkTarget::Local {
                declaration: semantic_declaration_identity(declaration),
            }
        }
        backend_semantic::ir::DeclarationLinkTarget::Stable(target) => {
            backend_engine::SemanticLinkTarget::Stable {
                fragment: *target.fragment.as_ref(),
                declaration: semantic_declaration_identity(target.declaration),
            }
        }
        backend_semantic::ir::DeclarationLinkTarget::Foreign(target) => {
            backend_engine::SemanticLinkTarget::Foreign {
                declaration: *target.foreign.as_bytes(),
                variant: match target.variant {
                    backend_semantic::ir::VariantAvailability::Known(variant) => {
                        Some(*variant.as_bytes())
                    }
                    backend_semantic::ir::VariantAvailability::Unavailable => None,
                },
            }
        }
        backend_semantic::ir::DeclarationLinkTarget::FragmentEntity(target) => {
            backend_engine::SemanticLinkTarget::FragmentEntity {
                fragment: *target.fragment.as_ref(),
                ordinal: target.ordinal,
            }
        }
    }
}

fn execute_semantic_diff(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    from: &backend_engine::PackageReference,
    to: &backend_engine::PackageReference,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let before = semantic_package_snapshot(daemon, compiler, from)?;
    let after = semantic_package_snapshot(daemon, compiler, to)?;
    if let (Some(before), Some(after)) = (before, after) {
        return diff_semantic_snapshots(&before, &after);
    }
    structural_package_diff(daemon, from, to)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StructuralDeclarationSummary {
    identity: backend_semantic::ir::DeclarationIdentity,
    fingerprint: [u8; 32],
}

fn structural_declaration_name(label: &str) -> Option<&str> {
    label
        .rsplit("::")
        .next()
        .filter(|name| !name.is_empty())
}

fn structural_declaration_fingerprint(row: &backend_engine::Row) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    if let Some(signature) = &row.signature {
        hasher.update(signature.as_bytes());
    }
    for fragment in row.document.iter() {
        match fragment {
            backend_library::Fragment::Text(text) => {
                hasher.update(text.as_bytes());
            }
            backend_library::Fragment::Code(text) => {
                hasher.update(text.as_bytes());
            }
            backend_library::Fragment::Link { label, .. } => {
                hasher.update(label.as_bytes());
            }
            backend_library::Fragment::Break => {
                hasher.update(b"\n");
            }
        }
    }
    *hasher.finalize().as_bytes()
}

fn structural_declaration_identity(
    name: &str,
    fingerprint: [u8; 32],
) -> backend_semantic::ir::DeclarationIdentity {
    let family = blake3::hash(name.as_bytes());
    backend_semantic::ir::DeclarationIdentity {
        family: backend_semantic::ir::DeclarationFamilyId::from_raw({
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&family.as_bytes()[..16]);
            bytes
        }),
        variant: backend_semantic::ir::VariantFingerprint::from_raw({
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&fingerprint[..16]);
            bytes
        }),
    }
}

fn structural_package_declarations(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: &backend_engine::PackageReference,
) -> Result<BTreeMap<String, StructuralDeclarationSummary>, BuiltinModelError> {
    let view = daemon.engine().daemon().library().view();
    let package_key = backend_engine::package_key(package.as_str());
    if view
        .row_ref(backend_engine::RowId::Package(package_key))
        .filter(|row| row.label == package.as_str())
        .is_none()
    {
        return Err(BuiltinModelError("diff package is not indexed".to_owned()));
    }
    let mut declarations = BTreeMap::new();
    let mut cursor = backend_engine::ViewPageCursor::first(view);
    loop {
        let page = view
            .page(cursor, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| {
                BuiltinModelError(format!("page structural diff view: {error:?}"))
            })?;
        for row in page.rows() {
            if row.package != Some(package_key) || row.kind.is_none() {
                continue;
            }
            let name = structural_declaration_name(&row.label)
                .ok_or_else(|| {
                    BuiltinModelError(
                        "structural diff declaration coordinate omitted a name".to_owned(),
                    )
                })?
                .to_owned();
            let fingerprint = structural_declaration_fingerprint(row);
            let summary = StructuralDeclarationSummary {
                identity: structural_declaration_identity(&name, fingerprint),
                fingerprint,
            };
            if declarations.insert(name, summary).is_some() {
                return Err(BuiltinModelError(
                    "structural diff package contains a duplicate declaration name".to_owned(),
                ));
            }
        }
        let Some(next) = page.next() else {
            break;
        };
        cursor = next;
    }
    Ok(declarations)
}

fn structural_package_diff(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    from: &backend_engine::PackageReference,
    to: &backend_engine::PackageReference,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let before = structural_package_declarations(daemon, from)?;
    let after = structural_package_declarations(daemon, to)?;
    let mut rows = Vec::new();
    let mut remaining = before;
    for (name, after_summary) in after {
        match remaining.remove(&name) {
            None => push_diff(
                &mut rows,
                &name,
                backend_engine::DeclarationChange::Added,
                None,
                Some(after_summary.identity),
            )?,
            Some(before_summary)
                if before_summary.fingerprint != after_summary.fingerprint =>
            {
                push_diff(
                    &mut rows,
                    &name,
                    backend_engine::DeclarationChange::Changed,
                    Some(before_summary.identity),
                    Some(after_summary.identity),
                )?;
            }
            Some(_) => {}
        }
    }
    for (name, before_summary) in remaining {
        push_diff(
            &mut rows,
            &name,
            backend_engine::DeclarationChange::Removed,
            Some(before_summary.identity),
            None,
        )?;
    }
    rows.sort_by(|left, right| left.label.as_str().cmp(right.label.as_str()));
    Ok(rows.into_boxed_slice())
}

fn diff_semantic_snapshots(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
) -> Result<Box<[backend_engine::DiffRecord]>, BuiltinModelError> {
    let mut rows = Vec::new();
    let mut before_at = 0;
    let mut after_at = 0;
    while before_at < before.declarations.len() || after_at < after.declarations.len() {
        let family = match (
            before.declarations.get(before_at),
            after.declarations.get(after_at),
        ) {
            (Some((left, _)), Some((right, _))) => left.family.min(right.family),
            (Some((left, _)), None) => left.family,
            (None, Some((right, _))) => right.family,
            (None, None) => break,
        };
        let before_end = family_end(&before.declarations, before_at, family);
        let after_end = family_end(&after.declarations, after_at, family);
        diff_semantic_family(
            &before.declarations[before_at..before_end],
            &after.declarations[after_at..after_end],
            &mut rows,
        )?;
        before_at = before_end;
        after_at = after_end;
    }
    diff_semantic_links(before, after, &mut rows)?;
    rows.sort_by(|left, right| {
        left.label.cmp(&right.label).then_with(|| {
            declaration_change_order(left.change).cmp(&declaration_change_order(right.change))
        })
    });
    Ok(rows.into_boxed_slice())
}

fn family_end(
    declarations: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
    start: usize,
    family: backend_semantic::ir::DeclarationFamilyId,
) -> usize {
    start + declarations[start..].partition_point(|(identity, _)| identity.family == family)
}

fn diff_semantic_family(
    before: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
    after: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
    rows: &mut Vec<backend_engine::DiffRecord>,
) -> Result<(), BuiltinModelError> {
    if let ([(before_id, before)], [(after_id, after)]) = (before, after) {
        if before_id != after_id
            || before.version.core_payload != after.version.core_payload
            || before.parent != after.parent
        {
            push_diff(
                rows,
                after.label,
                backend_engine::DeclarationChange::Changed,
                Some(*before_id),
                Some(*after_id),
            )?;
        }
        return Ok(());
    }

    let (_, added) = unmatched_counts(before, after);
    let mut left = 0;
    let mut right = 0;
    while left < before.len() || right < after.len() {
        match (before.get(left), after.get(right)) {
            (Some((before_id, before)), Some((after_id, after))) => match before_id.cmp(after_id) {
                std::cmp::Ordering::Less => {
                    let change = if added == 0 {
                        backend_engine::DeclarationChange::Removed
                    } else {
                        backend_engine::DeclarationChange::Indeterminate
                    };
                    push_diff(rows, before.label, change, Some(*before_id), None)?;
                    left += 1;
                }
                std::cmp::Ordering::Greater => {
                    push_diff(
                        rows,
                        after.label,
                        backend_engine::DeclarationChange::Added,
                        None,
                        Some(*after_id),
                    )?;
                    right += 1;
                }
                std::cmp::Ordering::Equal => {
                    if before.version.core_payload != after.version.core_payload
                        || before.parent != after.parent
                    {
                        push_diff(
                            rows,
                            after.label,
                            backend_engine::DeclarationChange::Changed,
                            Some(*before_id),
                            Some(*after_id),
                        )?;
                    }
                    left += 1;
                    right += 1;
                }
            },
            (Some((before_id, before)), None) => {
                push_diff(
                    rows,
                    before.label,
                    backend_engine::DeclarationChange::Removed,
                    Some(*before_id),
                    None,
                )?;
                left += 1;
            }
            (None, Some((after_id, after))) => {
                push_diff(
                    rows,
                    after.label,
                    backend_engine::DeclarationChange::Added,
                    None,
                    Some(*after_id),
                )?;
                right += 1;
            }
            (None, None) => break,
        }
    }
    Ok(())
}

fn unmatched_counts(
    before: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
    after: &[(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'_>,
    )],
) -> (usize, usize) {
    let mut left = 0;
    let mut right = 0;
    let mut removed = 0;
    let mut added = 0;
    while left < before.len() && right < after.len() {
        match before[left].0.cmp(&after[right].0) {
            std::cmp::Ordering::Less => {
                removed += 1;
                left += 1;
            }
            std::cmp::Ordering::Greater => {
                added += 1;
                right += 1;
            }
            std::cmp::Ordering::Equal => {
                left += 1;
                right += 1;
            }
        }
    }
    (removed + before.len() - left, added + after.len() - right)
}

type SemanticLinkDeltas =
    BTreeMap<backend_semantic::ir::DeclarationIdentity, Vec<backend_engine::SemanticLinkDelta>>;

fn diff_semantic_links(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
    rows: &mut Vec<backend_engine::DiffRecord>,
) -> Result<(), BuiltinModelError> {
    let (by_source, link_count) = collect_semantic_link_deltas(before, after, rows.len())?;
    attach_semantic_link_deltas(before, after, rows, by_source, link_count)
}

fn collect_semantic_link_deltas(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
    declaration_rows: usize,
) -> Result<(SemanticLinkDeltas, usize), BuiltinModelError> {
    let mut by_source = SemanticLinkDeltas::new();
    let mut left = 0;
    let mut right = 0;
    let mut total = 0_usize;
    while left < before.links.len() || right < after.links.len() {
        let (older, newer) = match (before.links.get(left), after.links.get(right)) {
            (Some(older), Some(newer)) => match older.key.cmp(&newer.key) {
                std::cmp::Ordering::Less => {
                    left += 1;
                    (Some(older), None)
                }
                std::cmp::Ordering::Greater => {
                    right += 1;
                    (None, Some(newer))
                }
                std::cmp::Ordering::Equal => {
                    left += 1;
                    right += 1;
                    if older.evidence == newer.evidence {
                        continue;
                    }
                    (Some(older), Some(newer))
                }
            },
            (Some(older), None) => {
                left += 1;
                (Some(older), None)
            }
            (None, Some(newer)) => {
                right += 1;
                (None, Some(newer))
            }
            (None, None) => break,
        };
        total = total.checked_add(1).ok_or_else(|| {
            BuiltinModelError("semantic graph diff result count overflowed".to_owned())
        })?;
        ensure_semantic_diff_bound(declaration_rows, total)?;
        let (source, delta) = semantic_link_delta(older, newer)?;
        by_source.entry(source).or_default().push(delta);
    }
    Ok((by_source, total))
}

fn semantic_link_delta(
    older: Option<&SemanticLinkSummary>,
    newer: Option<&SemanticLinkSummary>,
) -> Result<
    (
        backend_semantic::ir::DeclarationIdentity,
        backend_engine::SemanticLinkDelta,
    ),
    BuiltinModelError,
> {
    let summary = newer.or(older).ok_or_else(|| {
        BuiltinModelError("semantic graph diff lost both relation sides".to_owned())
    })?;
    let from = semantic_declaration_identity(summary.key.from);
    let target = semantic_link_target(summary.key.target);
    let relation = semantic_link_kind(summary.key.kind);
    let delta = match (older, newer) {
        (Some(older), None) => backend_engine::SemanticLinkDelta::Removed {
            from,
            target,
            relation,
            evidence: older.evidence.clone(),
        },
        (None, Some(newer)) => backend_engine::SemanticLinkDelta::Added {
            from,
            target,
            relation,
            evidence: newer.evidence.clone(),
        },
        (Some(older), Some(newer)) => backend_engine::SemanticLinkDelta::EvidenceChanged {
            from,
            target,
            relation,
            before: older.evidence.clone(),
            after: newer.evidence.clone(),
        },
        (None, None) => {
            return Err(BuiltinModelError(
                "semantic graph diff lost both relation sides".to_owned(),
            ));
        }
    };
    Ok((summary.key.from, delta))
}

fn attach_semantic_link_deltas(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
    rows: &mut Vec<backend_engine::DiffRecord>,
    by_source: SemanticLinkDeltas,
    link_count: usize,
) -> Result<(), BuiltinModelError> {
    for (source, links) in by_source {
        let index = semantic_diff_row(before, after, rows, source, link_count)?;
        let row = rows.get_mut(index).ok_or_else(|| {
            BuiltinModelError("semantic graph diff row index is invalid".to_owned())
        })?;
        let mut combined = Vec::from(std::mem::take(&mut row.links));
        combined.extend(links);
        row.links = combined.into_boxed_slice();
    }
    Ok(())
}

fn semantic_diff_row(
    before: &SemanticPackageSnapshot<'_>,
    after: &SemanticPackageSnapshot<'_>,
    rows: &mut Vec<backend_engine::DiffRecord>,
    source: backend_semantic::ir::DeclarationIdentity,
    link_count: usize,
) -> Result<usize, BuiltinModelError> {
    let identity = semantic_declaration_identity(source);
    if let Some(index) = rows
        .iter()
        .position(|row| row.before == Some(identity) || row.after == Some(identity))
    {
        return Ok(index);
    }
    let older = declaration(before, source);
    let newer = declaration(after, source);
    let label = newer.or(older).ok_or_else(|| {
        BuiltinModelError(
            "semantic graph relation source is absent from both package snapshots".to_owned(),
        )
    })?;
    push_diff(
        rows,
        label.label,
        backend_engine::DeclarationChange::Changed,
        older.map(|_| source),
        newer.map(|_| source),
    )?;
    ensure_semantic_diff_bound(rows.len(), link_count)?;
    Ok(rows.len() - 1)
}

fn ensure_semantic_diff_bound(
    declaration_rows: usize,
    link_count: usize,
) -> Result<(), BuiltinModelError> {
    if declaration_rows
        .checked_add(link_count)
        .is_none_or(|facts| facts > backend_engine::MAX_PRODUCT_ROWS)
    {
        return Err(BuiltinModelError(
            "semantic declaration and graph diff exceeds the bounded result contract".to_owned(),
        ));
    }
    Ok(())
}

fn declaration<'snapshot, 'view>(
    snapshot: &'snapshot SemanticPackageSnapshot<'view>,
    identity: backend_semantic::ir::DeclarationIdentity,
) -> Option<&'snapshot SemanticDeclaration<'view>> {
    snapshot
        .declarations
        .binary_search_by_key(&identity, |(identity, _)| *identity)
        .ok()
        .map(|index| &snapshot.declarations[index].1)
}

fn push_diff(
    rows: &mut Vec<backend_engine::DiffRecord>,
    label: &str,
    change: backend_engine::DeclarationChange,
    before: Option<backend_semantic::ir::DeclarationIdentity>,
    after: Option<backend_semantic::ir::DeclarationIdentity>,
) -> Result<(), BuiltinModelError> {
    if rows.len() == backend_engine::MAX_PRODUCT_ROWS {
        return Err(BuiltinModelError(
            "semantic diff exceeds the bounded result contract".to_owned(),
        ));
    }
    rows.push(backend_engine::DiffRecord {
        label: backend_engine::ProductText::new(label)
            .map_err(|error| BuiltinModelError(error.to_string()))?,
        change,
        before: before.map(semantic_declaration_identity),
        after: after.map(semantic_declaration_identity),
        links: Box::new([]),
    });
    Ok(())
}

const fn declaration_change_order(change: backend_engine::DeclarationChange) -> u8 {
    match change {
        backend_engine::DeclarationChange::Added => 0,
        backend_engine::DeclarationChange::Removed => 1,
        backend_engine::DeclarationChange::Changed => 2,
        backend_engine::DeclarationChange::Indeterminate => 3,
    }
}

fn execute_graph_query(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    request: &backend_engine::GraphQueryRequest,
) -> Result<backend_engine::GraphQueryPage, BuiltinModelError> {
    let owner_cursor = daemon.engine().daemon().library().cursor();
    let owner_view = daemon.engine().daemon().library().view().clone();
    let start = request
        .start_offset(owner_cursor)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    if request.control() == backend_engine::GraphQueryControl::Cancel {
        return Ok(backend_engine::GraphQueryPage {
            revision: request.page().basis(),
            source: owner_view.basis().object,
            rows: Box::new([]),
            terminal: backend_engine::PageTerminal::Cancelled,
        });
    }
    let variables = request
        .variables()
        .iter()
        .map(|(name, value)| graph_value_to_trustfall(value).map(|value| (name.clone(), value)))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = super::read_indexed_sources(&snapshot)?;
    let corpus = super::view_build::semantic_query_corpus(&snapshot, compiler, &sources)?;
    let (cancellation, control) = backend_extension_trustfall::SemanticQueryCancellation::new();
    let query = backend_extension_trustfall::SemanticQueryRequest::admit_page(
        corpus,
        request.query().to_owned(),
        variables,
        start,
        usize::from(request.page().limit().get()),
        cancellation,
    )
    .map_err(|error| BuiltinModelError(error.to_string()))?;
    let admitted_identity = query.identity().clone();
    let mut rows = Vec::new();
    let mut terminal = None;
    // Trustfall owns a genuinely asynchronous, lazy stream. Run that stream on
    // one reusable local executor and cross back into locald's synchronous
    // single-owner command loop through a one-event rendezvous. The capacity of
    // one is deliberate: the producer cannot evaluate an unbounded result set
    // ahead of the protocol page consumer, and dropping the receiver publishes
    // cancellation before the asynchronous producer can do more work.
    (|| -> Result<(), BuiltinModelError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        semantic_query_executor()?
            .send(SemanticQueryJob {
                query,
                identity: admitted_identity,
                events: sender,
            })
            .map_err(|_| BuiltinModelError("semantic query executor stopped".to_owned()))?;
        let consumed = (|| {
            while let Ok(event) = receiver.recv() {
                match event.map_err(BuiltinModelError)? {
                    backend_extension_trustfall::SemanticQueryEvent::Row(row)
                        if terminal.is_none() =>
                    {
                        rows.push(graph_row_from_trustfall(row.into_row())?);
                    }
                    backend_extension_trustfall::SemanticQueryEvent::Terminal(value)
                        if terminal.is_none() =>
                    {
                        terminal = Some(value);
                    }
                    _ => {
                        return Err(BuiltinModelError(
                            "structured graph query emitted events after its terminal".to_owned(),
                        ));
                    }
                }
            }
            Ok(())
        })();
        if consumed.is_err() {
            control.cancel();
        }
        drop(receiver);
        consumed
    })()?;
    let terminal = terminal.ok_or_else(|| {
        BuiltinModelError("structured graph query omitted its terminal".to_owned())
    })?;
    if terminal.rows() != rows.len() {
        return Err(BuiltinModelError(
            "structured graph query terminal row count mismatch".to_owned(),
        ));
    }
    let terminal = if terminal.is_complete() {
        backend_engine::PageTerminal::Complete
    } else if terminal.is_cancelled() {
        backend_engine::PageTerminal::Cancelled
    } else if terminal.is_limit_reached() {
        backend_engine::PageTerminal::More(
            request
                .next_continuation(owner_cursor, start.saturating_add(rows.len()))
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        )
    } else {
        return Err(BuiltinModelError(
            "structured graph query returned an unknown terminal".to_owned(),
        ));
    };
    Ok(backend_engine::GraphQueryPage {
        revision: request.page().basis(),
        source: owner_view.basis().object,
        rows: rows.into_boxed_slice(),
        terminal,
    })
}

fn graph_value_to_trustfall(
    value: &backend_engine::GraphValue,
) -> Result<backend_extension_trustfall::FieldValue, BuiltinModelError> {
    Ok(match value {
        backend_engine::GraphValue::Null => backend_extension_trustfall::FieldValue::Null,
        backend_engine::GraphValue::Boolean(value) => (*value).into(),
        backend_engine::GraphValue::Signed(value) => {
            backend_extension_trustfall::FieldValue::Int64(*value)
        }
        backend_engine::GraphValue::Unsigned(value) => {
            backend_extension_trustfall::FieldValue::Uint64(*value)
        }
        backend_engine::GraphValue::Float(bits) => {
            backend_extension_trustfall::FieldValue::Float64(f64::from_bits(*bits))
        }
        backend_engine::GraphValue::String(value) => value.as_str().into(),
        backend_engine::GraphValue::List(values) => backend_extension_trustfall::FieldValue::List(
            values
                .iter()
                .map(graph_value_to_trustfall)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
    })
}

fn graph_row_from_trustfall(
    row: backend_extension_trustfall::SemanticQueryRow,
) -> Result<backend_engine::GraphQueryRow, BuiltinModelError> {
    let fields = row
        .into_iter()
        .map(|(name, value)| {
            graph_value_from_trustfall(value).map(|value| (name.to_string(), value))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    backend_engine::GraphQueryRow::new(fields).map_err(|error| BuiltinModelError(error.to_string()))
}

fn graph_value_from_trustfall(
    value: backend_extension_trustfall::FieldValue,
) -> Result<backend_engine::GraphValue, BuiltinModelError> {
    Ok(match value {
        backend_extension_trustfall::FieldValue::Null => backend_engine::GraphValue::Null,
        backend_extension_trustfall::FieldValue::Boolean(value) => {
            backend_engine::GraphValue::Boolean(value)
        }
        backend_extension_trustfall::FieldValue::Int64(value) => {
            backend_engine::GraphValue::Signed(value)
        }
        backend_extension_trustfall::FieldValue::Uint64(value) => {
            backend_engine::GraphValue::Unsigned(value)
        }
        backend_extension_trustfall::FieldValue::Float64(value) => {
            backend_engine::GraphValue::finite_float(value)
                .map_err(|error| BuiltinModelError(error.to_string()))?
        }
        backend_extension_trustfall::FieldValue::String(value)
        | backend_extension_trustfall::FieldValue::Enum(value) => {
            backend_engine::GraphValue::String(value.to_string())
        }
        backend_extension_trustfall::FieldValue::List(values) => backend_engine::GraphValue::List(
            values
                .iter()
                .cloned()
                .map(graph_value_from_trustfall)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        ),
        _ => {
            return Err(BuiltinModelError(
                "structured graph query returned an unsupported value".to_owned(),
            ));
        }
    })
}

fn execute_search(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    snapshots: &mut super::query::SearchSnapshotOwner,
    remote_semantic: &mut super::query::RemoteSemantic,
    query: &backend_engine::Query,
) -> Result<CommandReply, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let coverage = super::admitted_coverage()?;
    let sources = super::read_indexed_sources(&snapshot)?;
    let semantic_evidence =
        super::view_build::semantic_query_corpus(&snapshot, compiler, &sources)?;
    let coordinator = snapshots
        .select(
            snapshot.root(),
            daemon.engine().daemon().library().view().clone(),
            coverage,
            semantic_evidence,
        )
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let local_query =
        super::query::LocalQuery::prefix(query.text(), usize::from(query.limit().get()))
            .map_err(|error| BuiltinModelError(error.to_string()))?;
    let local = coordinator
        .search_local(local_query)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let _ = remote_semantic.reconcile(coordinator, coverage);
    let result = remote_semantic.search(coordinator, coverage, local, query.text());
    let ranked_ids = result
        .rows
        .iter()
        .map(|ranked| ranked.row.id)
        .collect::<Vec<_>>();
    daemon
        .engine()
        .daemon()
        .library()
        .search_from_ranked_ids(query, &ranked_ids)
        .map(CommandReply::Search)
        .map_err(|error| BuiltinModelError(error.to_string()))
}

fn execute_certified_graph_query(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    request: &backend_engine::GraphQueryRequest,
    base: Option<WireCertificate>,
) -> Result<(CommandReply, Option<WireCertificate>), BuiltinModelError> {
    let command = Command::GraphQuery(request.clone());
    let reply = execute_graph_query(daemon, compiler, request).map_or_else(
        |error| {
            CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                error.to_string(),
            ))
        },
        CommandReply::GraphQueryPage,
    );
    let certificate = projection::reply_certificate(
        &command,
        &reply,
        daemon.engine().daemon().library().view(),
        daemon.engine().daemon().library().cursor(),
        base,
    )?;
    Ok((reply, certificate))
}

fn index_project_intent(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    compiler: &LocalCompilerClient,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    index_project_intent_at(
        daemon,
        package,
        label,
        Path::new(label),
        None,
        request_id,
        compiler,
    )
}

fn index_project_intent_at(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    source_root: &Path,
    coordinate: Option<&PackageUrl>,
    request_id: u64,
    compiler: &LocalCompilerClient,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let project_key = package.to_bytes();
    let before = relation
        .lookup(&project_key)
        .map_err(|error| BuiltinModelError(format!("read indexed project: {error}")))?;
    let old_files = match before.as_ref() {
        Some(record) => record
            .project_fields()
            .map(|fields| fields.files.to_vec())
            .ok_or_else(|| {
                BuiltinModelError("project key contains a source file record".to_owned())
            })?,
        None => Vec::new(),
    };
    let mut reusable = BTreeMap::new();
    for key in &old_files {
        let record = relation
            .lookup(key)
            .map_err(|error| BuiltinModelError(format!("read reusable source file: {error}")))?
            .ok_or_else(|| {
                BuiltinModelError("project frontier refers to a missing source file".to_owned())
            })?;
        if record.file_fields().is_none() {
            return Err(BuiltinModelError(
                "project frontier refers to a non-file record".to_owned(),
            ));
        }
        reusable.insert(*key, record);
    }
    let source_coordinate = source_root.to_string_lossy();
    let scan = ingest::scan_project(&source_coordinate, project_key, &reusable)
        .map_err(BuiltinModelError)?;
    let file_keys = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let project = ProductSourceRecord::project(label, scan.source_version, file_keys.clone())
        .map_err(BuiltinModelError)?;
    let mut changes = Vec::new();
    if before.as_ref() != Some(&project) {
        changes.push(BuiltinSourceChange {
            key: project_key,
            after: Some(project),
        });
    }
    for (key, record) in scan.files {
        let current = relation
            .lookup(&key)
            .map_err(|error| BuiltinModelError(format!("read indexed source file: {error}")))?;
        if current.as_ref() != Some(&record) {
            changes.push(BuiltinSourceChange {
                key,
                after: Some(record),
            });
        }
    }
    let selected = file_keys.into_iter().collect::<BTreeSet<_>>();
    changes.extend(
        old_files
            .into_iter()
            .filter(|key| !selected.contains(key))
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    // The source relation is content addressed. If its frontier is unchanged,
    // compiling again would publish identical images under a fresh journal
    // generation and turn an idempotent index request into a new semantic
    // history entry. Reuse the selected immutable generation until a source
    // change requires a new compiler transaction.
    let semantic_changes = if changes.is_empty() {
        Vec::new()
    } else {
        let semantic_context = SemanticCompilationContext::admit(
            package,
            label,
            source_root,
            coordinate,
            request_id,
            compiler,
        )?;
        compile_semantic_publications(daemon, &semantic_context, scan.compiler_sources)?
    };
    if changes.is_empty() && semantic_changes.is_empty() {
        return Ok(None);
    }
    BuiltinIntent::index_with_semantics(package, label, changes, semantic_changes).map(Some)
}

struct SemanticCompilationContext<'request> {
    package: backend_engine::PackageKey,
    package_reference: backend_engine::PackageReference,
    source_root: &'request Path,
    coordinate: Option<&'request PackageUrl>,
    correlation: CorrelationId,
    compiler: &'request LocalCompilerClient,
}

impl<'request> SemanticCompilationContext<'request> {
    fn admit(
        package: backend_engine::PackageKey,
        package_label: &str,
        source_root: &'request Path,
        coordinate: Option<&'request PackageUrl>,
        request_id: u64,
        compiler: &'request LocalCompilerClient,
    ) -> Result<Self, BuiltinModelError> {
        let package_reference = backend_engine::PackageReference::parse(package_label.to_owned())
            .map_err(|error| {
            BuiltinModelError(format!("semantic package reference: {error:?}"))
        })?;
        if backend_engine::package_key(package_reference.as_str()) != package {
            return Err(BuiltinModelError(
                "semantic package reference does not match its product key".to_owned(),
            ));
        }
        Ok(Self {
            package,
            package_reference,
            source_root,
            coordinate,
            correlation: CorrelationId(request_id),
            compiler,
        })
    }
}

fn compile_semantic_publications(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    context: &SemanticCompilationContext<'_>,
    sources: Vec<ingest::CompilerSource>,
) -> Result<Vec<BuiltinSemanticChange>, BuiltinModelError> {
    let mut by_profile = BTreeMap::<LanguageProfile, Vec<OwnedPackageSource>>::new();
    for source in sources {
        let profile = compile_profile(context.source_root, &source);
        by_profile.entry(profile).or_default().push(
            OwnedPackageSource::new(&source.relative_path, &source.source)
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        );
    }
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    let mut changes = Vec::with_capacity(by_profile.len().saturating_mul(2));
    for (profile, sources) in by_profile {
        let expected_artifacts = u32::try_from(sources.len())
            .map_err(|_| BuiltinModelError("semantic source count exceeds u32".to_owned()))?;
        let coordinate = semantic_coordinate(context.package, profile, context.coordinate)?;
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: context.correlation,
                profile,
                stage: backend_semantic::vocabulary::Stage::LowerIr,
            },
            coordinate.clone(),
        )
        .map_err(|error| BuiltinModelError(format!("semantic package profile: {error:?}")))?;
        let key = ProductSemanticPublicationKey::new(
            context.package_reference.clone(),
            coordinate,
            profile,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let value = match context.compiler.compile_package_sources(
            OwnedPackageSourceSet::new(
                request,
                context.source_root.to_path_buf(),
                sources.into_boxed_slice(),
            )
            .map_err(|error| BuiltinModelError(error.to_string()))?,
        ) {
            Ok(published)
                if published.publication.manifest.fragment_count == expected_artifacts
                    && published.images.len() == expected_artifacts as usize =>
            {
                ProductSemanticPublicationRecord::Published {
                    coverage: SemanticPublicationCoverage::Complete,
                    claim: SemanticPublicationClaim::admit(
                        published.publication.manifest,
                        published.publication.binding,
                    )
                    .map_err(|error| BuiltinModelError(error.to_owned()))?,
                }
            }
            Ok(_) => {
                ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::Rejected)
            }
            Err(error) => {
                ProductSemanticPublicationRecord::Unavailable(semantic_unavailable_reason(&error))
            }
        };
        if let ProductSemanticPublicationRecord::Published { claim, .. } = &value {
            let history_key = key.for_generation(claim.binding().identity);
            match relation.lookup(&history_key).map_err(|error| {
                BuiltinModelError(format!("read semantic publication history: {error}"))
            })? {
                None => changes.push(BuiltinSemanticChange {
                    key: history_key,
                    after: Some(value.clone()),
                }),
                Some(prior) if prior == value => {}
                Some(_) => {
                    return Err(BuiltinModelError(
                        "immutable semantic generation was rebound to another claim".to_owned(),
                    ));
                }
            }
        }
        if relation.lookup(&key).map_err(|error| {
            BuiltinModelError(format!("read selected semantic publication: {error}"))
        })? != Some(value.clone())
        {
            changes.push(BuiltinSemanticChange {
                key,
                after: Some(value),
            });
        }
    }
    Ok(changes)
}

/// Selects the exact profile one source is compiled under.
///
/// A file's extension names its language, but a Rust file's edition is a
/// fact of the crate that owns it: the authority checks it against Cargo's
/// own metadata and refuses a mismatch. Compiling every `.rs` file as edition
/// 2024 therefore sent every 2015, 2018, and 2021 crate (most of crates.io)
/// to a terminal `ProjectAuthority` failure and a structural-only answer. The
/// edition is read from the nearest `Cargo.toml` with a `[package]` table
/// between the file and the source root, exactly as Cargo resolves it.
fn compile_profile(source_root: &Path, source: &ingest::CompilerSource) -> LanguageProfile {
    let LanguageProfile::Rust(_) = source.profile else {
        return source.profile;
    };
    let mut directory = source_root.join(&source.relative_path);
    while directory.pop() && directory.starts_with(source_root) {
        let manifest = directory.join("Cargo.toml");
        let declares_package = std::fs::read_to_string(&manifest)
            .is_ok_and(|contents| contents.lines().any(|line| line.trim() == "[package]"));
        if declares_package {
            // An unreadable or unknown edition keeps the default profile; the
            // authority then reports the exact mismatch as a typed terminal
            // rather than this scan failing the whole package.
            return backend_frontend_rust::legacy::manifest_edition(&directory)
                .map_or(source.profile, LanguageProfile::Rust);
        }
    }
    // No owning manifest: the authority reports the missing project itself.
    source.profile
}

fn semantic_coordinate(
    package: backend_engine::PackageKey,
    profile: LanguageProfile,
    supplied: Option<&PackageUrl>,
) -> Result<PackageUrl, BuiltinModelError> {
    if let Some(supplied) = supplied
        && supplied.package_type().language() == profile.language()
    {
        return Ok(supplied.clone());
    }
    let package_type = match profile.language() {
        Language::Rust => "cargo",
        Language::TypeScript => "npm",
        Language::Python => "pypi",
        Language::Go => "golang",
        Language::Java => "maven",
        Language::CSharp => "nuget",
        Language::Clang => "generic",
    };
    let name = backend_engine::encode_id(package.as_bytes());
    PackageUrl::parse(format!("pkg:{package_type}/local-{name}@0.0.0-local"))
        .map_err(|error| BuiltinModelError(format!("construct local package identity: {error:?}")))
}

fn semantic_unavailable_reason(error: &PackageSemanticRuntimeError) -> SemanticUnavailableReason {
    match error {
        PackageSemanticRuntimeError::Package(PackageSemanticError::Compile {
            terminal, ..
        }) => match terminal.as_ref() {
            backend_library::interface::CompilerTerminal::Toolchain { .. }
            | backend_library::interface::CompilerTerminal::ToolingUnavailable { .. }
            | backend_library::interface::CompilerTerminal::Unavailable { .. } => {
                SemanticUnavailableReason::Toolchain
            }
            backend_library::interface::CompilerTerminal::PackageCancelled { .. }
            | backend_library::interface::CompilerTerminal::Cancelled { .. } => {
                SemanticUnavailableReason::Cancelled
            }
            backend_library::interface::CompilerTerminal::Compile {
                cause: backend_library::interface::CompilerCause::Authority { .. },
                ..
            }
            | backend_library::interface::CompilerTerminal::PackageSource { .. } => {
                SemanticUnavailableReason::ProjectAuthority
            }
            _ => SemanticUnavailableReason::Rejected,
        },
        PackageSemanticRuntimeError::Runtime(terminal) => match terminal {
            backend_library::interface::CompilerTerminal::Toolchain { .. }
            | backend_library::interface::CompilerTerminal::ToolingUnavailable { .. }
            | backend_library::interface::CompilerTerminal::Unavailable { .. }
            | backend_library::interface::CompilerTerminal::Runtime {
                cause: backend_library::interface::CompilerRuntimeCause::ToolchainProbeTimeout,
                ..
            } => SemanticUnavailableReason::Toolchain,
            backend_library::interface::CompilerTerminal::PackageCancelled { .. }
            | backend_library::interface::CompilerTerminal::Cancelled { .. }
            | backend_library::interface::CompilerTerminal::Runtime {
                cause: backend_library::interface::CompilerRuntimeCause::RequestCancelled,
                ..
            } => SemanticUnavailableReason::Cancelled,
            _ => SemanticUnavailableReason::Rejected,
        },
        PackageSemanticRuntimeError::Admission(_) | PackageSemanticRuntimeError::Package(_) => {
            SemanticUnavailableReason::Rejected
        }
    }
}

fn remove_project_intent(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let Some(record) = relation
        .lookup(&package.to_bytes())
        .map_err(|error| BuiltinModelError(format!("read indexed project: {error}")))?
    else {
        return Ok(None);
    };
    let files = record
        .project_fields()
        .map(|fields| fields.files)
        .ok_or_else(|| BuiltinModelError("project key contains a source file record".to_owned()))?;
    let semantic = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    let mut semantic_changes = Vec::new();
    let mut after = None;
    loop {
        let page = semantic
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("page semantic publications: {error}")))?;
        semantic_changes.extend(
            page.entries()
                .iter()
                .filter(|(key, _)| key.package_key() == package)
                .map(|(key, _)| BuiltinSemanticChange {
                    key: key.clone(),
                    after: None,
                }),
        );
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    BuiltinIntent::remove_project_with_semantics(package, label, files, semantic_changes).map(Some)
}

fn semantic_version_record(
    key: &ProductSemanticPublicationKey,
    coverage: SemanticPublicationCoverage,
    claim: SemanticPublicationClaim,
    selected: bool,
) -> backend_engine::SemanticVersionRecord {
    let binding = claim.binding();
    let manifest = claim.manifest();
    backend_engine::SemanticVersionRecord {
        package: key.package().clone(),
        coordinate: key.coordinate().clone(),
        profile: backend_engine::SemanticLanguageProfile::new(key.profile()),
        generation: backend_engine::SemanticGenerationId::new(*binding.identity.as_ref()),
        generation_root: *binding.generation.pinned_root.as_ref(),
        dependency_set: *binding.generation.dep_set.as_ref(),
        manifest: *manifest.identity.as_ref(),
        artifacts: manifest.fragment_count,
        semantic_bytes: manifest.byte_length,
        complete: matches!(coverage, SemanticPublicationCoverage::Complete),
        selected,
    }
}

fn semantic_versions(
    daemon: &ProductDaemon,
    package: &backend_engine::PackageReference,
) -> Result<Box<[backend_engine::SemanticVersionRecord]>, BuiltinModelError> {
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic version history: {error}")))?;
    let mut selected = BTreeMap::<(PackageUrl, LanguageProfile), [u8; 32]>::new();
    let mut unavailable = None;
    let mut generations = Vec::new();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| {
                BuiltinModelError(format!("page semantic version history: {error}"))
            })?;
        for (key, record) in page.entries() {
            if key.package() != package {
                continue;
            }
            key.admit_record(record).map_err(|error| {
                BuiltinModelError(format!("admit semantic version history: {error}"))
            })?;
            let (coverage, claim) = match record {
                ProductSemanticPublicationRecord::Published { coverage, claim } => {
                    (*coverage, *claim)
                }
                // One language whose authority is unavailable must not hide
                // the history every other language of the package published.
                // The typed refusal is kept for a package with no published
                // target at all, below.
                ProductSemanticPublicationRecord::Unavailable(reason) if key.is_selected() => {
                    unavailable.get_or_insert(*reason);
                    continue;
                }
                ProductSemanticPublicationRecord::Unavailable(_) => continue,
            };
            let target = (key.coordinate().clone(), key.profile());
            match key.selection() {
                SemanticPublicationSelection::Selected => {
                    if selected
                        .insert(target, *claim.binding().identity.as_ref())
                        .is_some()
                    {
                        return Err(BuiltinModelError(
                            "semantic version history has duplicate selected targets".to_owned(),
                        ));
                    }
                }
                SemanticPublicationSelection::Generation(_) => {
                    if generations.len() >= backend_engine::MAX_PRODUCT_ROWS {
                        return Err(BuiltinModelError(
                            "semantic version history exceeds the product row bound".to_owned(),
                        ));
                    }
                    generations.push((
                        target,
                        semantic_version_record(key, coverage, claim, false),
                    ));
                }
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    if selected.is_empty()
        && let Some(reason) = unavailable
    {
        return Err(BuiltinModelError(format!(
            "semantic publication unavailable: {reason}"
        )));
    }
    for (target, record) in &mut generations {
        record.selected = selected.get(target).copied() == Some(record.generation.to_bytes());
    }
    if selected.iter().any(|(selected_target, identity)| {
        !generations.iter().any(|(generation_target, record)| {
            generation_target == selected_target && record.generation.to_bytes() == *identity
        })
    }) {
        return Err(BuiltinModelError(
            "selected semantic generation is absent from immutable history".to_owned(),
        ));
    }
    Ok(generations
        .into_iter()
        .map(|(_, record)| record)
        .collect::<Vec<_>>()
        .into_boxed_slice())
}

type ProductDaemon = crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>;
type AdmittedReply = (CommandReply, Option<WireCertificate>);

pub(super) struct CommandAdapter {
    sql_projection: backend_extension_turso::TursoProjection,
    registry: Option<RegistryGateway>,
    product_state: super::ProductState,
    compiler: LocalCompilerClient,
    search_snapshots: super::query::SearchSnapshotOwner,
    remote_semantic: super::query::RemoteSemantic,
}

impl CommandAdapter {
    pub(super) fn new(
        sql_projection: backend_extension_turso::TursoProjection,
        registry: Option<RegistryGateway>,
        product_state: super::ProductState,
        compiler: LocalCompilerClient,
        search_snapshots: super::query::SearchSnapshotOwner,
        remote_semantic: super::query::RemoteSemantic,
    ) -> Self {
        Self {
            sql_projection,
            registry,
            product_state,
            compiler,
            search_snapshots,
            remote_semantic,
        }
    }

    pub(super) fn execute(
        &mut self,
        daemon: &mut ProductDaemon,
        body: &[u8],
    ) -> Result<Vec<u8>, BuiltinModelError> {
        let owner = daemon.engine().daemon().library().cursor();
        let request = backend_engine::decode_command_dto_for_owner(body, owner)
            .map_err(|error| BuiltinModelError(format!("decode command DTO: {error}")))?;
        let request_id = request.request_id;
        let certificate = request.certificate().cloned();
        let (reply, certificate) =
            self.dispatch(daemon, request.command, certificate, request_id)?;
        let mut reply = match reply {
            CommandReply::Health(root) => backend_engine::ReplyDto::health(
                request_id,
                root,
                daemon.engine().daemon().library().cursor(),
            ),
            reply => backend_engine::ReplyDto::new(request_id, reply),
        };
        if let Some(certificate) = certificate {
            reply = reply.with_certificate(certificate);
        }
        backend_engine::encode_reply_dto(&reply).map_err(BuiltinModelError)
    }

    fn dispatch(
        &mut self,
        daemon: &mut ProductDaemon,
        command: Command,
        certificate: Option<WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        match command {
            Command::Add { package } => self.add(daemon, package, certificate.as_ref(), request_id),
            Command::Remove { package } => {
                self.remove(daemon, package, certificate.as_ref(), request_id)
            }
            Command::Search(query) => self.search(daemon, &query, certificate),
            Command::Graph(query) => self.graph(daemon, query, certificate, false),
            Command::Related(query) => self.graph(daemon, query, certificate, true),
            Command::GraphQuery(request) => {
                execute_certified_graph_query(daemon, &self.compiler, &request, certificate)
            }
            Command::Surface(surface) => self.surface(daemon, surface, request_id),
            command => self.standard(daemon, &command, certificate),
        }
    }

    fn add(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        certificate: Option<&WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        let intent = if Path::new(&label).is_dir() {
            index_project_intent(daemon, package, &label, request_id, &self.compiler)?
        } else if label.starts_with("pkg:") || label.starts_with("PKG:") {
            self.registry_intent(daemon, package, &label, request_id)?
        } else {
            Some(BuiltinIntent::add(package, label.clone())?)
        };
        if let Some(intent) = intent {
            commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                BuiltinModelError(format!("commit product source intent: {error}"))
            })?;
        }
        self.publish_view(daemon)?;
        let intent_id =
            backend_engine::intent_id("request_package", requested_package.as_bytes());
        let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
            id: backend_engine::encode_id(intent_id.as_bytes()),
            token: "request_package".to_owned(),
            payload: requested_package.as_bytes().to_vec().into_boxed_slice(),
        });
        Ok((CommandReply::Added(intent_id), Some(certificate)))
    }

    fn registry_intent(
        &mut self,
        daemon: &ProductDaemon,
        package: backend_engine::PackageKey,
        label: &str,
        request_id: u64,
    ) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
        let coordinate =
            backend_engine::registry::PackageCoordinate::parse(label).map_err(|_| {
                BuiltinModelError(
                    "add target must be an admitted local directory or version-pinned package URL"
                        .to_owned(),
                )
            })?;
        if coordinate.as_str() != label {
            return Err(BuiltinModelError(
                "package URL is not in canonical form".to_owned(),
            ));
        }
        let Some(gateway) = self.registry.as_mut() else {
            // The coordinate itself is still an admitted, exact package
            // identity. Retain it with an explicit compiler-authority
            // unavailable terminal so every downstream surface observes the
            // semantic plane and no source-shaped placeholder is presented as
            // compiler truth. A configured registry continues through the
            // acquisition and compiler-authority path below.
            return BuiltinIntent::add(package, label.to_owned()).map(Some);
        };
        let archive = gateway
            .acquire(&coordinate)
            .map_err(|error| BuiltinModelError(format!("registry add: {error}")))?;
        let staged = gateway
            .stage_archive(&coordinate, &archive)
            .map_err(|error| BuiltinModelError(format!("stage registry archive: {error}")))?;
        index_project_intent_at(
            daemon,
            package,
            label,
            staged.path(),
            Some(&coordinate),
            request_id,
            &self.compiler,
        )
    }

    /// Resolves a document by the canonical coordinate carried in the
    /// caller's admitted key claim. Semantic rows use a compiler-owned symbol
    /// identity rather than `symbol_key(label)`, while the public document
    /// command intentionally accepts the coordinate an agent copied from a
    /// search page. The claim supplies that preimage, so resolve the row by
    /// exact label after checking the request root instead of returning a
    /// false not-found for a published semantic declaration.
    fn canonical_claim_document(
        daemon: &ProductDaemon,
        query: &backend_library::DocumentQuery,
        certificate: Option<&WireCertificate>,
    ) -> Option<backend_library::Document> {
        let label = certificate.and_then(|certificate| {
            certificate.claims.iter().find_map(|claim| match claim {
                WireClaim::Key {
                    schema: backend_engine::WireSchema::Symbol,
                    id,
                    value,
                } if id
                    == &backend_engine::encode_id(backend_engine::symbol_key(value).as_bytes())
                    && query.symbol().matches(backend_engine::symbol_key(value)) =>
                {
                    Some(value.as_str())
                }
                _ => None,
            })
        })?;
        let library = daemon.engine().daemon().library();
        let root = library.revision_root();
        if !query.basis().matches(root) {
            return None;
        }
        let row =
            library.view().rows().iter().find(|row| {
                row.label == label && matches!(row.id, backend_engine::RowId::Symbol(_))
            })?;
        let backend_engine::RowId::Symbol(_) = row.id else {
            return None;
        };
        let symbol = backend_library::symbol_key(label);
        let source_basis = backend_library::Basis {
            root,
            ..library.view().basis()
        };
        let mut document = backend_library::Document::new(symbol, root, row.document.clone())
            .with_source_basis(source_basis)
            .with_location(row.source.clone())
            .with_excerpt(row.excerpt.clone());
        document.signature.clone_from(&row.signature);
        Some(document)
    }

    fn remove(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        certificate: Option<&WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        if let Some(intent) = remove_project_intent(daemon, package, &label)? {
            commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                BuiltinModelError(format!("commit product source intent: {error}"))
            })?;
        }
        self.publish_view(daemon)?;
        let intent_id = backend_engine::intent_id("remove_package", requested_package.as_bytes());
        let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
            id: backend_engine::encode_id(intent_id.as_bytes()),
            token: "remove_package".to_owned(),
            payload: requested_package.as_bytes().to_vec().into_boxed_slice(),
        });
        Ok((CommandReply::Removed(intent_id), Some(certificate)))
    }

    fn publish_view(&mut self, daemon: &mut ProductDaemon) -> Result<(), BuiltinModelError> {
        // Reconcile even after a no-op source intent so a retry heals a crash
        // between the durable source commit and its derived view publication.
        let deployment = super::SemanticDeployment::from_remote(&self.remote_semantic);
        let deltas = publish_builtin_view(daemon, &self.compiler, deployment)
            .map_err(|error| BuiltinModelError(format!("publish product source view: {error}")))?;
        project_view_deltas(&mut self.sql_projection, daemon, &deltas)
    }

    fn search(
        &mut self,
        daemon: &ProductDaemon,
        query: &backend_engine::Query,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let command = Command::Search(query.clone());
        let reply = execute_search(
            daemon,
            &self.compiler,
            &mut self.search_snapshots,
            &mut self.remote_semantic,
            query,
        )
        .unwrap_or_else(|error| CommandReply::Error(error.to_string()));
        Self::certify(daemon, &command, reply, certificate)
    }

    fn graph(
        &self,
        daemon: &ProductDaemon,
        query: backend_engine::GraphNeighborhoodQuery,
        certificate: Option<WireCertificate>,
        include_incoming: bool,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let command = if include_incoming {
            Command::Related(query)
        } else {
            Command::Graph(query)
        };
        let query = Self::claimed_graph_source(daemon, query, certificate.as_ref());
        let reply = match execute_semantic_graph(daemon, &self.compiler, query, include_incoming)? {
            Some(snapshot) => CommandReply::Graph(snapshot),
            None => match execute_structural_call_graph(daemon, query, include_incoming)? {
                Some(snapshot) => CommandReply::Graph(snapshot),
                None => daemon
                    .engine()
                    .daemon()
                    .library()
                    .execute(command.clone())
                    .unwrap_or_else(|error| CommandReply::Failed(error.into())),
            },
        };
        Self::certify(daemon, &command, reply, certificate)
    }

    /// Resolves a graph source named by the canonical coordinate a caller
    /// copied from a result page.
    ///
    /// A client addresses a declaration by `symbol_key(coordinate)`, which is
    /// the row key of a structural declaration but not of a semantic one: a
    /// compiler-backed row is keyed by its compiler-owned identity. Like
    /// `canonical_claim_document`, this reads the coordinate from the
    /// caller's admitted key claim and, when no view row carries the
    /// requested key, selects the one row whose label is exactly that
    /// coordinate. Without it every `graph` and `related` request for a
    /// semantic declaration failed with "semantic graph source is absent".
    fn claimed_graph_source(
        daemon: &ProductDaemon,
        query: backend_engine::GraphNeighborhoodQuery,
        certificate: Option<&WireCertificate>,
    ) -> backend_engine::GraphNeighborhoodQuery {
        let library = daemon.engine().daemon().library();
        let view = library.view();
        let requested = query.resolve_symbol(view);
        if requested.is_some_and(|symbol| view.row(backend_engine::RowId::Symbol(symbol)).is_some())
            || !query.basis().matches(library.revision_root())
        {
            return query;
        }
        let Some(label) = certificate.and_then(|certificate| {
            certificate.claims.iter().find_map(|claim| match claim {
                WireClaim::Key {
                    schema: backend_engine::WireSchema::Symbol,
                    id,
                    value,
                } if id
                    == &backend_engine::encode_id(backend_engine::symbol_key(value).as_bytes())
                    && requested == Some(backend_engine::symbol_key(value)) =>
                {
                    Some(value.as_str())
                }
                _ => None,
            })
        }) else {
            return query;
        };
        view.rows()
            .iter()
            .find_map(|row| match row.id {
                backend_engine::RowId::Symbol(symbol) if row.label == label => Some(symbol),
                _ => None,
            })
            .map_or(query, |symbol| query.with_resolved_symbol(symbol))
    }

    fn surface(
        &mut self,
        daemon: &mut ProductDaemon,
        surface: backend_engine::SurfaceCommand,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let reply = match surface {
            backend_engine::SurfaceCommand::References { target } => {
                execute_references(daemon, &self.compiler, &target).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    CommandReply::Surface,
                )
            }
            backend_engine::SurfaceCommand::Diff { from, to } => {
                execute_semantic_diff(daemon, &self.compiler, &from, &to).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |rows| CommandReply::Surface(backend_engine::SurfaceReply::Diff(rows)),
                )
            }
            backend_engine::SurfaceCommand::SemanticVersions { package } => semantic_versions(
                daemon, &package,
            )
            .map_or_else(
                |error| {
                    CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                        error.to_string(),
                    ))
                },
                |records| {
                    CommandReply::Surface(backend_engine::SurfaceReply::SemanticVersions(records))
                },
            ),
            backend_engine::SurfaceCommand::SelectSemanticVersion {
                package,
                coordinate,
                profile,
                generation,
            } => self
                .select_semantic_version(
                    daemon, package, coordinate, profile, generation, request_id,
                )
                .map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |record| {
                        CommandReply::Surface(
                            backend_engine::SurfaceReply::SemanticVersionSelected(record),
                        )
                    },
                ),
            surface => {
                let catalog = self
                    .registry
                    .as_mut()
                    .map_or(Ok(Vec::new()), RegistryGateway::catalog)
                    .map_err(BuiltinModelError)?;
                let mut dependency_facts = self
                    .registry
                    .as_mut()
                    .map_or_else(Vec::new, RegistryGateway::dependency_facts);
                let indexed = super::read_indexed_sources(
                    &daemon.engine().daemon().owner().snapshot(),
                )?;
                for project in indexed.projects.values() {
                    if project.label.starts_with("pkg:") {
                        continue;
                    }
                    let project_root = std::path::Path::new(&project.label);
                    if !project_root.join("Cargo.toml").is_file() {
                        continue;
                    }
                    match super::local_manifest::cargo_dependency_facts(project_root) {
                        Ok(Some(fact)) => dependency_facts.push(fact),
                        Ok(None) => {}
                        Err(error) => return Err(BuiltinModelError(error)),
                    }
                }
                futures_executor::block_on(self.sql_projection.synchronize_package_graph(
                    daemon.engine().daemon().library().view().root(),
                    &dependency_facts,
                ))
                .map_err(|error| {
                    BuiltinModelError(format!("align package graph projection: {error}"))
                })?;
                self.product_state
                    .execute(
                        surface,
                        daemon.engine().daemon().library().view(),
                        &catalog,
                        &dependency_facts,
                    )
                    .map_or_else(
                        |error| {
                            CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                                error,
                            ))
                        },
                        CommandReply::Surface,
                    )
            }
        };
        Ok((reply, None))
    }

    fn select_semantic_version(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageReference,
        coordinate: PackageUrl,
        profile: backend_engine::SemanticLanguageProfile,
        generation: backend_engine::SemanticGenerationId,
        request_id: u64,
    ) -> Result<backend_engine::SemanticVersionRecord, BuiltinModelError> {
        let profile = profile
            .profile()
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let package_key = backend_engine::package_key(package.as_str());
        let selected_key = ProductSemanticPublicationKey::new(package.clone(), coordinate, profile)
            .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let history_key = selected_key
            .for_generation_bytes(generation.to_bytes())
            .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let relation = daemon
            .engine()
            .daemon()
            .owner()
            .snapshot()
            .relation::<BuiltinSemanticRelation>()
            .map_err(|error| {
                BuiltinModelError(format!("open semantic version history: {error}"))
            })?;
        let record = relation
            .lookup(&history_key)
            .map_err(|error| BuiltinModelError(format!("read semantic version history: {error}")))?
            .ok_or_else(|| BuiltinModelError("semantic generation is not retained".to_owned()))?;
        history_key.admit_record(&record).map_err(|error| {
            BuiltinModelError(format!("admit selected semantic generation: {error}"))
        })?;
        let ProductSemanticPublicationRecord::Published { coverage, claim } = record.clone() else {
            return Err(BuiltinModelError(
                "semantic generation history contains an unavailable terminal".to_owned(),
            ));
        };
        let before = relation.lookup(&selected_key).map_err(|error| {
            BuiltinModelError(format!("read selected semantic generation: {error}"))
        })?;
        if before != Some(record.clone()) {
            let intent = BuiltinIntent::select_semantic_generation(
                package_key,
                package.as_str(),
                selected_key.clone(),
                history_key,
                before,
                record,
            )?;
            commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                BuiltinModelError(format!("commit semantic generation selection: {error}"))
            })?;
        }
        self.publish_view(daemon)?;
        Ok(semantic_version_record(
            &selected_key,
            coverage,
            claim,
            true,
        ))
    }

    fn standard(
        &self,
        daemon: &ProductDaemon,
        command: &Command,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let reply = match command {
            Command::Document(query) | Command::Source(query) => {
                Self::canonical_claim_document(daemon, query, certificate.as_ref())
                    .map_or_else(
                        || daemon.engine().daemon().library().execute(command.clone()),
                        |document| Ok(CommandReply::Document(document)),
                    )
                    .unwrap_or_else(|error| CommandReply::Error(error.to_string()))
            }
            _ => daemon
                .engine()
                .daemon()
                .library()
                .execute(command.clone())
                .unwrap_or_else(|error| CommandReply::Error(error.to_string())),
        };
        let reply = semantic_readiness(reply, daemon, &self.remote_semantic, &self.compiler)?;
        Self::certify(daemon, command, reply, certificate)
    }

    fn certify(
        daemon: &ProductDaemon,
        command: &Command,
        reply: CommandReply,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let certificate = projection::reply_certificate(
            command,
            &reply,
            daemon.engine().daemon().library().view(),
            daemon.engine().daemon().library().cursor(),
            certificate,
        )?;
        Ok((reply, certificate))
    }
}

fn semantic_readiness(
    reply: CommandReply,
    daemon: &ProductDaemon,
    remote: &super::query::RemoteSemantic,
    compiler: &LocalCompilerClient,
) -> Result<CommandReply, BuiltinModelError> {
    let CommandReply::Readiness(report) = reply else {
        return Ok(reply);
    };
    let local = ingest::semantic_capabilities(report.capabilities(), compiler)
        .map_err(|error| BuiltinModelError(format!("local semantic readiness: {error}")))?;
    let capabilities = remote
        .inventory(&local)
        .map_err(|error| BuiltinModelError(format!("project semantic readiness: {error}")))?;
    // A deployment that configured no embedding provider has a terminally
    // unavailable semantic lane, and a health reply must say so instead of
    // dropping the fact on the floor. The published view root already folds the
    // same classification in, so this reconciliation is a fixed point for every
    // reply the owner certifies: `readiness_certificate` rejects a report whose
    // coverage differs from that root, and the lane is described exactly once.
    let coverage = super::reconcile_semantic_lane(
        report.coverage(),
        super::SemanticDeployment::from_remote(remote),
    );
    // Progress is read out of the committed source relation rather than out of
    // a counter the scan kept, so it describes the revision this very report
    // names. `readiness_certificate` compares revision, basis, coverage, and
    // row count; the counts are derived from the same owner snapshot, so they
    // cannot disagree with the root the certificate commits to.
    let progress = super::ingest_progress(&daemon.engine().daemon().owner().snapshot())?;
    Ok(CommandReply::Readiness(
        backend_engine::HealthReport::from_admitted_parts(
            report.revision(),
            report.basis(),
            coverage.into_boxed_slice(),
            report.row_count(),
            capabilities,
        )
        .with_progress(progress),
    ))
}

fn project_view_deltas(
    projection: &mut backend_extension_turso::TursoProjection,
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    deltas: &[backend_engine::CommittedViewDelta],
) -> Result<(), BuiltinModelError> {
    if deltas.is_empty() {
        return Ok(());
    }
    if let Err(incremental_error) = futures_executor::block_on(projection.apply_all(deltas)) {
        futures_executor::block_on(
            projection.synchronize(daemon.engine().daemon().library().view()),
        )
        .map_err(|rebuild_error| {
            BuiltinModelError(format!(
                "Turso projection delta failed ({incremental_error}); rebuild failed: {rebuild_error}"
            ))
        })?;
    }
    Ok(())
}

fn canonical_local_package(
    package: backend_engine::PackageKey,
    label: String,
) -> Result<(backend_engine::PackageKey, String), BuiltinModelError> {
    let path = Path::new(&label);
    if !path.is_dir() {
        return Ok((package, label));
    }
    let canonical = path.canonicalize().map_err(|error| {
        BuiltinModelError(format!("canonicalize local package {label}: {error}"))
    })?;
    let label = canonical.to_string_lossy().into_owned();
    Ok((backend_engine::package_key(&label), label))
}

fn certified_package_label(
    certificate: Option<&WireCertificate>,
    package: backend_engine::PackageKey,
) -> Result<String, BuiltinModelError> {
    let id = backend_engine::encode_id(package.as_bytes());
    let certificate = certificate
        .ok_or_else(|| BuiltinModelError("package command certificate is missing".to_owned()))?;
    let mut value = None;
    for claim in &certificate.claims {
        if let WireClaim::Key {
            schema: backend_engine::WireSchema::Package,
            id: claimed,
            value: text,
        } = claim
            && claimed == &id
        {
            if value.is_some() {
                return Err(BuiltinModelError(
                    "package command certificate has duplicate claims".to_owned(),
                ));
            }
            value = Some(text.clone());
        }
    }
    value.ok_or_else(|| {
        BuiltinModelError("package command certificate has no canonical package text".to_owned())
    })
}

fn commit_builtin_intent(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    request_id: u64,
    intent: &BuiltinIntent,
) -> Result<(), BuiltinModelError> {
    let request = BuiltinModel.request_id(intent);
    let expected = daemon.engine().daemon().owner().head().expectation();
    let receiver = daemon
        .client()
        .request(
            request_id,
            crate::Request::Commit {
                request,
                expected,
                intent: intent.clone(),
            },
        )
        .map_err(|error| BuiltinModelError(format!("queue builtin intent: {error:?}")))?;
    if !daemon.serve_one() {
        return Err(BuiltinModelError(
            "builtin intent owner did not make progress".to_owned(),
        ));
    }
    match crate::service::wait_for_daemon_reply(daemon, &receiver)
        .map_err(|error| BuiltinModelError(error.to_string()))?
    {
        backend_engine::DaemonReply::Commit(Ok(_)) => Ok(()),
        backend_engine::DaemonReply::Commit(Err(error)) => {
            Err(BuiltinModelError(error.to_string()))
        }
        _ => Err(BuiltinModelError(
            "builtin intent was sent to the wrong owner lane".to_owned(),
        )),
    }
}

#[cfg(test)]
mod semantic_diff_tests {
    use super::*;
    use backend_semantic::ir::{
        CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, EntityVersion,
        VariantFingerprint,
    };

    fn declaration(
        family: u16,
        variant: u16,
        payload: u8,
        label: &'static str,
    ) -> (DeclarationIdentity, SemanticDeclaration<'static>) {
        let mut family_bytes = [0; 16];
        family_bytes[..2].copy_from_slice(&family.to_be_bytes());
        let mut variant_bytes = [0; 16];
        variant_bytes[..2].copy_from_slice(&variant.to_be_bytes());
        let version = EntityVersion {
            family: DeclarationFamilyId::from_raw(family_bytes),
            variant: VariantFingerprint::from_raw(variant_bytes),
            core_payload: CorePayloadHash::from_raw([payload; 16]),
        };
        (
            version.identity(),
            SemanticDeclaration {
                label,
                version,
                parent: None,
            },
        )
    }

    fn snapshot(
        declarations: impl IntoIterator<Item = (DeclarationIdentity, SemanticDeclaration<'static>)>,
    ) -> SemanticPackageSnapshot<'static> {
        let mut declarations = declarations.into_iter().collect::<Vec<_>>();
        declarations.sort_unstable_by_key(|(identity, _)| *identity);
        SemanticPackageSnapshot {
            declarations,
            links: Vec::new(),
        }
    }

    #[test]
    fn singleton_variant_churn_is_one_change() -> Result<(), BuiltinModelError> {
        let before = snapshot([declaration(1, 1, 1, "old")]);
        let after = snapshot([declaration(1, 2, 2, "new")]);
        let rows = diff_semantic_snapshots(&before, &after)?;
        assert!(matches!(
            rows.as_ref(),
            [backend_engine::DiffRecord {
                change: backend_engine::DeclarationChange::Changed,
                ..
            }]
        ));
        assert_eq!(rows[0].label.as_str(), "new");
        Ok(())
    }

    #[test]
    fn ambiguous_overload_churn_never_claims_removal() -> Result<(), BuiltinModelError> {
        let before = snapshot([
            declaration(1, 1, 1, "before-a"),
            declaration(1, 2, 1, "before-b"),
        ]);
        let after = snapshot([
            declaration(1, 3, 2, "after-a"),
            declaration(1, 4, 2, "after-b"),
        ]);
        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter()
                .filter(|row| row.change == backend_engine::DeclarationChange::Indeterminate)
                .count(),
            2
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.change == backend_engine::DeclarationChange::Removed)
        );
        Ok(())
    }

    #[test]
    fn exact_identity_payload_change_is_reported() -> Result<(), BuiltinModelError> {
        let before = snapshot([declaration(1, 1, 1, "before")]);
        let after = snapshot([declaration(1, 1, 2, "after")]);
        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].change, backend_engine::DeclarationChange::Changed);
        Ok(())
    }

    #[test]
    fn link_only_change_retains_typed_endpoint_and_both_evidence_rows()
    -> Result<(), BuiltinModelError> {
        let source = declaration(1, 1, 1, "source");
        let target = declaration(2, 1, 1, "target");
        let key = StableLinkKey {
            from: source.0,
            target: backend_semantic::ir::DeclarationLinkTarget::Local(target.0),
            kind: backend_semantic::ir::LinkKind::Calls,
        };
        let source_path = backend_engine::ProductText::new("src/lib.rs")
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let before_evidence = backend_engine::SemanticLinkEvidence {
            confidence: backend_engine::SemanticConfidence::Syntactic,
            source: Some(backend_engine::SemanticSourceSpan {
                file: source_path.clone(),
                start: 4,
                end: 10,
            }),
        };
        let after_evidence = backend_engine::SemanticLinkEvidence {
            confidence: backend_engine::SemanticConfidence::Compiler,
            source: Some(backend_engine::SemanticSourceSpan {
                file: source_path,
                start: 4,
                end: 10,
            }),
        };
        let mut before = snapshot([source, target]);
        before.links.push(SemanticLinkSummary {
            key,
            evidence: before_evidence.clone(),
        });
        let source = declaration(1, 1, 1, "source");
        let target = declaration(2, 1, 1, "target");
        let mut after = snapshot([source, target]);
        after.links.push(SemanticLinkSummary {
            key,
            evidence: after_evidence.clone(),
        });

        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label.as_str(), "source");
        assert_eq!(rows[0].change, backend_engine::DeclarationChange::Changed);
        assert_eq!(
            rows[0].before,
            Some(semantic_declaration_identity(key.from))
        );
        assert_eq!(rows[0].after, Some(semantic_declaration_identity(key.from)));
        assert!(matches!(
            rows[0].links.as_ref(),
            [backend_engine::SemanticLinkDelta::EvidenceChanged {
                target: backend_engine::SemanticLinkTarget::Local { .. },
                relation: backend_engine::SemanticLinkKind::Calls,
                before,
                after,
                ..
            }] if before == &before_evidence && after == &after_evidence
        ));
        Ok(())
    }

    #[test]
    fn overload_merge_retains_exact_matches_and_marks_only_churn_ambiguous()
    -> Result<(), BuiltinModelError> {
        let before = snapshot([
            declaration(1, 1, 1, "stable"),
            declaration(1, 2, 1, "removed"),
        ]);
        let after = snapshot([
            declaration(1, 1, 1, "stable"),
            declaration(1, 3, 1, "added"),
        ]);
        let rows = diff_semantic_snapshots(&before, &after)?;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| {
            row.label.as_str() == "removed"
                && row.change == backend_engine::DeclarationChange::Indeterminate
        }));
        assert!(rows.iter().any(|row| {
            row.label.as_str() == "added" && row.change == backend_engine::DeclarationChange::Added
        }));
        Ok(())
    }

    #[test]
    fn result_bound_is_enforced_during_the_linear_merge() {
        let before = snapshot([]);
        let after = snapshot(
            (0..=backend_engine::MAX_PRODUCT_ROWS)
                .map(|index| declaration(index as u16, 1, 1, "added")),
        );
        assert!(diff_semantic_snapshots(&before, &after).is_err());
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
        let callee_symbol =
            super::super::view_build::semantic_symbol(package, fixture_version(2).identity());
        let mut facts = Vec::new();
        append_reference_facts(package, &bytes, callee_symbol, &mut facts)
            .map_err(|error| error.to_string())?;
        if facts.len() != 1 {
            return Err(format!("expected one reference fact, got {}", facts.len()));
        }
        let fact = &facts[0];
        if fact.site
            != super::super::view_build::semantic_symbol(package, fixture_version(1).identity())
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
        let absent =
            super::super::view_build::semantic_symbol(package, fixture_version(9).identity());
        let mut facts = Vec::new();
        append_reference_facts(package, &bytes, absent, &mut facts)
            .map_err(|error| error.to_string())?;
        if !facts.is_empty() {
            return Err(format!("absent target produced {} facts", facts.len()));
        }
        Ok(())
    }
}
