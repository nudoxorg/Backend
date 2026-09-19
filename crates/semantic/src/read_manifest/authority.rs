//! Authority scopes, complete replacements, and scoped fact sets.

use super::observations::ScopedReadManifest;
use crate::canonical::canonical_provenance;
use crate::support::{bytes, list};
use crate::{
    AuthorityId, AuthorityScopeVersion, AuthorityVersion, FacetCoverage, FacetKind, Provenance,
    SemanticError, SourceId, SourceVersion,
};
use backend_version::{AuthorizedCompleteCoverage, CoverageWitness, ObjectVersion, ScopeRoot};
use std::sync::Arc;

/// Immutable allocation pair for a half-open authority key range.
pub type SharedKeyRange = (Arc<[u8]>, Arc<[u8]>);

/// A declared authority scope for a replacement or result.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AuthorityScope {
    provenance: Provenance,
    /// Package/project identity bytes.
    package: Arc<[u8]>,
    /// Optional inclusive/exclusive logical key range.
    key_range: Option<SharedKeyRange>,
    /// Facet families owned by this scope.
    facets: Arc<[FacetKind]>,
}

impl AuthorityScope {
    /// Admits one immutable authority-owned scope.
    ///
    /// The provenance is retained as one checked value so authority/source
    /// identities cannot be mixed with revisions from another producer.
    /// Ranges are half-open and facet ownership is sorted and unique.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidAuthorityScope`] for an empty package
    /// or facet set, duplicate facets, or invalid range bounds.
    pub fn new(
        provenance: Provenance,
        package: Vec<u8>,
        key_range: Option<(Vec<u8>, Vec<u8>)>,
        mut facets: Vec<FacetKind>,
    ) -> Result<Self, SemanticError> {
        if package.is_empty()
            || key_range.as_ref().is_some_and(|(start, end)| start >= end)
            || facets.is_empty()
        {
            return Err(SemanticError::InvalidAuthorityScope);
        }
        facets.sort();
        if facets.windows(2).any(|window| window[0] == window[1]) {
            return Err(SemanticError::InvalidAuthorityScope);
        }
        Ok(Self {
            provenance,
            package: Arc::from(package),
            key_range: key_range.map(|(start, end)| (Arc::from(start), Arc::from(end))),
            facets: Arc::from(facets),
        })
    }

    /// Returns the authority identity.
    #[must_use]
    pub const fn authority(&self) -> AuthorityId {
        self.provenance.authority()
    }

    /// Returns the authority value version.
    #[must_use]
    pub const fn authority_version(&self) -> AuthorityVersion {
        self.provenance.authority_version()
    }

    /// Returns the source identity.
    #[must_use]
    pub const fn source(&self) -> SourceId {
        self.provenance.source()
    }

    /// Returns the source value version.
    #[must_use]
    pub const fn source_version(&self) -> SourceVersion {
        self.provenance.source_version()
    }

    /// Returns the complete source/authority provenance.
    #[must_use]
    pub const fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// Returns the package/project identity bytes.
    #[must_use]
    pub fn package(&self) -> &[u8] {
        &self.package
    }

    /// Returns the immutable package identity allocation for retained scope
    /// snapshots and publication receipts.
    #[must_use]
    pub fn shared_package(&self) -> Arc<[u8]> {
        Arc::clone(&self.package)
    }

    /// Returns the optional half-open logical key range.
    #[must_use]
    pub fn key_range(&self) -> Option<(&[u8], &[u8])> {
        self.key_range
            .as_ref()
            .map(|(start, end)| (start.as_ref(), end.as_ref()))
    }

    /// Returns the immutable range-bound allocations for a zero-copy scope
    /// handoff.
    #[must_use]
    pub fn shared_key_range(&self) -> Option<SharedKeyRange> {
        self.key_range
            .as_ref()
            .map(|(start, end)| (Arc::clone(start), Arc::clone(end)))
    }

    /// Returns the sorted facet families owned by this scope.
    #[must_use]
    pub fn facets(&self) -> &[FacetKind] {
        &self.facets
    }

