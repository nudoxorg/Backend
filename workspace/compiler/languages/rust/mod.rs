//! Lowering Rust into the surface IR via `cargo rustdoc`'s JSON output.
//!
//! Resolves documented local/workspace packages, memory-maps the produced
//! `target/doc/{crate}.json`, and produces an `ir::Index` plus a fq-name →
//! source-text map for downstream tree-sitter extraction.

pub mod context;
pub mod function;
pub mod generics;
pub mod item;
pub mod package;
pub mod traversal;
pub mod types;
