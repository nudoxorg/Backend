//! Lowering TypeScript into the surface IR via deno-doc.
//!
//! Discovers the declaration roots for an entry point (package.json
//! types/typings/module/main/exports, triple-slash references, ...) and lowers
//! the resulting documentation graph into an `ir::Index`.
