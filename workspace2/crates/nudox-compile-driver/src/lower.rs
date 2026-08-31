use nudox_compile_vocab::Language;
use nudox_ir_format::{EntityKind, PrimitiveType};

use crate::types::LoweringUnsupported;

pub(super) struct Declaration<'source> {
    pub(super) name: &'source [u8],
    pub(super) kind: EntityKind,
    pub(super) semantic_type: PrimitiveType,
}

pub(super) fn declaration<'source>(
    language: Language,
    source: &'source [u8],
) -> Result<Declaration<'source>, LoweringUnsupported> {
    match language {
        Language::Rust => rust_declaration(source),
        Language::Python => assignment(source),
        Language::Clang => clang_declaration(source),
        Language::TypeScript | Language::Go | Language::Java | Language::CSharp => {
            Err(LoweringUnsupported::NoSupportedDeclaration)
        }
    }
}

fn rust_declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let prefix = b"pub const ";
    let declaration_start = match source
        .windows(prefix.len())
        .position(|window| window == prefix)
    {
        Some(start) => start + prefix.len(),
        None if identifier_after(source, b"pub fn ").is_some() => {
            return Err(LoweringUnsupported::RustFunction);
        }
        None => return Err(LoweringUnsupported::NoSupportedDeclaration),
    };
    let declaration = &source[declaration_start..];
    let name_end = match declaration
        .iter()
        .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
    {
        Some(end) => end,
        None => declaration.len(),
    };
    let name = &declaration[..name_end];
    if name.is_empty() {
        return Err(LoweringUnsupported::NoSupportedDeclaration);
    }
    let after_name = &declaration[name_end..];
    let equals = after_name
        .iter()
        .position(|byte| *byte == b'=')
        .ok_or(LoweringUnsupported::RustConstantType)?;
    let colon = after_name[..equals]
        .iter()
        .position(|byte| *byte == b':')
        .ok_or(LoweringUnsupported::RustConstantType)?;
    let declared_type = trim_ascii(&after_name[colon + 1..equals]);
    let semantic_type = match declared_type {
        b"bool" => PrimitiveType::Bool,
        b"i32" => PrimitiveType::I32,
        b"&str" => PrimitiveType::String,
        _ => return Err(LoweringUnsupported::RustConstantType),
    };
    Ok(Declaration {
        name,
        kind: EntityKind::Constant,
        semantic_type,
    })
}

fn assignment(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let name_end = source
        .iter()
        .position(|byte| *byte == b'=')
        .ok_or(LoweringUnsupported::NoSupportedDeclaration)?;
    let name = trim_ascii(&source[..name_end]);
    let value = trim_ascii(&source[name_end + 1..]);
    let semantic_type = if value.starts_with(b"\"") || value.starts_with(b"'") {
        PrimitiveType::String
    } else if value == b"True" || value == b"False" {
        PrimitiveType::Bool
    } else if !value.is_empty() && value.iter().all(u8::is_ascii_digit) {
        PrimitiveType::I32
    } else {
        return Err(LoweringUnsupported::PythonAssignmentValue);
    };
    (!name.is_empty())
        .then_some(Declaration {
            name,
            kind: EntityKind::Constant,
            semantic_type,
        })
        .ok_or(LoweringUnsupported::PythonAssignmentName)
}

fn clang_declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    let prefix = b"const char *";
    let name = identifier_after(source, prefix).ok_or_else(|| {
        if source
            .windows(b"const".len())
            .any(|window| window == b"const")
        {
            LoweringUnsupported::ClangDeclarationForm
        } else {
            LoweringUnsupported::NoSupportedDeclaration
        }
    })?;
    Ok(Declaration {
        name,
        kind: EntityKind::Constant,
        semantic_type: PrimitiveType::String,
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