    /// Returns the immutable owned-facet allocation without copying it.
    #[must_use]
    pub fn shared_facets(&self) -> Arc<[FacetKind]> {
        Arc::clone(&self.facets)
    }

    /// Returns the exact full-width root that complete coverage must name.
    #[must_use]
    pub fn scope_root(&self) -> ScopeRoot {
        ScopeRoot::from_bytes(self.version().to_bytes())
    }

    /// Returns canonical scope bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        canonical_provenance(&self.provenance, &mut out);
        bytes(&mut out, &self.package);
        match &self.key_range {
            Some((start, end)) => {
                out.push(1);
                bytes(&mut out, start);
                bytes(&mut out, end);
            }
            None => out.push(0),
        }
        list(&mut out, &self.facets, |facet, encoded| {
            encoded.extend_from_slice(&facet.tag().to_be_bytes());
        });
        out
    }

    /// Returns the value version of this exact authority scope.
    #[must_use]
    pub fn version(&self) -> AuthorityScopeVersion {
        ObjectVersion::from_value(self)
    }
}

/// Complete replacement transition for one owned authority scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeReplacement {
    previous: Option<AuthorityScopeVersion>,
    scope: AuthorityScope,
    coverage: AuthorizedCompleteCoverage,
    basis: ScopedReadManifest,
    fence: u64,
}

impl ScopeReplacement {
    /// Admits a complete replacement tied to the exact declared scope.
    ///
    /// Every owned facet must occur in the scoped basis, and every basis read
    /// must name the replacement's full-width scope.  Completeness by itself
    /// is therefore insufficient to authorize deletion outside the basis.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidReplacement`] when coverage, basis,
    /// and scope roots or owned facets disagree.
    pub fn new(
        previous: Option<AuthorityScopeVersion>,
        scope: AuthorityScope,
        coverage: AuthorizedCompleteCoverage,
        basis: ScopedReadManifest,
        fence: u64,
    ) -> Result<Self, SemanticError> {
        if coverage.scope_root() != scope.scope_root() {
            return Err(SemanticError::InvalidReplacement);
        }
        if basis.observations().iter().any(|observation| {
            observation.read().scope_root() != scope.scope_root()
                || !scope.facets().contains(&observation.read().facet())
                || !matches!(observation.coverage(), CoverageWitness::Complete(_))
                || (!observation.read().is_negative() && observation.version().is_none())
        }) {
            return Err(SemanticError::InvalidReplacement);
        }
        if scope.facets().iter().any(|facet| {
            !basis
                .observations()
                .iter()
                .any(|observation| observation.read().facet() == *facet)
        }) {
            return Err(SemanticError::InvalidReplacement);
        }
        Ok(Self {
            previous,
            scope,
            coverage,
            basis,
            fence,
        })
    }

    /// Returns the expected previous scope version.
    #[must_use]
    pub const fn previous(&self) -> Option<AuthorityScopeVersion> {
        self.previous
    }

    /// Returns the new declared scope.
    #[must_use]
    pub const fn scope(&self) -> &AuthorityScope {
        &self.scope
    }

    /// Returns the admitted complete coverage.
    #[must_use]
    pub const fn coverage(&self) -> AuthorizedCompleteCoverage {
        self.coverage
    }

    /// Returns the exact scoped read basis.
    #[must_use]
    pub const fn basis(&self) -> &ScopedReadManifest {
        &self.basis
    }

    /// Returns the authority fence/epoch.
    #[must_use]
    pub const fn fence(&self) -> u64 {
        self.fence
    }
}

/// An authority result carrying its checked scope and value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityResult<T> {
    /// Owned authority scope.
    scope: AuthorityScope,
    /// Checked coverage witness.
    coverage: CoverageWitness,
    /// Result value.
    value: T,
}

