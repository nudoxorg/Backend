//! Graph-store model: the graph-native TerminusDB projection of the compiler IR.
//!
//! The IR (`ir` crate) is the working representation; these types are its
//! graph form for TerminusDB. `#[derive(TerminusDBModel)]` generates both the
//! schema (uploaded once) and the instance JSON-LD (per document), so there is
//! no hand-written schema emitter or serde glue — Rust is the source of truth.
//! Design rationale and the query cookbook live in GRAPH-ARCHITECTURE.md at
//! the repo root.
//!
//! - [`model`] holds the derived document types: [`model::Symbol`] nodes with
//!   real link edges (`member_of`/`implements`/`extends`/`mentions`/…), the
//!   inline [`model::Shape`] payload, and the reified
//!   [`model::Implementation`] / [`model::Reference`] relation nodes.
//! - [`link`] is the resolution pass: name-strings → symbol IRIs, total via
//!   stub minting.
//! - [`from_ir`] projects an indexed package into a [`from_ir::GraphCorpus`]
//!   (`from_ir::project`).

pub mod from_ir;
pub mod link;
pub mod model;
