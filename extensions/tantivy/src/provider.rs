//! Deterministic lexical provider and page admission.

use crate::identity::{QueryVersion, SchemaVersion};
use crate::{AdapterError, Binding, Cursor, Error, Limits, Query};
use backend_version::CoverageWitness;

/// A request sent to a lexical source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryRequest {
    /// Exact materialization binding.
    pub binding: Binding,
    /// Canonical term query.
    pub query: Query,
    /// Optional page cursor.
    pub cursor: Option<Cursor>,
    /// Requested page size.
    pub limit: usize,
}

impl QueryRequest {
    /// Creates a first-page request.
    ///
    /// # Errors
    ///
    /// Returns a typed error when terms or the requested page size exceed the
    /// supplied limits.
    pub fn first(
        binding: Binding,
        terms: Vec<String>,
        limit: usize,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if limit == 0 || limit > limits.max_page {
            return Err(Error::SizeLimit);
        }
        Ok(Self {
            binding,
            query: Query::new(terms, limits)?,
            cursor: None,
            limit,
        })
    }
}

/// A source page before adapter admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexicalPage {
    /// Provider schema tag.
    pub schema: SchemaVersion,
    /// Exact materialization binding.
    pub binding: Binding,
    /// Exact query identity.
    pub query: QueryVersion,
    /// Document IDs in source order.
    pub ids: Vec<u64>,
    /// Next page cursor.
    pub next: Option<Cursor>,
    /// Authority coverage.
    pub coverage: CoverageWitness,
}

/// A deterministic lexical source contract.
pub trait LexicalSource {
    /// Source-specific error.
    type Error;

    /// Fetches one page for a fully bound request.
    ///
    /// # Errors
    ///
    /// Implementations return their source failure or a typed boundary error.
    fn fetch(&self, request: &QueryRequest) -> Result<LexicalPage, Self::Error>;
}

/// An admitted deterministic lexical page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryResult {
    /// Exact materialization binding.
    pub binding: Binding,
    /// Exact query identity.
    pub query: QueryVersion,
    /// Complete coverage of the selected source scope.
    pub coverage: CoverageWitness,
    /// Canonically ordered matching IDs.
    pub ids: Vec<u64>,
    /// Cursor for the next page.
    pub next: Option<Cursor>,
}

/// Deterministic in-memory lexical source for tests and local fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySource {
    binding: Binding,
    query: QueryVersion,
    ids: Vec<u64>,
    coverage: CoverageWitness,
}

impl MemorySource {
    /// Creates a source and canonicalizes its visible IDs.
    ///
    /// # Errors
    ///
    /// Returns a typed error for incomplete coverage, zero IDs, duplicates, or
    /// size violations.
    pub fn new(
        binding: Binding,
        query: QueryVersion,
        coverage: CoverageWitness,
        mut ids: Vec<u64>,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if !coverage.state().is_complete() {
            return Err(Error::IncompleteCoverage);
        }
        if ids.contains(&0) {
            return Err(Error::MalformedInput);
        }
        ids.sort_unstable();
        if ids.windows(2).any(|window| window[0] == window[1]) {
            return Err(Error::MalformedInput);
        }
        if ids.len() > limits.max_documents {
            return Err(Error::SizeLimit);
        }
        Ok(Self {
            binding,
            query,
            ids,
            coverage,
        })
    }
}

impl LexicalSource for MemorySource {
    type Error = Error;

    fn fetch(&self, request: &QueryRequest) -> Result<LexicalPage, Self::Error> {
        if request.binding != self.binding {
            return Err(Error::StaleRoot);
        }
        if request.query.version != self.query {
            return Err(Error::StaleCursor);
        }
        if request.limit == 0 {
            return Err(Error::MalformedInput);
        }
        if let Some(cursor) = request.cursor
            && (cursor.binding() != request.binding
                || cursor.query() != request.query.version
                || cursor.offset() > self.ids.len())
        {
            return Err(Error::InvalidCursor);
        }
        let offset = request.cursor.map_or(0, Cursor::offset);
        let end = offset
            .checked_add(request.limit)
            .ok_or(Error::SizeLimit)?
            .min(self.ids.len());
        let next = (end < self.ids.len()).then(|| Cursor::new(self.binding, self.query, end));
        Ok(LexicalPage {
            schema: SchemaVersion::CURRENT,
            binding: self.binding,
            query: self.query,
            ids: self.ids[offset..end].to_vec(),
            next,
            coverage: self.coverage,
        })
    }
}

/// Bounded lexical source adapter.
#[derive(Clone, Debug)]
pub struct Adapter<S> {
    source: S,
    limits: Limits,
}

impl<S> Adapter<S> {
    /// Creates an adapter over a source contract.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`] when the configured limits are not
    /// usable.
    pub fn new(source: S, limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            source,
            limits: limits.validate()?,
        })
    }
}

impl<S: LexicalSource> Adapter<S> {
    /// Fetches and admits one deterministic result page.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError::Provider`] for source errors or
    /// [`AdapterError::Extension`] for stale roots/cursors, schema drift,
    /// incomplete coverage, malformed IDs, or size violations.
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
            if cursor.offset() > self.limits.max_documents {
                return Err(AdapterError::Extension(Error::InvalidCursor));
            }
        }
        let page = self.source.fetch(request).map_err(AdapterError::Provider)?;
        if page.schema != SchemaVersion::CURRENT {
            return Err(AdapterError::Extension(Error::SchemaDrift));
        }
        if page.binding != request.binding || page.query != request.query.version {
            return Err(AdapterError::Extension(Error::StaleRoot));
        }
        if !page.coverage.state().is_complete() {
            return Err(AdapterError::Extension(Error::IncompleteCoverage));
        }
        if page.ids.len() > request.limit || page.ids.len() > self.limits.max_page {
            return Err(AdapterError::Extension(Error::SizeLimit));
        }
        if page.ids.contains(&0) {
            return Err(AdapterError::Extension(Error::MalformedInput));
        }
        let mut ids = page.ids;
        ids.sort_unstable();
        if ids.windows(2).any(|window| window[0] == window[1]) {
            return Err(AdapterError::Extension(Error::MalformedInput));
        }
        let current_offset = request.cursor.map_or(0, Cursor::offset);
        if let Some(next) = page.next
            && (next.binding() != request.binding
                || next.query() != request.query.version
                || next.offset() <= current_offset
                || next.offset()
                    != current_offset
                        .checked_add(ids.len())
                        .ok_or(AdapterError::Extension(Error::SizeLimit))?
                || next.offset() > self.limits.max_documents)
        {
            return Err(AdapterError::Extension(Error::InvalidCursor));
        }
        Ok(QueryResult {
            binding: request.binding,
            query: request.query.version,
            coverage: page.coverage,
            ids,
            next: page.next,
        })
    }
}
