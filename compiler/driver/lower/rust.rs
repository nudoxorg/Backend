//! Defines lower rust behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower rust invariants and typed state transitions.
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

/// Emits every provable top-level Rust item: functions, structs, enums,
/// traits, implementations, modules, constants, statics, and type aliases.
///
/// Item bodies are skipped structurally by brace depth; only closed primitive
/// spellings become declared-type facts. A recognized declaration whose facts
/// stay outside this slice's provable set is recorded exactly in the
/// unsupported lane instead of being guessed or truncated.
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
            b"fn" => function(&mut scanner, facts, unsupported)?,
            b"struct" => record(
                &mut scanner,
                facts,
                EntityKind::Record,
                SemanticProductConstructor::PRODUCT,
            ),
            b"enum" => record(
                &mut scanner,
                facts,
                EntityKind::Enum,
                SemanticProductConstructor::UNION,
            ),
            b"trait" => record(
                &mut scanner,
                facts,
                EntityKind::Trait,
                SemanticProductConstructor::INTERSECTION,
            ),
            b"impl" => implementation(&mut scanner, facts),
            b"mod" => module(&mut scanner, facts),
            b"type" => alias(&mut scanner, facts),
            b"const" => match next_word(&mut scanner) {
                // The dispatched keyword's own follower was already read to
                // distinguish `const fn`; it is forwarded so it is never
                // consumed twice.
                Some(b"fn") => function(&mut scanner, facts, unsupported)?,
                Some(name) => constant(&mut scanner, facts, unsupported, name)?,
                None => {}
            },
            b"static" => static_item(&mut scanner, facts, unsupported),
            _ => {}
        }
    }
    Ok(())
}

/// Parses one function declaration: name, parameter list, and optional return
/// annotation. The proven return primitive becomes the declared-type fact.
/// A recognized `fn` whose parameter list stays unprovable is recorded exactly.
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

/// Parses one named record-family declaration whose header is the next word;
/// the body is skipped by the caller's depth filter. A header without a
/// provable name is skipped, never guessed.
fn record<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    kind: EntityKind,
    constructor: SemanticProductConstructor,
) {
    let Some(name) = word(next_top_level(scanner)) else {
        return;
    };
    let _ = push_fact(
        facts,
        SemanticFact::new(kind, name, FactType::Opaque, constructor),
    );
}

/// Parses one implementation block, naming it by its subject type: the last
/// non-`for` word outside angle brackets before the body. An implementation
/// without a provable subject is skipped.
fn implementation<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
) {
    let mut subject: Option<&[u8]> = None;
    let mut angle_depth = 0_u32;
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 {
            break;
        }
        match scanned.token {
            SyntaxToken::Symbol(b'<') => angle_depth = angle_depth.saturating_add(1),
            SyntaxToken::Symbol(b'>') => angle_depth = angle_depth.saturating_sub(1),
            SyntaxToken::Symbol(b'{') => break,
            SyntaxToken::Word(word) if word != b"for" && angle_depth == 0 => {
                subject = Some(word);
            }
            _ => {}
        }
    }
    if let Some(name) = subject {
        let _ = push_fact(
            facts,
            SemanticFact::new(
                EntityKind::Implementation,
                name,
                FactType::Opaque,
                SemanticProductConstructor::PRODUCT,
            ),
        );
    }
}

/// Parses one module declaration; the body is skipped by the depth filter.
fn module<'source>(scanner: &mut DeclarationScanner<'source>, facts: &mut FactSet<'source>) {
    let Some(name) = word(next_top_level(scanner)) else {
        return;
    };
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

