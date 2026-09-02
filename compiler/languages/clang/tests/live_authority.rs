//! Proves direct C and C++ libclang collection when a test environment provisions libclang.
//! The tests require the `native-test` feature and fail if the configured authority is unavailable.
//! They exercise profiles, macros, includes, overload identities, recursive types, docs, and references.

#![cfg(feature = "native-test")]

use compiler_languages_clang::{
    ClangInput, ClangScratch, CollectError, DeclarationFact, DeclarationId, DeclarationKind,
    DefinitionState, DiagnosticFact, IncludeFact, ReferenceFact, ReferenceKind, ReferenceTarget,
    SourceDependencyKind, SourceSpan, TypeEdge, TypeFact, TypeId, collect,
    facts::{TypeKind, TypeQualifiers},
};
use compiler_vocabulary::{CStandard, CxxStandard};

/// Identifies the one direct native fact a live authority proof requires.
#[derive(Clone, Copy, Debug, thiserror::Error)]
enum RequiredFact {
    /// A declaration kind was not emitted.
    #[error("declaration {kind:?}")]
    Declaration {
        /// Required direct native declaration kind.
        kind: DeclarationKind,
    },
    /// A recursive type kind was not emitted.
    #[error("type {kind:?}")]
    Type {
        /// Required direct native recursive type kind.
        kind: TypeKind,
    },
    /// A recursive type relation was not emitted.
    #[error("type relation {relation:?}")]
    TypeRelation {
        /// Required direct native recursive type relation.
        relation: compiler_languages_clang::TypeRelation,
    },
    /// A source dependency kind was not emitted.
    #[error("source dependency {kind:?}")]
    Dependency {
        /// Required direct native source dependency kind.
        kind: SourceDependencyKind,
    },
    /// A local call target was not resolved by libclang.
    #[error("local call")]
    LocalCall,
    /// A declaration documentation span was not emitted.
    #[error("documentation")]
    Documentation,
    /// Distinct overload declarations did not retain distinct native identities.
    #[error("distinct overload identity")]
    OverloadIdentity,
}

/// Holds exact native collection failures and missing fact claims for the live authority tests.
#[derive(Debug, thiserror::Error)]
enum TestError {
    /// Direct libclang collection returned its exact typed error.
    #[error(transparent)]
    Collection(#[from] CollectError),
    /// A required direct native fact was absent from the returned bounded fact prefixes.
    #[error("missing direct native fact: {0}")]
    Missing(RequiredFact),
}

#[test]
fn c_authority_retains_macro_include_docs_recursive_types_and_local_calls() -> Result<(), TestError>
{
    let source = br#"
#include "authority_missing_header.h"
#define SCALE(value) ((value) * 2)
/// Adds two values.
int add(int left, int right) { return left + right; }
int caller(int *value) { return SCALE(add(*value, 2)); }
"#;
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::C {
                file_name: c"authority.c",
                source,
                standard: CStandard::C23,
            },
            scratch,
        )?;
        require_declaration(&facts, DeclarationKind::Macro)?;
        require_declaration(&facts, DeclarationKind::Function)?;
        require_dependency(&facts, SourceDependencyKind::Include)?;
        require_type(&facts, TypeKind::Pointer)?;
        require_local_call(&facts)?;
        if facts
            .declarations
            .iter()
            .any(|declaration| declaration.documentation.is_some())
        {
            Ok(())
        } else {
            Err(TestError::Missing(RequiredFact::Documentation))
        }
    })
}

#[test]
fn cxx_authority_keeps_overload_identity_and_template_type_edges() -> Result<(), TestError> {
    let source = br"
template <typename Item> struct Box { Item value; };
int score(int value) { return value; }
double score(double value) { return value; }
int caller() { Box<int> value{2}; return score(value.value); }
";
    with_scratch(|scratch| {
        let facts = collect(
            ClangInput::Cxx {
                file_name: c"authority.cc",
                source,
                standard: CxxStandard::Cxx23,
            },
            scratch,
        )?;
        require_declaration(&facts, DeclarationKind::Template)?;
        require_declaration(&facts, DeclarationKind::Function)?;
        require_type(&facts, TypeKind::Function)?;
        require_type_relation(
            &facts,
            compiler_languages_clang::TypeRelation::TemplateArgument,
        )?;
        require_local_call(&facts)?;
        distinct_score_overload_identity(&facts, source)
    })
}

/// Builds enough caller-owned typed capacity for a small but deliberately rich native fixture.
fn with_scratch<Output>(
    run: impl for<'scratch> FnOnce(ClangScratch<'scratch>) -> Result<Output, TestError>,
) -> Result<Output, TestError> {
    let mut declarations = [empty_declaration(); DECLARATION_SLOTS];
    let mut types = [empty_type(); TYPE_SLOTS];
    let mut type_edges = [empty_type_edge(); TYPE_EDGE_SLOTS];
    let mut references = [empty_reference(); REFERENCE_SLOTS];
    let mut diagnostics = [empty_diagnostic(); DIAGNOSTIC_SLOTS];
    let mut includes = [empty_include(); DEPENDENCY_SLOTS];
    run(ClangScratch {
        declarations: &mut declarations,
        types: &mut types,
        type_edges: &mut type_edges,
        references: &mut references,
        diagnostics: &mut diagnostics,
        includes: &mut includes,
    })
}

