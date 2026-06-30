//! Lowering TypeScript into the surface IR via deno-doc.
//!
//! Discovers the declaration roots for an entry point (package.json
//! types/typings/module/main/exports, triple-slash references, ...) and lowers
//! the resulting documentation graph into an `ir::Index`.

pub mod entry_point;
pub mod function;
pub mod item;
pub mod package;
pub mod traversal;
pub mod types;
