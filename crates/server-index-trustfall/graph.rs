//! Defines graph behavior for `server-index-trustfall`, whose purpose is to adapt borrowed graph facts to typed Trustfall queries.
//! This module owns the graph invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Borrowed graph adapter and typed Trustfall result boundary.

use std::{
    collections::BTreeMap,
    convert::Infallible,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};

use compiler_ir::{
    Confidence, EntityId, FactAvailability, Ir, LinkId, LinkKind, LinkOccurrenceId, LinkTarget,
    SemanticCoreReader, SemanticImageView, SemanticReader, SourceSpan,
};
use futures_core::Stream;
use futures_util::stream;
use server_index_graph_vector::{Cancellation, GraphAuthority, PartitionId, ValidatedGraphView};
use thiserror::Error;
use trustfall::{
    FieldValue, Schema,
    provider::{
        Adapter, AsVertex, AsyncBasicAdapter, AsyncContextOutcomeStream, AsyncContextStream,
        AsyncNeighborStream, ContextIterator, ContextOutcomeIterator, EdgeParameters,
        ResolveEdgeInfo, ResolveInfo, Typename, VertexIterator, async_helpers,
        resolve_coercion_with, resolve_neighbors_with, resolve_property_with, resolve_typename,
    },
};
use trustfall_core::{
    frontend::{error::FrontendError, parse},
    interpreter::{
        error::{ExecutionError, QueryArgumentsError},
        execution::interpret_ir,
        interpret_ir_async,
    },
    ir::IndexedQuery,
    schema::error::InvalidSchemaError,
};

use crate::schema::{
    GRAPH_SCHEMA, NEIGHBORS_QUERY, SEMANTIC_GRAPH_SCHEMA, SEMANTIC_NEIGHBORS_QUERY,
};

const ENTITY_HALF_BITS: u32 = 16;
static PARSED_GRAPH_SCHEMA: OnceLock<Result<Schema, TrustfallUpstreamDiagnostic>> = OnceLock::new();
static PARSED_NEIGHBORS_QUERY: OnceLock<Result<Arc<IndexedQuery>, TrustfallUpstreamDiagnostic>> =
    OnceLock::new();
static PARSED_SEMANTIC_SCHEMA: OnceLock<Result<Schema, TrustfallUpstreamDiagnostic>> =
    OnceLock::new();
static PARSED_SEMANTIC_QUERY: OnceLock<Result<Arc<IndexedQuery>, TrustfallUpstreamDiagnostic>> =
    OnceLock::new();

/// One typed graph fact returned by the synchronous Trustfall projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrustfallHit {
    /// Snapshot and recipe authority that defines this graph fact.
    pub authority: GraphAuthority,
    /// Partition that supplied this edge.
    pub partition: PartitionId,
    /// Canonical neighboring entity.
    pub entity: EntityId,
}

/// One typed neighbor read directly from the canonical compiler IR.
///
/// Link kind and confidence remain available instead of being erased into the
/// older partition-only graph projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrTrustfallHit {
    /// Canonical neighboring entity.
    pub entity: EntityId,
    /// Semantic relation represented by the edge.
    pub kind: LinkKind,
    /// Strongest compiler evidence retained for this logical edge.
    pub confidence: Confidence,
}

/// One canonical local link yielded by lazy Trustfall execution over a reopened semantic image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTrustfallHit {
    /// Canonical neighboring declaration coordinate in the same image.
    pub entity: EntityId,
    /// Semantic relation retained by the compiler image.
    pub kind: LinkKind,
    /// Strongest compiler evidence retained for the relation.
    pub confidence: Confidence,
}

/// One authority-observed local link occurrence from a reopened semantic image.
///
/// Unlike [`SemanticTrustfallHit`], this retains every source site and its
/// occurrence-local confidence.
#[derive(Clone, Copy)]
pub struct SemanticOccurrenceHit<'view, 'bytes> {
    image: &'view SemanticImageView<'bytes>,
    occurrence: LinkOccurrenceId,
    link: LinkId,
    /// Canonical neighboring declaration coordinate in the same image.
    entity: EntityId,
    /// Semantic relation retained by the compiler image.
    kind: LinkKind,
    /// Confidence attached to this particular observed occurrence.
    confidence: Confidence,
    /// Correlated source coordinate and authority availability.
    provenance: OccurrenceSourceEvidence<'view>,
}

impl<'view, 'bytes> SemanticOccurrenceHit<'view, 'bytes> {
    /// Returns the canonical neighboring declaration.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }
    /// Returns the stable exhaustive occurrence coordinate.
    #[must_use]
    pub const fn occurrence(self) -> LinkOccurrenceId {
        self.occurrence
    }
    /// Returns the canonical relation coordinate for this occurrence.
    #[must_use]
    pub const fn link(self) -> LinkId {
        self.link
    }
    /// Returns the semantic relation.
    #[must_use]
    pub const fn kind(self) -> LinkKind {
        self.kind
    }
    /// Returns confidence attached to this occurrence.
    #[must_use]
    pub const fn confidence(self) -> Confidence {
        self.confidence
    }
    /// Returns correlated source evidence.
    #[must_use]
    pub const fn provenance(self) -> OccurrenceSourceEvidence<'view> {
        self.provenance
    }
    /// Returns the reopened image that authenticated this candidate.
    #[must_use]
    pub const fn image(self) -> &'view SemanticImageView<'bytes> {
        self.image
    }
}

/// Closed provenance state for one occurrence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OccurrenceSourceEvidence<'view> {
    /// The authority captured this exact source coordinate.
    Captured(CapturedOccurrenceSpan<'view>),
    /// The authority did not provide a source coordinate.
    Unavailable,
}

/// Captured source coordinate whose construction is restricted to validated image rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapturedOccurrenceSpan<'view> {
    span: SourceSpan,
    path: &'view [u8],
}

impl<'view> CapturedOccurrenceSpan<'view> {
    /// Returns the exact source coordinate.
    #[must_use]
    pub const fn span(self) -> SourceSpan {
        self.span
    }
    /// Returns the borrowed non-UTF-8 source path bytes.
    #[must_use]
    pub const fn path(self) -> &'view [u8] {
        self.path
    }
}

