//! The `server-index-retrieval` crate exists to compose exact, lexical, graph, and vector retrieval under one snapshot authority.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Server-only operation terminals over a sealed durable index snapshot.
//!
//! Exact, lexical, Tantivy, Trustfall, and Qdrant retain their narrow synchronous contracts. This
//! crate is the sole runtime boundary that samples cancellation and classifies their outcomes into
//! one closed five-class operation vocabulary.

mod boundary;
mod exact;
mod lexical;
mod qdrant;
mod source;
mod tantivy;
mod terminal;
mod trustfall;

pub use boundary::{
    RetrievalBoundary, RetrievalBoundaryError, RetrievalBoundaryEvidence,
    RetrievalBoundaryEvidenceView, RetrievalBoundaryView,
};
pub use exact::ExactRoute;
pub use lexical::LexicalRoute;
pub use source::{
    CanonicalOccurrenceSource, CanonicalSource, CanonicalSourceError,
    OwnedCanonicalOccurrenceSource, OwnedCanonicalOccurrenceSpan, OwnedCanonicalSource,
    OwnedUnavailableOccurrenceSource, SourceWireError, VerifiedSourceImage,
    resolve_occurrence_source, resolve_occurrence_sources, resolve_tantivy_source,
    resolve_tantivy_sources,
};
pub use terminal::{
    CancellationCause, RetrievalAbsence, RetrievalCoverage, RetrievalDegradation, RetrievalFailure,
    RetrievalOperationTerminal, RetrievalResult, VectorAuthoritySurface, VectorDegradation,
    VectorRoute,
};
