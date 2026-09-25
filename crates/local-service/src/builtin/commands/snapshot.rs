use super::super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinModelError, BuiltinSemanticRelation,
    BuiltinValidator, activate_semantic_publication,
};
use backend_engine::application::LocalCompilerClient;
use backend_engine::builtin::{ProductSemanticPublicationRecord, SemanticPublicationCoverage};
use backend_semantic::ir::{
    SemanticReader as _, SemanticSnapshot, SemanticStableLinks, StableLinkKey,
};
use std::mem::size_of;
pub(super) struct SemanticDeclaration<'view> {
    pub(super) label: &'view str,
    pub(super) version: backend_semantic::ir::EntityVersion,
    pub(super) parent: Option<backend_semantic::ir::DeclarationIdentity>,
}

pub(super) struct SemanticPackageSnapshot<'view> {
    pub(super) declarations: Vec<(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'view>,
    )>,
    pub(super) links: Vec<SemanticLinkSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SemanticLinkSummary {
    pub(super) key: StableLinkKey,
    pub(super) evidence: backend_engine::SemanticLinkEvidence,
}

const MAX_DIFF_DECLARATIONS: usize = (super::super::MAX_REBUILD_BYTES / 4)
    / size_of::<(
        backend_semantic::ir::DeclarationIdentity,
        SemanticDeclaration<'static>,
    )>();
const MAX_DIFF_LINKS: usize =
    (super::super::MAX_REBUILD_BYTES / 4) / size_of::<SemanticLinkSummary>();

pub(super) fn semantic_package_snapshot<'view>(
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
        let symbol = super::super::view_build::semantic_symbol(package, identity);
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

pub(super) fn semantic_link_evidence<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
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

pub(super) const fn semantic_confidence(
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

pub(super) const fn semantic_declaration_identity(
    identity: backend_semantic::ir::DeclarationIdentity,
) -> backend_engine::SemanticDeclarationIdentity {
    backend_engine::SemanticDeclarationIdentity {
        family: *identity.family.as_bytes(),
        variant: *identity.variant.as_bytes(),
    }
}

pub(super) const fn semantic_link_kind(
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

pub(super) fn semantic_link_target(
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