impl<'view> OccurrenceSourceEvidence<'view> {
    /// Returns the captured coordinate, if provenance was available.
    #[must_use]
    pub const fn source(self) -> Option<SourceSpan> {
        match self {
            Self::Captured(span) => Some(span.span()),
            Self::Unavailable => None,
        }
    }
    /// Returns borrowed source-path bytes when captured.
    #[must_use]
    pub const fn path(self) -> Option<&'view [u8]> {
        match self {
            Self::Captured(span) => Some(span.path()),
            Self::Unavailable => None,
        }
    }

    /// Returns the exact authority availability state.
    #[must_use]
    pub const fn availability(self) -> FactAvailability {
        match self {
            Self::Captured(_) => FactAvailability::Captured,
            Self::Unavailable => FactAvailability::Unavailable,
        }
    }
}

type SemanticRows<'view> = Pin<
    Box<
        dyn Stream<Item = Result<BTreeMap<Arc<str>, FieldValue>, ExecutionError<Infallible>>>
            + 'view,
    >,
>;

/// Lazy bounded-memory result stream for a fixed semantic-image neighbor query.
///
/// The stream borrows the validated image and cancellation authority independently. It owns only
/// Trustfall's query pipeline; semantic rows and links are decoded from the image on demand.
pub struct SemanticTrustfallStream<'rows, 'cancel> {
    rows: SemanticRows<'rows>,
    cancellation: &'cancel Cancellation,
    done: bool,
}

type SemanticOccurrenceRows<'view, 'bytes> = Box<
    dyn Iterator<Item = Result<SemanticOccurrenceHit<'view, 'bytes>, TrustfallGraphError>> + 'view,
>;

/// Lazy bounded-memory result stream for exhaustive occurrence-level traversal.
pub struct SemanticOccurrenceStream<'image, 'cancel, 'bytes> {
    rows: SemanticOccurrenceRows<'image, 'bytes>,
    cancellation: &'cancel Cancellation,
    done: bool,
}

impl<'image, 'cancel, 'bytes> Stream for SemanticOccurrenceStream<'image, 'cancel, 'bytes> {
    type Item = Result<SemanticOccurrenceHit<'image, 'bytes>, TrustfallGraphError>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        if self.cancellation.is_cancelled() {
            self.done = true;
            return Poll::Ready(Some(Err(TrustfallGraphError::Cancelled)));
        }
        match self.rows.next() {
            Some(Ok(hit)) => Poll::Ready(Some(Ok(hit))),
            Some(Err(error)) => {
                self.done = true;
                Poll::Ready(Some(Err(error)))
            }
            None => {
                self.done = true;
                Poll::Ready(None)
            }
        }
    }
}

impl Stream for SemanticTrustfallStream<'_, '_> {
    type Item = Result<SemanticTrustfallHit, TrustfallGraphError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        if self.cancellation.is_cancelled() {
            self.done = true;
            return Poll::Ready(Some(Err(TrustfallGraphError::Cancelled)));
        }
        match self.rows.as_mut().poll_next(context) {
            Poll::Ready(Some(Ok(row))) => Poll::Ready(Some(decode_semantic_hit(&row))),
            Poll::Ready(Some(Err(cause))) => {
                self.done = true;
                Poll::Ready(Some(Err(execution_error(cause))))
            }
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Trustfall's async interpreter bound directly to one validated semantic image.
pub struct SemanticTrustfallGraph<'image, 'cancel, 'bytes> {
    image: &'image SemanticImageView<'bytes>,
    cancellation: &'cancel Cancellation,
}

impl<'image, 'cancel, 'bytes> SemanticTrustfallGraph<'image, 'cancel, 'bytes> {
    /// Borrows canonical semantic-image bytes and a cooperative cancellation authority.
    #[must_use]
    pub const fn new(
        image: &'image SemanticImageView<'bytes>,
        cancellation: &'cancel Cancellation,
    ) -> Self {
        Self {
            image,
            cancellation,
        }
    }
}

impl<'image, 'cancel, 'bytes: 'image> SemanticTrustfallGraph<'image, 'cancel, 'bytes> {
    /// Starts a lazy fixed neighbor query without reconstructing graph rows.
    ///
    /// # Errors
    ///
    /// Returns a typed cancellation terminal before query setup, or a stable diagnostic if the
    /// crate-owned schema, query, or arguments are rejected by Trustfall.
    pub fn neighbors(
        &self,
        source: EntityId,
    ) -> Result<SemanticTrustfallStream<'image, 'cancel>, TrustfallGraphError> {
        if self.cancellation.is_cancelled() {
            return Err(TrustfallGraphError::Cancelled);
        }
        let schema = parsed_semantic_schema()?;
        let query = parsed_semantic_query(schema)?;
        let adapter = Arc::new(BorrowedSemanticAdapter { image: self.image });
        let arguments = Arc::new(source_arguments(source));
        let rows = interpret_ir_async(adapter, query, arguments).map_err(|cause| {
            TrustfallGraphError::UpstreamRejected {
                diagnostic: TrustfallUpstreamDiagnostic::Arguments(argument_diagnostic(cause)),
            }
        })?;
        Ok(SemanticTrustfallStream {
            rows,
            cancellation: self.cancellation,
            done: false,
        })
    }
}

impl<'image, 'cancel, 'bytes> SemanticTrustfallGraph<'image, 'cancel, 'bytes> {
    /// Starts an exhaustive lazy local occurrence traversal over the reopened image.
    ///
    /// This operation intentionally does not use the representative-level
    /// `neighbors` query: repeated observations of one logical link remain
    /// distinct rows, including their source coordinates and confidence.
    pub fn occurrence_neighbors(
        &self,
        source: EntityId,
    ) -> Result<SemanticOccurrenceStream<'image, 'cancel, 'bytes>, TrustfallGraphError> {
        self.occurrences(source, false)
    }

    /// Starts an exhaustive lazy traversal of local occurrences targeting `source`.
    ///
    /// Incoming traversal borrows the canonical occurrence lane and does not materialize a
    /// reverse graph. The consumer's result bound remains the allocation boundary.
    pub fn incoming_occurrence_neighbors(
        &self,
        source: EntityId,
    ) -> Result<SemanticOccurrenceStream<'image, 'cancel, 'bytes>, TrustfallGraphError> {
        self.occurrences(source, true)
    }

    fn occurrences(
        &self,
        source: EntityId,
        incoming: bool,
    ) -> Result<SemanticOccurrenceStream<'image, 'cancel, 'bytes>, TrustfallGraphError> {
        if self.cancellation.is_cancelled() {
            return Err(TrustfallGraphError::Cancelled);
        }
        let image = self.image;
        let rows = Box::new(
            image
                .link_occurrences()
                .map(move |(id, occurrence)| {
                    let Some(link) = image.link(occurrence.link) else {
                        return Err(TrustfallGraphError::OccurrenceEvidenceRejected);
                    };
                    let LinkTarget::Local(entity) = link.target else {
                        return Ok(None);
                    };
                    if (incoming && entity != source) || (!incoming && link.from != source) {
                        return Ok(None);
                    }
                    let Some(authority) = image.occurrence_authority(id) else {
                        return Err(TrustfallGraphError::OccurrenceEvidenceRejected);
                    };
                    let provenance = match (authority.source, occurrence.source) {
                        (FactAvailability::Captured, Some(span)) => {
                            let Some(path) = image.atom(span.file()) else {
                                return Err(TrustfallGraphError::OccurrenceEvidenceRejected);
                            };
                            OccurrenceSourceEvidence::Captured(CapturedOccurrenceSpan {
                                span,
                                path,
                            })
                        }
                        (FactAvailability::Unavailable, None) => {
                            OccurrenceSourceEvidence::Unavailable
                        }
                        _ => return Err(TrustfallGraphError::OccurrenceEvidenceRejected),
                    };
                    Ok(Some(SemanticOccurrenceHit {
                        image,
                        occurrence: id,
                        link: occurrence.link,
                        entity: if incoming { link.from } else { entity },
                        kind: link.kind,
                        confidence: occurrence.confidence,
                        provenance,
                    }))
                })
                .filter_map(|row| match row {
                    Ok(Some(hit)) => Some(Ok(hit)),
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                }),
        ) as SemanticOccurrenceRows<'image, 'bytes>;
        Ok(SemanticOccurrenceStream {
            rows,
            cancellation: self.cancellation,
            done: false,
        })
    }
}

