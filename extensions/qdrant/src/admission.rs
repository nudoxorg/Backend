//! Remote candidate admission, explicit approximation metadata, and coverage.

use crate::contracts::{ApproximationMetadata, SearchQuality};
use crate::{Authority, CandidateId, Limits, Recipe, Root, Tombstones};
use backend_version::{
    AuthorityScopeEvidence, Coverage, CoverageWitness, DeltaError, ObservedScopeEvidence,
    ScopeRoot, admit_complete_coverage, partial_coverage,
};
use std::collections::BTreeSet;
use std::fmt;

/// Compatibility candidates retaining exact vector version claims.
///
/// The legacy scalar admission functions do not carry a workspace or
/// frontier, so this value cannot authorize a published snapshot. Use
/// [`crate::SearchResult`] for a fully bound provider page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidates {
    /// Exact vector-facts relation root.
    pub root: Root,
    /// ANN recipe version.
    pub recipe: Recipe,
    /// Authority/capability version.
    pub authority: Authority,
    /// Complete source coverage.
    pub coverage: CoverageWitness,
    /// Retractions applied to the page.
    pub tombstones: Tombstones,
    /// Canonically ordered logical candidate identities.
    pub ids: Vec<CandidateId>,
    /// Exactness/recall metadata retained from the provider.
    pub quality: SearchQuality,
}

/// Compatibility reranked candidates.
///
/// Reranking preserves quality metadata but this legacy value remains outside
/// publication authority because it has no workspace, manifest, or frontier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reranked {
    /// Exact vector-facts relation root.
    pub root: Root,
    /// ANN recipe version.
    pub recipe: Recipe,
    /// Authority/capability version.
    pub authority: Authority,
    /// Complete coverage retained from candidate admission.
    pub coverage: CoverageWitness,
    /// Canonically ranked candidate identities.
    pub ids: Vec<CandidateId>,
    /// Quality remains explicit; reranking cannot upgrade an ANN result to
    /// an exact claim.
    pub quality: SearchQuality,
}

/// Adapter failures for the backwards-compatible free functions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The authority did not provide complete coverage.
    IncompleteCoverage,
    /// A requested relation root is stale.
    StaleRoot,
    /// The query binding or cursor is stale.
    StaleCursor,
    /// A cursor was malformed or moved backwards.
    InvalidCursor,
    /// The provider advertised another schema.
    SchemaDrift,
    /// Approximation metadata did not bind to the requested recipe/model
    /// contract or used an out-of-range recall claim.
    ApproximationMismatch,
    /// Input violated the typed boundary.
    MalformedInput,
    /// A configured bound was invalid.
    InvalidLimits,
    /// A configured size bound was exceeded.
    SizeLimit,
    /// A deletion named no visible candidate.
    MissingCandidate,
    /// The lower relation state rejected the transition.
    State(backend_version::StateError),
    /// The exact transition could not be prepared or applied.
    Delta(DeltaError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::IncompleteCoverage => "incomplete vector coverage",
            Self::StaleRoot => "stale vector relation root",
            Self::StaleCursor => "stale vector cursor",
            Self::InvalidCursor => "invalid vector cursor",
            Self::SchemaDrift => "vector provider schema drift",
            Self::ApproximationMismatch => "vector approximation metadata mismatch",
            Self::MalformedInput => "malformed vector adapter input",
            Self::InvalidLimits => "invalid vector adapter limits",
            Self::SizeLimit => "vector adapter size limit exceeded",
            Self::MissingCandidate => "candidate deletion named no visible candidate",
            Self::State(_) => "invalid vector relation state",
            Self::Delta(_) => "invalid vector relation delta",
        })
    }
}

impl std::error::Error for Error {}

/// Failure from either the provider or the typed extension boundary.
#[derive(Debug)]
pub enum AdapterError<E> {
    /// Provider-specific failure.
    Provider(E),
    /// Typed adapter rejection.
    Extension(Error),
}

impl<E: fmt::Display> fmt::Display for AdapterError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => write!(formatter, "vector provider failure: {error}"),
            Self::Extension(error) => error.fmt(formatter),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for AdapterError<E> {}

/// Admits and canonicalizes remote candidates under a complete witness.
///
/// # Errors
///
/// Returns [`Error::IncompleteCoverage`], [`Error::MalformedInput`], or
/// [`Error::SizeLimit`] when the witness, identities, or bounds are invalid.
pub fn accept_remote(
    root: Root,
    recipe: Recipe,
    authority: Authority,
    coverage: CoverageWitness,
    tombstones: Tombstones,
    ids: Vec<CandidateId>,
) -> Result<Candidates, Error> {
    admit_remote(
        root,
        recipe,
        authority,
        coverage,
        tombstones,
        ids,
        RemotePolicy::new(SearchQuality::Exact, Limits::default()),
    )
}

/// Admits a remote result with an explicit exact or approximate quality claim.
///
/// # Errors
///
/// Returns [`Error::ApproximationMismatch`] when approximate metadata does not
/// match `recipe`, or a typed admission error for invalid coverage, identities,
/// tombstones, or bounds.
pub fn accept_remote_with_quality(
    root: Root,
    recipe: Recipe,
    authority: Authority,
    coverage: CoverageWitness,
    tombstones: Tombstones,
    ids: Vec<CandidateId>,
    quality: SearchQuality,
) -> Result<Candidates, Error> {
    admit_remote(
        root,
        recipe,
        authority,
        coverage,
        tombstones,
        ids,
        RemotePolicy::new(quality, Limits::default()),
    )
}

