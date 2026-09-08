//! Compatibility projection and coverage admission.

use crate::contracts::{GraphRow, canonical_reads};
use crate::delta::normalize_row;
use crate::{Authority, Limits, Read, Recipe, Root};
use backend_version::{
    AuthorityScopeEvidence, Coverage, CoverageWitness, DeltaError, ObservedScopeEvidence,
    ScopeRoot, admit_complete_coverage, partial_coverage,
};
use std::fmt;

/// Compatibility input retained for callers that do not yet carry a workspace
/// root or read-manifest version. It cannot authorize a published snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryInput {
    /// Exact semantic graph root.
    pub root: Root,
    /// Graph recipe version.
    pub recipe: Recipe,
    /// Semantic authority version.
    pub authority: Authority,
    /// Declared reads.
    pub reads: Vec<Read>,
    /// Unkeyed projected rows.
    pub rows: Vec<Vec<String>>,
}

/// Compatibility projection result.
///
/// It is intentionally not a publication authority because its legacy
/// signature has no workspace, frontier, or coverage binding. Use the
/// [`crate::Adapter`] result for a fully bound provider page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Projection {
    /// Exact semantic graph root.
    pub root: Root,
    /// Graph recipe version.
    pub recipe: Recipe,
    /// Semantic authority version.
    pub authority: Authority,
    /// Declared reads in canonical order.
    pub reads: Vec<Read>,
    /// Deterministically ordered unique rows.
    pub rows: Vec<Vec<String>>,
}

/// Adapter failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The source did not provide complete graph coverage.
    IncompleteCoverage,
    /// The requested relation root was stale.
    StaleRoot,
    /// A cursor was bound to another root/query.
    StaleCursor,
    /// A cursor was malformed or moved backwards.
    InvalidCursor,
    /// The source advertised another schema.
    SchemaDrift,
    /// The read manifest did not match the exact query binding.
    ReadManifestDrift,
    /// A read was absent or empty.
    UndeclaredRead,
    /// Input violated the logical schema.
    MalformedInput,
    /// Limits were zero or inconsistent.
    InvalidLimits,
    /// A bounded input/output limit was exceeded.
    SizeLimit,
    /// A row deletion named no visible row.
    MissingRow,
    /// The lower relation state rejected the rows.
    State(backend_version::StateError),
    /// The exact lower relation delta was rejected.
    Delta(DeltaError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::IncompleteCoverage => "incomplete graph coverage",
            Self::StaleRoot => "stale graph relation root",
            Self::StaleCursor => "stale graph cursor",
            Self::InvalidCursor => "invalid graph cursor",
            Self::SchemaDrift => "graph source schema drift",
            Self::ReadManifestDrift => "graph read-manifest drift",
            Self::UndeclaredRead => "undeclared graph read",
            Self::MalformedInput => "malformed graph adapter input",
            Self::InvalidLimits => "invalid graph adapter limits",
            Self::SizeLimit => "graph adapter size limit exceeded",
            Self::MissingRow => "graph row deletion named no visible row",
            Self::State(_) => "invalid graph relation state",
            Self::Delta(_) => "invalid graph relation delta",
        })
    }
}

impl std::error::Error for Error {}

/// Failure from a source or the graph extension boundary.
#[derive(Debug)]
pub enum AdapterError<E> {
    /// Source-specific failure.
    Provider(E),
    /// Typed extension rejection.
    Extension(Error),
}

impl<E: fmt::Display> fmt::Display for AdapterError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => write!(formatter, "graph source failure: {error}"),
            Self::Extension(error) => error.fmt(formatter),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for AdapterError<E> {}

/// Executes compatibility validation and canonical row projection.
///
/// # Errors
///
/// Returns [`Error::UndeclaredRead`], [`Error::MalformedInput`], or
/// [`Error::SizeLimit`] for invalid read/row input.
pub fn execute(input: &QueryInput) -> Result<Projection, Error> {
    let (reads, _, _) = canonical_reads(input.reads.clone(), Limits::default())?;
    if input.rows.len() > Limits::default().max_rows {
        return Err(Error::SizeLimit);
    }
    let mut rows = input.rows.clone();
    let mut total = 0usize;
    for (index, values) in rows.iter().enumerate() {
        let key = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(Error::SizeLimit)?;
        let mut row = GraphRow {
            key,
            values: values.clone(),
        };
        normalize_row(&mut row, Limits::default(), &mut total)?;
    }
    rows.sort();
    rows.dedup();
    Ok(Projection {
        root: input.root,
        recipe: input.recipe,
        authority: input.authority,
        reads,
        rows,
    })
}

/// Creates a complete coverage witness for deterministic fakes.
///
/// # Errors
///
/// Returns [`Error::IncompleteCoverage`] if lower admission rejects the scope.
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

/// Creates an incomplete witness for rejection tests.
#[must_use]
pub fn incomplete_coverage(scope: u64, state: Coverage) -> CoverageWitness {
    match state {
        Coverage::Partial => CoverageWitness::Partial(partial_coverage(scope, state)),
        Coverage::Unavailable => CoverageWitness::Unavailable(partial_coverage(scope, state)),
        Coverage::Unsupported => CoverageWitness::Unsupported(partial_coverage(scope, state)),
        Coverage::Complete => CoverageWitness::Partial(partial_coverage(scope, Coverage::Partial)),
    }
}
