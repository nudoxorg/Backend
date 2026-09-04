//! Owns the in-process TypeScript and TSX syntax-and-binding authority boundary.
//! Preserves OXC's arena-borrowed program, module record, symbols, and diagnostics.
//! Beside it sits the first-class checker authority: a typed transaction against
//! the real TypeScript checker with its own bounded subprocess protocol.
#![forbid(unsafe_code)]

mod authority;
mod checker;
mod coordinate;
mod error;

pub use authority::{OxcDeclaration, OxcDeclarationKind, OxcModule, analyze, with_analysis};
pub use checker::{
    BoundDeclaration, BoundNarrowing, BoundReference, Checker, CheckerError, CheckerIndex,
    Declaration, LiteralBase, MappedModifier, Narrowing, ObjectMember, Origin, Reference, Report,
    TemplatePart, TypeTree, source_digest,
};
pub use coordinate::{CoordinateError, Utf8Span, Utf8ToUtf16Cursor, Utf16Span};
pub use error::AuthorityError;
// Borrowed OXC vocabulary re-exports so lowering can read span-pinned syntax
// without naming the arena-borrowed AST crate at its own dependency boundary.
pub use oxc_semantic::{ReferenceFlags, Scoping, Semantic, SymbolFlags, SymbolId};
pub use oxc_span::{GetSpan, Span};
