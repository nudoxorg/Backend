//! Defines lower behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_ir::{EntityKind, PrimitiveType};
use compiler_vocabulary::Language;

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
