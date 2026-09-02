//! Projects direct libclang declarations into the canonical fact lane.
//! Owns no scanner, token reconstruction, source guessing, or native process fallback.
//! Retains the exact libclang collection failure until the driver authority terminal.

use core::sync::atomic::AtomicBool;

use compiler_ir::{EntityKind, SemanticProductConstructor};
use compiler_languages_clang::{
    ClangInput, ClangScratch, CollectError, DeclarationFact, DeclarationId, DeclarationKind,
    DiagnosticFact, IncludeFact, ReferenceFact, SourceSpan, TypeEdge, TypeFact,
    collect_cancellable,
};
use compiler_vocabulary::{LanguageProfile, LoweringUnsupported};

use crate::lower::{FactSet, LEAF_PRODUCT, SemanticFact, push_fact};

/// Exact direct-authority rejection while borrowing libclang declarations.
#[derive(Debug)]
pub(crate) enum ClangCollectError {
    /// libclang failed before yielding a complete fact image.
    Authority(CollectError),
    /// The bounded canonical declaration lane rejected a direct fact.
    Lowering(LoweringUnsupported),
}

/// Streams every named libclang declaration into the canonical declaration lane.
///
/// The storage lanes are deliberately stack-bound and exact-capacity: libclang
/// returns its typed required-slot terminal instead of truncating any native
/// fact. Recursive types, references, diagnostics, and dependencies remain
/// borrowed through the collection transaction for their dedicated planes;
/// this declaration-only compact lane does not rewrite any of them.
pub(crate) fn collect<'source>(
    profile: LanguageProfile,
    source: &'source [u8],
    cancelled: &AtomicBool,
    facts: &mut FactSet<'source>,
) -> Result<(), ClangCollectError> {
    let input = ClangInput::from_profile(c"nudox-input", source, profile)
        .map_err(|_| ClangCollectError::Lowering(LoweringUnsupported::ClangDeclarationForm))?;
    let mut declarations = [empty_declaration(); DECLARATION_CAPACITY];
    let mut types = [empty_type(); TYPE_CAPACITY];
    let mut type_edges = [empty_type_edge(); TYPE_EDGE_CAPACITY];
    let mut references = [empty_reference(); REFERENCE_CAPACITY];
    let mut diagnostics = [empty_diagnostic(); DIAGNOSTIC_CAPACITY];
    let mut includes = [empty_include(); INCLUDE_CAPACITY];
    let authority = collect_cancellable(
        input,
        ClangScratch {
            declarations: &mut declarations,
            types: &mut types,
            type_edges: &mut type_edges,
            references: &mut references,
            diagnostics: &mut diagnostics,
            includes: &mut includes,
        },
        cancelled,
    )
    .map_err(ClangCollectError::Authority)?;
    for declaration in authority.declarations {
        let Some(name_span) = declaration.name else {
            return Err(ClangCollectError::Lowering(
                LoweringUnsupported::ClangDeclarationForm,
            ));
        };
        let Some(name) = source_at(source, name_span) else {
            return Err(ClangCollectError::Lowering(
                LoweringUnsupported::ClangDeclarationForm,
            ));
        };
        let Some(kind) = entity_kind(declaration.kind) else {
            return Err(ClangCollectError::Lowering(
                LoweringUnsupported::ClangDeclarationForm,
            ));
        };
        push_fact(facts, SemanticFact::new(kind, name, constructor(kind)))
            .map_err(ClangCollectError::Lowering)?;
    }
    Ok(())
}

const DECLARATION_CAPACITY: usize = 128;
const TYPE_CAPACITY: usize = 512;
const TYPE_EDGE_CAPACITY: usize = 1_024;
const REFERENCE_CAPACITY: usize = 512;
const DIAGNOSTIC_CAPACITY: usize = 128;
const INCLUDE_CAPACITY: usize = 128;

const fn empty_span() -> SourceSpan {
    SourceSpan { start: 0, end: 0 }
}

const fn empty_declaration() -> DeclarationFact {
    DeclarationFact {
        id: DeclarationId { raw: 0 },
        kind: DeclarationKind::Unknown,
        definition: compiler_languages_clang::DefinitionState::Declaration,
        identity: None,
        span: empty_span(),
        name: None,
        owner: None,
        documentation: None,
        type_root: None,
    }
}

