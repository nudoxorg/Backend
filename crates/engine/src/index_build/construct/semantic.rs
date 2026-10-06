//! Direct index projection from a durable full semantic image.

use core::mem::MaybeUninit;

use crate::publication::{OpenedSemanticArtifact, semantic_immutable::SemanticImageArtifactFacts};
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, ExactRow, ExactSegment, LexicalOrderKey, LexicalRow,
    LexicalScore, LexicalSegment,
};
use backend_semantic::ir::{
    AnonymousCallableAnchorView, ItemName, ItemNameView, SemanticCoreReader, SemanticReader,
};

use super::{
    BuildAdmissionError, BuildDerivationError, BuildError, BuildRegion, DerivationResult,
    ENTITY_NAME_SCORE_UNITS, MAX_INDEX_ROWS, PreparedIndex, PreparedIndexView, initialize,
    selected_region,
};
use crate::index_build::fact::{EntityFact, LinkKinds, SemanticTypeFact};

/// Caller-owned regions for allocation-free semantic-image index projection.
pub struct SemanticIndexBuildScratch<'output> {
    /// Canonical semantic entity facts.
    pub entities: &'output mut [MaybeUninit<EntityFact<'output>>],
    /// Exact rows keyed by semantic-image identity and entity coordinate.
    pub exact_rows: &'output mut [MaybeUninit<ExactRow<'output>>],
    /// Lexical rows borrowing names from the semantic image.
    pub lexical_rows: &'output mut [MaybeUninit<LexicalRow<'output>>],
}

/// Exact no-write capacity of semantic index output regions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticIndexBuildCapacity {
    /// Available semantic entity slots.
    pub entities: usize,
    /// Available exact row slots.
    pub exact_rows: usize,
    /// Available lexical row slots.
    pub lexical_rows: usize,
}

impl SemanticIndexBuildScratch<'_> {
    /// Reports complete reusable capacity without writing caller storage.
    #[must_use]
    pub const fn capacity(&self) -> SemanticIndexBuildCapacity {
        SemanticIndexBuildCapacity {
            entities: self.entities.len(),
            exact_rows: self.exact_rows.len(),
            lexical_rows: self.lexical_rows.len(),
        }
    }
}

/// Checks all semantic index output bounds before the first write.
pub fn preflight_semantic(
    artifact: &OpenedSemanticArtifact<'_, '_>,
    capacity: SemanticIndexBuildCapacity,
) -> Result<(), BuildAdmissionError> {
    SemanticRequired::from_artifact(artifact).check(capacity)
}

/// Builds exact and lexical segments directly from the complete semantic reader.
#[allow(
    clippy::result_large_err,
    reason = "cold segment failures retain exact borrowed rows and typed derivation causes"
)]
pub fn build_semantic<'opened: 'output, 'semantic: 'output, 'output>(
    artifact: &'opened OpenedSemanticArtifact<'_, 'semantic>,
    scratch: SemanticIndexBuildScratch<'output>,
) -> Result<PreparedIndex<'output>, BuildError<'output>> {
    let required = SemanticRequired::from_artifact(artifact);
    required.check(scratch.capacity())?;
    let SemanticIndexBuildScratch {
        entities,
        exact_rows,
        lexical_rows,
    } = scratch;
    let entities = selected_region(entities, required.entities, BuildRegion::Entities)?;
    let exact_rows = selected_region(exact_rows, required.entities, BuildRegion::ExactRows)?;
    let lexical_rows = selected_region(
        lexical_rows,
        required.lexical_rows,
        BuildRegion::LexicalRows,
    )?;
    let image = artifact
        .fragment
        .facts
        .semantic_image
        .ok_or(BuildDerivationError::MissingSemanticImage)?;
    let entities = derive_semantic_entities(entities, artifact, image)?;
    let exact_rows = initialize(
        BuildRegion::ExactRows,
        exact_rows,
        entities.iter(),
        |entity| {
            Ok(ExactRow::present(
                entity.exact_key.as_ref(),
                entity.exact_value.as_ref(),
            ))
        },
    )?;
    let exact =
        ExactSegment::new(exact_rows.into_shared()).map_err(|cause| BuildError::Exact { cause })?;

    let lexical_entities = entities
        .iter()
        .filter_map(|entity| entity.name.named_bytes().map(|name| (entity, name)))
        .collect::<Vec<_>>();
    let mut lexical_rows = initialize(
        BuildRegion::LexicalRows,
        lexical_rows,
        lexical_entities.into_iter(),
        |(entity, name)| {
            Ok(LexicalRow::new(
                name,
                EntityDocumentId {
                    artifact: EntityArtifactIdentity::Semantic(image.identity),
                    entity: entity.entity,
                },
                LexicalScore::from(ENTITY_NAME_SCORE_UNITS),
            ))
        },
    )?;
    lexical_rows.sort_unstable_by_key(|row| LexicalOrderKey::from(*row));
    let lexical = LexicalSegment::new(lexical_rows.into_shared())
        .map_err(|cause| BuildError::Lexical { cause })?;

    Ok(PreparedIndex(PreparedIndexView {
        fragment: artifact.fragment.facts,
        entities,
        exact,
        lexical,
    }))
}

