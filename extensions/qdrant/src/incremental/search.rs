//! Version-bound ANN search facade.
//!
//! The child modules separate immutable base admission, provider contracts,
//! index lifecycle, quality evidence, and exact scoring.  They all share the
//! typed relation delta and binding from the parent incremental boundary.

mod base;
mod index;
mod provider;
mod quality;
mod score;

pub use base::AnnBase;
pub use index::{RefreshOutcome, VectorIndex};
pub use provider::{
    AnnCursor, AnnPage, AnnSource, ScoredCandidate, VectorQueryBinding, VectorSearchRequest,
    VectorSearchResult,
};
