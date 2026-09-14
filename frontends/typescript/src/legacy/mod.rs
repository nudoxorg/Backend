//! Owns the in-process TypeScript and TSX syntax-and-binding authority boundary.
//! Preserves OXC's arena-borrowed program, module record, symbols, and diagnostics.
//! Beside it sits the first-class checker authority: a typed transaction against
//! the real TypeScript checker with its own bounded subprocess protocol.

mod authority;
mod checker;
mod coordinate;
mod error;

pub use self::authority::{
    OxcDeclaration, OxcDeclarationKind, OxcModule, SyntaxMappedModifier, analyze,
    syntax_mapped_modifier, with_analysis,
};
pub use self::checker::{
    BoundDeclaration, BoundNarrowing, BoundReference, Checker, CheckerError, CheckerIndex,
    Declaration, ExplicitTypeScriptChecker, LiteralBase, MappedModifier, Narrowing, ObjectMember,
    Origin, Reference, Report, TemplatePart, TypeScriptCheckerProgram,
    TypeScriptCheckerProgramError, TypeScriptCheckerProgramView, TypeScriptModuleRoot,
    TypeScriptModuleRootView, TypeTree, source_digest,
};
pub use self::coordinate::{CoordinateError, Utf8Span, Utf8ToUtf16Cursor, Utf16Span};
pub use self::error::AuthorityError;
// Borrowed OXC vocabulary re-exports so lowering can read span-pinned syntax
// without naming the arena-borrowed AST crate at its own dependency boundary.
pub use oxc_semantic::{ReferenceFlags, Scoping, Semantic, SymbolFlags, SymbolId};
pub use oxc_span::{GetSpan, Span};
pub use oxc_syntax::node::NodeId;
