use nudox_ir_format::{EntityKind, PrimitiveType};

use crate::{
    lower::{
        Declaration,
        scanner::{DeclarationScanner, SyntaxToken},
    },
    types::LoweringUnsupported,
};

/// Admits only top-level `const char *name = ...` declarations.
pub(super) fn declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 0 || !matches!(scanned.token, SyntaxToken::Word(b"const")) {
            continue;
        }
        return parse_const_char(&mut scanner);
    }
    Err(LoweringUnsupported::NoSupportedDeclaration)
}

fn parse_const_char<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Result<Declaration<'source>, LoweringUnsupported> {
    if !matches!(next_top_level(scanner), Some(SyntaxToken::Word(b"char")))
        || !matches!(next_top_level(scanner), Some(SyntaxToken::Symbol(b'*')))
    {
        return Err(LoweringUnsupported::ClangDeclarationForm);
    }
    let name = match next_top_level(scanner) {
        Some(SyntaxToken::Word(name)) => name,
        Some(SyntaxToken::Symbol(_)) | Some(SyntaxToken::Literal) | None => {
            return Err(LoweringUnsupported::ClangDeclarationForm);
        }
    };
    Ok(Declaration {
        name,
        kind: EntityKind::Constant,
        semantic_type: PrimitiveType::String,
    })
}

fn next_top_level<'source>(
    scanner: &mut DeclarationScanner<'source>,
) -> Option<SyntaxToken<'source>> {
    scanner
        .next()
        .and_then(|scanned| (scanned.brace_depth == 0).then_some(scanned.token))
}
