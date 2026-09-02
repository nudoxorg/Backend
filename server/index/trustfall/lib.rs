//! The `server-index-trustfall` crate exists to adapt borrowed graph facts to typed Trustfall queries.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Synchronous Trustfall projection over a validated borrowed graph view.

mod graph;
mod schema;

pub use graph::{
    IrTrustfallGraph, IrTrustfallHit, TrustfallArgumentDiagnostic, TrustfallGraph,
    TrustfallGraphError, TrustfallHit, TrustfallOutputCause, TrustfallOutputField,
    TrustfallOutputNumber, TrustfallQueryDiagnostic, TrustfallSchemaDiagnostic, TrustfallTerminal,
    TrustfallUpstreamDiagnostic,
};
