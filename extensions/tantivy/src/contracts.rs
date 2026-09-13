//! Query and source-facing lexical contracts.

use crate::{Binding, Error, Limits, QueryVersion};
use backend_semantic::EntityId;
use std::cmp::Ordering;

/// Closed lexical matching grammar shared by local and remote execution.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MatchMode {
    /// The normalized indexed token must equal the query token.
    Exact,
    /// The normalized indexed token must begin with the query token.
    Prefix,
}

/// Closed case behavior shared by local and remote lexical execution.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CaseSensitivity {
    /// Preserve and compare token bytes exactly.
    Sensitive,
    /// Fold ASCII case while retaining non-ASCII bytes.
    FoldAscii,
}

/// Typed indexed field selection.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FieldSelection {
    /// Search every indexed field.
    All,
    /// Search only one canonical field name.
    Only(String),
}

/// A lossless lexical rank. Prefix quality is compared as an exact ratio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Relevance {
    exact: bool,
    matched_bytes: u32,
    term_bytes: u32,
    field_weight: u16,
    matched_clauses: u16,
}

impl Relevance {
    /// Creates a complete-token relevance value for an already ranked provider hit.
    #[must_use]
    pub const fn exact(field_weight: u16) -> Self {
        Self {
            exact: true,
            matched_bytes: 1,
            term_bytes: 1,
            field_weight,
            matched_clauses: 1,
        }
    }
    pub(crate) fn new(
        query_bytes: usize,
        term_bytes: usize,
        field_weight: u16,
        matched_clauses: usize,
    ) -> Result<Self, Error> {
        if query_bytes > term_bytes {
            return Err(Error::MalformedInput);
        }
        let exact = query_bytes == term_bytes;
        Ok(Self {
            exact,
            matched_bytes: if exact {
                1
            } else {
                u32::try_from(query_bytes).map_err(|_| Error::SizeLimit)?
            },
            term_bytes: if exact {
                1
            } else {
                u32::try_from(term_bytes).map_err(|_| Error::SizeLimit)?
            },
            field_weight,
            matched_clauses: u16::try_from(matched_clauses).map_err(|_| Error::SizeLimit)?,
        })
    }

    /// Returns whether this contribution is a complete token match.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        self.exact
    }

    pub(crate) const fn all_documents() -> Self {
        Self {
            exact: true,
            matched_bytes: 1,
            term_bytes: 1,
            field_weight: 0,
            matched_clauses: 0,
        }
    }

    pub(crate) fn combine(self, other: Self) -> Result<Self, Error> {
        let weaker = if self < other { self } else { other };
        Ok(Self {
            exact: self.exact && other.exact,
            matched_bytes: weaker.matched_bytes,
            term_bytes: weaker.term_bytes,
            field_weight: self.field_weight.max(other.field_weight),
            matched_clauses: self
                .matched_clauses
                .checked_add(other.matched_clauses)
                .ok_or(Error::SizeLimit)?,
        })
    }
}

impl Ord for Relevance {
    fn cmp(&self, other: &Self) -> Ordering {
        self.matched_clauses
            .cmp(&other.matched_clauses)
            .then_with(|| self.exact.cmp(&other.exact))
            .then_with(|| {
                (u64::from(self.matched_bytes) * u64::from(other.term_bytes))
                    .cmp(&(u64::from(other.matched_bytes) * u64::from(self.term_bytes)))
            })
            .then_with(|| self.field_weight.cmp(&other.field_weight))
    }
}

impl PartialOrd for Relevance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A globally meaningful ranked search result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankedHit {
    /// Stable semantic declaration identity.
    pub document: EntityId,
    /// Lossless recipe rank.
    pub relevance: Relevance,
}

