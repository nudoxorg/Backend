//! Semantic admission and maintenance errors.

use backend_flow::FlowError;
use std::fmt;

/// Errors raised while admitting or incrementally maintaining semantic facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticError {
    /// A set/replacement contained the same fact twice.
    DuplicateFact,
    /// A dependency manifest is already retained by the same registry.
    DuplicateManifest,
    /// A dependency manifest contained the same read twice.
    DuplicateRead,
    /// Multiplicity/support was zero or otherwise invalid.
    InvalidSupport,
    /// A supplied source span had reversed bounds.
    InvalidSpan,
    /// A complete replacement was required but coverage was incomplete.
    IncompleteCoverage,
    /// A negative dependency lacked a complete witness.
    IncompleteNegativeFact,
    /// A negative constructor was used for a positive read.
    NegativeReadRequired,
    /// A positive constructor was used for a negative read.
    PositiveReadRequired,
    /// A weighted update had zero weight.
    ZeroWeight,
    /// A retraction had no surviving support.
    MissingSupport,
    /// A repeated occurrence address changed its value.
    ConflictingOccurrence,
    /// A checked integer operation overflowed.
    Overflow,
    /// A complete update changed the authority scope without replacement.
    ScopeMismatch,
    /// A value/state pair does not obey the facet coverage law.
    InvalidCoverageState,
    /// A typed value does not belong to the logical facet key.
    InvalidFacetBinding,
    /// A type value does not obey its logical type binding.
    InvalidTypeBinding,
    /// An SCC/component descriptor has inconsistent members, edges, or inputs.
    InvalidComponent,
    /// Authority/source provenance does not match its bound identities.
    InvalidProvenance,
    /// A source or declaration identity is malformed.
    InvalidIdentity,
    /// A typed source/value payload is malformed.
    InvalidValue,
    /// An authority scope has invalid identity, range, or facet ownership.
    InvalidAuthorityScope,
    /// A replacement is not safe for the declared authority scope.
    InvalidReplacement,
    /// A range or selector has invalid bounds.
    InvalidSelector,
    /// A dependency graph would make a recipe validate itself, directly or
    /// through another recipe.
    DependencyCycle,
    /// A retained output does not match the exact requested dependency roots.
    ReuseMismatch,
    /// A versioned semantic lease or pin names a generation that is no
    /// longer the selected live generation for its reader.
    StaleGeneration,
    /// A reader already has a live generation with a different manifest.
    ConflictingGeneration,
    /// Retention cannot admit another rooted generation under its explicit
    /// memory envelope.
    RetentionCapacity,
    /// A bounded dependency/index operation exceeded its work envelope.
    ReuseWorkLimit,
    /// A flow operator rejected an incomplete frontier.
    Flow(FlowError),
}

impl fmt::Display for SemanticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "semantic admission error: {self:?}")
    }
}

impl std::error::Error for SemanticError {}
