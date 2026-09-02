//! Projects source-backed OXC bindings into the canonical declaration lane.
//! Keeps TypeScript syntax and lexical authority in-process beside the configured checker.
//! Contains no token reconstruction, fallback collector, or declaration guessing.

use compiler_ir::{EntityKind, SemanticProductConstructor};
use compiler_languages_typescript::{AuthorityError, OxcDeclarationKind, with_analysis};
use compiler_vocabulary::TypeScriptSource;

use crate::{
    lower::{FactSet, LEAF_PRODUCT, SemanticFact, push_fact},
    types::LoweringUnsupported,
};

/// Exact direct-authority rejection while borrowing OXC declaration facts.
#[derive(Debug)]
pub(crate) enum TypeScriptCollectError {
    /// The caller's bytes cannot be the UTF-8 source OXC requires.
    Utf8(std::str::Utf8Error),
    /// OXC retained syntax or lexical diagnostics for the exact source.
    Authority(AuthorityError),
    /// The canonical bounded declaration lane cannot admit every OXC symbol.
    Lowering(LoweringUnsupported),
    /// An OXC declaration span could not name a slice of the admitted source.
    Span { start: u32, end: u32 },
}

/// Streams every OXC-bound declaration into canonical declaration facts.
///
/// This accepts no reconstructed token stream. The exact TypeScript checker is
/// run by the driver before this lowering step; OXC contributes its distinct
/// syntax, lexical-binding, source-coordinate, and declaration authorities.
pub(crate) fn collect<'source>(
    profile: TypeScriptSource,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), TypeScriptCollectError> {
    let source = std::str::from_utf8(source).map_err(TypeScriptCollectError::Utf8)?;
    with_analysis(profile, source, |module| {
        for declaration in module.declarations() {
            let start = usize::try_from(declaration.name.start).map_err(|_| {
                TypeScriptCollectError::Span {
                    start: declaration.name.start,
                    end: declaration.name.end,
                }
            })?;
            let end = usize::try_from(declaration.name.end).map_err(|_| {
                TypeScriptCollectError::Span {
                    start: declaration.name.start,
                    end: declaration.name.end,
                }
            })?;
            let Some(name) = source.as_bytes().get(start..end) else {
                return Err(TypeScriptCollectError::Span {
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
            .map_err(TypeScriptCollectError::Lowering)?;
        }
        Ok(())
    })
    .map_err(TypeScriptCollectError::Authority)?
}

const fn entity_kind(kind: OxcDeclarationKind) -> EntityKind {
    match kind {
        OxcDeclarationKind::Constant => EntityKind::Constant,
        OxcDeclarationKind::Variable => EntityKind::Static,
        OxcDeclarationKind::Function => EntityKind::Function,
        OxcDeclarationKind::Class => EntityKind::Record,
        OxcDeclarationKind::Interface => EntityKind::Trait,
        OxcDeclarationKind::TypeAlias => EntityKind::Alias,
        OxcDeclarationKind::Enum => EntityKind::Enum,
        OxcDeclarationKind::EnumMember => EntityKind::Variant,
        OxcDeclarationKind::Namespace => EntityKind::Module,
        OxcDeclarationKind::TypeParameter => EntityKind::Parameter,
        OxcDeclarationKind::Import => EntityKind::Reexport,
    }
}

const fn constructor(kind: OxcDeclarationKind) -> SemanticProductConstructor {
    match kind {
        OxcDeclarationKind::Function => SemanticProductConstructor::function(0, 0),
        OxcDeclarationKind::Class => SemanticProductConstructor::PRODUCT,
        OxcDeclarationKind::Interface => SemanticProductConstructor::INTERSECTION,
        OxcDeclarationKind::Enum => SemanticProductConstructor::UNION,
        _ => LEAF_PRODUCT,
    }
}

#[cfg(test)]
mod tests {
    use compiler_vocabulary::TypeScriptSource;

    use crate::lower::FactSet;

    use super::{TypeScriptCollectError, collect};

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("OXC TypeScript authority rejected the valid fixture: {0:?}")]
        Collect(TypeScriptCollectError),
        #[error("OXC declaration count differed from the canonical lane")]
        Count,
        #[error("OXC interface declaration was not retained as the canonical trait kind")]
        InterfaceKind,
    }

    #[test]
    fn oxc_bound_symbols_fill_the_canonical_declaration_lane() -> Result<(), TestError> {
        let mut facts = FactSet::new();
        collect(
            TypeScriptSource::TypeScript,
            b"import { foreign } from 'pkg'; export class Box {} export const value = foreign;",
            &mut facts,
        )
        .map_err(TestError::Collect)?;
        if facts.len() != 3 {
            return Err(TestError::Count);
        }
        Ok(())
    }

    #[test]
    fn oxc_interface_is_never_rewritten_as_a_record() -> Result<(), TestError> {
        let mut facts = FactSet::new();
        collect(
            TypeScriptSource::TypeScript,
            b"export interface Shape { area(): number; }",
            &mut facts,
        )
        .map_err(TestError::Collect)?;
        if facts.len() != 1 || facts.kind_at(0) != Some(compiler_ir::EntityKind::Trait) {
            return Err(TestError::InterfaceKind);
        }
        Ok(())
    }
}