/// Canonicalizes query terms and derives their typed identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    /// Canonical query terms.
    pub terms: Vec<String>,
    /// Exact or prefix token selection.
    pub match_mode: MatchMode,
    /// Indexed fields admitted by this query.
    pub fields: FieldSelection,
    /// Explicit case behavior.
    pub case: CaseSensitivity,
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
    pub fn new(terms: Vec<String>, limits: Limits) -> Result<Self, Error> {
        Self::with_policy(terms, MatchMode::Exact, FieldSelection::All, limits)
    }

    /// Creates a canonical prefix query over all indexed fields.
    ///
    /// # Errors
    ///
    /// Returns a typed error when terms or limits fail query admission.
    pub fn prefix(terms: Vec<String>, limits: Limits) -> Result<Self, Error> {
        Self::with_policy(terms, MatchMode::Prefix, FieldSelection::All, limits)
    }

    /// Creates a canonical query with explicit matching and field policy.
    ///
    /// # Errors
    ///
    /// Returns a typed error when terms, fields, or limits fail query admission.
    pub fn with_policy(
        terms: Vec<String>,
        match_mode: MatchMode,
        fields: FieldSelection,
        limits: Limits,
    ) -> Result<Self, Error> {
        Self::with_options(
            terms,
            match_mode,
            fields,
            CaseSensitivity::FoldAscii,
            limits,
        )
    }

    /// Creates a canonical query with matching, field, and case policy explicit.
    ///
    /// # Errors
    ///
    /// Returns a typed error when terms, fields, or limits fail query admission.
    pub fn with_options(
        mut terms: Vec<String>,
        match_mode: MatchMode,
        fields: FieldSelection,
        case: CaseSensitivity,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        for term in &mut terms {
            if case == CaseSensitivity::FoldAscii {
                *term = term.to_ascii_lowercase();
            }
        }
        terms.sort();
        terms.dedup();
        let bytes = admit_query_bytes(&terms, match_mode, &fields, case, limits)?;
        Ok(Self {
            version: QueryVersion::from_value(&bytes),
            terms,
            match_mode,
            fields,
            case,
        })
    }

    pub(crate) fn validate(&self, limits: Limits) -> Result<(), Error> {
        let bytes = admit_query_bytes(
            &self.terms,
            self.match_mode,
            &self.fields,
            self.case,
            limits.validate()?,
        )?;
        let canonical_order = self.terms.windows(2).all(|pair| pair[0] < pair[1]);
        let canonical_case = self.case == CaseSensitivity::Sensitive
            || self.terms.iter().all(|term| {
                term.as_bytes()
                    .iter()
                    .all(|byte| !byte.is_ascii_uppercase())
            });
        if !canonical_order || !canonical_case || QueryVersion::from_value(&bytes) != self.version {
            return Err(Error::MalformedInput);
        }
        Ok(())
    }
}

fn admit_query_bytes(
    terms: &[String],
    match_mode: MatchMode,
    fields: &FieldSelection,
    case: CaseSensitivity,
    limits: Limits,
) -> Result<Vec<u8>, Error> {
    if terms.iter().any(String::is_empty) {
        return Err(Error::MalformedInput);
    }
    if terms.len() > limits.max_terms
        || terms.iter().any(|term| term.len() > limits.max_field_bytes)
    {
        return Err(Error::SizeLimit);
    }
    let mut bytes = vec![
        match match_mode {
            MatchMode::Exact => 1,
            MatchMode::Prefix => 2,
        },
        match case {
            CaseSensitivity::Sensitive => 1,
            CaseSensitivity::FoldAscii => 2,
        },
    ];
    match fields {
        FieldSelection::All => bytes.push(0),
        FieldSelection::Only(field) => {
            if field.is_empty() {
                return Err(Error::MalformedInput);
            }
            if field.len() > limits.max_field_bytes {
                return Err(Error::SizeLimit);
            }
            bytes.push(1);
            bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
            bytes.extend_from_slice(field.as_bytes());
        }
    }
    for term in terms {
        bytes.extend_from_slice(&(term.len() as u64).to_be_bytes());
        bytes.extend_from_slice(term.as_bytes());
    }
    Ok(bytes)
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
