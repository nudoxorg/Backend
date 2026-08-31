use nudox_ir_format::{EntityKind, PrimitiveType};

use crate::{
    lower::{
        Declaration,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    types::LoweringUnsupported,
};

/// Admits only top-level `const name: boolean|number|string = ...` and
/// `function name(...): boolean|number|string { ... }` declarations.
pub(super) fn declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 {
            continue;
        }
        match scanned.token {
            SyntaxToken::Word(b"export") => match next_top_level(&mut scanner) {
                Some(SyntaxToken::Word(b"const")) => return parse_const(&mut scanner),
                Some(SyntaxToken::Word(b"function")) => return parse_function(&mut scanner),
                _ => continue,
            },
            SyntaxToken::Word(b"const") => return parse_const(&mut scanner),
            SyntaxToken::Word(b"function") => return parse_function(&mut scanner),
            _ => {}
        }
    }
    Err(LoweringUnsupported::TypeScriptDeclarationForm)
}

fn parse_const<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Result<Declaration<'source>, LoweringUnsupported> {
    let name =
        word(next_top_level(scanner)).ok_or(LoweringUnsupported::TypeScriptDeclarationForm)?;
    expect_symbol(scanner, b':')?;
    let semantic_type = primitive_type(
        word(next_top_level(scanner)).ok_or(LoweringUnsupported::TypeScriptDeclarationType)?,
    )?;
    while let Some(token) = next_top_level(scanner) {
        match token {
            SyntaxToken::Symbol(b'=') => {
                return Ok(Declaration {
                    name,
                    kind: EntityKind::Constant,
                    semantic_type,
                });
            }
            SyntaxToken::Symbol(b';') => break,
            _ => {}
        }
    }
    Err(LoweringUnsupported::TypeScriptDeclarationForm)
}

fn parse_function<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Result<Declaration<'source>, LoweringUnsupported> {
    let name =
        word(next_top_level(scanner)).ok_or(LoweringUnsupported::TypeScriptDeclarationForm)?;
    expect_symbol(scanner, b'(')?;
    skip_parenthesized(scanner)?;
    expect_symbol(scanner, b':')?;
    let semantic_type = primitive_type(
        word(next_top_level(scanner)).ok_or(LoweringUnsupported::TypeScriptDeclarationType)?,
    )?;
    Ok(Declaration {
        name,
        kind: EntityKind::Function,
        semantic_type,
    })
}

fn next_top_level<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Option<SyntaxToken<'source>> {
    scanner
        .next()
        .filter(|scanned| scanned.brace_depth == 0)
        .map(|scanned| scanned.token)
}

fn expect_symbol(
    scanner: &mut DeclarationScanner<'_>,
    expected: u8,
) -> Result<(), LoweringUnsupported> {
    matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(actual)) if actual == expected)
        .then_some(())
        .ok_or(LoweringUnsupported::TypeScriptDeclarationForm)
}

fn skip_parenthesized(scanner: &mut DeclarationScanner<'_>) -> Result<(), LoweringUnsupported> {
    let mut depth = 1_u32;
    while let Some(token) = next_top_level(scanner) {
        match token {
            SyntaxToken::Symbol(b'(') => depth = depth.saturating_add(1),
            SyntaxToken::Symbol(b')') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Err(LoweringUnsupported::TypeScriptDeclarationForm)
}

fn word(token: Option<SyntaxToken<'_>>) -> Option<&[u8]> {
    match token {
        Some(SyntaxToken::Word(word)) => Some(word),
        Some(SyntaxToken::Symbol(_)) | Some(SyntaxToken::Literal) | None => None,
    }
}

fn primitive_type(word: &[u8]) -> Result<PrimitiveType, LoweringUnsupported> {
    match word {
        b"boolean" => Ok(PrimitiveType::Bool),
        b"number" => Ok(PrimitiveType::I32),
        b"string" => Ok(PrimitiveType::String),
        _ => Err(LoweringUnsupported::TypeScriptDeclarationType),
    }
}
