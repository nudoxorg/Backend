//! Deterministic graph provider and page admission.

use crate::contracts::{Cursor, GraphRow};
use crate::delta::normalize_row;
use crate::identity::{QueryVersion, SchemaVersion};
use crate::{AdapterError, Binding, Error, Limits, Query, Read};
use backend_version::CoverageWitness;
use std::collections::BTreeSet;

/// Query request sent to a graph source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryRequest {
    /// Exact graph binding.
    pub binding: Binding,
    /// Canonical read/query descriptor.
    pub query: Query,
    /// Optional page cursor.
    pub cursor: Option<Cursor>,
    /// Requested page size.
    pub limit: usize,
}

impl QueryRequest {
    /// Creates a first-page graph request and checks the read-manifest pin.
    ///
    /// # Errors
    ///
    /// Returns a typed error for undeclared/duplicate reads, a manifest drift,
    /// or an invalid page size.
    pub fn first(
        binding: Binding,
        reads: Vec<Read>,
        limit: usize,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if limit == 0 || limit > limits.max_page {
            return Err(Error::SizeLimit);
        }
        let query = Query::new(reads, limits)?;
        if query.read_manifest != binding.read_manifest {
            return Err(Error::ReadManifestDrift);
        }
        Ok(Self {
            binding,
            query,
            cursor: None,
            limit,
        })
    }
}

/// One source page before typed adapter admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphPage {
    /// Source schema tag.
    pub schema: SchemaVersion,
    /// Exact graph binding.
    pub binding: Binding,
    /// Exact query/read-set identity.
    pub query: QueryVersion,
    /// Projected rows in source order.
    pub rows: Vec<GraphRow>,
    /// Cursor for the next page.
    pub next: Option<Cursor>,
    /// Authority coverage.
    pub coverage: CoverageWitness,
}

/// Graph source contract. The extension remains independent of a query engine.
pub trait GraphSource {
    /// Source-specific error.
    type Error;

    /// Fetches one bounded page for an exact graph request.
    ///
    /// # Errors
    ///
    /// Implementations return their source failure or a typed malformed/stale
    /// page that the adapter will reject.
    fn fetch(&self, request: &QueryRequest) -> Result<GraphPage, Self::Error>;
}

/// Admitted deterministic graph query result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryResult {
    /// Exact graph binding.
    pub binding: Binding,
    /// Exact query/read-set identity.
    pub query: QueryVersion,
    /// Complete graph coverage.
    pub coverage: CoverageWitness,
    /// Canonically ordered rows.
    pub rows: Vec<GraphRow>,
    /// Cursor for the next page.
    pub next: Option<Cursor>,
}

/// Deterministic in-memory graph source for local fallback and tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySource {
    binding: Binding,
    query: QueryVersion,
    rows: Vec<GraphRow>,
    coverage: CoverageWitness,
}

impl MemorySource {
    /// Creates a source from complete rows and canonicalizes their ordering.
    ///
    /// # Errors
    ///
    /// Returns a typed error for incomplete coverage, duplicate/malformed rows,
    /// or a size violation.
    pub fn new(
        binding: Binding,
        query: QueryVersion,
        coverage: CoverageWitness,
        mut rows: Vec<GraphRow>,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if !matches!(coverage, CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage);
        }
        if rows.len() > limits.max_rows {
            return Err(Error::SizeLimit);
        }
        let mut seen = BTreeSet::new();
        let mut total = 0usize;
        for row in &mut rows {
            normalize_row(row, limits, &mut total)?;
            if !seen.insert(row.key) {
                return Err(Error::MalformedInput);
            }
        }
        rows.sort_by_key(|row| row.key);
        Ok(Self {
            binding,
            query,
            rows,
            coverage,
        })
    }
}

impl GraphSource for MemorySource {
    type Error = Error;

