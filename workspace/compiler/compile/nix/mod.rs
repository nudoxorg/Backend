//! Lowering Nix into the surface IR — a sixth producer language.
//!
//! Nix is unusual among our producers in being *hybrid static + dynamic*, run
//! entirely in-process (like Python/pyrefly and TypeScript/deno_doc, not the
//! Go/Java subprocess-oracle pattern):
//!
//!   1. **Static layer** (`syntax` + `docs` + `sig`): every `.nix` file is
//!      parsed once with `rnix` (rowan CST). This yields the binding tree, doc
//!      comments (RFC 145 `/** */` + the legacy `/* Type: … */` convention),
//!      lambda formals *with default expressions*, and — uniquely — a parser
//!      for the informal `::` type-signature convention that no existing Nix
//!      tool implements. This layer alone exceeds `nixdoc` and needs zero
//!      evaluation.
//!   2. **Dynamic layer** (`eval` + `walker` + `options` + `builtins`): the
//!      flake's outputs are hermetically evaluated with the vendored
//!      `snix-eval`. This yields the *real* output tree (through re-exports,
//!      `//` merges, `callPackage`), alias groups by shared lambda span,
//!      required-flags for formals, package `meta`, NixOS options, and the
//!      builtins reference.
//!   3. **Fusion** (`context`): a runtime `Closure`'s `lambda` span is resolved
//!      back to the `rnix` node it came from, marrying runtime facts to the
//!      static record (doc comment, defaults, declared signature).
//!
//! Acquisition (FlakeHub resolution + tarball fetch + input materialization)
//! lives in `traversal`; by the time [`lower_package`] runs, the flake tree is
//! already materialized at `root`.

pub mod builtins;
pub mod context;
pub mod docs;
pub mod error;
pub mod eval;
pub mod function;
pub mod item;
pub mod options;
pub mod package;
pub mod producer;
pub mod sig;
pub mod syntax;
pub mod traversal;
pub mod types;
pub mod walker;

// Re-exports for the public surface / error mapping in the compiler pipeline.
pub use error::{NixError, Result};
pub use producer::NixProducer;

/// One-shot entry point re-export so call sites (e.g.
/// `languages::nix::lower_package`) and `GenerateError` can use a stable path,
/// mirroring the Go/Python producers.
pub use context::lower_package;
