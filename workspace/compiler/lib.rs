//! Compiler — handles the compilation of a package into the three resolutions
//! we care about:
//!
//! - **Treesitter CST** (`treesitter`): for resolution of implementation.
//! - **Surface IR** (the `ir` crate + `parse`): for resolution of function and
//!   structure contracts.
//! - **Tarred Source** (`tar`): for source referencing.
//!
//! The processed output is handed to `linked_data` for graph-store emission.

pub mod linked_data;
pub mod parse;
pub mod tar;
pub mod treesitter;
