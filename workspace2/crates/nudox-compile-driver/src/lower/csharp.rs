use nudox_ir_format::{EntityKind, PrimitiveType};

use crate::{
    lower::{
        Declaration,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    types::LoweringUnsupported,
};

/// Admits only declaration-scope `const bool|int|string name = ...` and
/// `static bool|int|string name(...)` forms. Method-local and nested-type declarations are absent.
pub(super) fn declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth > 1 {
            continue;
        }
        match scanned.token {
            SyntaxToken::Word(b"const") => {
                if let Ok(declaration) = parse_const(&mut scanner, scanned.brace_depth) {
                    return Ok(declaration);
                }
            }
            SyntaxToken::Word(b"static") => {
                if let Ok(declaration) = parse_function(&mut scanner, scanned.brace_depth) {
                    return Ok(declaration);
                }
            }
            _ => {}
        }
    }
    Err(LoweringUnsupported::CSharpDeclarationForm)
}

fn parse_const<'source>(
    scanner: &mut DeclarationScanner<'source>,
    brace_depth: u32,
) -> Result<Declaration<'source>, LoweringUnsupported> {
    let semantic_type = primitive_type(word(next_at_depth(scanner, brace_depth))?)?;
    let name = word(next_at_depth(scanner, brace_depth))?;
    while let Some(token) = next_at_depth(scanner, brace_depth) {
        match token {
            SyntaxToken::Symbol(b'=') => {
                return Ok(Declaration {
                    name,
                    kind: EntityKind::Constant,
                    semantic_type,
                });
            }
            SyntaxToken::Symbol(b';') | SyntaxToken::Symbol(b'{') | SyntaxToken::Symbol(b'}') => {
                break;
            }
            _ => {}
        }
    }
    Err(LoweringUnsupported::CSharpDeclarationForm)
}

fn parse_function<'source>(
    scanner: &mut DeclarationScanner<'source>,
    brace_depth: u32,
) -> Result<Declaration<'source>, LoweringUnsupported> {
    let semantic_type = primitive_type(word(next_at_depth(scanner, brace_depth))?)?;
    let name = word(next_at_depth(scanner, brace_depth))?;
    matches!(
        next_at_depth(scanner, brace_depth),
        Some(SyntaxToken::Symbol(b'('))
    )
    .then_some(Declaration {
        name,
        kind: EntityKind::Function,
        semantic_type,
    })
    .ok_or(LoweringUnsupported::CSharpDeclarationForm)
}

fn next_at_depth<'source>(
    scanner: &mut DeclarationScanner<'source>,
    brace_depth: u32,
) -> Option<SyntaxToken<'source>> {
    scanner
        .next()
        .filter(|scanned| scanned.brace_depth == brace_depth)
        .map(|scanned| scanned.token)
}

fn word(token: Option<SyntaxToken<'_>>) -> Result<&[u8], LoweringUnsupported> {
    match token {
        Some(SyntaxToken::Word(word)) => Ok(word),
        Some(SyntaxToken::Symbol(_)) | Some(SyntaxToken::Literal) | None => {
            Err(LoweringUnsupported::CSharpDeclarationForm)
        }
    }
}

fn primitive_type(word: &[u8]) -> Result<PrimitiveType, LoweringUnsupported> {
    match word {
        b"bool" => Ok(PrimitiveType::Bool),
        b"int" => Ok(PrimitiveType::I32),
        b"string" => Ok(PrimitiveType::String),
        _ => Err(LoweringUnsupported::CSharpDeclarationType),
    }
}