/// Requires two distinct USR-derived identities among C++ function declarations.
fn distinct_score_overload_identity(
    facts: &compiler_languages_clang::ClangFacts<'_>,
    source: &[u8],
) -> Result<(), TestError> {
    let mut first = None;
    for declaration in facts.declarations {
        if declaration.kind != DeclarationKind::Function
            || declaration
                .name
                .is_none_or(|span| source_at(source, span) != b"score")
        {
            continue;
        }
        let Some(identity) = declaration.identity else {
            continue;
        };
        if first.is_some_and(|observed| observed != identity) {
            return Ok(());
        }
        first = Some(identity);
    }
    Err(TestError::Missing(RequiredFact::OverloadIdentity))
}

const DECLARATION_SLOTS: usize = 32;
const TYPE_SLOTS: usize = 96;
const TYPE_EDGE_SLOTS: usize = 192;
const REFERENCE_SLOTS: usize = 64;
const DIAGNOSTIC_SLOTS: usize = 16;
const DEPENDENCY_SLOTS: usize = 16;

const fn empty_span() -> SourceSpan {
    SourceSpan { start: 0, end: 0 }
}

const fn empty_declaration() -> DeclarationFact {
    DeclarationFact {
        id: DeclarationId { raw: 0 },
        kind: DeclarationKind::Unknown,
        definition: DefinitionState::Declaration,
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
        id: TypeId { raw: 0 },
        kind: TypeKind::Unknown,
        qualifiers: TypeQualifiers {
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
        source: TypeId { raw: 0 },
        relation: compiler_languages_clang::facts::TypeRelation::Pointee,
        target: TypeId { raw: 0 },
    }
}

const fn empty_reference() -> ReferenceFact {
    ReferenceFact {
        kind: ReferenceKind::Value,
        span: empty_span(),
        owner: None,
        target: ReferenceTarget::Unresolved,
    }
}

const fn empty_diagnostic() -> DiagnosticFact {
    DiagnosticFact {
        severity: compiler_languages_clang::facts::DiagnosticSeverity::Ignored,
        location: None,
        category: 0,
        message: None,
    }
}

const fn empty_include() -> IncludeFact {
    IncludeFact {
        kind: SourceDependencyKind::Include,
        span: empty_span(),
        resolved: None,
    }
}

/// Requires one declaration kind without inspecting native names or source scanner output.
fn require_declaration(
    facts: &compiler_languages_clang::ClangFacts<'_>,
    kind: DeclarationKind,
) -> Result<(), TestError> {
    facts
        .declarations
        .iter()
        .any(|declaration| declaration.kind == kind)
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::Declaration { kind }))
}

/// Requires one type kind from the directly emitted recursive type graph.
fn require_type(
    facts: &compiler_languages_clang::ClangFacts<'_>,
    kind: TypeKind,
) -> Result<(), TestError> {
    facts
        .types
        .iter()
        .any(|type_fact| type_fact.kind == kind)
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::Type { kind }))
}

/// Requires one direct relation from the recursive native type graph.
fn require_type_relation(
    facts: &compiler_languages_clang::ClangFacts<'_>,
    relation: compiler_languages_clang::TypeRelation,
) -> Result<(), TestError> {
    facts
        .type_edges
        .iter()
        .any(|edge| edge.relation == relation)
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::TypeRelation { relation }))
}

/// Requires one direct include or module dependency fact.
fn require_dependency(
    facts: &compiler_languages_clang::ClangFacts<'_>,
    kind: SourceDependencyKind,
) -> Result<(), TestError> {
    facts
        .includes
        .iter()
        .any(|dependency| dependency.kind == kind)
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::Dependency { kind }))
}

/// Requires one call expression whose direct libclang target resolves inside the main source.
fn require_local_call(facts: &compiler_languages_clang::ClangFacts<'_>) -> Result<(), TestError> {
    facts
        .references
        .iter()
        .any(|reference| {
            reference.kind == ReferenceKind::Call
                && matches!(reference.target, ReferenceTarget::Local(_))
        })
        .then_some(())
        .ok_or(TestError::Missing(RequiredFact::LocalCall))
}

/// Borrows one proven source span without permitting invalid native coordinates to panic a test.
fn source_at(source: &[u8], span: SourceSpan) -> &[u8] {
    let Ok(start) = usize::try_from(span.start) else {
        return &[];
    };
    let Ok(end) = usize::try_from(span.end) else {
        return &[];
    };
    match source.get(start..end) {
        Some(bytes) => bytes,
        None => &[],
    }
}