/// Allocation-free fixed Trustfall projection over the canonical IR itself.
///
/// This is the fast path for the common `neighbors` operation: it reads the
/// IR's outgoing CSR index directly and writes caller-owned storage. No graph
/// rows, serialized segment, `Arc`, `Box`, or result map is introduced. The
/// dynamic Trustfall adapter remains available for arbitrary query plans.
#[derive(Clone, Copy)]
pub struct IrTrustfallGraph<'ir> {
    ir: &'ir Ir,
}

impl<'ir> IrTrustfallGraph<'ir> {
    /// Borrows the same immutable IR used by compilers, renderers, and IR-VCS.
    #[must_use]
    pub const fn new(ir: &'ir Ir) -> Self {
        Self { ir }
    }

    /// Writes all local outgoing neighbors in stable dense-coordinate order.
    ///
    /// External links stay in the canonical IR but are omitted because this
    /// operation promises local [`EntityId`] results.
    pub fn neighbors(
        &self,
        source: EntityId,
        output: &mut [Option<IrTrustfallHit>],
    ) -> Result<usize, TrustfallGraphError> {
        let required = self
            .ir
            .links_from(source)
            .filter(|(_, link)| matches!(link.target, LinkTarget::Local(_)))
            .count();
        if output.len() < required {
            return Err(TrustfallGraphError::InsufficientOutput {
                required,
                available: output.len(),
            });
        }

        let mut written = 0;
        for (_, link) in self.ir.links_from(source) {
            let LinkTarget::Local(entity) = link.target else {
                continue;
            };
            output[written] = Some(IrTrustfallHit {
                entity,
                kind: link.kind,
                confidence: link.confidence,
            });
            written += 1;
        }
        Ok(written)
    }
}

/// Complete terminal of one Trustfall neighbor operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrustfallTerminal {
    /// Snapshot and recipe authority retained through the synchronous adapter boundary.
    pub authority: GraphAuthority,
    /// Number of initialized caller output slots.
    pub written: usize,
}

/// Closed output field vocabulary for the fixed Trustfall operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustfallOutputField {
    /// High sixteen bits of an entity identity.
    EntityHigh,
    /// Low sixteen bits of an entity identity.
    EntityLow,
    /// Graph partition coordinate.
    Partition,
    /// Compiler semantic-link kind.
    LinkKind,
    /// Compiler confidence lattice value.
    Confidence,
}

/// Exact integral wire value rejected while decoding one fixed Trustfall output field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustfallOutputNumber {
    /// A signed Trustfall integer value.
    Signed(i64),
    /// An unsigned Trustfall integer value.
    Unsigned(u64),
}

/// Closed reason a fixed Trustfall output field could not become a `u16` coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustfallOutputCause {
    /// Trustfall omitted the declared fixed output field.
    Missing,
    /// Trustfall supplied a value outside the declared integer grammar.
    NonInteger,
    /// Trustfall supplied an integer outside the accepted `u16` coordinate range.
    OutOfRange {
        /// Exact rejected signed or unsigned integer value.
        observed: TrustfallOutputNumber,
    },
    /// A bounded integer did not name a member of the compiler's closed vocabulary.
    UnknownCode {
        /// Rejected bounded integer.
        observed: u16,
    },
}

/// Stable schema diagnostic category derived from Trustfall's non-exhaustive upstream error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustfallSchemaDiagnostic {
    /// The crate-owned schema did not satisfy Trustfall's grammar.
    Syntax,
    /// The schema parsed but its declarations were semantically inconsistent.
    Semantic,
    /// Trustfall retained several schema failures.
    Multiple,
    /// A future Trustfall schema variant was rejected without losing its closed source stage.
    UpstreamFuture,
}

/// Stable query diagnostic category derived from Trustfall's non-exhaustive upstream error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustfallQueryDiagnostic {
    /// The crate-owned query did not satisfy Trustfall's grammar.
    Syntax,
    /// The query parsed but failed schema/query validation.
    Validation,
    /// Trustfall's query-planning fallback rejected the fixed operation.
    Planning,
    /// Trustfall retained several query failures.
    Multiple,
    /// A future Trustfall query variant was rejected without losing its closed source stage.
    UpstreamFuture,
}

/// Stable typed argument diagnostic derived from Trustfall's fixed interpreter boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustfallArgumentDiagnostic {
    /// A fixed query argument was missing.
    Missing,
    /// The fixed adapter supplied an argument the query did not consume.
    Unused,
    /// A fixed query argument did not satisfy Trustfall's declared type.
    Type,
    /// Trustfall retained several argument failures.
    Multiple,
}

/// Closed retained source for one upstream Trustfall rejection.
///
/// Trustfall's upstream diagnostics are non-exhaustive and unsuitable for this stable public
/// adapter boundary. The stage and category retain the strongest portable evidence without a
/// display string, dynamic payload, or erased error object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustfallUpstreamDiagnostic {
    /// Schema parsing or validation failed.
    Schema(TrustfallSchemaDiagnostic),
    /// Fixed query parsing, validation, or planning failed.
    Query(TrustfallQueryDiagnostic),
    /// Fixed query variable binding failed before iteration began.
    Arguments(TrustfallArgumentDiagnostic),
}

