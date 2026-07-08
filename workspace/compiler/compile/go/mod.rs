//! Lowering Go into the surface IR via a vendored `go/types` oracle.
//!
//! The Go side is deliberately thin: a small Go program (vendored under
//! `oracle/`) loads the target module with full static type information
//! (`golang.org/x/tools/go/packages` `LoadAllSyntax` + `go/types`) and
//! serializes everything the type system knows — unexported items, doc
//! comments, generics with constraint type sets, embedded fields and
//! interfaces, receivers, iota const groups, aliases vs defined types —
//! into ONE exhaustive JSON document. All lowering, shaping,
//! classification, and doc parsing happens here in Rust.
//!
//! Resolution order per module:
//!   1. Discover the module root and path via `go.mod` (`package`).
//!   2. Run the vendored oracle over the module root (`package::run_oracle`),
//!      deserializing its JSON into the `oracle` schema mirror.
//!   3. Lower every package's declarations into `ir::kind::Entry`
//!      (`context` / `item` / `function` / `types` / `docstring`).
//!   4. Tree-sitter CST extraction for function bodies is handled
//!      elsewhere in the compiler.

pub mod context;
pub mod docstring;
pub mod function;
pub mod item;
pub mod oracle;
pub mod package;
pub mod traversal;
pub mod types;
