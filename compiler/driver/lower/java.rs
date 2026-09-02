//! Defines lower java behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower java invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_ir::{EntityKind, PrimitiveType};
use compiler_ir_vocabulary::SemanticProductConstructor;

use crate::{
    lower::{
        FactSet, FactType, LEAF_PRODUCT, SemanticFact, UnsupportedDeclaration, UnsupportedLane,
        UnsupportedReason, push_fact,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    types::LoweringUnsupported,
};

/// Emits every provable top-level Java declaration: classes, interfaces,
/// enums, records, and their methods and fields.
///
/// Member bodies, parameter lists, and nested type scopes are skipped
/// structurally by brace depth; a member's return or declared type is
/// committed only when a closed primitive is spelled directly before the
/// member name. Nested types are recorded exactly instead of being flattened
/// into member facts.
pub(super) fn collect<'source>(
    source: &'source [u8],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) -> Result<(), LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    let mut previous: Option<&'source [u8]> = None;
    let mut before_previous: Option<&'source [u8]> = None;
    // The last two member words were consecutive, so the earlier word is the
    // member's directly spelled type.
    let mut adjacent = false;
    // Brace countdown of one nested type's scope while its members stay
    // invisible to this top-level scan.
    let mut nested: Option<u32> = None;
    while let Some(scanned) = scanner.next() {
        if let Some(counter) = nested.as_mut() {
            match scanned.token {
                SyntaxToken::Symbol(b'{') => *counter = counter.saturating_add(1),
                SyntaxToken::Symbol(b'}') => {
                    if *counter == 1 {
                        nested = None;
                    } else {
                        *counter -= 1;
                    }
                }
                _ => {}
            }
            continue;
        }
        match (scanned.brace_depth, scanned.token) {
            (0, SyntaxToken::Word(word)) => match word {
                b"class" | b"record" => {
                    top_type(&mut scanner, facts, EntityKind::Record)?;
                    reset(&mut previous, &mut before_previous, &mut adjacent);
                }
                b"interface" => {
                    top_type(&mut scanner, facts, EntityKind::Trait)?;
                    reset(&mut previous, &mut before_previous, &mut adjacent);
                }
                b"enum" => {
                    top_type(&mut scanner, facts, EntityKind::Enum)?;
                    reset(&mut previous, &mut before_previous, &mut adjacent);
                }
                _ => {}
            },
            (1, SyntaxToken::Word(word)) if is_java_type_keyword(word) => {
                // A nested type is a recognized declaration outside this
                // compact slice; it is recorded exactly, and its scope is
                // never flattened into member facts.
                if let Some(name) = word_of(next_word(&mut scanner)) {
                    unsupported.record(UnsupportedDeclaration {
                        name,
                        reason: UnsupportedReason::NeedsFrontend,
                    });
                    nested = Some(0);
                }
                reset(&mut previous, &mut before_previous, &mut adjacent);
            }
            (1, SyntaxToken::Word(word)) => {
                adjacent = previous.is_some();
                before_previous = previous;
                previous = Some(word);
            }
            (1, SyntaxToken::Symbol(b'(')) => {
                // A method: its name is the word before the parameter list
                // and its return type is the directly preceding word. The
                // parameter list itself never contributes member facts.
                if let (Some(name), Some(returned)) = (previous, before_previous) {
                    if name != b"new" {
                        let fact_type = if adjacent {
                            java_type(returned)
                        } else {
                            FactType::Opaque
                        };
                        let _ = push_fact(
                            facts,
                            SemanticFact::new(
                                EntityKind::Function,
                                name,
                                fact_type,
                                SemanticProductConstructor::function(0, 0),
                            ),
                        );
                    }
                }
                let _ = scanner.skip_balanced_parens();
                reset(&mut previous, &mut before_previous, &mut adjacent);
            }
            (1, SyntaxToken::Symbol(b'=')) => {
                // An initialized field declares a constant value.
                if let (Some(name), Some(declared)) = (previous, before_previous) {
                    let fact_type = if adjacent {
                        java_type(declared)
                    } else {
                        FactType::Opaque
                    };
                    let _ = push_fact(
                        facts,
                        SemanticFact::new(EntityKind::Constant, name, fact_type, LEAF_PRODUCT),
                    );
                }
                reset(&mut previous, &mut before_previous, &mut adjacent);
            }
            (1, SyntaxToken::Symbol(b';')) => {
                // A field declaration without an initializer.
                if let (Some(name), Some(declared)) = (previous, before_previous) {
                    let fact_type = if adjacent {
                        java_type(declared)
                    } else {
                        FactType::Opaque
                    };
                    let _ = push_fact(
                        facts,
                        SemanticFact::new(EntityKind::Field, name, fact_type, LEAF_PRODUCT),
                    );
                }
                reset(&mut previous, &mut before_previous, &mut adjacent);
            }
            (1, SyntaxToken::Symbol(b'{'))
            | (1, SyntaxToken::Symbol(b'}'))
            | (1, SyntaxToken::Symbol(b','))
            | (1, SyntaxToken::Literal) => {
                // Member boundaries, enum-constant lists, and literal
                // initializers close a word run.
                reset(&mut previous, &mut before_previous, &mut adjacent);
            }
            _ => {}
        }
    }
    Ok(())
}

fn top_type<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    kind: EntityKind,
) -> Result<(), LoweringUnsupported> {
    let Some(name) = scanner.next().and_then(|scanned| match scanned.token {
        SyntaxToken::Word(word) => Some(word),
        _ => None,
    }) else {
        return Err(LoweringUnsupported::JavaDeclarationForm);
    };
    let constructor = match kind {
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        _ => SemanticProductConstructor::PRODUCT,
    };
    push_fact(
        facts,
        SemanticFact::new(kind, name, FactType::Opaque, constructor),
    )?;
    Ok(())
}

/// Commits a closed Java primitive exactly; every other spelled type stays
/// opaque without being guessed.
fn java_type(word: &[u8]) -> FactType {
    match word {
        b"boolean" => FactType::Primitive(PrimitiveType::Bool),
        b"int" => FactType::Primitive(PrimitiveType::I32),
        b"String" => FactType::Primitive(PrimitiveType::String),
        _ => FactType::Opaque,
    }
}

fn is_java_type_keyword(word: &[u8]) -> bool {
    matches!(word, b"class" | b"record" | b"interface" | b"enum")
}

fn next_word<'source>(scanner: &mut DeclarationScanner<'source>) -> Option<SyntaxToken<'source>> {
    scanner.next().map(|scanned| scanned.token)
}

fn word_of(token: Option<SyntaxToken<'_>>) -> Option<&[u8]> {
    match token {
        Some(SyntaxToken::Word(word)) => Some(word),
        Some(SyntaxToken::Symbol(_)) | Some(SyntaxToken::Literal) | None => None,
    }
}

fn reset<'source>(
    previous: &mut Option<&'source [u8]>,
    before_previous: &mut Option<&'source [u8]>,
    adjacent: &mut bool,
) {
    *previous = None;
    *before_previous = None;
    *adjacent = false;
}
