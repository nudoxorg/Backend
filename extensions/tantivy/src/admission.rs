//! Compatibility materialization and coverage admission.

use crate::delta::DocumentChange;
use crate::{Authority, Limits, Query, Recipe, Root};
use backend_version::{
    ClosedRelationScope, Coverage, CoverageWitness, DeltaError, ScopeRoot, partial_coverage,
};
use std::fmt;

/// Compatibility materialization retaining the original call shape.
///
/// This value is not a publication authority because the compatibility
/// signature predates workspace, manifest, and frontier binding. Use the
/// [`crate::Adapter`] result for a fully bound provider page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Materialization {
    /// Exact visible document root.
    pub root: Root,
    /// Lexical recipe version.
    pub recipe: Recipe,
    /// Complete coverage witness.
    pub coverage: CoverageWitness,
    /// Authority version.
    pub authority: Authority,
    /// Canonically ordered document mutations.
    pub changes: Vec<DocumentChange>,
}

/// Adapter failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The source did not provide complete coverage.
    IncompleteCoverage,
    /// An exact relation root did not match.
    StaleRoot,
    /// A query cursor was bound to another source/query.
    StaleCursor,
    /// A cursor was malformed or moved backwards.
    InvalidCursor,
    /// A source advertised an unsupported schema.
    SchemaDrift,
    /// Input violated the logical schema.
    MalformedInput,
    /// Limits were zero or inconsistent.
    InvalidLimits,
    /// A bounded input/output limit was exceeded.
    SizeLimit,
    /// Incremental maintenance exceeded its declared overlay budget.
    RebuildRequired,
    /// A deletion named no visible document.
    MissingDocument,
    /// A document state rejected duplicate or malformed keys.
    State(backend_version::StateError),
    /// A typed exact delta was rejected.
    Delta(DeltaError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::IncompleteCoverage => "incomplete lexical coverage",
            Self::StaleRoot => "stale lexical relation root",
            Self::StaleCursor => "stale lexical cursor",
            Self::InvalidCursor => "invalid lexical cursor",
            Self::SchemaDrift => "lexical source schema drift",
            Self::MalformedInput => "malformed lexical adapter input",
            Self::InvalidLimits => "invalid lexical adapter limits",
            Self::SizeLimit => "lexical adapter size limit exceeded",
            Self::RebuildRequired => "lexical overlay requires a bounded rebuild",
            Self::MissingDocument => "document deletion named no visible document",
            Self::State(_) => "invalid lexical relation state",
            Self::Delta(_) => "invalid lexical relation delta",
        })
    }
}

impl std::error::Error for Error {}

/// Failure from a source or the lexical extension boundary.
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
            Self::Provider(error) => write!(formatter, "lexical source failure: {error}"),
            Self::Extension(error) => error.fmt(formatter),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for AdapterError<E> {}

/// Canonicalizes a lexical change stream under a complete witness.
///
/// # Errors
///
/// Returns a typed error when coverage, document fields, duplicate keys, or
/// size limits are invalid.
pub fn materialize(
    root: Root,
    recipe: Recipe,
    authority: Authority,
    coverage: CoverageWitness,
    changes: Vec<DocumentChange>,
) -> Result<Materialization, Error> {
    materialize_with_limits(
        root,
        recipe,
        authority,
        coverage,
        changes,
        Limits::default(),
    )
}

/// Bounded variant of [`materialize`].
///
/// # Errors
///
/// Returns a typed error for incomplete coverage, malformed changes, duplicate
/// document IDs, or a size violation.
pub fn materialize_with_limits(
    root: Root,
    recipe: Recipe,
    authority: Authority,
    coverage: CoverageWitness,
    mut changes: Vec<DocumentChange>,
    limits: Limits,
) -> Result<Materialization, Error> {
    let limits = limits.validate()?;
    if !matches!(coverage, CoverageWitness::Complete(_)) {
        return Err(Error::IncompleteCoverage);
    }
    if changes.len() > limits.max_documents {
        return Err(Error::SizeLimit);
    }
    changes.sort_by_key(DocumentChange::id);
    if changes
        .windows(2)
        .any(|window| window[0].id() == window[1].id())
    {
        return Err(Error::MalformedInput);
    }
    let mut total = 0usize;
    for change in &mut changes {
        if change.id() == 0 {
            return Err(Error::MalformedInput);
        }
        if let DocumentChange::Add { fields, .. } = change {
            *fields = crate::delta::normalize_fields(std::mem::take(fields), limits, &mut total)?;
        }
    }
    Ok(Materialization {
        root,
        recipe,
        coverage,
        authority,
        changes,
    })
}

/// Executes a deterministic term projection against the exact requested root.
///
/// # Errors
///
/// Returns a typed error for another root, incomplete coverage, invalid terms,
/// or malformed/oversized materialization contents.
pub fn query(
    materialization: &Materialization,
    root: Root,
    terms: &[&str],
) -> Result<Vec<u64>, Error> {
    if materialization.root != root {
        return Err(Error::StaleRoot);
    }
    let materialization = materialize_with_limits(
        materialization.root,
        materialization.recipe,
        materialization.authority,
        materialization.coverage,
        materialization.changes.clone(),
        Limits::default(),
    )?;
    let query = Query::new(
        terms.iter().map(|term| (*term).to_owned()).collect(),
        Limits::default(),
    )?;
    let mut docs = std::collections::BTreeMap::new();
    for change in &materialization.changes {
        match change {
            DocumentChange::Add { id, fields } => {
                docs.insert(*id, fields);
            }
            DocumentChange::Delete { id } => {
                docs.remove(id);
            }
        }
    }
    Ok(docs
        .into_iter()
        .filter_map(|(id, fields)| {
            let hay = fields
                .iter()
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            query
                .terms
                .iter()
                .all(|term| hay.contains(term))
                .then_some(id)
        })
        .collect())
}

/// Creates an incomplete witness for rejection tests.
#[must_use]
pub fn incomplete_coverage(scope: u64, state: Coverage) -> CoverageWitness {
    match state {
        Coverage::Partial | Coverage::Complete => CoverageWitness::Partial(partial_coverage(scope)),
        Coverage::Unavailable => CoverageWitness::Unavailable(partial_coverage(scope)),
        Coverage::Unsupported => CoverageWitness::Unsupported(partial_coverage(scope)),
        Coverage::Closed => CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(
            ScopeRoot::from_u64(scope),
        )),
    }
}
