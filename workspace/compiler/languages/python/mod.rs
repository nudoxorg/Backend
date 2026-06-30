//! Lowering Python into the surface IR via Pyrefly.
//!
//! Uses Pyrefly's type-checker to resolve a Python package's public API,
//! then lowers the resulting type-query results into an `ir::Index`.
//!
//! Resolution order per package:
//!   1. Discover the package root via `pyproject.toml` / `setup.py` / `setup.cfg`.
//!   2. Run pyrefly's checker over the source tree to resolve all names and types.
//!   3. Lower pyrefly's `Type` / `Handle` / module-info into `ir::kind::Entry`.
//!   4. Tree-sitter CST extraction for function bodies.

pub mod function;
pub mod item;
pub mod module;
pub mod package;
pub mod traversal;
pub mod types;