    fn fetch(&self, request: &QueryRequest) -> Result<GraphPage, Self::Error> {
        if request.binding != self.binding {
            return Err(Error::StaleRoot);
        }
        if request.query.version != self.query {
            return Err(Error::ReadManifestDrift);
        }
        if let Some(cursor) = request.cursor
            && (cursor.binding() != request.binding
                || cursor.query() != request.query.version
                || cursor.offset() > self.rows.len())
        {
            return Err(Error::InvalidCursor);
        }
        let offset = request.cursor.map_or(0, Cursor::offset);
        let end = offset
            .checked_add(request.limit)
            .ok_or(Error::SizeLimit)?
            .min(self.rows.len());
        let next = (end < self.rows.len()).then(|| Cursor::new(self.binding, self.query, end));
        Ok(GraphPage {
            schema: SchemaVersion::CURRENT,
            binding: self.binding,
            query: self.query,
            rows: self.rows[offset..end].to_vec(),
            next,
            coverage: self.coverage,
        })
    }
}

/// Bounded graph source adapter.
#[derive(Clone, Debug)]
pub struct Adapter<S> {
    source: S,
    limits: Limits,
}

impl<S> Adapter<S> {
    /// Creates an adapter over a graph source.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`] when limits are unusable.
    pub fn new(source: S, limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            source,
            limits: limits.validate()?,
        })
    }
}

impl<S: GraphSource> Adapter<S> {
    /// Fetches and admits one deterministic graph page.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError::Provider`] for source failures or
    /// [`AdapterError::Extension`] for stale roots/cursors, schema drift,
    /// incomplete coverage, malformed rows, or size violations.
    pub fn query(&self, request: &QueryRequest) -> Result<QueryResult, AdapterError<S::Error>> {
        if request.limit == 0 || request.limit > self.limits.max_page {
            return Err(AdapterError::Extension(Error::SizeLimit));
        }
        request
            .query
            .validate(self.limits)
            .map_err(AdapterError::Extension)?;
        if let Some(cursor) = request.cursor {
            if cursor.binding() != request.binding || cursor.query() != request.query.version {
                return Err(AdapterError::Extension(Error::StaleCursor));
            }
            if cursor.offset() > self.limits.max_rows {
                return Err(AdapterError::Extension(Error::InvalidCursor));
            }
        }
        if request.query.read_manifest != request.binding.read_manifest {
            return Err(AdapterError::Extension(Error::ReadManifestDrift));
        }
        let page = self.source.fetch(request).map_err(AdapterError::Provider)?;
        if page.schema != SchemaVersion::CURRENT {
            return Err(AdapterError::Extension(Error::SchemaDrift));
        }
        if page.binding != request.binding || page.query != request.query.version {
            return Err(AdapterError::Extension(Error::StaleRoot));
        }
        if !matches!(page.coverage, CoverageWitness::Complete(_)) {
            return Err(AdapterError::Extension(Error::IncompleteCoverage));
        }
        if page.rows.len() > request.limit || page.rows.len() > self.limits.max_page {
            return Err(AdapterError::Extension(Error::SizeLimit));
        }
        let mut rows = page.rows;
        let mut seen = BTreeSet::new();
        let mut total = 0usize;
        for row in &mut rows {
            normalize_row(row, self.limits, &mut total).map_err(AdapterError::Extension)?;
            if !seen.insert(row.key) {
                return Err(AdapterError::Extension(Error::MalformedInput));
            }
        }
        rows.sort_by_key(|row| row.key);
        let current_offset = request.cursor.map_or(0, Cursor::offset);
        if let Some(next) = page.next
            && (next.binding() != request.binding
                || next.query() != request.query.version
                || next.offset() <= current_offset
                || next.offset()
                    != current_offset
                        .checked_add(rows.len())
                        .ok_or(AdapterError::Extension(Error::SizeLimit))?
                || next.offset() > self.limits.max_rows)
        {
            return Err(AdapterError::Extension(Error::InvalidCursor));
        }
        Ok(QueryResult {
            binding: request.binding,
            query: request.query.version,
            coverage: page.coverage,
            rows,
            next: page.next,
        })
    }
}