/// Admits an explicitly approximate result with recipe, model, and recall
/// metadata. The metadata is retained through local reranking.
///
/// # Errors
///
/// Returns [`Error::ApproximationMismatch`] when the metadata names another
/// recipe or an invalid recall range, or a typed admission error for invalid
/// coverage, identities, or tombstones.
pub fn accept_remote_approximate(
    root: Root,
    recipe: Recipe,
    authority: Authority,
    coverage: CoverageWitness,
    tombstones: Tombstones,
    ids: Vec<CandidateId>,
    metadata: ApproximationMetadata,
) -> Result<Candidates, Error> {
    accept_remote_with_quality(
        root,
        recipe,
        authority,
        coverage,
        tombstones,
        ids,
        SearchQuality::Approximate(metadata),
    )
}

/// Bounded variant of [`accept_remote`].
///
/// # Errors
///
/// Returns a typed error when the witness, identities, tombstones, or bounds
/// are invalid.
pub fn accept_remote_with_limits(
    root: Root,
    recipe: Recipe,
    authority: Authority,
    coverage: CoverageWitness,
    tombstones: Tombstones,
    ids: Vec<CandidateId>,
    limits: Limits,
) -> Result<Candidates, Error> {
    admit_remote(
        root,
        recipe,
        authority,
        coverage,
        tombstones,
        ids,
        RemotePolicy::new(SearchQuality::Exact, limits),
    )
}

#[derive(Clone, Copy)]
struct RemotePolicy {
    quality: SearchQuality,
    limits: Limits,
}

impl RemotePolicy {
    const fn new(quality: SearchQuality, limits: Limits) -> Self {
        Self { quality, limits }
    }
}

fn admit_remote(
    root: Root,
    recipe: Recipe,
    authority: Authority,
    coverage: CoverageWitness,
    tombstones: Tombstones,
    mut ids: Vec<CandidateId>,
    policy: RemotePolicy,
) -> Result<Candidates, Error> {
    let limits = policy.limits.validate()?;
    if !policy.quality.validate(recipe) {
        return Err(Error::ApproximationMismatch);
    }
    if !coverage.state().is_complete() {
        return Err(Error::IncompleteCoverage);
    }
    let tombstones = Tombstones::new(tombstones.0, limits)?;
    if ids.len() > limits.max_candidates {
        return Err(Error::SizeLimit);
    }
    if ids.iter().any(|id| !id.is_valid()) {
        return Err(Error::MalformedInput);
    }
    ids.retain(|id| !tombstones.contains(*id));
    ids.sort_unstable();
    ids.dedup();
    Ok(Candidates {
        root,
        recipe,
        authority,
        coverage,
        tombstones,
        ids,
        quality: policy.quality,
    })
}

/// Applies a deterministic local score function to accepted candidates.
///
/// # Errors
///
/// Returns a typed error when the requested root differs, coverage is
/// incomplete, or the public candidate value is malformed or oversized.
pub fn rerank(
    candidates: &Candidates,
    root: Root,
    mut score: impl FnMut(CandidateId) -> u64,
) -> Result<Reranked, Error> {
    if candidates.root != root {
        return Err(Error::StaleRoot);
    }
    if !candidates.quality.validate(candidates.recipe) {
        return Err(Error::ApproximationMismatch);
    }
    let limits = Limits::default();
    if !candidates.coverage.state().is_complete() {
        return Err(Error::IncompleteCoverage);
    }
    if candidates.ids.len() > limits.max_candidates {
        return Err(Error::SizeLimit);
    }
    let tombstones = Tombstones::new(candidates.tombstones.0.clone(), limits)?;
    let mut seen = BTreeSet::new();
    for id in &candidates.ids {
        if !id.is_valid() || tombstones.contains(*id) || !seen.insert(*id) {
            return Err(Error::MalformedInput);
        }
    }
    let mut scores = candidates
        .ids
        .iter()
        .copied()
        .map(|id| (id, score(id)))
        .collect::<Vec<_>>();
    scores.sort_by(|(left_id, left_score), (right_id, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| left_id.cmp(right_id))
    });
    Ok(Reranked {
        root: candidates.root,
        recipe: candidates.recipe,
        authority: candidates.authority,
        coverage: candidates.coverage,
        ids: scores.into_iter().map(|(id, _)| id).collect(),
        quality: candidates.quality,
    })
}

/// Creates a complete coverage witness for deterministic tests and local
/// fakes after comparing the declared and observed scope roots.
///
/// # Errors
///
/// Returns [`Error::IncompleteCoverage`] if the lower coverage admission
/// rejects the scope pair.
pub fn complete_coverage(scope: ScopeRoot) -> Result<CoverageWitness, Error> {
    let declaration =
        AuthorityScopeEvidence::from_object_version(Authority::from_value(scope.as_bytes()));
    let observed = ObservedScopeEvidence::from_object_version(
        declaration.observation_permit(),
        Authority::from_value(scope.as_bytes()),
    );
    admit_complete_coverage(declaration, observed)
        .map(CoverageWitness::Complete)
        .map_err(|_| Error::IncompleteCoverage)
}

/// Creates a non-authoritative witness for tests that exercise rejection.
#[must_use]
pub fn incomplete_coverage(scope: u64, state: Coverage) -> CoverageWitness {
    match state {
        Coverage::Partial => CoverageWitness::Partial(partial_coverage(scope, state)),
        Coverage::Unavailable => CoverageWitness::Unavailable(partial_coverage(scope, state)),
        Coverage::Unsupported => CoverageWitness::Unsupported(partial_coverage(scope, state)),
        Coverage::Complete => CoverageWitness::Partial(partial_coverage(scope, Coverage::Partial)),
    }
}
