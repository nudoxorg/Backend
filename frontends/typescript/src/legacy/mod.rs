//! Owns the in-process TypeScript and TSX syntax-and-binding authority boundary.
//! Preserves OXC's arena-borrowed program, module record, symbols, and diagnostics.
//! Beside it sits the first-class checker authority: a typed transaction against
//! the real TypeScript checker with its own bounded subprocess protocol.
//!
//! CANONICAL AUTHORITY PATH: this `legacy` module IS the production
//! native-authority lane. The engine driver imports its symbols directly from
//! this module; there is no intervening adapter. The crate-level
//! `syntax_frontend()` constructor is the separate, documented structural
//! baseline and never substitutes for this authority.

mod authority;
mod checker;
mod coordinate;
mod error;

pub use self::authority::{
    OxcDeclaration, OxcDeclarationKind, OxcModule, SyntaxMappedModifier, analyze,
    analyze_with_declaration, syntax_mapped_modifier, with_analysis, with_analysis_declaration,
};
pub use self::checker::{
    BoundDeclaration, BoundNarrowing, BoundReference, Checker, CheckerError, CheckerIndex,
    Declaration, ExplicitTypeScriptChecker, LiteralBase, MappedModifier, Narrowing, ObjectMember,
    Origin, Parameter, Reference, Report, TemplatePart, TypeScriptCheckerProgram,
    TypeScriptCheckerProgramError, TypeScriptCheckerProgramView, TypeScriptModuleRoot,
    TypeScriptModuleRootView, TypeTree, source_digest,
};
pub use self::coordinate::{CoordinateError, Utf8Span, Utf8ToUtf16Cursor, Utf16Span};
pub use self::error::AuthorityError;
// Borrowed OXC vocabulary re-exports so lowering can read span-pinned syntax
// without naming the arena-borrowed AST crate at its own dependency boundary.
pub use oxc_ast::AstKind;
pub use oxc_semantic::{ReferenceFlags, Scoping, Semantic, SymbolFlags, SymbolId};
pub use oxc_span::{GetSpan, Span};
pub use oxc_syntax::node::NodeId;
