//! Versioned semantic retention with explicit lease and pin roots.
//!
//! Generation identity, owner transitions, and root lifetimes are deliberately
//! separated into private modules.  The public facade exports only proof
//! carrying handles and the owner operations that can validate them.

mod model;
mod owner;
mod roots;

pub use model::{
    CurrentGeneration, GenerationInvalidation, RetentionBudget, RetentionCounters, RetentionStats,
    SemanticGeneration, SemanticGenerationRoot, VersionedRetention,
};
pub use roots::{SemanticPin, SemanticReservation};
