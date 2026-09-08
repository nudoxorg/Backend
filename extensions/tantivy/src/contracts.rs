//! Query and source-facing lexical contracts.

use crate::{Binding, Error, Limits, QueryVersion};

/// Canonicalizes query terms and derives their typed identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    /// Canonical lower-case terms.
    pub terms: Vec<String>,
    /// Identity of the canonical term set.
    pub version: QueryVersion,
}

impl Query {
    /// Creates a canonical term query under a bound.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedInput`] for empty term values or
    /// [`Error::SizeLimit`] when the term count or byte budget is exceeded.
    pub fn new(mut terms: Vec<String>, limits: Limits) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if terms.iter().any(String::is_empty) {
            return Err(Error::MalformedInput);
        }
        if terms.len() > limits.max_terms {
            return Err(Error::SizeLimit);
        }
        for term in &mut terms {
            *term = term.to_ascii_lowercase();
            if term.len() > limits.max_field_bytes {
                return Err(Error::SizeLimit);
            }
        }
        terms.sort();
        terms.dedup();
        let mut bytes = Vec::new();
        for term in &terms {
            bytes.extend_from_slice(&(term.len() as u64).to_be_bytes());
            bytes.extend_from_slice(term.as_bytes());
        }
        Ok(Self {
            version: QueryVersion::from_value(&bytes),
            terms,
        })
    }

    pub(crate) fn validate(&self, limits: Limits) -> Result<(), Error> {
        if Self::new(self.terms.clone(), limits)? != *self {
            return Err(Error::MalformedInput);
        }
        Ok(())
    }
}

/// A bounded page cursor bound to all lexical inputs and terms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    binding: Binding,
    query: QueryVersion,
    offset: usize,
}

impl Cursor {
    /// Creates a cursor. The adapter validates its offset and exact binding
    /// before use.
    #[must_use]
    pub const fn new(binding: Binding, query: QueryVersion, offset: usize) -> Self {
        Self {
            binding,
            query,
            offset,
        }
    }

    /// Returns the exact materialization binding.
    #[must_use]
    pub(crate) const fn binding(self) -> Binding {
        self.binding
    }

    /// Returns the exact query identity.
    #[must_use]
    pub(crate) const fn query(self) -> QueryVersion {
        self.query
    }

    /// Returns the next row offset.
    #[must_use]
    pub(crate) const fn offset(self) -> usize {
        self.offset
    }
}