/// Parses one type alias; a single closed primitive target becomes the
/// declared-type fact, any other target stays opaque. A bracketed or generic
/// target never commits its inner primitive word.
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
            SyntaxToken::Symbol(b'[') | SyntaxToken::Symbol(b'<') | SyntaxToken::Symbol(b'(') => {
                // A compound target (`[bool; 4]`, `Vec<bool>`, `fn()`) is not
                // its inner primitive word.
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

/// Parses one constant whose follower word was already read by the dispatch.
/// A closed primitive (or `&str`) becomes the declared-type fact; a constant
/// whose type is outside the closed recipe, or whose type is not spelled at
/// all, is recorded exactly instead of guessed.
fn constant<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
    name: &'source [u8],
) -> Result<(), LoweringUnsupported> {
    if !matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(b':'))) {
        unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::NeedsFrontend,
        });
        return Ok(());
    }
    let primitive = match next_top_level(scanner) {
        Some(SyntaxToken::Word(word)) => primitive_type(word),
        Some(SyntaxToken::Symbol(b'&'))
            if matches!(next_top_level(scanner), Some(SyntaxToken::Word(b"str"))) =>
        {
            Some(PrimitiveType::String)
        }
        _ => None,
    };
    match primitive {
        Some(primitive) => {
            push_fact(
                facts,
                SemanticFact::new(
                    EntityKind::Constant,
                    name,
                    FactType::Primitive(primitive),
                    LEAF_PRODUCT,
                ),
            )?;
        }
        None => unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::ClosedValueType,
        }),
    }
    Ok(())
}

/// Parses one static; a static whose type is outside the closed primitive set
/// is recorded exactly instead of guessed.
fn static_item<'source>(
    scanner: &mut DeclarationScanner<'source>,
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) {
    let mut name = word(next_top_level(scanner));
    if name == Some(b"mut") {
        name = word(next_top_level(scanner));
    }
    let Some(name) = name else {
        return;
    };
    if !matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(b':'))) {
        unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::NeedsFrontend,
        });
        return;
    }
    let primitive = match next_top_level(scanner) {
        Some(SyntaxToken::Word(word)) => primitive_type(word),
        Some(SyntaxToken::Symbol(b'&'))
            if matches!(next_top_level(scanner), Some(SyntaxToken::Word(b"str"))) =>
        {
            Some(PrimitiveType::String)
        }
        _ => None,
    };
    match primitive {
        Some(primitive) => {
            let _ = push_fact(
                facts,
                SemanticFact::new(
                    EntityKind::Static,
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

/// Classifies the tokens after the parameter list: an optional `->` return
/// annotation followed by `where` clauses, ending at `{` or `;`. A bracketed
/// or generic annotation never commits its inner primitive word.
fn return_annotation(scanner: &mut DeclarationScanner<'_>) -> FactType {
    let mut fact_type = FactType::Opaque;
    let mut after_arrow = false;
    let mut after_ampersand = false;
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 {
            break;
        }
        match scanned.token {
            SyntaxToken::Symbol(b'-') => {}
            SyntaxToken::Symbol(b'>') => after_arrow = true,
            SyntaxToken::Symbol(b'&') if after_arrow => after_ampersand = true,
            SyntaxToken::Symbol(b'[') | SyntaxToken::Symbol(b'<') | SyntaxToken::Symbol(b'(')
                if after_arrow =>
            {
                // A compound annotation (`[bool; 4]`, `Vec<bool>`) is not its
                // inner primitive word.
                after_arrow = false;
                after_ampersand = false;
            }
            SyntaxToken::Word(b"str") if after_arrow && after_ampersand => {
                fact_type = FactType::Primitive(PrimitiveType::String);
                after_arrow = false;
                after_ampersand = false;
            }
            SyntaxToken::Word(word) if after_arrow => {
                if let Some(primitive) = primitive_type(word) {
                    fact_type = FactType::Primitive(primitive);
                }
                after_arrow = false;
            }
            SyntaxToken::Symbol(b'{') | SyntaxToken::Symbol(b';') => break,
            _ => {}
        }
    }
    fact_type
}

fn next_word<'source>(scanner: &mut DeclarationScanner<'source>) -> Option<&'source [u8]> {
    word(next_top_level(scanner))
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
        b"bool" => Some(PrimitiveType::Bool),
        b"i32" => Some(PrimitiveType::I32),
        b"str" => Some(PrimitiveType::String),
        _ => None,
    }
}
