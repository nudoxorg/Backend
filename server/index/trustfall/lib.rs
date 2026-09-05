//! The `server-index-trustfall` crate exists to adapt borrowed graph facts to typed Trustfall queries.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Trustfall projections over validated borrowed graph and semantic-image facts.

mod graph;
mod schema;

pub use graph::{
    CapturedOccurrenceSpan, IrTrustfallGraph, IrTrustfallHit, OccurrenceSourceEvidence,
    SemanticOccurrenceHit, SemanticOccurrenceStream, SemanticTrustfallGraph, SemanticTrustfallHit,
    SemanticTrustfallStream, TrustfallArgumentDiagnostic, TrustfallGraph, TrustfallGraphError,
    TrustfallHit, TrustfallOutputCause, TrustfallOutputField, TrustfallOutputNumber,
    TrustfallQueryDiagnostic, TrustfallSchemaDiagnostic, TrustfallTerminal,
    TrustfallUpstreamDiagnostic,
};
