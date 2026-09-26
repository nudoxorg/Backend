use super::super::{
    BuiltinModelError, BuiltinSemanticRelation, IndexedSources, MAX_REBUILD_BYTES,
    MAX_REBUILD_PACKAGES, WorkspaceSnapshot, activate_semantic_publication,
};
use super::MAX_SEMANTIC_DOCUMENT_BYTES;
use super::MAX_SEMANTIC_QUERY_ROWS;
use super::identity::{
    declaration_kind, external_semantic_symbol, fragment_text, query_external_id, query_package_id,
    query_semantic_id, semantic_coordinate, semantic_symbol,
};
use super::call_join::{
    ProjectCallableIndex, foreign_display_name, join_project_call, join_project_field,
    join_project_mention, join_project_value, project_paths_for_package,
};
use super::compiled_source_path;
use super::semantic::semantic_row_content;
use super::structural::{StructuralParent, StructuralProjectionPlan};
use backend_engine::application::{DocumentationSession, LocalCompilerClient};
use backend_engine::builtin::ProductSemanticPublicationRecord;
use backend_engine::{Fragment, RowId};
use backend_semantic::ir::{
    DeclarationIdentity, ExternalId, ExternalTargetIdentity, LinkTarget,
    SemanticCoreReader as _, SemanticImageView, SemanticReader as _,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn semantic_query_corpus(
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
    let structural_plan = StructuralProjectionPlan::of(sources, &complete)?;
    append_structural_query_facts(sources, &structural_plan, &mut facts)?;
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
) -> Result<BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>, BuiltinModelError>
{
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic query relation: {error}")))?;
    let mut complete = BTreeSet::new();
    let mut pending = Vec::new();
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
            pending.push(PendingQueryPublication {
                package: project.package,
                label: project.label.clone(),
                coordinate: key.coordinate().clone(),
                profile: key.profile(),
                activated,
            });
            complete.insert((
                project.package.to_bytes(),
                super::super::ingest::lane_profile(key.profile()),
            ));
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }

    let mut package_bytes = BTreeMap::<backend_engine::PackageKey, Vec<&[u8]>>::new();
    let mut package_published =
        BTreeMap::<backend_engine::PackageKey, BTreeSet<DeclarationIdentity>>::new();
    let mut package_paths = BTreeMap::<backend_engine::PackageKey, BTreeSet<String>>::new();
    for pending_publication in &pending {
        package_paths
            .entry(pending_publication.package)
            .or_insert_with(|| project_paths_for_package(sources, pending_publication.package));
        let published = package_published
            .entry(pending_publication.package)
            .or_default();
        for image_bytes in pending_publication.activated.images() {
            let bytes = image_bytes.as_ref();
            package_bytes
                .entry(pending_publication.package)
                .or_default()
                .push(bytes);
            let image = SemanticImageView::reopen(bytes).map_err(|error| {
                BuiltinModelError(format!("reopen semantic query image: {error}"))
            })?;
            let session = DocumentationSession::new(&image);
            for entity in session.canonical_entities() {
                let entity = entity.map_err(|error| {
                    BuiltinModelError(format!("read semantic query declaration: {error}"))
                })?;
                published.insert(entity.entity.version.identity());
            }
        }
    }
    let mut package_indexes = BTreeMap::<backend_engine::PackageKey, ProjectCallableIndex>::new();
    for (package, bytes) in &package_bytes {
        package_indexes.insert(*package, ProjectCallableIndex::build_from_bytes(bytes)?);
    }

    let mut ids = BTreeSet::new();
    for pending_publication in pending {
        let PendingQueryPublication {
            package,
            label,
            coordinate,
            profile,
            activated,
        } = pending_publication;
        let project_paths = package_paths
            .get(&package)
            .cloned()
            .unwrap_or_default();
        let published = package_published
            .get(&package)
            .cloned()
            .unwrap_or_default();
        let callable_index = package_indexes.get(&package);
        for image_bytes in activated.images() {
            let image = SemanticImageView::reopen(image_bytes.as_ref()).map_err(|error| {
                BuiltinModelError(format!("reopen semantic query image: {error}"))
            })?;
            let caller_path = compiled_source_path(&image)?;
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
            let mut external_targets =
                BTreeMap::<String, (ExternalTargetIdentity, ExternalId)>::new();
            for entity in session.canonical_entities() {
                let entity = entity.map_err(|error| {
                    BuiltinModelError(format!("read semantic query declaration: {error}"))
                })?;
                let identity = entity.entity.version.identity();
                let id = query_semantic_id(package, identity);
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
                    profile,
                    &image,
                    &entity,
                    package,
                    image_digest,
                )?;
                let parent = entity
                    .entity
                    .parent
                    .map(|parent| {
                        let parent = session.entity(parent).map_err(|error| {
                            BuiltinModelError(format!(
                                "read semantic query parent declaration: {error}"
                            ))
                        })?;
                        let parent_identity = parent.entity.version.identity();
                        if !identities.contains(&parent_identity) {
                            return Err(BuiltinModelError(
                                "semantic query parent is outside the canonical entity closure"
                                    .to_owned(),
                            ));
                        }
                        Ok(query_semantic_id(package, parent_identity))
                    })
                    .transpose()?;
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
                                related.push(query_semantic_id(package, target));
                            }
                        }
                        LinkTarget::External(external) => {
                            if let Some(callable_index) = callable_index {
                                if let Some(joined) = join_project_call(
                                    &image,
                                    link.kind,
                                    external,
                                    &caller_path,
                                    &project_paths,
                                    callable_index,
                                    &published,
                                )? {
                                    related.push(query_semantic_id(package, joined));
                                    continue;
                                }
                                if let Some(joined) = join_project_mention(
                                    &image,
                                    link.kind,
                                    external,
                                    &caller_path,
                                    &project_paths,
                                    callable_index,
                                    &published,
                                )? {
                                    related.push(query_semantic_id(package, joined));
                                    continue;
                                }
                                if let Some(joined) = join_project_field(
                                    &image,
                                    link.kind,
                                    external,
                                    &caller_path,
                                    &project_paths,
                                    callable_index,
                                    &published,
                                )? {
                                    related.push(query_semantic_id(package, joined));
                                    continue;
                                }
                                if let Some(joined) = join_project_value(
                                    &image,
                                    link.kind,
                                    external,
                                    &caller_path,
                                    &project_paths,
                                    callable_index,
                                    &published,
                                )? {
                                    related.push(query_semantic_id(package, joined));
                                    continue;
                                }
                            }
                            let identity = ExternalTargetIdentity::capture(&image, external)
                                .map_err(|error| {
                                    BuiltinModelError(format!(
                                        "identify semantic query external target: {error}"
                                    ))
                                })?;
                            let target_id = query_external_id(package, image_digest, identity);
                            external_targets
                                .entry(target_id.clone())
                                .or_insert((identity, external));
                            related.push(target_id);
                        }
                    }
                }
                related.sort_unstable();
                related.dedup();
                facts.push(backend_extension_trustfall::SemanticQueryFact::new(
                    backend_extension_trustfall::SemanticQueryEvidence::Compiler(
                        backend_extension_trustfall::CompilerSemanticEvidence::new(
                            package,
                            coordinate.clone(),
                            profile,
                            identity,
                            image_digest,
                            image.image_facts(),
                        ),
                    ),
                    backend_extension_trustfall::SemanticQueryPresentation {
                        id,
                        kind: declaration_kind(entity.entity.kind).name().to_owned(),
                        coordinate: semantic_coordinate(&label, identity, &name),
                        name,
                        signature: content.signature,
                        documentation: fragment_text(&content.document),
                        score: None,
                        project: Some(query_package_id(package)),
                        parent,
                        related: related.into_boxed_slice(),
                    },
                ));
            }
            for (id, (target, external)) in external_targets {
                if !ids.insert(id.clone()) {
                    return Err(BuiltinModelError(
                        "semantic query publication contains a duplicate external target identity"
                            .to_owned(),
                    ));
                }
                let name = match foreign_display_name(&image, external)? {
                    Some(display) => display,
                    None => "external semantic target".to_owned(),
                };
                facts.push(backend_extension_trustfall::SemanticQueryFact::new(
                    backend_extension_trustfall::SemanticQueryEvidence::CompilerExternalTarget(
                        backend_extension_trustfall::CompilerExternalTargetEvidence::new(
                            package,
                            coordinate.clone(),
                            profile,
                            target,
                            image_digest,
                            image.image_facts(),
                        ),
                    ),
                    backend_extension_trustfall::SemanticQueryPresentation {
                        coordinate: format!("{label}::external::{id}"),
                        id,
                        kind: "external".to_owned(),
                        name,
                        signature: None,
                        documentation: String::new(),
                        score: None,
                        project: Some(query_package_id(package)),
                        parent: None,
                        related: Box::new([]),
                    },
                ));
            }
        }
    }
    Ok(complete)
}

