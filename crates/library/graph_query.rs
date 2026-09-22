//! Transport-neutral bounded structured graph-query contract.

use crate::{
    Cursor, Frontier, PageContinuation, PageRequest, PageTerminal, QueryLimit, ViewRecipeId,
    ViewRevision, view_identity_bytes,
};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum structured-query text admitted at a process boundary.
pub const MAX_GRAPH_QUERY_BYTES: usize = 32 * 1024;
/// Maximum variables or projected fields in one structured-query value map.
pub const MAX_GRAPH_QUERY_FIELDS: usize = 128;
/// Maximum nesting depth for list-valued query inputs and outputs.
pub const MAX_GRAPH_VALUE_DEPTH: usize = 8;
/// Maximum aggregate string bytes in one query input or result row.
pub const MAX_GRAPH_VALUE_BYTES: usize = 64 * 1024;

/// A transport-neutral scalar accepted by structured graph queries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphValue {
    /// Null.
    Null,
    /// Boolean.
    Boolean(bool),
    /// Signed integer.
    Signed(i64),
    /// Unsigned integer.
    Unsigned(u64),
    /// Finite IEEE-754 value encoded by its canonical bits.
    Float(u64),
    /// UTF-8 string.
    String(String),
    /// Bounded recursive list.
    List(Box<[Self]>),
}

impl GraphValue {
    /// Creates a finite floating-point value.
    ///
    /// # Errors
    ///
    /// Returns [`GraphQueryError::NonFiniteFloat`] for NaN or infinity.
    pub fn finite_float(value: f64) -> Result<Self, GraphQueryError> {
        if value.is_finite() {
            Ok(Self::Float(value.to_bits()))
        } else {
            Err(GraphQueryError::NonFiniteFloat)
        }
    }

    /// Returns the represented finite floating-point value.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(bits) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }

    fn admitted_size(&self, depth: usize) -> Result<usize, GraphQueryError> {
        if depth > MAX_GRAPH_VALUE_DEPTH {
            return Err(GraphQueryError::ValueTooDeep);
        }
        match self {
            Self::String(value) => Ok(value.len()),
            Self::List(values) => values.iter().try_fold(0usize, |size, value| {
                size.checked_add(value.admitted_size(depth + 1)?)
                    .ok_or(GraphQueryError::ValuesTooLarge)
            }),
            Self::Null
            | Self::Boolean(_)
            | Self::Signed(_)
            | Self::Unsigned(_)
            | Self::Float(_) => Ok(16),
        }
    }
}

/// Semantic rejection of a structured graph-query request or row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphQueryError {
    /// Query text was empty.
    EmptyQuery,
    /// Query text exceeded the wire contract.
    QueryTooLarge,
    /// Too many variables or projected fields were supplied.
    TooManyFields,
    /// A variable or field name was empty.
    EmptyFieldName,
    /// Recursive list nesting exceeded the contract.
    ValueTooDeep,
    /// Aggregate query-value bytes exceeded the contract.
    ValuesTooLarge,
    /// A floating-point value was NaN or infinite.
    NonFiniteFloat,
    /// A continuation belongs to another query, root, or owner stream.
    ContinuationMismatch,
    /// A continuation offset is not representable on this host.
    OffsetTooLarge,
}

impl fmt::Display for GraphQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyQuery => "graph query cannot be empty",
            Self::QueryTooLarge => "graph query exceeds its text bound",
            Self::TooManyFields => "graph query exceeds its field-count bound",
            Self::EmptyFieldName => "graph query contains an empty field name",
            Self::ValueTooDeep => "graph query value exceeds its nesting bound",
            Self::ValuesTooLarge => "graph query values exceed their byte bound",
            Self::NonFiniteFloat => "graph query contains a non-finite float",
            Self::ContinuationMismatch => "graph query continuation does not match its request",
            Self::OffsetTooLarge => "graph query continuation offset is not representable",
        })
    }
}

impl std::error::Error for GraphQueryError {}

/// Cooperative disposition carried by a graph-query request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphQueryControl {
    /// Execute or resume the query.
    Continue,
    /// Cancel this exact root/query-bound execution.
    Cancel,
}

/// A canonical, transport-neutral graph-query input admitted independently of
/// any owner revision or page budget.
///
/// Keeping this value separate from [`GraphQueryRequest`] gives every caller
/// one admission boundary. A client can admit an input before it performs a
/// revision lookup, and then bind that exact value to a page without parsing
/// or validating it again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedGraphQueryInput {
    query: String,
    variables: Box<[(String, GraphValue)]>,
}

impl AdmittedGraphQueryInput {
    /// Admits query text and canonicalizes its variable map.
    ///
    /// # Errors
    ///
    /// Returns [`GraphQueryError`] when text, field, nesting, or byte bounds
    /// fail.
    pub fn new(
        query: impl Into<String>,
        variables: BTreeMap<String, GraphValue>,
    ) -> Result<Self, GraphQueryError> {
        let query = query.into();
        admit_query_text(&query)?;
        let variables = admit_fields(variables)?;
        Ok(Self { query, variables })
    }

