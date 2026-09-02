//! Defines lower clang behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower clang invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
#![allow(
    clippy::indexing_slicing,
    reason = "statement and field word runs are split from proven separator positions before indexing, and every run slot is admitted before it is read"
)]

use compiler_ir::{EntityKind, PrimitiveType, SemanticProductConstructor};

use crate::{
    lower::{
        FactSet, FactType, LEAF_PRODUCT, SemanticFact, UnsupportedDeclaration, UnsupportedLane,
        UnsupportedReason, push_fact,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    types::LoweringUnsupported,
};

/// Word-run marker for one pointer declarator or parameter-list opener.
const STAR: &[u8] = b"*";
/// Word-run marker for one parenthesized group.
const PAREN: &[u8] = b"(";
/// Word-run marker for one array declarator bracket.
const BRACKET: &[u8] = b"[";

/// Emits every provable top-level C declaration: functions, struct records
/// with their named fields, enums with their enumerators, typedefs, and
/// closed constants.
///
/// Pointer, array, record, and compound object types are recorded exactly as
/// unsupported; a declaration is never guessed into a closed recipe.
pub(super) fn collect<'source>(
    source: &'source [u8],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) -> Result<(), LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    // One reused scratch run per top-level statement, field, or enumerator.
    let mut run: Vec<&'source [u8]> = Vec::new();
    let mut mode = Mode::Top;
    while let Some(scanned) = scanner.next() {
        match (mode, scanned.brace_depth, scanned.token) {
            (Mode::Top, 0, SyntaxToken::Word(word)) => run.push(word),
            (Mode::Top, 0, SyntaxToken::Symbol(b'*')) => run.push(STAR),
            (Mode::Top, 0, SyntaxToken::Symbol(b'(')) => run.push(PAREN),
            (Mode::Top, 0, SyntaxToken::Symbol(b'['))
            | (Mode::Top, 0, SyntaxToken::Symbol(b']')) => {
                run.push(BRACKET);
            }
            (Mode::Top, 0, SyntaxToken::Symbol(b';')) => {
                top_statement(&run, facts, unsupported);
                run.clear();
            }
            (Mode::Top, 0, SyntaxToken::Symbol(b'{')) => {
                mode = body_mode(&run);
                top_statement(&run, facts, unsupported);
                run.clear();
            }
            (Mode::Fields, 1, SyntaxToken::Word(word)) => run.push(word),
            (Mode::Fields, 1, SyntaxToken::Symbol(b'*')) => run.push(STAR),
            (Mode::Fields, 1, SyntaxToken::Symbol(b'['))
            | (Mode::Fields, 1, SyntaxToken::Symbol(b']')) => {
                run.push(BRACKET);
            }
            (Mode::Fields, 1, SyntaxToken::Symbol(b'{')) => {
                // A nested record definition's header words never describe
                // the enclosing record's next field.
                run.clear();
            }
            (Mode::Fields, 1, SyntaxToken::Symbol(b';')) => {
                field(&run, facts);
                run.clear();
            }
            (Mode::Enumerators, 1, SyntaxToken::Word(word)) => run.push(word),
            (Mode::Enumerators, 1, SyntaxToken::Symbol(b',')) => {
                enumerator(&run, facts);
                run.clear();
            }
            // Any deeper brace group (function bodies, nested records) is
            // structurally invisible to this top-level scan.
            _ => {}
        }
    }
    Ok(())
}

/// Scan mode after one top-level `{`.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Top,
    /// Named struct fields, one `;`-terminated declaration each.
    Fields,
    /// Enumerators, one `,`-separated declaration each.
    Enumerators,
}

fn body_mode(run: &[&[u8]]) -> Mode {
    match run.first().copied() {
        Some(b"struct") => Mode::Fields,
        Some(b"enum") => Mode::Enumerators,
        _ => Mode::Top,
    }
}

