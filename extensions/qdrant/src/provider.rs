//! Deterministic candidate provider and page admission.

use crate::contracts::{
    CandidatePage, CandidateSource, Cursor, SearchQuality, SearchRequest, SearchResult,
};
use crate::identity::SchemaVersion;
use crate::{AdapterError, Binding, CandidateId, Error, Limits};
use backend_version::CoverageWitness;

/// A deterministic in-memory source used by local execution and tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySource {
    binding: Binding,
    ids: Vec<CandidateId>,
    coverage: CoverageWitness,
}

impl MemorySource {
    /// Creates a source from unique logical identities and canonicalizes their
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`], [`Error::IncompleteCoverage`],
    /// [`Error::MalformedInput`], or [`Error::SizeLimit`] when the source is
    /// not bounded and complete.
    pub fn new(
        binding: Binding,
        coverage: CoverageWitness,
        ids: Vec<CandidateId>,
        limits: Limits,
    ) -> Result<Self, Error> {
        limits.validate()?;
        if !coverage.state().is_complete() {
            return Err(Error::IncompleteCoverage);
        }
        let mut ids = ids;
        if ids.iter().any(|id| !id.is_valid()) {
            return Err(Error::MalformedInput);
        }
        ids.sort_unstable();
        if ids.windows(2).any(|window| window[0] == window[1]) {
            return Err(Error::MalformedInput);
        }
        if ids.len() > limits.max_candidates {
            return Err(Error::SizeLimit);
        }
        Ok(Self {
            binding,
            ids,
            coverage,
        })
    }
}

impl CandidateSource for MemorySource {
    type Error = Error;

    fn fetch(&self, request: &SearchRequest) -> Result<CandidatePage, Self::Error> {
        if request.binding != self.binding {
            return Err(Error::StaleRoot);
        }
        if request.limit == 0 {
            return Err(Error::MalformedInput);
        }
        if let Some(cursor) = request.cursor
            && (cursor.binding() != request.binding || cursor.offset() > self.ids.len())
        {
            return Err(Error::InvalidCursor);
        }
        let offset = request.cursor.map_or(0, Cursor::offset);
        let end = offset
            .checked_add(request.limit)
            .ok_or(Error::SizeLimit)?
            .min(self.ids.len());
        let ids = self.ids[offset..end].to_vec();
        let next = (end < self.ids.len()).then(|| Cursor::new(self.binding, end));
        Ok(CandidatePage {
            schema: SchemaVersion::CURRENT,
            binding: self.binding,
            ids,
            next,
            coverage: self.coverage,
            quality: SearchQuality::Exact,
        })
    }
}

/// A provider adapter that validates and canonicalizes every page.
#[derive(Clone, Debug)]
pub struct Adapter<S> {
    source: S,
    limits: Limits,
}

impl<S> Adapter<S> {
    /// Creates a bounded adapter over a provider contract.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`] when the limits are not usable.
    pub fn new(source: S, limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            source,
            limits: limits.validate()?,
        })
    }

    /// Returns the configured limits.
    #[must_use]
    pub const fn limits(&self) -> Limits {
        self.limits
    }
}

impl<S: CandidateSource> Adapter<S> {
    /// Fetches, validates, and deterministically orders one result page.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError::Provider`] for a provider failure or
    /// [`AdapterError::Extension`] for stale roots/cursors, schema drift,
    /// incomplete coverage, malformed input, or a size violation.
    pub fn search(&self, request: SearchRequest) -> Result<SearchResult, AdapterError<S::Error>> {
        if request.limit == 0 || request.limit > self.limits.max_page {
            return Err(AdapterError::Extension(Error::SizeLimit));
        }
        if let Some(cursor) = request.cursor {
            if cursor.binding() != request.binding {
                return Err(AdapterError::Extension(Error::StaleCursor));
            }
            if cursor.offset() > self.limits.max_candidates {
                return Err(AdapterError::Extension(Error::InvalidCursor));
            }
        }
        let page = self
            .source
            .fetch(&request)
            .map_err(AdapterError::Provider)?;
        if page.schema != SchemaVersion::CURRENT {
            return Err(AdapterError::Extension(Error::SchemaDrift));
        }
        if page.binding != request.binding {
            return Err(AdapterError::Extension(Error::StaleRoot));
        }
        if !page.quality.validate(request.binding.recipe) {
            return Err(AdapterError::Extension(Error::ApproximationMismatch));
        }
        if !page.coverage.state().is_complete() {
            return Err(AdapterError::Extension(Error::IncompleteCoverage));
        }
        if page.ids.len() > request.limit || page.ids.len() > self.limits.max_candidates {
            return Err(AdapterError::Extension(Error::SizeLimit));
        }
        if page.ids.iter().any(|id| !id.is_valid()) {
            return Err(AdapterError::Extension(Error::MalformedInput));
        }
        let current_offset = request.cursor.map_or(0, Cursor::offset);
        let mut ids = page.ids;
        ids.sort_unstable();
        if ids.windows(2).any(|window| window[0] == window[1]) {
            return Err(AdapterError::Extension(Error::MalformedInput));
        }
        let expected_next = current_offset
            .checked_add(ids.len())
            .ok_or(AdapterError::Extension(Error::SizeLimit))?;
        if let Some(next) = page.next
            && (next.binding() != request.binding
                || next.offset() <= current_offset
                || next.offset() != expected_next
                || next.offset() > self.limits.max_candidates)
        {
            return Err(AdapterError::Extension(Error::InvalidCursor));
        }
        Ok(SearchResult {
            binding: request.binding,
            coverage: page.coverage,
            ids,
            next: page.next,
            quality: page.quality,
        })
    }
}
