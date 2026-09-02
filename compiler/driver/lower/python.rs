//! Projects source-backed Ruff AST declarations into the canonical fact lane.
//! Keeps parsing in-process and accepts no line scanner or reconstructed tokens.
//! Leaves Python checker-derived types and resolutions to their dedicated authority seam.

use compiler_ir::{EntityKind, SemanticProductConstructor};
use compiler_languages_python::{ExtractionError, RuffDeclarationKind, with_module};
use compiler_vocabulary::PythonVersion;

use crate::{
    lower::{FactSet, LEAF_PRODUCT, SemanticFact, push_fact},
    types::LoweringUnsupported,
};

/// Exact direct-authority rejection while borrowing Ruff declarations.
#[derive(Debug)]
pub(crate) enum PythonCollectError {
    /// Ruff rejected the caller's selected Python grammar or source bytes.
    Authority(ExtractionError),
    /// Canonical declaration admission rejected an exact borrowed fact.
    Lowering(LoweringUnsupported),
    /// Ruff returned a declaration span outside the exact caller source.
    Span { start: u32, end: u32 },
}

/// Streams top-level Ruff declarations into canonical declaration facts.
pub(crate) fn collect<'source>(
    profile: PythonVersion,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), PythonCollectError> {
    with_module(source, profile, |module| {
        for declaration in module.declarations() {
            let Some(name) = source_at(source, declaration.name.start, declaration.name.end) else {
                return Err(PythonCollectError::Span {
                    start: declaration.name.start,
                    end: declaration.name.end,
                });
            };
            push_fact(
                facts,
                SemanticFact::new(
                    entity_kind(declaration.kind),
                    name, 
                    constructor(declaration.kind),
                ),
            )
            .map_err(PythonCollectError::Lowering)?;
        }
        Ok(())
    })
    .map_err(PythonCollectError::Authority)?
}

fn source_at(source: &[u8], start: u32, end: u32) -> Option<&[u8]> {
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    source.get(start..end)
}

const fn entity_kind(kind: RuffDeclarationKind) -> EntityKind {
    match kind {
        RuffDeclarationKind::Class => EntityKind::Record,
        RuffDeclarationKind::Function => EntityKind::Function,
        RuffDeclarationKind::Static => EntityKind::Static,
    }
}

const fn constructor(kind: RuffDeclarationKind) -> SemanticProductConstructor {
    match kind {
        RuffDeclarationKind::Class => SemanticProductConstructor::PRODUCT,
        RuffDeclarationKind::Function => SemanticProductConstructor::function(0, 0),
        RuffDeclarationKind::Static => LEAF_PRODUCT,
    }
}

#[cfg(test)]
mod tests {
    use compiler_ir::EntityKind;
    use compiler_vocabulary::PythonVersion;

    use super::{FactSet, PythonCollectError, collect};

    /// Holds source authority failures without an assertion panic.
    #[derive(Debug, thiserror::Error)]
    enum TestError {
        /// Ruff or fact admission rejected the valid source fixture.
        #[error("Ruff declaration authority rejected the fixture: {0:?}")]
        Collect(PythonCollectError),
        /// The class declaration was not admitted from the Ruff AST.
        #[error("Ruff class declaration was absent from the canonical fact lane")]
        Class,
        /// A mutable Python module binding was admitted as a constant.
        #[error("Ruff assignment binding did not retain mutable static semantics")]
        Static,
    }

    #[test]
    fn ruff_class_declaration_flows_into_the_shared_fact_lane() -> Result<(), TestError> {
        let mut facts = FactSet::new();
        collect(
            PythonVersion::Python313,
            b"class RuffAuthority:\n    pass\n",
            &mut facts,
        )
        .map_err(TestError::Collect)?;
        if facts.kind_at(0) == Some(EntityKind::Record) {
            Ok(())
        } else {
            Err(TestError::Class)
        }
    }

    #[test]
    fn ruff_assignment_is_not_admitted_as_a_constant() -> Result<(), TestError> {
        let mut facts = FactSet::new();
        collect(PythonVersion::Python313, b"value = 1\n", &mut facts)
            .map_err(TestError::Collect)?;
        if facts.kind_at(0) == Some(EntityKind::Static) {
            Ok(())
        } else {
            Err(TestError::Static)
        }
    }
}
