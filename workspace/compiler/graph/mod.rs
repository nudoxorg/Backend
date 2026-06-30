//! Graph-store model: the TerminusDB-shaped projection of the compiler IR.
//!
//! The IR (`ir` crate) is the working representation; these types are its
//! document form for TerminusDB. `#[derive(TerminusDBModel)]` generates both the
//! schema (uploaded once) and the instance JSON-LD (per document), so there is
//! no hand-written schema emitter or serde glue — Rust is the source of truth.
//!
//! - [`model`] holds the derived document types.
//! - [`from_ir`] projects `ir::*` into them (`from_ir::entry` / `From<&ir::Entry>`).

pub mod from_ir;
pub mod model;
