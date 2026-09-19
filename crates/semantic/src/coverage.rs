//! Coverage witnesses and explicit facet absence states.

use backend_version::{
    AuthorizedCompleteCoverage, CoverageWitness, ScopeRoot, UntrustedCoverageScope,
};

/// Whether a complete facet contains a value or records an explicit absence.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Deletion {
    /// A value is present and live.
    Live,
    /// The authority captured an empty set/value.
    CapturedEmpty,
    /// The authority completely observed that no value exists at this key.
    Absent,
    /// A prior live fact was removed by an admitted replacement.
    Deleted,
}

/// Coarse availability exposed to readers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Availability {
    /// A live value was captured.
    Present,
    /// A complete observation captured an empty value or set.
    CapturedEmpty,
    /// A complete observation captured absence at a key.
    Absent,
    /// A complete replacement removed a previously owned fact.
    Deleted,
    /// The authority could not observe the scope.
    Unavailable,
    /// The authority does not implement the facet.
    Unsupported,
    /// Only a subset of the scope was observed.
    Partial,
}

/// Coverage and explicit value state for one semantic facet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FacetCoverage {
    witness: CoverageWitness,
    deletion: Deletion,
}

impl FacetCoverage {
    /// Creates a live complete observation from an admitted witness.
    #[must_use]
    pub const fn complete(value: AuthorizedCompleteCoverage) -> Self {
        Self {
            witness: CoverageWitness::Complete(value),
            deletion: Deletion::Live,
        }
    }

    /// Creates a complete observation of an empty value/set.
    #[must_use]
    pub const fn captured_empty(value: AuthorizedCompleteCoverage) -> Self {
        Self {
            witness: CoverageWitness::Complete(value),
            deletion: Deletion::CapturedEmpty,
        }
    }

    /// Creates a complete observation that a key has no value.
    #[must_use]
    pub const fn absent(value: AuthorizedCompleteCoverage) -> Self {
        Self {
            witness: CoverageWitness::Complete(value),
            deletion: Deletion::Absent,
        }
    }

    /// Creates an admitted complete deletion from a replacement scope.
    #[must_use]
    pub const fn deleted(value: AuthorizedCompleteCoverage) -> Self {
        Self {
            witness: CoverageWitness::Complete(value),
            deletion: Deletion::Deleted,
        }
    }

    /// Creates a partial observation for a scope.
    #[must_use]
    pub fn partial<S: ScopeIdentity>(scope: S) -> Self {
        Self::partial_scope(scope.scope_root())
    }

    /// Creates a partial observation for an exact authority scope.
    #[must_use]
    pub const fn partial_scope(scope: ScopeRoot) -> Self {
        Self {
            witness: CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(scope)),
            deletion: Deletion::Live,
        }
    }

    /// Creates an unavailable observation for a scope.
    #[must_use]
    pub fn unavailable<S: ScopeIdentity>(scope: S) -> Self {
        Self::unavailable_scope(scope.scope_root())
    }

    /// Creates an unavailable observation for an exact authority scope.
    #[must_use]
    pub const fn unavailable_scope(scope: ScopeRoot) -> Self {
        Self {
            witness: CoverageWitness::Unavailable(UntrustedCoverageScope::from_scope_root(scope)),
            deletion: Deletion::Live,
        }
    }

    /// Creates an unsupported observation for a scope.
    #[must_use]
    pub fn unsupported<S: ScopeIdentity>(scope: S) -> Self {
        Self::unsupported_scope(scope.scope_root())
    }

    /// Creates an unsupported observation for an exact authority scope.
    #[must_use]
    pub const fn unsupported_scope(scope: ScopeRoot) -> Self {
        Self {
            witness: CoverageWitness::Unsupported(UntrustedCoverageScope::from_scope_root(scope)),
            deletion: Deletion::Live,
        }
    }

    /// Returns the exact observation state visible to a reader.
    #[must_use]
    pub const fn availability(self) -> Availability {
        match self.witness {
            CoverageWitness::Complete(_) => match self.deletion {
                Deletion::Live => Availability::Present,
                Deletion::CapturedEmpty => Availability::CapturedEmpty,
                Deletion::Absent => Availability::Absent,
                Deletion::Deleted => Availability::Deleted,
            },
            CoverageWitness::Partial(_)
            | CoverageWitness::UntrustedComplete(_)
            | CoverageWitness::Closed(_) => Availability::Partial,
            CoverageWitness::Unavailable(_) => Availability::Unavailable,
            CoverageWitness::Unsupported(_) => Availability::Unsupported,
        }
    }

    /// Returns whether this observation can authorize a scoped replacement.
    #[must_use]
    pub const fn authoritative(self) -> bool {
        matches!(self.witness, CoverageWitness::Complete(_))
    }

    /// Returns whether this witness proves complete coverage.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self.witness, CoverageWitness::Complete(_))
    }

    /// Returns the underlying witness.
    #[must_use]
    pub const fn witness(self) -> CoverageWitness {
        self.witness
    }

    /// Returns the explicit value/deletion state.
    #[must_use]
    pub const fn deletion(self) -> Deletion {
        self.deletion
    }

    /// Returns the complete backend scope identity carried by this witness.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        self.witness.scope_root()
    }

    /// Compares the full scope identity of two observations.
    #[must_use]
    pub fn same_scope(self, other: Self) -> bool {
        self.scope_root() == other.scope_root()
    }
}

/// Input accepted by compatibility constructors for non-authoritative coverage.
///
/// New callers should pass [`ScopeRoot`] so the complete authority scope is
/// retained.  The numeric implementation is retained only for old callers;
/// it is expanded to the backend's full-width scope representation before it
/// enters a witness.
pub trait ScopeIdentity {
    /// Converts this value to the exact backend scope root.
    fn scope_root(self) -> ScopeRoot;
}

impl ScopeIdentity for ScopeRoot {
    fn scope_root(self) -> ScopeRoot {
        self
    }
}

impl ScopeIdentity for u64 {
    fn scope_root(self) -> ScopeRoot {
        ScopeRoot::from_u64(self)
    }
}

/// A value paired with per-facet coverage and explicit absence state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FacetObservation<T> {
    value: Option<T>,
    coverage: FacetCoverage,
}

impl<T> FacetObservation<T> {
    /// Constructs a complete live value.
    #[must_use]
    pub fn present(value: T, coverage: AuthorizedCompleteCoverage) -> Self {
        Self {
            value: Some(value),
            coverage: FacetCoverage::complete(coverage),
        }
    }

    /// Constructs a complete captured-empty value.
    #[must_use]
    pub fn captured_empty(coverage: AuthorizedCompleteCoverage) -> Self {
        Self {
            value: None,
            coverage: FacetCoverage::captured_empty(coverage),
        }
    }

    /// Constructs a complete absence observation.
    #[must_use]
    pub fn absent(coverage: AuthorizedCompleteCoverage) -> Self {
        Self {
            value: None,
            coverage: FacetCoverage::absent(coverage),
        }
    }

    /// Constructs a complete deletion observation.
    #[must_use]
    pub fn deleted(coverage: AuthorizedCompleteCoverage) -> Self {
        Self {
            value: None,
            coverage: FacetCoverage::deleted(coverage),
        }
    }

    /// Returns the captured value, when the observation is live.
    #[must_use]
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Returns the checked coverage and explicit state.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Splits the observation into its value and checked coverage.
    #[must_use]
    pub fn into_parts(self) -> (Option<T>, FacetCoverage) {
        (self.value, self.coverage)
    }
}
