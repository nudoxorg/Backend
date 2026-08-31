use nudox_compile_vocab::Language;
use nudox_ir_format::{EntityKind, PrimitiveType};

pub(super) struct Declaration<'source> {
    pub(super) name: &'source [u8],
    pub(super) kind: EntityKind,
    pub(super) semantic_type: PrimitiveType,
}

pub(super) fn declaration<'source>(
    language: Language,
    source: &'source [u8],
) -> Option<Declaration<'source>> {
    match language {
        Language::Rust => rust_declaration(source),
        Language::Python => assignment(source),
        Language::Clang => clang_declaration(source),
        Language::TypeScript | Language::Go | Language::Java | Language::CSharp => None,
    }
}

fn rust_declaration(source: &[u8]) -> Option<Declaration<'_>> {
    if let Some(name) = identifier_after(source, b"pub fn ") {
        return Some(Declaration {
            name,
            kind: EntityKind::Function,
            semantic_type: PrimitiveType::I32,
        });
    }
    let name = identifier_after(source, b"pub const ")?;
    let semantic_type = if source
        .windows(b": bool".len())
        .any(|window| window == b": bool")
    {
        PrimitiveType::Bool
    } else {
        PrimitiveType::I32
    };
    Some(Declaration {
        name,
        kind: EntityKind::Constant,
        semantic_type,
    })
}

fn assignment(source: &[u8]) -> Option<Declaration<'_>> {
    let name_end = source.iter().position(|byte| *byte == b'=')?;
    let name = trim_ascii(&source[..name_end]);
    (!name.is_empty()).then_some(Declaration {
        name,
        kind: EntityKind::Constant,
        semantic_type: PrimitiveType::I32,
    })
}

fn clang_declaration(source: &[u8]) -> Option<Declaration<'_>> {
    let prefix = b"const char *";
    let name = identifier_after(source, prefix)?;
    Some(Declaration {
        name,
        kind: EntityKind::Constant,
        semantic_type: PrimitiveType::I32,
    })
}

fn identifier_after<'source>(source: &'source [u8], prefix: &[u8]) -> Option<&'source [u8]> {
    let start = source
        .windows(prefix.len())
        .position(|window| window == prefix)?
        + prefix.len();
    let name = &source[start..];
    let end = name
        .iter()
        .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
        .unwrap_or(name.len());
    let name = &name[..end];
    (!name.is_empty()).then_some(name)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |index| index + 1);
    &bytes[start..end]
}
