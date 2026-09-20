//! Version-bound vector maintenance split by ownership boundary.
//!
//! The vector leaf keeps three concerns separate: exact point decoding and
//! model facts, bounded overlay transitions, and provider-facing search.  All
//! three operate on the same immutable [`crate::Binding`] and
//! [`backend_version::CoverageWitness`]; none owns a workspace head or can
//! manufacture a newer semantic root.

mod limits;
mod overlay;
mod plan;
mod search;
mod vector;

pub use limits::{OverlayLimits, RefreshKind};
pub use overlay::ExactOverlay;
pub use plan::RefreshPlan;
pub use search::{
    AnnBase, AnnPage, AnnSource, RefreshOutcome, ScoredCandidate, VectorIndex, VectorSearchRequest,
    VectorSearchResult,
};
pub use vector::{Metric, VectorFacts, VectorPoint, VectorQuery};
