//! Defines lower typescript behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower typescript invariants and typed state transitions.
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

/// Emits every provable top-level TypeScript declaration: functions,
/// interfaces, classes, enums, type aliases, and annotated constants.
///
/// Unannotated constants are recorded exactly instead of guessed; member
/// bodies are skipped structurally by brace depth.
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
            b"function" => function(&mut scanner, facts, unsupported)?,
            b"interface" => record(&mut scanner, facts, EntityKind::Record),
            b"class" => record(&mut scanner, facts, EntityKind::Record),
            b"enum" => record(&mut scanner, facts, EntityKind::Enum),
            b"type" => alias(&mut scanner, facts),
            b"const" => constant(&mut scanner, facts, unsupported),
            _ => {}
        }
    }
    Ok(())
}

fn function<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) -> Result<(), LoweringUnsupported> {
    let Some(name) = word(next_top_level(scanner)) else {
        // No provable name: nothing is recorded and nothing is guessed.
        return Ok(());
    };
    // Generic parameters may precede the parameter list.
    let mut parameter_list = false;
    loop {
        match next_top_level(scanner) {
            Some(SyntaxToken::Symbol(b'(')) => {
                parameter_list = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    if !parameter_list || !scanner.skip_balanced_parens() {
        unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::NeedsFrontend,
        });
        return Ok(());
    }
    let fact_type = return_annotation(scanner);
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

fn record<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    kind: EntityKind,
) {
    let constructor = if kind == EntityKind::Enum {
        SemanticProductConstructor::UNION
    } else {
        SemanticProductConstructor::PRODUCT
    };
    if let Some(name) = word(next_top_level(scanner)) {
        let _ = push_fact(
            facts,
            SemanticFact::new(kind, name, FactType::Opaque, constructor),
        );
    }
}

fn alias<'source>(scanner: &mut DeclarationScanner<'source>, facts: &mut FactSet<'source>) {
    let Some(name) = word(next_top_level(scanner)) else {
        return;
    };
    let mut fact_type = FactType::Opaque;
    let mut after_equals = false;
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 {
            break;
        }
        match scanned.token {
            SyntaxToken::Symbol(b'=') => after_equals = true,
            SyntaxToken::Symbol(b'[') | SyntaxToken::Symbol(b'<') => {
                // A compound target (`boolean[]`, `Array<boolean>`) is not its
                // inner primitive word.
                after_equals = false;
            }
            SyntaxToken::Word(word) if after_equals => {
                if let Some(primitive) = primitive_type(word) {
                    fact_type = FactType::Primitive(primitive);
                }
                break;
            }
            SyntaxToken::Symbol(b';') => break,
            _ => {}
        }
    }
    let _ = push_fact(
        facts,
        SemanticFact::new(EntityKind::Alias, name, fact_type, LEAF_PRODUCT),
    );
}

fn constant<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) {
    let Some(name) = word(next_top_level(scanner)) else {
        return;
    };
    let Some(SyntaxToken::Symbol(b':')) = next_top_level(scanner) else {
        // An unannotated constant's type is not provable at the source
        // level, so it is recorded exactly instead of guessed.
        unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::ClosedValueType,
        });
        return;
    };
    let fact_type = match next_top_level(scanner) {
        Some(SyntaxToken::Word(word)) => {
            let typed = primitive_type(word);
            if typed.is_some() && matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(b'[')))
            {
                // An array spelling (`boolean[]`) is not its element
                // primitive.
                None
            } else {
                typed
            }
        }
        _ => None,
    };
    match fact_type {
        Some(primitive) => {
            let _ = push_fact(
                facts,
                SemanticFact::new(
                    EntityKind::Constant,
                    name,
                    FactType::Primitive(primitive),
                    LEAF_PRODUCT,
                ),
            );
        }
        None => unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::ClosedValueType,
        }),
    }
}

/// Classifies the tokens after the parameter list: an optional `:` return
/// annotation ending at the body. A bracketed or generic annotation never
/// commits its inner primitive word.
fn return_annotation(scanner: &mut DeclarationScanner<'_>) -> FactType {
    let mut fact_type = FactType::Opaque;
    let mut after_colon = false;
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 {
            break;
        }
        match scanned.token {
            SyntaxToken::Symbol(b':') => after_colon = true,
            SyntaxToken::Symbol(b'[') | SyntaxToken::Symbol(b'<') if after_colon => {
                // A compound annotation (`boolean[]`, `Array<boolean>`) is not
                // its inner primitive word.
                after_colon = false;
            }
            SyntaxToken::Word(word) if after_colon => {
                if let Some(primitive) = primitive_type(word) {
                    fact_type = FactType::Primitive(primitive);
                }
                after_colon = false;
            }
            SyntaxToken::Symbol(b'{') | SyntaxToken::Symbol(b';') => break,
            _ => {}
        }
    }
    fact_type
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

fn primitive_type(word: &[u8]) -> Option<PrimitiveType> {
    match word {
        b"boolean" => Some(PrimitiveType::Bool),
        b"number" => Some(PrimitiveType::I32),
        b"string" => Some(PrimitiveType::String),
        _ => None,
    }
}
