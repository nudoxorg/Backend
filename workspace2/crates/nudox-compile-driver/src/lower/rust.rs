use nudox_ir_format::{EntityKind, PrimitiveType};

use crate::{
    lower::{
        Declaration,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    types::LoweringUnsupported,
};

/// Admits only top-level `pub const name: bool|i32|&str = ...` declarations.
pub(super) fn declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 || !matches!(scanned.token, SyntaxToken::Word(b"pub")) {
            continue;
        }
        match next_top_level(&mut scanner) {
            Some(SyntaxToken::Word(b"const")) => return parse_const(&mut scanner),
            Some(SyntaxToken::Word(b"fn")) => return Err(LoweringUnsupported::RustFunction),
            Some(SyntaxToken::Word(_))
            | Some(SyntaxToken::Symbol(_))
            | Some(SyntaxToken::Literal)
            | None => {}
        }
    }
    Err(LoweringUnsupported::NoSupportedDeclaration)
}

fn parse_const<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Result<Declaration<'source>, LoweringUnsupported> {
    let name = word(next_top_level(scanner)).ok_or(LoweringUnsupported::RustConstantType)?;
    expect_symbol(scanner, b':')?;
    let semantic_type = match next_top_level(scanner) {
        Some(SyntaxToken::Word(b"bool")) => PrimitiveType::Bool,
        Some(SyntaxToken::Word(b"i32")) => PrimitiveType::I32,
        Some(SyntaxToken::Symbol(b'&'))
            if matches!(next_top_level(scanner), Some(SyntaxToken::Word(b"str"))) =>
        {
            PrimitiveType::String
        }
        Some(SyntaxToken::Word(_))
        | Some(SyntaxToken::Symbol(_))
        | Some(SyntaxToken::Literal)
        | None => return Err(LoweringUnsupported::RustConstantType),
    };
    expect_symbol(scanner, b'=')?;
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
        .and_then(|scanned| (scanned.brace_depth == 0).then_some(scanned.token))
}

fn expect_symbol(
    scanner: &mut DeclarationScanner<'_>,
    expected: u8,
) -> Result<(), LoweringUnsupported> {
    matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(actual)) if actual == expected)
        .then_some(())
        .ok_or(LoweringUnsupported::RustConstantType)
}

fn word(token: Option<SyntaxToken<'_>>) -> Option<&[u8]> {
    match token {
        Some(SyntaxToken::Word(word)) => Some(word),
        Some(SyntaxToken::Symbol(_)) | Some(SyntaxToken::Literal) | None => None,
    }
}
