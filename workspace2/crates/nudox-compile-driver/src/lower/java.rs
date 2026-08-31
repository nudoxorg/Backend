use nudox_ir_format::{EntityKind, PrimitiveType};

use super::{
    Declaration, java_top_level_type_name,
    scanner::{DeclarationScanner, SyntaxToken},
};
use crate::types::LoweringUnsupported;

/// Admits one primitive initialized field or method signature inside a top-level Java type.
///
/// `javac` owns Java syntax and type admission. This compact pass only recognizes the flat
/// fixture shapes (`... String NAME = ...` or `... int NAME(...)`) and otherwise falls back to
/// the enclosing type fact.
pub(super) fn declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let mut scanner = DeclarationScanner::new(source);
    let enclosing_type = java_top_level_type_name(source);
    let mut primitive = None;
    let mut name = None;
    while let Some(scanned) = scanner.next() {
        if scanned.brace_depth != 1 {
            continue;
        }
        match scanned.token {
            SyntaxToken::Word(word) => {
                if let Ok(semantic_type) = primitive_type(word) {
                    primitive = Some(semantic_type);
                    name = None;
                } else if primitive.is_some() {
                    name = Some(word);
                }
            }
            SyntaxToken::Symbol(b'=') => {
                if let (Some(semantic_type), Some(name)) = (primitive, name) {
                    return Ok(Declaration {
                        name,
                        kind: EntityKind::Constant,
                        semantic_type,
                    });
                }
            }
            SyntaxToken::Symbol(b'(') => {
                if let (Some(semantic_type), Some(name)) = (primitive, name) {
                    return Ok(Declaration {
                        name,
                        kind: EntityKind::Function,
                        semantic_type,
                    });
                }
            }
            SyntaxToken::Symbol(b';') | SyntaxToken::Symbol(b'{') | SyntaxToken::Literal => {
                primitive = None;
                name = None;
            }
            SyntaxToken::Symbol(_) => {}
        }
    }
    enclosing_type
        .map(|name| Declaration {
            name,
            kind: EntityKind::Record,
            semantic_type: PrimitiveType::String,
        })
        .ok_or(LoweringUnsupported::JavaDeclarationForm)
}

fn primitive_type(word: &[u8]) -> Result<PrimitiveType, LoweringUnsupported> {
    match word {
        b"boolean" => Ok(PrimitiveType::Bool),
        b"int" => Ok(PrimitiveType::I32),
        b"String" => Ok(PrimitiveType::String),
        _ => Err(LoweringUnsupported::JavaDeclarationForm),
    }
}