    /// Returns the exact admitted query text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns canonical, name-sorted variables.
    #[must_use]
    pub fn variables(&self) -> &[(String, GraphValue)] {
        &self.variables
    }

    /// Returns the identity of the exact query text and canonical variables.
    #[must_use]
    pub fn recipe(&self) -> ViewRecipeId {
        let payload = self.recipe_payload();
        view_identity_bytes(&[b"graph-query", &payload])
    }

    /// Returns the canonical key preimage for the query recipe.
    ///
    /// Proof-producing owners include this bounded value when a continuation
    /// cursor changes the ordinary view recipe slot to the graph-query recipe.
    #[must_use]
    pub fn recipe_preimage(&self) -> Box<[u8]> {
        let payload = self.recipe_payload();
        let mut preimage = Vec::new();
        append_bytes(&mut preimage, b"graph-query");
        append_bytes(&mut preimage, &payload);
        preimage.into_boxed_slice()
    }

    fn recipe_payload(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        append_bytes(&mut bytes, self.query.as_bytes());
        for (name, value) in &self.variables {
            append_bytes(&mut bytes, name.as_bytes());
            append_value(&mut bytes, value);
        }
        bytes
    }
}

/// A bounded, root-pinned structured graph query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphQueryRequest {
    input: AdmittedGraphQueryInput,
    page: PageRequest,
    control: GraphQueryControl,
}

impl GraphQueryRequest {
    /// Admits a first-page query and its canonical variable map.
    ///
    /// # Errors
    ///
    /// Returns [`GraphQueryError`] when text, field, nesting, or byte bounds fail.
    pub fn new(
        query: impl Into<String>,
        variables: BTreeMap<String, GraphValue>,
        basis: impl Into<ViewRevision>,
        limit: QueryLimit,
    ) -> Result<Self, GraphQueryError> {
        let input = AdmittedGraphQueryInput::new(query, variables)?;
        Ok(Self::bind(input, basis, limit))
    }

    /// Binds an already admitted input to an owner revision and page budget.
    ///
    /// This method performs no query admission. Callers that receive an
    /// [`AdmittedGraphQueryInput`] from another boundary can therefore bind it
    /// after a revision lookup without repeating validation or canonicalizing
    /// a second copy.
    #[must_use]
    pub fn bind(
        input: AdmittedGraphQueryInput,
        basis: impl Into<ViewRevision>,
        limit: QueryLimit,
    ) -> Self {
        Self {
            input,
            page: PageRequest::new(basis, limit),
            control: GraphQueryControl::Continue,
        }
    }

    /// Resumes from the preceding page's opaque continuation.
    #[must_use]
    pub const fn with_continuation(mut self, continuation: PageContinuation) -> Self {
        self.page = self.page.with_continuation(continuation);
        self
    }

    /// Requests cancellation for this exact root and query identity.
    #[must_use]
    pub const fn cancelled(mut self) -> Self {
        self.control = GraphQueryControl::Cancel;
        self
    }

    /// Returns the exact query text.
    #[must_use]
    pub fn query(&self) -> &str {
        self.input.query()
    }

    /// Returns canonical, name-sorted variables.
    #[must_use]
    pub fn variables(&self) -> &[(String, GraphValue)] {
        self.input.variables()
    }

    /// Returns the admitted query input this page is bound to.
    #[must_use]
    pub const fn input(&self) -> &AdmittedGraphQueryInput {
        &self.input
    }

    /// Returns the shared revision, limit, and continuation page contract.
    #[must_use]
    pub const fn page(&self) -> PageRequest {
        self.page
    }

    /// Returns the cooperative execution disposition.
    #[must_use]
    pub const fn control(&self) -> GraphQueryControl {
        self.control
    }

    /// Returns the identity of the exact query text and canonical variables.
    #[must_use]
    pub fn recipe(&self) -> ViewRecipeId {
        self.input.recipe()
    }

    /// Returns the canonical key preimage for the query recipe.
    ///
    /// Proof-producing owners include this bounded value when a continuation
    /// cursor changes the ordinary view recipe slot to the graph-query recipe.
    #[must_use]
    pub fn recipe_preimage(&self) -> Box<[u8]> {
        self.input.recipe_preimage()
    }