/// Exact failure while executing the fixed synchronous Trustfall operation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TrustfallGraphError {
    /// Trustfall rejected one crate-owned static or interpreter stage.
    #[error("Trustfall rejected the crate-owned operation: {diagnostic:?}")]
    UpstreamRejected {
        /// Closed stage and stable category retained from the upstream rejection.
        diagnostic: TrustfallUpstreamDiagnostic,
    },
    /// Cooperative cancellation won before the next lazy result was decoded.
    #[error("Trustfall semantic query was cancelled")]
    Cancelled,
    /// Trustfall reported an execution failure not attributable to this infallible image adapter.
    #[error("Trustfall rejected lazy semantic query execution")]
    ExecutionRejected,
    /// A reopened occurrence row violated its validated link/provenance correlation.
    #[error("semantic occurrence evidence was inconsistent")]
    OccurrenceEvidenceRejected,
    /// The caller's output cannot preserve every result from the immutable graph view.
    #[error("Trustfall graph output has {available} slots, required {required}")]
    InsufficientOutput {
        /// Complete graph fact count required by the selected source.
        required: usize,
        /// Caller-provided output capacity.
        available: usize,
    },
    /// A fixed Trustfall output field was not a valid unsigned sixteen-bit coordinate.
    #[error("Trustfall graph output field {field:?} was rejected: {cause:?}")]
    InvalidOutputField {
        /// Declared output field rejected by the decoder.
        field: TrustfallOutputField,
        /// Closed missing, type, or range cause retained from the output row.
        cause: TrustfallOutputCause,
    },
    /// Trustfall output cardinality disagreed with the borrowed graph facts that seeded it.
    #[error("Trustfall graph emitted {observed} facts after a preflight of {expected}")]
    CardinalityMismatch {
        /// Count established from the already validated borrowed graph view.
        expected: usize,
        /// Count emitted by Trustfall for the same fixed query.
        observed: usize,
    },
}

/// Synchronous Trustfall adapter over one already validated borrowed graph view.
///
/// This type owns no graph data, task, runtime, or transport. The crate-owned static schema and
/// query are parsed once into immutable process state. Each call still pays Trustfall's required
/// adapter/argument [`Arc`]s, boxed iterators, result maps, and a bounded preflight edge pass; those
/// allocations are confined to the upstream dynamic ABI while canonical graph facts stay borrowed.
#[derive(Debug)]
pub struct TrustfallGraph<'view> {
    view: &'view ValidatedGraphView<'view>,
}

impl<'view> TrustfallGraph<'view> {
    /// Binds Trustfall to an already validated, pinned borrowed graph view.
    #[must_use]
    pub const fn new(view: &'view ValidatedGraphView<'view>) -> Self {
        Self { view }
    }

    /// Executes the fixed typed neighbor operation and writes a stable output order.
    pub fn neighbors(
        &self,
        source: EntityId,
        output: &mut [Option<TrustfallHit>],
    ) -> Result<TrustfallTerminal, TrustfallGraphError> {
        let required = self.source_count(source);
        if output.len() < required {
            return Err(TrustfallGraphError::InsufficientOutput {
                required,
                available: output.len(),
            });
        }

        let schema = parsed_schema()?;
        let query = parsed_query(schema)?;
        let adapter = Arc::new(BorrowedGraphAdapter {
            view: self.view,
            schema,
        });
        let arguments = Arc::new(source_arguments(source));
        let rows = match interpret_ir(adapter, query, arguments) {
            Ok(rows) => rows,
            Err(cause) => {
                return Err(TrustfallGraphError::UpstreamRejected {
                    diagnostic: TrustfallUpstreamDiagnostic::Arguments(argument_diagnostic(cause)),
                });
            }
        };

        for slot in output.iter_mut().take(required) {
            *slot = None;
        }
        let mut written = 0_usize;
        for row in rows {
            let row = row.map_err(execution_error)?;
            let hit = decode_hit(&row, self.view.authority)?;
            insert_sorted(output, &mut written, hit);
        }
        if written != required {
            return Err(TrustfallGraphError::CardinalityMismatch {
                expected: required,
                observed: written,
            });
        }
        Ok(TrustfallTerminal {
            authority: self.view.authority,
            written,
        })
    }

    fn source_count(&self, source: EntityId) -> usize {
        self.view
            .rows
            .iter()
            .flat_map(|row| row.edges)
            .filter(|edge| edge.source == source)
            .count()
    }
}

fn parsed_schema() -> Result<&'static Schema, TrustfallGraphError> {
    cached_schema(&PARSED_GRAPH_SCHEMA, GRAPH_SCHEMA).map_err(upstream_error)
}

fn parsed_query(schema: &Schema) -> Result<Arc<IndexedQuery>, TrustfallGraphError> {
    cached_query(&PARSED_NEIGHBORS_QUERY, schema, NEIGHBORS_QUERY).map_err(upstream_error)
}

fn parsed_semantic_schema() -> Result<&'static Schema, TrustfallGraphError> {
    cached_schema(&PARSED_SEMANTIC_SCHEMA, SEMANTIC_GRAPH_SCHEMA).map_err(upstream_error)
}

fn parsed_semantic_query(schema: &Schema) -> Result<Arc<IndexedQuery>, TrustfallGraphError> {
    cached_query(&PARSED_SEMANTIC_QUERY, schema, SEMANTIC_NEIGHBORS_QUERY).map_err(upstream_error)
}

fn cached_schema<'schema>(
    cache: &'schema OnceLock<Result<Schema, TrustfallUpstreamDiagnostic>>,
    grammar: &str,
) -> Result<&'schema Schema, TrustfallUpstreamDiagnostic> {
    match cache.get_or_init(|| {
        Schema::parse(grammar)
            .map_err(|cause| TrustfallUpstreamDiagnostic::Schema(schema_diagnostic(cause)))
    }) {
        Ok(schema) => Ok(schema),
        Err(cause) => Err(*cause),
    }
}

fn cached_query(
    cache: &OnceLock<Result<Arc<IndexedQuery>, TrustfallUpstreamDiagnostic>>,
    schema: &Schema,
    query: &str,
) -> Result<Arc<IndexedQuery>, TrustfallUpstreamDiagnostic> {
    match cache.get_or_init(|| {
        parse(schema, query)
            .map_err(|cause| TrustfallUpstreamDiagnostic::Query(query_diagnostic(cause)))
    }) {
        Ok(query) => Ok(Arc::clone(query)),
        Err(cause) => Err(*cause),
    }
}

