use nudox_ir_format::{EntityKind, PrimitiveType};

use crate::{
    lower::{
        Declaration,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    types::LoweringUnsupported,
};

/// Admits only top-level single-line `const name [bool|string|int] = literal` and
/// return-typed `func name(...) primitive` declarations.
pub(super) fn declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 {
            continue;
        }
        match scanned.token {
            SyntaxToken::Word(b"const") => return parse_const(&mut scanner),
            SyntaxToken::Word(b"func") => return parse_func(&mut scanner),
            SyntaxToken::Word(_) | SyntaxToken::Symbol(_) | SyntaxToken::Literal => {}
        }
    }
    Err(LoweringUnsupported::GoDeclarationForm)
}

fn parse_func<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Result<Declaration<'source>, LoweringUnsupported> {
    let name = match scanner.next() {
        Some(scanned) if scanned.brace_depth == 0 => match scanned.token {
            SyntaxToken::Word(name) => name,
            SyntaxToken::Symbol(b'(') => {
                skip_balanced(scanner, b'(', b')');
                word(next_top_level(scanner))?
            }
            SyntaxToken::Symbol(_) | SyntaxToken::Literal => {
                return Err(LoweringUnsupported::GoDeclarationForm);
            }
        },
        _ => return Err(LoweringUnsupported::GoDeclarationForm),
    };
    if !matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(b'('))) {
        return Err(LoweringUnsupported::GoDeclarationForm);
    }
    skip_balanced(scanner, b'(', b')');
    let return_type = match next_top_level(scanner) {
        Some(SyntaxToken::Word(word)) => primitive_type(word)?,
        Some(SyntaxToken::Symbol(_)) | Some(SyntaxToken::Literal) | None => {
            return Err(LoweringUnsupported::GoDeclarationForm);
        }
    };
    Ok(Declaration {
        name,
        kind: EntityKind::Function,
        semantic_type: return_type,
    })
}

fn parse_const<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Result<Declaration<'source>, LoweringUnsupported> {
    let name = word(next_top_level(scanner))?;
    let type_or_equals = next_top_level(scanner).ok_or(LoweringUnsupported::GoDeclarationForm)?;
    let semantic_type = match type_or_equals {
        SyntaxToken::Symbol(b'=') => value_type(next_top_level(scanner))?,
        SyntaxToken::Word(word) => {
            expect_equals(scanner)?;
            primitive_type(word)?
        }
        SyntaxToken::Symbol(_) | SyntaxToken::Literal => {
            return Err(LoweringUnsupported::GoDeclarationForm);
        }
    };
    Ok(Declaration {
        name,
        kind: EntityKind::Constant,
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

fn word(token: Option<SyntaxToken<'_>>) -> Result<&[u8], LoweringUnsupported> {
    match token {
        Some(SyntaxToken::Word(word)) => Ok(word),
        Some(SyntaxToken::Symbol(_)) | Some(SyntaxToken::Literal) | None => {
            Err(LoweringUnsupported::GoDeclarationForm)
        }
    }
}

fn expect_equals(scanner: &mut DeclarationScanner<'_>) -> Result<(), LoweringUnsupported> {
    matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(b'=')))
        .then_some(())
        .ok_or(LoweringUnsupported::GoDeclarationForm)
}

fn skip_balanced(scanner: &mut DeclarationScanner<'_>, open: u8, close: u8) {
    let mut depth = 1_u32;
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 {
            continue;
        }
        match scanned.token {
            SyntaxToken::Symbol(byte) if byte == open => depth = depth.saturating_add(1),
            SyntaxToken::Symbol(byte) if byte == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return;
                }
            }
            SyntaxToken::Word(_) | SyntaxToken::Symbol(_) | SyntaxToken::Literal => {}
        }
    }
}

fn primitive_type(word: &[u8]) -> Result<PrimitiveType, LoweringUnsupported> {
    match word {
        b"bool" => Ok(PrimitiveType::Bool),
        b"int" => Ok(PrimitiveType::I32),
        b"string" => Ok(PrimitiveType::String),
        _ => Err(LoweringUnsupported::GoDeclarationType),
    }
}

fn value_type(token: Option<SyntaxToken<'_>>) -> Result<PrimitiveType, LoweringUnsupported> {
    match token {
        Some(SyntaxToken::Literal) => Ok(PrimitiveType::String),
        Some(SyntaxToken::Word(b"true" | b"false")) => Ok(PrimitiveType::Bool),
        Some(SyntaxToken::Word(_)) | Some(SyntaxToken::Symbol(_)) | None => {
            Err(LoweringUnsupported::GoDeclarationType)
        }
    }
}