    /// Admits this page's continuation against the selected owner cursor.
    ///
    /// # Errors
    ///
    /// Returns [`GraphQueryError::ContinuationMismatch`] for a foreign or
    /// stale continuation and [`GraphQueryError::OffsetTooLarge`] when its
    /// offset is not representable on this host.
    pub fn start_offset(&self, owner: Cursor) -> Result<usize, GraphQueryError> {
        if !self.page.basis().matches(owner.root()) {
            return Err(GraphQueryError::ContinuationMismatch);
        }
        let Some(continuation) = self.page.continuation() else {
            return Ok(0);
        };
        let cursor = continuation.cursor();
        if cursor.recipe() != self.recipe()
            || cursor.version() != owner.version()
            || cursor.branch() != owner.branch()
            || cursor.log() != owner.log()
            || cursor.schema() != owner.schema()
            || cursor.root() != owner.root()
            || cursor.sequence() > owner.sequence()
        {
            return Err(GraphQueryError::ContinuationMismatch);
        }
        usize::try_from(cursor.query_offset()).map_err(|_| GraphQueryError::OffsetTooLarge)
    }

    /// Creates the next opaque continuation after owner-side execution.
    ///
    /// # Errors
    ///
    /// Returns [`GraphQueryError::ContinuationMismatch`] when the selected
    /// owner cursor does not match the request revision.
    pub fn next_continuation(
        &self,
        owner: Cursor,
        next_offset: usize,
    ) -> Result<PageContinuation, GraphQueryError> {
        if !self.page.basis().matches(owner.root()) {
            return Err(GraphQueryError::ContinuationMismatch);
        }
        let cursor = Cursor::for_view(
            self.recipe(),
            owner.version(),
            Frontier::new(
                owner.branch(),
                owner.log(),
                owner.schema(),
                owner.root(),
                owner.sequence(),
            ),
        )
        .with_query_offset(next_offset as u64);
        Ok(PageContinuation::from_cursor(cursor))
    }
}

/// One bounded projected row from a structured graph query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphQueryRow(Box<[(String, GraphValue)]>);

impl GraphQueryRow {
    /// Admits a canonical projected field map.
    ///
    /// # Errors
    ///
    /// Returns [`GraphQueryError`] when field, nesting, or byte bounds fail.
    pub fn new(fields: BTreeMap<String, GraphValue>) -> Result<Self, GraphQueryError> {
        admit_fields(fields).map(Self)
    }

    /// Returns canonical, name-sorted projected fields.
    #[must_use]
    pub fn fields(&self) -> &[(String, GraphValue)] {
        &self.0
    }
}

/// One bounded structured graph-query result page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphQueryPage {
    /// Immutable owner revision used to execute the query.
    pub revision: ViewRevision,
    /// Complete source object observed by that revision.
    pub source: crate::SemanticObject,
    /// Bounded projected rows.
    pub rows: Box<[GraphQueryRow]>,
    /// Exactly one complete, continuation, or cancelled terminal.
    pub terminal: PageTerminal,
}

fn admit_query_text(query: &str) -> Result<(), GraphQueryError> {
    if query.is_empty() {
        Err(GraphQueryError::EmptyQuery)
    } else if query.len() > MAX_GRAPH_QUERY_BYTES {
        Err(GraphQueryError::QueryTooLarge)
    } else {
        Ok(())
    }
}

fn admit_fields(
    fields: BTreeMap<String, GraphValue>,
) -> Result<Box<[(String, GraphValue)]>, GraphQueryError> {
    if fields.len() > MAX_GRAPH_QUERY_FIELDS {
        return Err(GraphQueryError::TooManyFields);
    }
    let mut bytes = 0usize;
    for (name, value) in &fields {
        if name.is_empty() {
            return Err(GraphQueryError::EmptyFieldName);
        }
        let value_bytes = value.admitted_size(0)?;
        bytes = bytes
            .checked_add(name.len())
            .and_then(|size| size.checked_add(value_bytes))
            .ok_or(GraphQueryError::ValuesTooLarge)?;
        if bytes > MAX_GRAPH_VALUE_BYTES {
            return Err(GraphQueryError::ValuesTooLarge);
        }
    }
    Ok(fields.into_iter().collect::<Vec<_>>().into_boxed_slice())
}

fn append_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn append_value(output: &mut Vec<u8>, value: &GraphValue) {
    match value {
        GraphValue::Null => output.push(0),
        GraphValue::Boolean(value) => output.extend_from_slice(&[1, u8::from(*value)]),
        GraphValue::Signed(value) => {
            output.push(2);
            output.extend_from_slice(&value.to_be_bytes());
        }
        GraphValue::Unsigned(value) => {
            output.push(3);
            output.extend_from_slice(&value.to_be_bytes());
        }
        GraphValue::Float(bits) => {
            output.push(4);
            output.extend_from_slice(&bits.to_be_bytes());
        }
        GraphValue::String(value) => {
            output.push(5);
            append_bytes(output, value.as_bytes());
        }
        GraphValue::List(values) => {
            output.push(6);
            output.extend_from_slice(&(values.len() as u64).to_be_bytes());
            for value in values {
                append_value(output, value);
            }
        }
    }
}
