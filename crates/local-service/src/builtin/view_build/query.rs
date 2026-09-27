use super::super::{
    BuiltinModelError, BuiltinSemanticRelation, IndexedSources, MAX_REBUILD_BYTES,
    MAX_REBUILD_PACKAGES, WorkspaceSnapshot, activate_semantic_publication,
};
use super::MAX_SEMANTIC_DOCUMENT_BYTES;
use super::MAX_SEMANTIC_QUERY_ROWS;
use super::call_join::{
    ProjectCallableIndex, foreign_display_name, join_project_call, join_project_field,
    join_project_mention, join_project_value, project_paths_for_package,
};
use super::compiled_source_path;
use super::identity::{
    declaration_kind, external_semantic_symbol, fragment_text, query_external_id, query_package_id,
    query_semantic_id, semantic_coordinate, semantic_symbol,
};
use super::semantic::semantic_row_content;
use super::structural::{StructuralParent, StructuralProjectionPlan};
use backend_engine::application::{DocumentationSession, LocalCompilerClient};
use backend_engine::builtin::ProductSemanticPublicationRecord;
use backend_engine::{Fragment, RowId};
use backend_semantic::ir::{
    DeclarationIdentity, ExternalId, ExternalTargetIdentity, LinkTarget, SemanticCoreReader as _,
    SemanticImageView, SemanticReader as _,
};
use std::collections::{BTreeMap, BTreeSet};

pub(in crate::builtin) fn semantic_query_corpus(
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    sources: &IndexedSources,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut super::image_rows::ImageRowResidence,
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

    let complete = append_compiler_query_facts(
        snapshot,
        compiler,
        sources,
        &mut facts,
        generations,
        image_rows,
    )?;
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
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut super::image_rows::ImageRowResidence,
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
            let activated =
                activate_semantic_publication(compiler, key, *claim, generations, image_rows)?;
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

    let opened = open_query_publications(&pending)?;
    let mut package_published =
        BTreeMap::<backend_engine::PackageKey, BTreeSet<DeclarationIdentity>>::new();
    let mut package_paths = BTreeMap::<backend_engine::PackageKey, BTreeSet<String>>::new();
    let mut package_views =
        BTreeMap::<backend_engine::PackageKey, Vec<&SemanticImageView<'_>>>::new();
    for publication in &opened {
        package_paths
            .entry(publication.package)
            .or_insert_with(|| project_paths_for_package(sources, publication.package));
        let published = package_published.entry(publication.package).or_default();
        let views = package_views.entry(publication.package).or_default();
        for image in &publication.images {
            published.extend(image.identities.iter().copied());
            views.push(&image.view);
        }
    }
    let mut package_indexes = BTreeMap::<backend_engine::PackageKey, ProjectCallableIndex>::new();
    for (package, views) in &package_views {
        package_indexes.insert(*package, ProjectCallableIndex::build_from_views(views)?);
    }

    let mut ids = BTreeSet::new();
    for publication in &opened {
        let package = publication.package;
        let label = publication.label;
        let coordinate = publication.coordinate;
        let profile = publication.profile;
        let project_paths = package_paths.get(&package).cloned().unwrap_or_default();
        let published = package_published.get(&package).cloned().unwrap_or_default();
        let callable_index = package_indexes.get(&package);
        for opened_image in &publication.images {
            let image = &opened_image.view;
            let caller_path = &opened_image.path;
            let session = DocumentationSession::new(image);
            let identities = &opened_image.identities;
            let image_digest = opened_image.digest;
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
                        BuiltinModelError("semantic query declaration name is not UTF-8".to_owned())
                    })?
                    .to_owned();
                let content = semantic_row_content(profile, image, &entity, package, image_digest)?;
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
                                    image,
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
                                    image,
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
                                    image,
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
                                    image,
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
                            let identity = ExternalTargetIdentity::capture(image, external)
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
                let name = match foreign_display_name(image, external)? {
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

struct OpenedQueryImage<'a> {
    view: SemanticImageView<'a>,
    path: String,
    digest: [u8; 32],
    identities: BTreeSet<DeclarationIdentity>,
}

struct OpenedQueryPublication<'a> {
    package: backend_engine::PackageKey,
    label: &'a str,
    coordinate: &'a backend_semantic::vocabulary::PackageUrl,
    profile: backend_semantic::vocabulary::LanguageProfile,
    images: Vec<OpenedQueryImage<'a>>,
}

