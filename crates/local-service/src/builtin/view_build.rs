//! Typed source-to-document row projection.

use super::{
    BuiltinModelError, BuiltinSemanticRelation, IndexedSources, MAX_REBUILD_BYTES,
    MAX_REBUILD_PACKAGES, WorkspaceSnapshot, activate_semantic_publication,
};
use backend_engine::builtin::ProductSemanticPublicationRecord;
use backend_engine::{DeclarationKind, Fragment, Row, RowId, ViewRoot, product_source_file_key};
use compiler_application::{DocumentationFragment, DocumentationSession, LocalCompilerClient};
use compiler_ir::{
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

fn declaration_symbol(
    coordinate: &str,
    kind: DeclarationKind,
    signature: &str,
    occurrences: &mut BTreeMap<RowId, u32>,
) -> RowId {
    let canonical = RowId::Symbol(backend_engine::symbol_key(coordinate));
    let occurrence = occurrences.entry(canonical).or_default();
    let symbol = if *occurrence == 0 {
        canonical
    } else {
        RowId::Symbol(backend_engine::symbol_key(&format!(
            "{coordinate}\0{}\0{signature}\0{occurrence}",
            kind.name()
        )))
    };
    *occurrence = occurrence.saturating_add(1);
    symbol
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
}

pub(super) fn rows_for_indexed_sources(
    initial: &ViewRoot,
    sources: IndexedSources,
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
) -> Result<ProjectedRows, BuiltinModelError> {
    if sources.projects.len() > MAX_REBUILD_PACKAGES {
        return Err(BuiltinModelError(
            "workspace package rows exceed the rebuild row bound".to_owned(),
        ));
    }
    let semantics = semantic_rows(
        snapshot,
        compiler,
        &sources.projects,
        initial,
        MAX_REBUILD_PACKAGES - sources.projects.len(),
    )?;
    let source_capacity = projected_source_capacity(&sources, &semantics.complete)?;
    let total_capacity = source_capacity
        .checked_add(semantics.rows.len())
        .filter(|count| *count <= MAX_REBUILD_PACKAGES)
        .ok_or_else(|| {
            BuiltinModelError(
                "workspace semantic declarations exceed the rebuild row bound".to_owned(),
            )
        })?;
    let mut projection = SourceRowProjection::new(initial, &sources.projects, total_capacity)?;
    for (file_key, record) in sources.files {
        projection.append_file(file_key, &record, &semantics.complete)?;
    }
    let rows = projection.finish(semantics.rows)?;
    Ok(ProjectedRows {
        rows,
        activated: semantics.activated,
    })
}

struct SourceRowProjection<'a> {
    initial: &'a ViewRoot,
    projects: &'a BTreeMap<[u8; 32], super::IndexedProject>,
    rows: Vec<Row>,
    selected_files: BTreeSet<([u8; 32], [u8; 32])>,
    symbol_occurrences: BTreeMap<RowId, u32>,
}

impl<'a> SourceRowProjection<'a> {
    fn new(
        initial: &'a ViewRoot,
        projects: &'a BTreeMap<[u8; 32], super::IndexedProject>,
        capacity: usize,
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
            symbol_occurrences: BTreeMap::new(),
        })
    }

    fn append_file(
        &mut self,
        file_key: [u8; 32],
        record: &super::ProductSourceRecord,
        complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
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
        if semantic_profile_is_complete(complete, Some(project.package), path)? {
            return Ok(());
        }
        let module_coordinate = format!("{}::{path}", project.label);
        let module = backend_engine::symbol_key(&module_coordinate);
        for declaration in declarations.iter() {
            let coordinate = if declaration.kind() == DeclarationKind::Module {
                module_coordinate.clone()
            } else {
                format!(
                    "{}::{path}:{}::{}",
                    project.label,
                    declaration.line(),
                    declaration.name()
                )
            };
            let symbol = declaration_symbol(
                &coordinate,
                declaration.kind(),
                declaration.signature(),
                &mut self.symbol_occurrences,
            );
            let mut document = Vec::with_capacity(1);
            if declaration.kind() == DeclarationKind::Module {
                document.push(Fragment::Text(format!(
                    "{} source · {path}",
                    language.name()
                )));
            } else {
                let prose = if declaration.documentation().is_empty() {
                    format!(
                        "{} in {path}:{}",
                        declaration.kind_name(),
                        declaration.line()
                    )
                } else {
                    declaration.documentation().to_owned()
                };
                document.push(Fragment::Text(prose));
            }
            let mut row =
                Row::in_package(symbol, self.initial.basis(), project.package, coordinate)
                    .with_document(document)
                    .with_signature(declaration.signature())
                    .with_kind(declaration.kind())
                    .with_source(declaration.location().clone())
                    .with_excerpt(declaration.source_excerpt().clone());
            if symbol != RowId::Symbol(module) {
                row = row.with_parent(module);
            }
            self.rows.push(row);
        }
        Ok(())
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

fn semantic_profile_is_complete(
    complete: &BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
    package: Option<backend_engine::PackageKey>,
    path: &str,
) -> Result<bool, BuiltinModelError> {
    let Some(package) = package else {
        return Ok(false);
    };
    let profile =
        super::ingest::source_profile(std::path::Path::new(path)).map_err(BuiltinModelError)?;
    Ok(complete.contains(&(package.to_bytes(), profile)))
}

struct SemanticRows {
    rows: Vec<Row>,
    complete: BTreeSet<([u8; 32], backend_semantic::vocabulary::LanguageProfile)>,
    activated: BTreeSet<(
        backend_engine::PackageKey,
        backend_semantic::vocabulary::LanguageProfile,
    )>,
}

fn semantic_rows(
    snapshot: &WorkspaceSnapshot,
    compiler: &LocalCompilerClient,
    projects: &BTreeMap<[u8; 32], super::IndexedProject>,
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
    let mut activated_publications = BTreeSet::new();
    let mut symbols = BTreeSet::new();
    let mut remaining_bytes = MAX_REBUILD_BYTES;
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
            let ProductSemanticPublicationRecord::Published {
                coverage: backend_engine::builtin::SemanticPublicationCoverage::Complete,
                claim,
            } = record
            else {
                continue;
            };
            let project = projects.get(key.package_key().as_bytes()).ok_or_else(|| {
                BuiltinModelError(
                    "semantic publication refers to a missing package frontier".to_owned(),
                )
            })?;
            let activated = activate_semantic_publication(compiler, key, *claim)?;
            for image in activated.images() {
                let view = SemanticImageView::reopen(image.as_ref()).map_err(|error| {
                    BuiltinModelError(format!("reopen activated semantic image: {error}"))
                })?;
                let mut sink = SemanticRowSink {
                    initial,
                    symbols: &mut symbols,
                    rows: &mut rows,
                    capacity: row_capacity,
                    remaining_bytes: &mut remaining_bytes,
                };
                append_image_rows(&view, project, key.profile(), &mut sink)?;
            }
            complete.insert((project.package.to_bytes(), key.profile()));
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
        activated: activated_publications,
    })
}