const fn empty_type() -> TypeFact {
    TypeFact {
        id: compiler_languages_clang::TypeId { raw: 0 },
        kind: compiler_languages_clang::TypeKind::Unknown,
        qualifiers: compiler_languages_clang::TypeQualifiers {
            is_const: false,
            is_volatile: false,
            is_restrict: false,
        },
        declaration: None,
        array_len: None,
    }
}

const fn empty_type_edge() -> TypeEdge {
    TypeEdge {
        source: compiler_languages_clang::TypeId { raw: 0 },
        relation: compiler_languages_clang::TypeRelation::Pointee,
        target: compiler_languages_clang::TypeId { raw: 0 },
    }
}

const fn empty_reference() -> ReferenceFact {
    ReferenceFact {
        kind: compiler_languages_clang::ReferenceKind::Value,
        span: empty_span(),
        owner: None,
        target: compiler_languages_clang::ReferenceTarget::Unresolved,
    }
}

const fn empty_diagnostic() -> DiagnosticFact {
    DiagnosticFact {
        severity: compiler_languages_clang::DiagnosticSeverity::Ignored,
        location: None,
        category: 0,
        message: None,
    }
}

const fn empty_include() -> IncludeFact {
    IncludeFact {
        kind: compiler_languages_clang::SourceDependencyKind::Include,
        span: empty_span(),
        resolved: None,
    }
}

fn source_at(source: &[u8], span: SourceSpan) -> Option<&[u8]> {
    let start = usize::try_from(span.start).ok()?;
    let end = usize::try_from(span.end).ok()?;
    source.get(start..end)
}

const fn entity_kind(kind: DeclarationKind) -> Option<EntityKind> {
    match kind {
        DeclarationKind::Namespace => Some(EntityKind::Module),
        DeclarationKind::Record => Some(EntityKind::Record),
        DeclarationKind::Enumeration => Some(EntityKind::Enum),
        DeclarationKind::Enumerator => Some(EntityKind::Variant),
        DeclarationKind::Function
        | DeclarationKind::Method
        | DeclarationKind::Constructor
        | DeclarationKind::Destructor => Some(EntityKind::Function),
        DeclarationKind::Field => Some(EntityKind::Field),
        DeclarationKind::Variable => Some(EntityKind::Static),
        DeclarationKind::Parameter => Some(EntityKind::Parameter),
        DeclarationKind::TypeAlias => Some(EntityKind::Alias),
        DeclarationKind::Unknown | DeclarationKind::Macro | DeclarationKind::Template => None,
    }
}

const fn constructor(kind: EntityKind) -> SemanticProductConstructor {
    match kind {
        EntityKind::Function => SemanticProductConstructor::function(0, 0),
        EntityKind::Record => SemanticProductConstructor::PRODUCT,
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        EntityKind::Constant
        | EntityKind::Module
        | EntityKind::Field
        | EntityKind::Alias
        | EntityKind::Implementation
        | EntityKind::Variant
        | EntityKind::Static
        | EntityKind::Reexport
        | EntityKind::Parameter => LEAF_PRODUCT,
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicBool;

    use compiler_vocabulary::{CStandard, LanguageProfile};

    use super::{ClangCollectError, EntityKind, FactSet, collect};

    /// Holds the direct native authority terminal without reducing it to text.
    #[derive(Debug, thiserror::Error)]
    enum TestError {
        /// libclang or canonical admission rejected the exact fixture.
        #[error("direct libclang collection failed: {0:?}")]
        Collect(ClangCollectError),
        /// Direct authority did not admit its named function declaration.
        #[error("direct libclang function declaration was absent from the canonical lane")]
        MissingFunction,
    }

    #[test]
    fn direct_libclang_declaration_flows_into_the_shared_fact_lane() -> Result<(), TestError> {
        let cancelled = AtomicBool::new(false);
        let mut facts = FactSet::new();
        collect(
            LanguageProfile::C(CStandard::C23),
            b"int direct_authority(void) { return 0; }",
            &cancelled,
            &mut facts,
        )
        .map_err(TestError::Collect)?;
        if facts.kind_at(0) == Some(EntityKind::Function) {
            Ok(())
        } else {
            Err(TestError::MissingFunction)
        }
    }
}