fn open_query_publications<'a>(
    pending: &'a [PendingQueryPublication],
) -> Result<Vec<OpenedQueryPublication<'a>>, BuiltinModelError> {
    let mut opened = Vec::with_capacity(pending.len());
    for publication in pending {
        let mut images = Vec::new();
        let mut rest = publication.activated.images();
        while let Some((bytes, next)) = rest.split_first() {
            let view = bytes.reopen().map_err(|error| {
                BuiltinModelError(format!("reopen semantic query image: {error}"))
            })?;
            let path = compiled_source_path(&view)?;
            let digest = *blake3::hash(bytes.as_ref()).as_bytes();
            let mut identities = BTreeSet::new();
            let session = DocumentationSession::new(&view);
            for entity in session.canonical_entities() {
                let entity = entity.map_err(|error| {
                    BuiltinModelError(format!("read semantic query declaration: {error}"))
                })?;
                identities.insert(entity.entity.version.identity());
            }
            images.push(OpenedQueryImage {
                view,
                path,
                digest,
                identities,
            });
            rest = next;
        }
        opened.push(OpenedQueryPublication {
            package: publication.package,
            label: publication.label.as_str(),
            coordinate: &publication.coordinate,
            profile: publication.profile,
            images,
        });
    }
    Ok(opened)
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

/// Times one query-image validation and index build against the three
/// validations the corpus used to pay before emitting facts.
///
/// Images are built before the timer. `once` validates each image, records
/// its declarations, and indexes the open views. `triple` repeats the old
/// prefix: an identity validation, an index validation, and the validation
/// the fact pass used to perform. Fact construction sits outside both timers.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_semantic_query_walk() {
    const IMAGES: usize = 32;
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let images = (0..IMAGES)
        .map(|index| {
            let path = format!("src/file{index}.rs");
            let salt = u8::try_from(index).expect("image index");
            super::image_reopen::fixture_semantic_image_salted(&path, salt).expect("fixture")
        })
        .collect::<Vec<_>>();
    let once_proof = index_open_views(&images).expect("once");
    let triple_proof = index_reopened_views(&images).expect("triple");
    (once_proof == IMAGES * 4 && triple_proof == once_proof)
        .then_some(())
        .expect("query walk dropped a declaration");
    let once = sample(WARMUPS, SAMPLES, || {
        index_open_views(&images).expect("once")
    });
    let triple = sample(WARMUPS, SAMPLES, || {
        index_reopened_views(&images).expect("triple")
    });
    let (once_median, once_p95) = percentiles(&once);
    let (triple_median, triple_p95) = percentiles(&triple);
    println!(
        "semantic_query_walk images={IMAGES} declarations={once_proof} once_median_ns={once_median} once_p95_ns={once_p95} triple_median_ns={triple_median} triple_p95_ns={triple_p95}"
    );
}

fn index_open_views(images: &[Vec<u8>]) -> Result<usize, BuiltinModelError> {
    let mut opened = Vec::with_capacity(images.len());
    let mut declarations = 0;
    for bytes in images {
        let view = SemanticImageView::reopen(bytes)
            .map_err(|error| BuiltinModelError(format!("reopen semantic query image: {error}")))?;
        let path = compiled_source_path(&view)?;
        let digest = *blake3::hash(bytes).as_bytes();
        let session = DocumentationSession::new(&view);
        for entity in session.canonical_entities() {
            let entity = entity.map_err(|error| {
                BuiltinModelError(format!("read semantic query declaration: {error}"))
            })?;
            std::hint::black_box(entity.entity.version.identity());
            declarations += 1;
        }
        std::hint::black_box((path, digest));
        opened.push(view);
    }
    let views = opened.iter().collect::<Vec<_>>();
    let _ = ProjectCallableIndex::build_from_views(&views)?;
    Ok(declarations)
}

