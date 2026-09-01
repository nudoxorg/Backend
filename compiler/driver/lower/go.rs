//! Defines lower go behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower go invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_ir::{EntityKind, PrimitiveType, SemanticProductConstructor};

use crate::{
    lower::{
        FactSet, FactType, LEAF_PRODUCT, SemanticFact, UnsupportedDeclaration, UnsupportedLane,
        UnsupportedReason, push_fact,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    types::LoweringUnsupported,
};

/// Emits every provable top-level Go declaration: package, functions, type
/// declarations, constants, and variables.
///
/// Grouped declarations inside parentheses have no line structure at the
/// token level and are skipped rather than guessed; an untyped variable whose
/// value is outside the closed literal recipe is recorded exactly.
pub(super) fn collect<'source>(
    source: &'source [u8],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) -> Result<(), LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 {
            continue;
        }
        let SyntaxToken::Word(word) = scanned.token else {
            continue;
        };
        match word {
            b"package" => package(&mut scanner, facts),
            b"func" => function(&mut scanner, facts, unsupported)?,
            b"type" => type_declaration(&mut scanner, facts),
            b"const" => value(&mut scanner, facts, unsupported, EntityKind::Constant),
            b"var" => value(&mut scanner, facts, unsupported, EntityKind::Static),
            _ => {}
        }
    }
    Ok(())
}

fn package<'source>(scanner: &mut DeclarationScanner<'source>, facts: &mut FactSet<'source>) {
    if let Some(name) = word(next_top_level(scanner)) {
        let _ = push_fact(
            facts,
            SemanticFact::new(
                EntityKind::Module,
                name,
                FactType::Opaque,
                SemanticProductConstructor::PRODUCT,
            ),
        );
    }
}

/// Parses one function or method declaration: optional receiver, name,
/// parameter list, and an optional result type. A recognized `func` whose
/// signature stays unprovable is recorded exactly instead of aborting the
/// whole declaration set.
fn function<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) -> Result<(), LoweringUnsupported> {
    // Read the first token exactly once. A receiver is parenthesized; a
    // receiverless function starts directly with its name. The old probe
    // consumed the name of receiverless functions before it could be used.
    let first = next_top_level(scanner);
    if matches!(first, Some(SyntaxToken::Symbol(b'('))) && !scanner.skip_balanced_parens() {
        return Ok(());
    }
    let Some(name) = (if matches!(first, Some(SyntaxToken::Symbol(b'('))) {
        word(next_top_level(scanner))
    } else {
        word(first)
    }) else {
        // No provable name: nothing is recorded and nothing is guessed.
        return Ok(());
    };
    if !matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(b'('))) {
        unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::NeedsFrontend,
        });
        return Ok(());
    }
    if !scanner.skip_balanced_parens() {
        unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::NeedsFrontend,
        });
        return Ok(());
    }
    let fact_type = result_type(scanner);
    push_fact(
        facts,
        SemanticFact::new(
            EntityKind::Function,
            name,
            fact_type,
            SemanticProductConstructor::function(0, 0),
        ),
    )?;
    Ok(())
}

fn type_declaration<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
) {
    let Some(name) = word(next_top_level(scanner)) else {
        return;
    };
    let (kind, constructor) = match next_top_level(scanner) {
        Some(SyntaxToken::Word(b"struct")) => {
            (EntityKind::Record, SemanticProductConstructor::PRODUCT)
        }
        Some(SyntaxToken::Word(b"interface")) => {
            (EntityKind::Trait, SemanticProductConstructor::INTERSECTION)
        }
        Some(SyntaxToken::Word(_)) => (EntityKind::Alias, LEAF_PRODUCT),
        // Grouped or generic forms have no provable single declaration here.
        _ => return,
    };
    let _ = push_fact(
        facts,
        SemanticFact::new(kind, name, FactType::Opaque, constructor),
    );
}

fn value<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
    kind: EntityKind,
) {
    // A parenthesized declaration group has no provable member structure at
    // the token level.
    let after_name = next_top_level(scanner);
    if matches!(after_name, Some(SyntaxToken::Symbol(b'('))) {
        let _ = scanner.skip_balanced_parens();
        return;
    }
    let Some(name) = word(after_name) else {
        return;
    };
    let mut fact_type = None;
    match next_top_level(scanner) {
        Some(SyntaxToken::Symbol(b'=')) => {
            fact_type = literal_type(next_top_level(scanner));
        }
        Some(SyntaxToken::Word(word)) => {
            fact_type = go_type(word);
        }
        _ => {}
    }
    match fact_type {
        Some(primitive) => {
            let _ = push_fact(
                facts,
                SemanticFact::new(kind, name, FactType::Primitive(primitive), LEAF_PRODUCT),
            );
        }
        None => unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::ClosedValueType,
        }),
    }
}

/// Classifies the result type after the parameter list: a single closed
/// primitive word is proven; named results, parenthesized results, and
/// non-closed types stay opaque.
fn result_type(scanner: &mut DeclarationScanner<'_>) -> FactType {
    match next_top_level(scanner) {
        Some(SyntaxToken::Word(word)) => {
            go_type(word).map_or(FactType::Opaque, FactType::Primitive)
        }
        Some(SyntaxToken::Symbol(b'(')) => {
            let fact_type = match next_top_level(scanner) {
                Some(SyntaxToken::Word(word)) => go_type(word),
                _ => None,
            };
            let _ = scanner.skip_balanced_parens();
            fact_type.map_or(FactType::Opaque, |primitive| FactType::Primitive(primitive))
        }
        _ => FactType::Opaque,
    }
}

fn literal_type(token: Option<SyntaxToken<'_>>) -> Option<PrimitiveType> {
    match token {
        Some(SyntaxToken::Literal) => Some(PrimitiveType::String),
        Some(SyntaxToken::Word(b"true" | b"false")) => Some(PrimitiveType::Bool),
        _ => None,
    }
}

fn next_top_level<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Option<SyntaxToken<'source>> {
    scanner
        .next()
        .and_then(|scanned| (scanned.brace_depth == 0).then_some(scanned.token))
}

fn word(token: Option<SyntaxToken<'_>>) -> Option<&[u8]> {
    match token {
        Some(SyntaxToken::Word(word)) => Some(word),
        Some(SyntaxToken::Symbol(_)) | Some(SyntaxToken::Literal) | None => None,
    }
}

fn go_type(word: &[u8]) -> Option<PrimitiveType> {
    match word {
        b"bool" => Some(PrimitiveType::Bool),
        b"int" => Some(PrimitiveType::I32),
        b"string" => Some(PrimitiveType::String),
        _ => None,
    }
}
