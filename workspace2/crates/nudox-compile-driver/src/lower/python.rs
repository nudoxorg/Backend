use nudox_ir_format::{EntityKind, PrimitiveType};

use crate::{lower::Declaration, types::LoweringUnsupported};

/// Admits only an unindented `identifier = Bool|I32|String` assignment.
pub(super) fn declaration(source: &[u8]) -> Result<Declaration<'_>, LoweringUnsupported> {
    for line in source.split(|byte| *byte == b'\n') {
        if line.is_empty() || line[0].is_ascii_whitespace() || line.starts_with(b"#") {
            continue;
        }
        let Some(equals) = line.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let name = trim_ascii(&line[..equals]);
        if !is_identifier(name) {
            continue;
        }
        let value = trim_ascii(&line[equals + 1..]);
        let semantic_type = if value.starts_with(b"\"") || value.starts_with(b"'") {
            PrimitiveType::String
        } else if value == b"True" || value == b"False" {
            PrimitiveType::Bool
        } else if !value.is_empty() && value.iter().all(u8::is_ascii_digit) {
            PrimitiveType::I32
        } else {
            return Err(LoweringUnsupported::PythonAssignmentValue);
        };
        return Ok(Declaration {
            name,
            kind: EntityKind::Constant,
            semantic_type,
        });
    }
    Err(LoweringUnsupported::NoSupportedDeclaration)
}

fn is_identifier(name: &[u8]) -> bool {
    let Some((first, rest)) = name.split_first() else {
        return false;
    };
    (*first == b'_' || first.is_ascii_alphabetic())
        && rest
            .iter()
            .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .map_or(bytes.len(), core::convert::identity);
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |index| index + 1);
    &bytes[start..end]
}