const fn upstream_error(diagnostic: TrustfallUpstreamDiagnostic) -> TrustfallGraphError {
    TrustfallGraphError::UpstreamRejected { diagnostic }
}

fn schema_diagnostic(cause: InvalidSchemaError) -> TrustfallSchemaDiagnostic {
    match cause {
        InvalidSchemaError::MultipleErrors(_) => TrustfallSchemaDiagnostic::Multiple,
        InvalidSchemaError::SchemaParseError(_) => TrustfallSchemaDiagnostic::Syntax,
        InvalidSchemaError::InvalidTypeWideningOfInheritedField(..)
        | InvalidSchemaError::InvalidTypeNarrowingOfInheritedFieldParameter(..)
        | InvalidSchemaError::InheritedFieldMissingParameters(..)
        | InvalidSchemaError::InheritedFieldUnexpectedParameters(..)
        | InvalidSchemaError::InvalidDefaultValueForFieldParameter(..)
        | InvalidSchemaError::CircularImplementsRelationships(..)
        | InvalidSchemaError::MissingTransitiveInterfaceImplementation(..)
        | InvalidSchemaError::MissingRequiredField(..)
        | InvalidSchemaError::AmbiguousFieldOrigin(..)
        | InvalidSchemaError::PropertyFieldWithParameters(..)
        | InvalidSchemaError::InvalidEdgeType(..)
        | InvalidSchemaError::UnknownPropertyOrEdgeType(..)
        | InvalidSchemaError::PropertyFieldOnRootQueryType(..)
        | InvalidSchemaError::EdgePointsToRootQueryType(..)
        | InvalidSchemaError::ReservedFieldName(..)
        | InvalidSchemaError::ReservedTypeName(..)
        | InvalidSchemaError::ImplementingNonExistentType(..)
        | InvalidSchemaError::ImplementingNonInterface(..)
        | InvalidSchemaError::DuplicateFieldDefinition(..)
        | InvalidSchemaError::DuplicateTypeOrInterfaceDefinition(..) => {
            TrustfallSchemaDiagnostic::Semantic
        }
        _ => TrustfallSchemaDiagnostic::UpstreamFuture,
    }
}

fn query_diagnostic(cause: FrontendError) -> TrustfallQueryDiagnostic {
    match cause {
        FrontendError::MultipleErrors(_) => TrustfallQueryDiagnostic::Multiple,
        FrontendError::ParseError(_) => TrustfallQueryDiagnostic::Syntax,
        FrontendError::OtherError(_) => TrustfallQueryDiagnostic::Planning,
        FrontendError::UndefinedTagInFilter(..)
        | FrontendError::TagUsedBeforeDefinition(..)
        | FrontendError::TagUsedOutsideItsFoldedSubquery(..)
        | FrontendError::UnusedTags(..)
        | FrontendError::MultipleOutputsWithSameName(..)
        | FrontendError::MultipleTagsWithSameName(..)
        | FrontendError::ExplicitTagNameRequired(..)
        | FrontendError::FilterTypeError(..)
        | FrontendError::UnsupportedDirectiveOnProperty(..)
        | FrontendError::UnsupportedEdgeOutput(..)
        | FrontendError::UnsupportedEdgeFilter(..)
        | FrontendError::UnsupportedEdgeTag(..)
        | FrontendError::UnsupportedDirectiveOnFoldedEdge(..)
        | FrontendError::MissingRequiredEdgeParameter(..)
        | FrontendError::UnexpectedEdgeParameter(..)
        | FrontendError::InvalidEdgeParameterType(..)
        | FrontendError::RecursingNonRecursableEdge(..)
        | FrontendError::RecursionToSubtype(..)
        | FrontendError::AmbiguousOriginEdgeRecursion(..)
        | FrontendError::EdgeRecursionNeedingMultipleCoercions(..)
        | FrontendError::PropertyMetaFieldUsedAsEdge(..)
        | FrontendError::ValidationError(..) => TrustfallQueryDiagnostic::Validation,
        _ => TrustfallQueryDiagnostic::UpstreamFuture,
    }
}

fn argument_diagnostic(cause: QueryArgumentsError) -> TrustfallArgumentDiagnostic {
    match cause {
        QueryArgumentsError::MissingArguments(_) => TrustfallArgumentDiagnostic::Missing,
        QueryArgumentsError::UnusedArguments(_) => TrustfallArgumentDiagnostic::Unused,
        QueryArgumentsError::ArgumentTypeError(..) => TrustfallArgumentDiagnostic::Type,
        QueryArgumentsError::MultipleErrors(_) => TrustfallArgumentDiagnostic::Multiple,
    }
}

#[derive(Clone, Copy, Debug)]
enum GraphVertex {
    Entity(EntityId),
    Neighbor {
        entity: EntityId,
        partition: PartitionId,
    },
}

impl Typename for GraphVertex {
    fn typename(&self) -> &'static str {
        match self {
            Self::Entity(_) => "Entity",
            Self::Neighbor { .. } => "Neighbor",
        }
    }
}

#[derive(Debug)]
struct BorrowedGraphAdapter<'view, 'schema> {
    view: &'view ValidatedGraphView<'view>,
    schema: &'schema Schema,
}

impl<'view> Adapter<'view> for BorrowedGraphAdapter<'view, '_> {
    type Vertex = GraphVertex;