/// Lowers one complete top-level statement or body opener.
fn top_statement<'source>(
    run: &[&'source [u8]],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) {
    let Some(last) = run.last().copied() else {
        return;
    };
    match run.first().copied() {
        Some(b"typedef") => {
            // A struct/enum typedef's alias name follows its body, outside
            // this run, so no compact alias fact is provable here.
            if run.len() >= 3
                && run.get(1) != Some(&b"struct".as_slice())
                && run.get(1) != Some(&b"enum".as_slice())
            {
                let _ = push_fact(
                    facts,
                    SemanticFact::new(EntityKind::Alias, last, typedef_type(run), LEAF_PRODUCT),
                );
            }
        }
        Some(b"struct") | Some(b"enum") => {
            let (kind, constructor) = if run.first() == Some(&b"enum".as_slice()) {
                (EntityKind::Enum, SemanticProductConstructor::UNION)
            } else {
                (EntityKind::Record, SemanticProductConstructor::PRODUCT)
            };
            if let Some(name) = run.get(1).copied().filter(|word| *word != STAR) {
                let _ = push_fact(
                    facts,
                    SemanticFact::new(kind, name, FactType::Opaque, constructor),
                );
            }
        }
        Some(b"const") => {
            if let Some(name) = object_name(run) {
                match const_type(run) {
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
        }
        _ => {
            if let Some(paren) = run.iter().position(|word| *word == PAREN) {
                // A pointer declarator directly after the parameter-list
                // opener is a function-pointer object, not a function.
                if run.get(paren + 1) == Some(&STAR) {
                    if let Some(name) = object_name(run) {
                        unsupported.record(UnsupportedDeclaration {
                            name,
                            reason: UnsupportedReason::NeedsFrontend,
                        });
                    }
                    return;
                }
                // A function definition or prototype: its name is the word
                // before the parameter list, and its return type is the
                // directly preceding single word.
                let mut name_end = paren;
                while name_end > 0 && run[name_end - 1] == STAR {
                    name_end -= 1;
                }
                let Some(name) = run
                    .get(name_end.wrapping_sub(1))
                    .copied()
                    .filter(|word| *word != PAREN)
                else {
                    return;
                };
                let mut fact_type = FactType::Opaque;
                if name_end == 2
                    && let Some(ret) = run.first()
                {
                    fact_type = match *ret {
                        b"int" => FactType::Primitive(PrimitiveType::I32),
                        b"bool" | b"_Bool" => FactType::Primitive(PrimitiveType::Bool),
                        _ => FactType::Opaque,
                    };
                }
                let _ = push_fact(
                    facts,
                    SemanticFact::new(
                        EntityKind::Function,
                        name,
                        fact_type,
                        SemanticProductConstructor::function(0, 0),
                    ),
                );
                return;
            }
            // A plain object definition with a proven name: object storage
            // stays outside this compact slice and is recorded exactly.
            if let Some(name) = object_name(run) {
                unsupported.record(UnsupportedDeclaration {
                    name,
                    reason: UnsupportedReason::NeedsFrontend,
                });
            }
        }
    }
}

/// Lowers one `;`-terminated struct field: its name is the final word and its
/// closed type comes from the leading type words. An array declarator never
/// commits its element primitive.
fn field<'source>(run: &[&'source [u8]], facts: &mut FactSet<'source>) {
    if run.contains(&PAREN) {
        // Function-pointer fields need the real frontend's type authority.
        return;
    }
    let Some(name) = object_name(run) else {
        return;
    };
    let fact_type = if run.contains(&BRACKET) {
        FactType::Opaque
    } else {
        match run.first().copied() {
            Some(b"int") if !run.contains(&STAR) => FactType::Primitive(PrimitiveType::I32),
            Some(b"bool") | Some(b"_Bool") => FactType::Primitive(PrimitiveType::Bool),
            Some(b"char") if run.contains(&STAR) => FactType::Primitive(PrimitiveType::String),
            _ => FactType::Opaque,
        }
    };
    let _ = push_fact(
        facts,
        SemanticFact::new(EntityKind::Field, name, fact_type, LEAF_PRODUCT),
    );
}

/// Lowers one enumerator name: the run's first word, which precedes any
/// explicit value assignment.
fn enumerator<'source>(run: &[&'source [u8]], facts: &mut FactSet<'source>) {
    if let Some(name) = run
        .first()
        .copied()
        .filter(|word| *word != STAR && *word != PAREN)
    {
        let _ = push_fact(
            facts,
            SemanticFact::new(EntityKind::Variant, name, FactType::Opaque, LEAF_PRODUCT),
        );
    }
}

/// The declared object name of a word run: its final plain word.
fn object_name<'run>(run: &[&'run [u8]]) -> Option<&'run [u8]> {
    run.last()
        .copied()
        .filter(|word| *word != STAR && *word != PAREN)
}

/// Classifies a `const` word run: `const char *NAME` text, `const int`
/// integers, `const bool` booleans; every other type, including array
/// declarators, stays unsupported.
fn const_type(run: &[&[u8]]) -> Option<PrimitiveType> {
    if run.contains(&BRACKET) {
        return None;
    }
    match run.get(1).copied() {
        Some(b"int") if !run.contains(&STAR) => Some(PrimitiveType::I32),
        Some(b"bool") | Some(b"_Bool") => Some(PrimitiveType::Bool),
        Some(b"char") => Some(PrimitiveType::String),
        _ => None,
    }
}

/// Classifies a typedef target: a single spelled closed primitive keeps its
/// type; every other target, including array declarators, stays opaque.
fn typedef_type(run: &[&[u8]]) -> FactType {
    if run.contains(&BRACKET) {
        return FactType::Opaque;
    }
    match run.get(1).copied() {
        Some(b"int") if !run.contains(&STAR) => FactType::Primitive(PrimitiveType::I32),
        Some(b"bool") | Some(b"_Bool") => FactType::Primitive(PrimitiveType::Bool),
        _ => FactType::Opaque,
    }
}
