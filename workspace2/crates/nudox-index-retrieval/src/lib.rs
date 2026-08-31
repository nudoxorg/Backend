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
mod tantivy;
mod terminal;
mod trustfall;

pub use boundary::RetrievalBoundary;
pub use exact::ExactRoute;
pub use lexical::LexicalRoute;
pub use terminal::{
    CancellationCause, RetrievalAbsence, RetrievalCoverage, RetrievalDegradation, RetrievalFailure,
    RetrievalOperationTerminal, RetrievalResult, VectorAuthoritySurface,
};