impl<T> AuthorityResult<T> {
    /// Constructs a result with admitted complete coverage.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ScopeMismatch`] when the witness does not
    /// name the declared scope.
    pub fn complete(
        scope: AuthorityScope,
        coverage: AuthorizedCompleteCoverage,
        value: T,
    ) -> Result<Self, SemanticError> {
        if coverage.scope_root() != scope.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        Ok(Self {
            scope,
            coverage: CoverageWitness::Complete(coverage),
            value,
        })
    }

    /// Constructs a result with an explicitly non-complete witness.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ScopeMismatch`] for another scope, or
    /// [`SemanticError::InvalidCoverageState`] when the witness is complete.
    pub fn partial(
        scope: AuthorityScope,
        coverage: CoverageWitness,
        value: T,
    ) -> Result<Self, SemanticError> {
        if coverage.scope_root() != scope.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        if matches!(coverage, CoverageWitness::Complete(_)) {
            return Err(SemanticError::InvalidCoverageState);
        }
        Ok(Self {
            scope,
            coverage,
            value,
        })
    }

    /// Returns the declared authority scope.
    #[must_use]
    pub const fn scope(&self) -> &AuthorityScope {
        &self.scope
    }

    /// Returns the exact coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Returns the result payload.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Returns whether this result can authorize absent-row deletion.
    #[must_use]
    pub fn may_replace_scope(&self) -> bool {
        matches!(self.coverage, CoverageWitness::Complete(value)
            if value.scope_root() == self.scope.scope_root())
    }
}

/// A typed fact set with explicit replacement coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactSet<T: Ord> {
    /// Coverage for the fact scope.
    coverage: FacetCoverage,
    /// Sorted, duplicate-free facts.
    facts: Arc<[T]>,
}

impl<T: Ord> FactSet<T> {
    /// Sorts and rejects duplicate facts.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DuplicateFact`] when a logical fact occurs
    /// more than once, or [`SemanticError::InvalidCoverageState`] when an
    /// explicit absence carries facts.
    pub fn new(coverage: FacetCoverage, mut facts: Vec<T>) -> Result<Self, SemanticError> {
        if coverage.deletion() != crate::Deletion::Live && !facts.is_empty() {
            return Err(SemanticError::InvalidCoverageState);
        }
        facts.sort();
        if facts.windows(2).any(|window| window[0] == window[1]) {
            Err(SemanticError::DuplicateFact)
        } else {
            Ok(Self {
                coverage,
                facts: Arc::from(facts),
            })
        }
    }

    /// Returns the checked fact coverage.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Returns the sorted, duplicate-free facts.
    #[must_use]
    pub fn facts(&self) -> &[T] {
        &self.facts
    }

    /// Returns the immutable fact backing storage without copying it.
    #[must_use]
    pub fn shared_facts(&self) -> Arc<[T]> {
        Arc::clone(&self.facts)
    }
}

impl<T: Ord + Clone> FactSet<T> {
    /// Computes rows deleted by a complete scoped replacement.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::IncompleteCoverage`] when the replacement
    /// lacks an admitted complete witness.
    pub fn deleted_by_replacement(
        previous: &[T],
        replacement: &Self,
    ) -> Result<Vec<T>, SemanticError> {
        if !replacement.coverage.authoritative() {
            return Err(SemanticError::IncompleteCoverage);
        }
        Ok(previous
            .iter()
            .filter(|fact| replacement.facts.binary_search(fact).is_err())
            .cloned()
            .collect())
    }

    /// Computes deletions while also checking the expected full-width scope.
    /// This is the safe boundary for callers that have a declared authority
    /// scope available.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ScopeMismatch`] when the replacement witness
    /// names another scope, or [`SemanticError::IncompleteCoverage`] when it
    /// is not authoritative.
    pub fn deleted_by_replacement_in_scope(
        previous: &[T],
        replacement: &Self,
        scope: ScopeRoot,
    ) -> Result<Vec<T>, SemanticError> {
        if replacement.coverage.scope_root() != scope {
            return Err(SemanticError::ScopeMismatch);
        }
        Self::deleted_by_replacement(previous, replacement)
    }
}