    fn resolve_starting_vertices(
        &self,
        edge_name: &Arc<str>,
        _parameters: &EdgeParameters,
        _resolve_info: &ResolveInfo,
    ) -> VertexIterator<'view, Self::Vertex> {
        if edge_name.as_ref() != "Entities" {
            return Box::new(core::iter::empty());
        }
        Box::new(
            self.view
                .rows
                .iter()
                .flat_map(|row| row.edges)
                .enumerate()
                .filter_map(|(edge_index, edge)| {
                    let is_first_source = self
                        .view
                        .rows
                        .iter()
                        .flat_map(|row| row.edges)
                        .take(edge_index)
                        .all(|previous| previous.source != edge.source);
                    is_first_source.then_some(GraphVertex::Entity(edge.source))
                }),
        )
    }

    fn resolve_property<Vertex: AsVertex<Self::Vertex> + 'view>(
        &self,
        contexts: ContextIterator<'view, Vertex>,
        type_name: &Arc<str>,
        property_name: &Arc<str>,
        _resolve_info: &ResolveInfo,
    ) -> ContextOutcomeIterator<'view, Vertex, FieldValue> {
        if property_name.as_ref() == "__typename" {
            return resolve_typename(contexts, self.schema, type_name);
        }
        match property_name.as_ref() {
            "high" => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Entity(entity) => FieldValue::Int64(i64::from(entity_high(*entity))),
                GraphVertex::Neighbor { .. } => FieldValue::Null,
            }),
            "low" => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Entity(entity) => FieldValue::Int64(i64::from(entity_low(*entity))),
                GraphVertex::Neighbor { .. } => FieldValue::Null,
            }),
            "entityHigh" => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Neighbor { entity, .. } => {
                    FieldValue::Int64(i64::from(entity_high(*entity)))
                }
                GraphVertex::Entity(_) => FieldValue::Null,
            }),
            "entityLow" => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Neighbor { entity, .. } => {
                    FieldValue::Int64(i64::from(entity_low(*entity)))
                }
                GraphVertex::Entity(_) => FieldValue::Null,
            }),
            "partition" => resolve_property_with(contexts, |vertex| match vertex {
                GraphVertex::Neighbor { partition, .. } => {
                    FieldValue::Int64(i64::from(partition.raw))
                }
                GraphVertex::Entity(_) => FieldValue::Null,
            }),
            _ => resolve_property_with(contexts, |_| FieldValue::Null),
        }
    }

    fn resolve_neighbors<Vertex: AsVertex<Self::Vertex> + 'view>(
        &self,
        contexts: ContextIterator<'view, Vertex>,
        _type_name: &Arc<str>,
        edge_name: &Arc<str>,
        _parameters: &EdgeParameters,
        _resolve_info: &ResolveEdgeInfo,
    ) -> ContextOutcomeIterator<'view, Vertex, VertexIterator<'view, Self::Vertex>> {
        if edge_name.as_ref() != "neighbors" {
            return resolve_neighbors_with(contexts, |_| Box::new(core::iter::empty()));
        }
        resolve_neighbors_with(contexts, |vertex| match *vertex {
            GraphVertex::Entity(source) => Box::new(
                self.view
                    .rows
                    .iter()
                    .flat_map(|row| row.edges)
                    .filter(move |edge| edge.source == source)
                    .map(|edge| GraphVertex::Neighbor {
                        entity: edge.target,
                        partition: edge.partition,
                    }),
            ),
            GraphVertex::Neighbor { .. } => Box::new(core::iter::empty()),
        })
    }

    fn resolve_coercion<Vertex: AsVertex<Self::Vertex> + 'view>(
        &self,
        contexts: ContextIterator<'view, Vertex>,
        _type_name: &Arc<str>,
        _coerce_to_type: &Arc<str>,
        _resolve_info: &ResolveInfo,
    ) -> ContextOutcomeIterator<'view, Vertex, bool> {
        resolve_coercion_with(contexts, |_| false)
    }
}

#[derive(Clone, Copy, Debug)]
enum SemanticVertex {
    Entity(EntityId),
    Link {
        entity: EntityId,
        kind: LinkKind,
        confidence: Confidence,
    },
}

#[derive(Clone, Copy)]
enum SemanticProperty {
    SourceHigh,
    SourceLow,
    EntityHigh,
    EntityLow,
    Kind,
    Confidence,
    Unknown,
}

impl SemanticProperty {
    const fn from_name(name: &str) -> Self {
        match name.as_bytes() {
            b"high" => Self::SourceHigh,
            b"low" => Self::SourceLow,
            b"entityHigh" => Self::EntityHigh,
            b"entityLow" => Self::EntityLow,
            b"kind" => Self::Kind,
            b"confidence" => Self::Confidence,
            _ => Self::Unknown,
        }
    }
}

impl Typename for SemanticVertex {
    fn typename(&self) -> &'static str {
        match self {
            Self::Entity(_) => "SemanticEntity",
            Self::Link { .. } => "SemanticLink",
        }
    }
}

struct BorrowedSemanticAdapter<'view, 'bytes> {
    image: &'view SemanticImageView<'bytes>,
}

impl<'view, 'bytes: 'view> AsyncBasicAdapter<'view> for BorrowedSemanticAdapter<'view, 'bytes> {
    type Vertex = SemanticVertex;

    fn resolve_starting_vertices(
        &self,
        edge_name: &str,
        _parameters: &EdgeParameters,
    ) -> AsyncNeighborStream<'view, Self::Vertex> {
        if edge_name != "Entities" {
            return Box::pin(stream::empty());
        }
        Box::pin(stream::iter(
            self.image
                .canonical_entities()
                .map(|entity| SemanticVertex::Entity(entity.id)),
        ))
    }

    fn resolve_property<Vertex: AsVertex<Self::Vertex> + 'view>(
        &self,
        contexts: AsyncContextStream<'view, Vertex>,
        _type_name: &str,
        property_name: &str,
    ) -> AsyncContextOutcomeStream<'view, Vertex, FieldValue> {
        let property = SemanticProperty::from_name(property_name);
        async_helpers::resolve_property_with(contexts, move |vertex| match property {
            SemanticProperty::SourceHigh => match vertex {
                SemanticVertex::Entity(entity) => {
                    FieldValue::Int64(i64::from(entity_high(*entity)))
                }
                SemanticVertex::Link { .. } => FieldValue::Null,
            },
            SemanticProperty::SourceLow => match vertex {
                SemanticVertex::Entity(entity) => FieldValue::Int64(i64::from(entity_low(*entity))),
                SemanticVertex::Link { .. } => FieldValue::Null,
            },
            SemanticProperty::EntityHigh => match vertex {
                SemanticVertex::Link { entity, .. } => {
                    FieldValue::Int64(i64::from(entity_high(*entity)))
                }
                SemanticVertex::Entity(_) => FieldValue::Null,
            },
            SemanticProperty::EntityLow => match vertex {
                SemanticVertex::Link { entity, .. } => {
                    FieldValue::Int64(i64::from(entity_low(*entity)))
                }
                SemanticVertex::Entity(_) => FieldValue::Null,
            },
            SemanticProperty::Kind => match vertex {
                SemanticVertex::Link { kind, .. } => {
                    FieldValue::Int64(i64::from(link_kind_code(*kind)))
                }
                SemanticVertex::Entity(_) => FieldValue::Null,
            },
            SemanticProperty::Confidence => match vertex {
                SemanticVertex::Link { confidence, .. } => {
                    FieldValue::Int64(i64::from(confidence_code(*confidence)))
                }
                SemanticVertex::Entity(_) => FieldValue::Null,
            },
            SemanticProperty::Unknown => FieldValue::Null,
        })
    }

    fn resolve_neighbors<Vertex: AsVertex<Self::Vertex> + 'view>(
        &self,
        contexts: AsyncContextStream<'view, Vertex>,
        _type_name: &str,
        edge_name: &str,
        _parameters: &EdgeParameters,
    ) -> AsyncContextOutcomeStream<'view, Vertex, AsyncNeighborStream<'view, Self::Vertex>> {
        if edge_name != "outgoing" {
            return async_helpers::resolve_neighbors_with(contexts, |_| Box::pin(stream::empty()));
        }
        let image = self.image;
        async_helpers::resolve_neighbors_with(contexts, move |vertex| match *vertex {
            SemanticVertex::Entity(source) => Box::pin(stream::iter(
                image.links_from(source).filter_map(|(_, link)| {
                    let LinkTarget::Local(entity) = link.target else {
                        return None;
                    };
                    Some(SemanticVertex::Link {
                        entity,
                        kind: link.kind,
                        confidence: link.confidence,
                    })
                }),
            )),
            SemanticVertex::Link { .. } => Box::pin(stream::empty()),
        })
    }

    fn resolve_coercion<Vertex: AsVertex<Self::Vertex> + 'view>(
        &self,
        contexts: AsyncContextStream<'view, Vertex>,
        _type_name: &str,
        _coerce_to_type: &str,
    ) -> AsyncContextOutcomeStream<'view, Vertex, bool> {
        async_helpers::resolve_coercion_with(contexts, |_| false)
    }
}