struct PendingQueryPublication {
    package: backend_engine::PackageKey,
    label: String,
    coordinate: backend_semantic::vocabulary::PackageUrl,
    profile: backend_semantic::vocabulary::LanguageProfile,
    activated: super::super::ActivatedProductSemantics,
}


pub(super) fn append_structural_query_facts(
    sources: &IndexedSources,
    structural_plan: &StructuralProjectionPlan,
    facts: &mut Vec<backend_extension_trustfall::SemanticQueryFact>,
) -> Result<(), BuiltinModelError> {
    for (file_key, record) in &sources.files {
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
        let Some(profile) = super::super::ingest::source_profile(std::path::Path::new(file.path))
            .map_err(BuiltinModelError)?
        else {
            continue;
        };
        if structural_plan.file(*file_key).is_none() {
            continue;
        }
        for (index, declaration) in file.declarations.iter().enumerate() {
            let prepared = structural_plan.declaration(*file_key, index)?;
            let id = prepared.id.stable_key();
            let coordinate = prepared.coordinate.clone();
            let documentation = if prepared.is_file_module {
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
            let parent = prepared
                .parent
                .as_deref()
                .map(|coordinate| structural_plan.parent_id(*file_key, coordinate))
                .transpose()?
                .map(|parent| match parent {
                    StructuralParent::Symbol(id) => id.stable_key(),
                    StructuralParent::Package(package) => RowId::Package(package).stable_key(),
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