fn index_reopened_views(images: &[Vec<u8>]) -> Result<usize, BuiltinModelError> {
    let mut declarations = 0;
    let mut borrowed = Vec::with_capacity(images.len());
    for bytes in images {
        let view = SemanticImageView::reopen(bytes)
            .map_err(|error| BuiltinModelError(format!("reopen semantic query image: {error}")))?;
        let session = DocumentationSession::new(&view);
        for entity in session.canonical_entities() {
            let entity = entity.map_err(|error| {
                BuiltinModelError(format!("read semantic query declaration: {error}"))
            })?;
            std::hint::black_box(entity.entity.version.identity());
            declarations += 1;
        }
        borrowed.push(bytes.as_slice());
    }
    let _ = ProjectCallableIndex::build_from_bytes(&borrowed)?;
    for bytes in images {
        let view = SemanticImageView::reopen(bytes)
            .map_err(|error| BuiltinModelError(format!("reopen semantic query image: {error}")))?;
        let path = compiled_source_path(&view)?;
        let digest = *blake3::hash(bytes).as_bytes();
        let session = DocumentationSession::new(&view);
        for entity in session.canonical_entities() {
            let entity = entity.map_err(|error| {
                BuiltinModelError(format!("read semantic query declaration: {error}"))
            })?;
            std::hint::black_box(entity.entity.version.identity());
        }
        std::hint::black_box((path, digest));
    }
    Ok(declarations)
}

fn sample<T>(warmups: usize, samples: usize, mut body: impl FnMut() -> T) -> Vec<u128> {
    for _ in 0..warmups {
        let _ = body();
    }
    let mut samples_ns = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = std::time::Instant::now();
        let _ = body();
        samples_ns.push(started.elapsed().as_nanos());
    }
    samples_ns
}

#[allow(clippy::indexing_slicing)]
fn percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::ProjectCallableIndex;
    use backend_semantic::ir::{ItemKind, SemanticImageView};
    use std::collections::BTreeSet;

    #[test]
    fn an_open_view_indexes_the_same_declaration_as_its_bytes() {
        let bytes =
            super::super::image_reopen::fixture_semantic_image("src/worker.rs").expect("fixture");
        let from_bytes = ProjectCallableIndex::build_from_bytes(&[&bytes]).expect("bytes");
        let view = SemanticImageView::reopen(&bytes).expect("reopen");
        let from_view = ProjectCallableIndex::build_from_views(&[&view]).expect("view");
        let paths = BTreeSet::from(["src/worker.rs".to_owned()]);
        let byte_worker = from_bytes
            .resolve_mention(&paths, "Worker", ItemKind::Record)
            .expect("byte worker");
        let view_worker = from_view
            .resolve_mention(&paths, "Worker", ItemKind::Record)
            .expect("view worker");
        assert_eq!(byte_worker, view_worker);
        let byte_field = from_bytes
            .resolve_mention(&paths, "name", ItemKind::Field)
            .expect("byte field");
        let view_field = from_view
            .resolve_mention(&paths, "name", ItemKind::Field)
            .expect("view field");
        assert_eq!(byte_field, view_field);
    }

    #[test]
    fn two_open_files_keep_distinct_workers() {
        let first = super::super::image_reopen::fixture_semantic_image_salted("src/a.rs", 0)
            .expect("first");
        let second = super::super::image_reopen::fixture_semantic_image_salted("src/b.rs", 1)
            .expect("second");
        let first_view = SemanticImageView::reopen(&first).expect("first view");
        let second_view = SemanticImageView::reopen(&second).expect("second view");
        let index =
            ProjectCallableIndex::build_from_views(&[&first_view, &second_view]).expect("index");
        let first_paths = BTreeSet::from(["src/a.rs".to_owned()]);
        let second_paths = BTreeSet::from(["src/b.rs".to_owned()]);
        let first_worker = index
            .resolve_mention(&first_paths, "Worker", ItemKind::Record)
            .expect("first worker");
        let second_worker = index
            .resolve_mention(&second_paths, "Worker", ItemKind::Record)
            .expect("second worker");
        assert_ne!(first_worker, second_worker);
        let both = BTreeSet::from(["src/a.rs".to_owned(), "src/b.rs".to_owned()]);
        assert!(
            index
                .resolve_mention(&both, "Worker", ItemKind::Record)
                .is_none()
        );
    }
}