fn source_arguments(source: EntityId) -> BTreeMap<Arc<str>, FieldValue> {
    BTreeMap::from([
        (
            Arc::<str>::from("sourceHigh"),
            FieldValue::Int64(i64::from(entity_high(source))),
        ),
        (
            Arc::<str>::from("sourceLow"),
            FieldValue::Int64(i64::from(entity_low(source))),
        ),
    ])
}

fn decode_hit(
    row: &BTreeMap<Arc<str>, FieldValue>,
    authority: GraphAuthority,
) -> Result<TrustfallHit, TrustfallGraphError> {
    let high = output_half(row, "entityHigh", TrustfallOutputField::EntityHigh)?;
    let low = output_half(row, "entityLow", TrustfallOutputField::EntityLow)?;
    let partition = output_half(row, "partition", TrustfallOutputField::Partition)?;
    Ok(TrustfallHit {
        authority,
        partition: PartitionId::new(partition),
        entity: EntityId::new((u32::from(high) << ENTITY_HALF_BITS) | u32::from(low)),
    })
}

fn decode_semantic_hit(
    row: &BTreeMap<Arc<str>, FieldValue>,
) -> Result<SemanticTrustfallHit, TrustfallGraphError> {
    let high = output_half(row, "entityHigh", TrustfallOutputField::EntityHigh)?;
    let low = output_half(row, "entityLow", TrustfallOutputField::EntityLow)?;
    let kind = output_half(row, "kind", TrustfallOutputField::LinkKind)?;
    let confidence = output_half(row, "confidence", TrustfallOutputField::Confidence)?;
    Ok(SemanticTrustfallHit {
        entity: EntityId::new((u32::from(high) << ENTITY_HALF_BITS) | u32::from(low)),
        kind: decode_link_kind(kind)?,
        confidence: decode_confidence(confidence)?,
    })
}

const fn link_kind_code(kind: LinkKind) -> u16 {
    match kind {
        LinkKind::Calls => 0,
        LinkKind::MethodCall => 1,
        LinkKind::TypeReference => 2,
        LinkKind::Reads => 3,
        LinkKind::Writes => 4,
        LinkKind::Imports => 5,
        LinkKind::Implements => 6,
        LinkKind::Overrides => 7,
        LinkKind::Reexports => 8,
        LinkKind::Inherits => 9,
        LinkKind::Documents => 10,
    }
}

fn decode_link_kind(observed: u16) -> Result<LinkKind, TrustfallGraphError> {
    match observed {
        0 => Ok(LinkKind::Calls),
        1 => Ok(LinkKind::MethodCall),
        2 => Ok(LinkKind::TypeReference),
        3 => Ok(LinkKind::Reads),
        4 => Ok(LinkKind::Writes),
        5 => Ok(LinkKind::Imports),
        6 => Ok(LinkKind::Implements),
        7 => Ok(LinkKind::Overrides),
        8 => Ok(LinkKind::Reexports),
        9 => Ok(LinkKind::Inherits),
        10 => Ok(LinkKind::Documents),
        _ => Err(TrustfallGraphError::InvalidOutputField {
            field: TrustfallOutputField::LinkKind,
            cause: TrustfallOutputCause::UnknownCode { observed },
        }),
    }
}

const fn confidence_code(confidence: Confidence) -> u16 {
    match confidence {
        Confidence::Syntactic => 0,
        Confidence::Heuristic => 1,
        Confidence::Indexed => 2,
        Confidence::Imported => 3,
        Confidence::Compiler => 4,
    }
}

fn decode_confidence(observed: u16) -> Result<Confidence, TrustfallGraphError> {
    match observed {
        0 => Ok(Confidence::Syntactic),
        1 => Ok(Confidence::Heuristic),
        2 => Ok(Confidence::Indexed),
        3 => Ok(Confidence::Imported),
        4 => Ok(Confidence::Compiler),
        _ => Err(TrustfallGraphError::InvalidOutputField {
            field: TrustfallOutputField::Confidence,
            cause: TrustfallOutputCause::UnknownCode { observed },
        }),
    }
}

fn execution_error(cause: ExecutionError<Infallible>) -> TrustfallGraphError {
    match cause {
        ExecutionError::Adapter(never) => match never {},
        _ => TrustfallGraphError::ExecutionRejected,
    }
}

fn output_half(
    row: &BTreeMap<Arc<str>, FieldValue>,
    name: &str,
    field: TrustfallOutputField,
) -> Result<u16, TrustfallGraphError> {
    let Some(value) = row.get(name) else {
        return Err(TrustfallGraphError::InvalidOutputField {
            field,
            cause: TrustfallOutputCause::Missing,
        });
    };
    match value {
        FieldValue::Int64(observed) => {
            u16::try_from(*observed).map_err(|_| TrustfallGraphError::InvalidOutputField {
                field,
                cause: TrustfallOutputCause::OutOfRange {
                    observed: TrustfallOutputNumber::Signed(*observed),
                },
            })
        }
        FieldValue::Uint64(observed) => {
            u16::try_from(*observed).map_err(|_| TrustfallGraphError::InvalidOutputField {
                field,
                cause: TrustfallOutputCause::OutOfRange {
                    observed: TrustfallOutputNumber::Unsigned(*observed),
                },
            })
        }
        _ => Err(TrustfallGraphError::InvalidOutputField {
            field,
            cause: TrustfallOutputCause::NonInteger,
        }),
    }
}

