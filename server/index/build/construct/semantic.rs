//! Direct index projection from a durable full semantic image.

use core::mem::MaybeUninit;

use compiler_ir::{SemanticCoreReader, SemanticReader};
use compiler_publication::{
    OpenedSemanticArtifact, semantic_immutable::SemanticImageArtifactFacts,
};
use server_index_core::{
    EntityArtifactIdentity, EntityDocumentId, ExactRow, ExactSegment, LexicalOrderKey, LexicalRow,
    LexicalScore, LexicalSegment,
};

use super::{
    BuildAdmissionError, BuildDerivationError, BuildError, BuildRegion, DerivationResult,
    ENTITY_NAME_SCORE_UNITS, MAX_INDEX_ROWS, PreparedIndex, PreparedIndexView, initialize,
    selected_region,
};
use crate::fact::EntityFact;

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
    let lexical_rows = selected_region(lexical_rows, required.entities, BuildRegion::LexicalRows)?;
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

    let mut lexical_rows = initialize(
        BuildRegion::LexicalRows,
        lexical_rows,
        entities.iter(),
        |entity| {
            Ok(LexicalRow::new(
                entity.name,
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
            let name = artifact.semantic_image.atom(entity.name).ok_or(
                BuildDerivationError::MissingAtom {
                    entity: entity.id,
                    name: entity.name,
                },
            )?;
            Ok(EntityFact::new_semantic(
                EntityDocumentId {
                    artifact: EntityArtifactIdentity::Semantic(image.identity),
                    entity: entity.id,
                },
                entity.id,
                name,
                entity.kind,
                entity.semantic_type,
            ))
        },
    )
    .map(|initialized| initialized.into_shared())
}

#[derive(Clone, Copy)]
struct SemanticRequired {
    entities: usize,
}

impl SemanticRequired {
    fn from_artifact(artifact: &OpenedSemanticArtifact<'_, '_>) -> Self {
        Self {
            entities: artifact.semantic_image.canonical_entities().len(),
        }
    }

    fn check(self, capacity: SemanticIndexBuildCapacity) -> Result<(), BuildAdmissionError> {
        if self.entities > MAX_INDEX_ROWS {
            return Err(BuildAdmissionError::EntityLimit {
                maximum: MAX_INDEX_ROWS,
                observed: self.entities,
            });
        }
        for (region, available) in [
            (BuildRegion::Entities, capacity.entities),
            (BuildRegion::ExactRows, capacity.exact_rows),
            (BuildRegion::LexicalRows, capacity.lexical_rows),
        ] {
            if available < self.entities {
                return Err(BuildAdmissionError::OutputTooSmall {
                    region,
                    required: self.entities,
                    available,
                });
            }
        }
        Ok(())
    }
}
