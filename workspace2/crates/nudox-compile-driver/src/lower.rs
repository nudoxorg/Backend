use nudox_compile_vocab::Language;
use nudox_ir_format::{EntityKind, PrimitiveType};

use crate::types::LoweringUnsupported;

mod clang;
mod csharp;
mod go;
mod java;
mod python;
mod rust;
mod scanner;
mod typescript;

pub(super) struct Declaration<'source> {
    pub(super) name: &'source [u8],
    pub(super) kind: EntityKind,
    pub(super) semantic_type: PrimitiveType,
}

pub(crate) fn java_top_level_type_name(source: &[u8]) -> Option<&[u8]> {
    scanner::java_top_level_type_name(source)
}

pub(super) fn declaration<'source>(
    language: Language,
    source: &'source [u8],
) -> Result<Declaration<'source>, LoweringUnsupported> {
    match language {
        Language::Rust => rust::declaration(source),
        Language::Python => python::declaration(source),
        Language::Clang => clang::declaration(source),
        Language::TypeScript => typescript::declaration(source),
        Language::CSharp => csharp::declaration(source),
        Language::Go => go::declaration(source),
        Language::Java => java::declaration(source),
    }
}