struct SemanticRowSink<'a> {
    initial: &'a ViewRoot,
    symbols: &'a mut BTreeSet<RowId>,
    rows: &'a mut Vec<Row>,
    capacity: usize,
    remaining_bytes: &'a mut usize,
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
        let mut row = Row::in_package(
            RowId::Symbol(symbol),
            sink.initial.basis(),
            project.package,
            coordinate,
        )
        .with_kind(declaration_kind(entity.entity.kind))
        .with_document(content.document);
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
            sink.rows
                .push(Row::new(RowId::Symbol(symbol), sink.initial.basis(), label));
        }
    }
    Ok(())
}

fn semantic_row_content<Reader: compiler_ir::SemanticReader + ?Sized>(
    profile: backend_semantic::vocabulary::LanguageProfile,
    reader: &Reader,
    entity: &compiler_application::DocumentationEntity<'_, Reader>,
    package: backend_engine::PackageKey,
    image_identity: [u8; 32],
) -> Result<SemanticRowContent, BuiltinModelError> {
    let type_depth = NonZeroUsize::new(MAX_SEMANTIC_TYPE_DEPTH)
        .ok_or_else(|| BuiltinModelError("semantic type depth bound must be nonzero".to_owned()))?;
    let prepared = compiler_ir::prepare_semantic_document(
        profile,
        reader,
        entity.entity.id,
        compiler_ir::CanonicalTypeRenderLimits::new(type_depth),
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

fn documentation_fragment<Reader: compiler_ir::SemanticReader + ?Sized>(
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
                compiler_application::DocumentationTarget::Local(target) => {
                    semantic_symbol(package, target.entity.version.identity())
                }
                compiler_application::DocumentationTarget::External { id, .. } => {
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

fn semantic_signature<Reader: compiler_ir::SemanticReader + ?Sized>(
    entity: &compiler_application::DocumentationEntity<'_, Reader>,
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
        .prepare_canonical(compiler_ir::CanonicalTypeRenderLimits::new(type_depth))
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
    let mut occurrences = BTreeMap::new();
    for (_, record) in &sources.files {
        let file = record
            .file_fields()
            .ok_or_else(|| BuiltinModelError("expected structural source file".to_owned()))?;
        let project = sources.projects.get(&file.project).ok_or_else(|| {
            BuiltinModelError("structural source refers to a missing project".to_owned())
        })?;
        let profile = super::ingest::source_profile(std::path::Path::new(file.path))
            .map_err(BuiltinModelError)?;
        if complete.contains(&(project.package.to_bytes(), profile)) {
            continue;
        }
        let module_coordinate = format!("{}::{}", project.label, file.path);
        let module_symbol = backend_engine::symbol_key(&module_coordinate);
        let has_module = file
            .declarations
            .iter()
            .any(|declaration| declaration.kind() == DeclarationKind::Module);
        for declaration in file.declarations.iter() {
            let coordinate = if declaration.kind() == DeclarationKind::Module {
                module_coordinate.clone()
            } else {
                format!(
                    "{}::{}:{}::{}",
                    project.label,
                    file.path,
                    declaration.line(),
                    declaration.name()
                )
            };
            let id = declaration_symbol(
                &coordinate,
                declaration.kind(),
                declaration.signature(),
                &mut occurrences,
            )
            .stable_key();
            let documentation = if declaration.kind() == DeclarationKind::Module {
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
            let parent = (declaration.kind() != DeclarationKind::Module && has_module)
                .then(|| RowId::Symbol(module_symbol).stable_key());
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
    use super::semantic_profile_is_complete;
    use backend_semantic::vocabulary::{CStandard, LanguageProfile};
    use std::collections::BTreeSet;

    #[test]
    fn complete_c_publication_does_not_suppress_cxx_fallback() -> Result<(), String> {
        let package = backend_engine::package_key("mixed-c-cxx");
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
}