fn derive_semantic_entities<'opened: 'output, 'semantic: 'output, 'output>(
    output: &'output mut [MaybeUninit<EntityFact<'output>>],
    artifact: &'opened OpenedSemanticArtifact<'_, 'semantic>,
    image: SemanticImageArtifactFacts,
) -> DerivationResult<&'output [EntityFact<'output>]> {
    initialize(
        BuildRegion::Entities,
        output,
        artifact.semantic_image.canonical_entities(),
        |entity| {
            let name = match entity.name {
                ItemName::Named(atom) => {
                    ItemNameView::Named(artifact.semantic_image.atom(atom).ok_or(
                        BuildDerivationError::MissingAtom {
                            entity: entity.id,
                            name: atom,
                        },
                    )?)
                }
                ItemName::AnonymousCallable(atom) => {
                    let bytes = artifact.semantic_image.atom(atom).ok_or(
                        BuildDerivationError::MissingAtom {
                            entity: entity.id,
                            name: atom,
                        },
                    )?;
                    let anchor = AnonymousCallableAnchorView::try_from_encoded(bytes).ok_or(
                        BuildDerivationError::InvalidAnonymousCallableAnchor { entity: entity.id },
                    )?;
                    ItemNameView::AnonymousCallable { anchor }
                }
            };
            let semantic_type = match entity.semantic_type {
                Some(coordinate) => {
                    let ty = artifact.semantic_image.ty(coordinate).ok_or(
                        BuildDerivationError::MissingTypeNode {
                            entity: entity.id,
                            semantic_type: coordinate,
                        },
                    )?;
                    Some(SemanticTypeFact {
                        coordinate,
                        class: ty.tag(),
                    })
                }
                None => None,
            };
            let links = artifact
                .semantic_image
                .links_from(entity.id)
                .fold(LinkKinds::NONE, |kinds, (_, link)| kinds.insert(link.kind));
            Ok(EntityFact::new_semantic(
                EntityDocumentId {
                    artifact: EntityArtifactIdentity::Semantic(image.identity),
                    entity: entity.id,
                },
                entity.id,
                name,
                entity.kind,
                semantic_type,
                links,
            ))
        },
    )
    .map(|initialized| initialized.into_shared())
}

#[derive(Clone, Copy)]
struct SemanticRequired {
    entities: usize,
    lexical_rows: usize,
}

impl SemanticRequired {
    fn from_artifact(artifact: &OpenedSemanticArtifact<'_, '_>) -> Self {
        let entities = artifact.semantic_image.canonical_entities();
        Self {
            entities: entities.len(),
            lexical_rows: entities
                .filter(|entity| matches!(entity.name, ItemName::Named(_)))
                .count(),
        }
    }

    fn check(self, capacity: SemanticIndexBuildCapacity) -> Result<(), BuildAdmissionError> {
        if self.entities > MAX_INDEX_ROWS {
            return Err(BuildAdmissionError::EntityLimit {
                maximum: MAX_INDEX_ROWS,
                observed: self.entities,
            });
        }
        for (region, required, available) in [
            (BuildRegion::Entities, self.entities, capacity.entities),
            (BuildRegion::ExactRows, self.entities, capacity.exact_rows),
            (
                BuildRegion::LexicalRows,
                self.lexical_rows,
                capacity.lexical_rows,
            ),
        ] {
            if available < required {
                return Err(BuildAdmissionError::OutputTooSmall {
                    region,
                    required,
                    available,
                });
            }
        }
        Ok(())
    }
}
