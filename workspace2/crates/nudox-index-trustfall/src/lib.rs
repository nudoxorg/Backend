#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Synchronous Trustfall projection over a validated borrowed graph view.

mod graph;
mod schema;

pub use graph::{
    TrustfallArgumentDiagnostic, TrustfallGraph, TrustfallGraphError, TrustfallHit,
    TrustfallOutputCause, TrustfallOutputField, TrustfallOutputNumber, TrustfallQueryDiagnostic,
    TrustfallSchemaDiagnostic, TrustfallTerminal, TrustfallUpstreamDiagnostic,
};