fn entity_high(entity: EntityId) -> u16 {
    let [first, second, _, _] = entity.raw.to_be_bytes();
    u16::from_be_bytes([first, second])
}

fn entity_low(entity: EntityId) -> u16 {
    let [_, _, third, fourth] = entity.raw.to_be_bytes();
    u16::from_be_bytes([third, fourth])
}

fn insert_sorted(
    output: &mut [Option<TrustfallHit>],
    written: &mut usize,
    candidate: TrustfallHit,
) {
    let mut insertion = *written;
    for (index, hit) in output.iter().copied().take(*written).enumerate() {
        if hit.is_some_and(|current| candidate_precedes(candidate, current)) {
            insertion = index;
            break;
        }
    }
    for index in (insertion + 1..=*written).rev() {
        output[index] = output[index - 1];
    }
    output[insertion] = Some(candidate);
    *written += 1;
}

fn candidate_precedes(left: TrustfallHit, right: TrustfallHit) -> bool {
    (left.entity.raw, left.partition.raw) < (right.entity.raw, right.partition.raw)
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;
    use std::hint::black_box;

    use allocation_counter::{AllocationInfo, measure};
    use compiler_ir::{
        BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts,
        EntityVersion, FactAvailability, IrBuilder, ItemKind, LinkKind, OccurrenceAuthorityFacts,
        ParentageAuthority, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
        VariantFingerprint, Visibility,
    };
    use server_index_graph_vector::{GraphEdge, GraphRow, ProjectionId};
    use server_index_vocabulary::IndexSnapshotId;
    use trustfall::provider::check_adapter_invariants;

    use super::*;

    #[test]
    fn borrowed_adapter_obeys_trustfall_resolution_invariants() {
        let authority = GraphAuthority::new(
            IndexSnapshotId::from_canonical_bytes(b"trustfall-invariant-snapshot"),
            ProjectionId::new(1),
        );
        let partition = PartitionId::new(1);
        let edges = [GraphEdge::new(
            authority,
            partition,
            EntityId::new(1),
            EntityId::new(2),
        )];
        let rows = [GraphRow {
            partition,
            edges: &edges,
        }];
        let view = ValidatedGraphView::try_new(authority, &rows).expect("valid graph facts");
        let schema = Schema::parse(GRAPH_SCHEMA).expect("fixed graph schema");

        check_adapter_invariants(
            &schema,
            BorrowedGraphAdapter {
                view: &view,
                schema: &schema,
            },
        );
    }

    #[test]
    fn canonical_ir_fast_path_reads_csr_without_graph_projection() {
        let versions = [
            EntityVersion {
                family: DeclarationFamilyId::from_raw([1; 16]),
                variant: VariantFingerprint::from_raw([2; 16]),
                core_payload: CorePayloadHash::from_raw([3; 16]),
            },
            EntityVersion {
                family: DeclarationFamilyId::from_raw([4; 16]),
                variant: VariantFingerprint::from_raw([5; 16]),
                core_payload: CorePayloadHash::from_raw([6; 16]),
            },
        ];
        let items = [b"source".as_slice(), b"target".as_slice()].map(|name| TreeItemInput {
            name,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                visibility: FactAvailability::Captured,
                members: FactAvailability::Captured,
                documentation: FactAvailability::Captured,
                attributes: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        });
        let links = [TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts::default(),
            source: None,
        }];
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &links,
            })
            .expect("valid borrowed tree");
        let ir = builder.finish().expect("valid canonical IR");
        let mut output = [None; 1];
        let graph = IrTrustfallGraph::new(&ir);
        let mut result = None;
        let allocations = measure(|| {
            result = Some(black_box(graph.neighbors(EntityId::new(0), &mut output)));
        });
        assert_eq!(allocations, AllocationInfo::default());
        let written = result
            .expect("measurement executed")
            .expect("direct neighbors");
        assert_eq!(written, 1);
        assert_eq!(output[0].map(|hit| hit.entity), Some(EntityId::new(1)));
    }

    #[test]
    fn cached_schema_rejection_remains_typed_after_a_later_valid_grammar() {
        let cache = OnceLock::new();
        let diagnostic = TrustfallUpstreamDiagnostic::Schema(TrustfallSchemaDiagnostic::Syntax);
        assert!(cache.set(Err(diagnostic)).is_ok());

        let observed = cached_schema(&cache, GRAPH_SCHEMA);
        assert!(matches!(observed, Err(cause) if cause == diagnostic));
    }

    #[test]
    fn borrowed_adapter_retains_no_dynamic_abi_state_between_calls() {
        assert_eq!(
            size_of::<TrustfallGraph<'static>>(),
            size_of::<&ValidatedGraphView<'static>>()
        );
    }

    #[test]
    fn missing_output_field_retains_its_closed_cause() {
        let row = BTreeMap::new();
        let result = output_half(&row, "partition", TrustfallOutputField::Partition);
        assert!(matches!(
            result,
            Err(TrustfallGraphError::InvalidOutputField {
                field: TrustfallOutputField::Partition,
                cause: TrustfallOutputCause::Missing,
            })
        ));
    }

    #[test]
    fn non_integer_output_field_retains_its_closed_cause() {
        let row = BTreeMap::from([(Arc::<str>::from("partition"), FieldValue::Boolean(true))]);
        let result = output_half(&row, "partition", TrustfallOutputField::Partition);
        assert!(matches!(
            result,
            Err(TrustfallGraphError::InvalidOutputField {
                field: TrustfallOutputField::Partition,
                cause: TrustfallOutputCause::NonInteger,
            })
        ));
    }

    #[test]
    fn out_of_range_output_field_retains_its_exact_integer() {
        let row = BTreeMap::from([(Arc::<str>::from("partition"), FieldValue::Int64(-1))]);
        let result = output_half(&row, "partition", TrustfallOutputField::Partition);
        assert!(matches!(
            result,
            Err(TrustfallGraphError::InvalidOutputField {
                field: TrustfallOutputField::Partition,
                cause: TrustfallOutputCause::OutOfRange {
                    observed: TrustfallOutputNumber::Signed(-1),
                },
            })
        ));
    }
}
