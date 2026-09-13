//! Shared extension-pool grammar.
//!
//! The public module is intentionally a narrow façade: `model` owns the
//! input contract, `admit` proves fresh inputs, `encode` owns current writes,
//! and `reopen` owns compatible borrowed discovery. Callers never depend on
//! storage choreography or legacy decoding details.

mod admit;
mod encode;
mod grammar;
mod model;
mod reopen;

pub use model::*;
pub use reopen::*;
