#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Synchronous Trustfall projection over a validated borrowed graph view.

mod graph;
mod schema;

pub use graph::{
    TrustfallGraph, TrustfallGraphError, TrustfallHit, TrustfallOutputField, TrustfallStaticPhase,
    TrustfallTerminal,
};
