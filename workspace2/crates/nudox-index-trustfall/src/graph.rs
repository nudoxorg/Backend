//! Borrowed graph adapter and typed Trustfall result boundary.

use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

use nudox_index_graph_vector::{GraphAuthority, PartitionId, ValidatedGraphView};
use nudox_ir_vocab::EntityId;
use thiserror::Error;
use trustfall::{
    FieldValue, Schema,
    provider::{
        Adapter, AsVertex, ContextIterator, ContextOutcomeIterator, EdgeParameters,
        ResolveEdgeInfo, ResolveInfo, Typename, VertexIterator, resolve_coercion_with,
        resolve_neighbors_with, resolve_property_with, resolve_typename,
    },
};
use trustfall_core::{
    frontend::{error::FrontendError, parse},
    interpreter::{error::QueryArgumentsError, execution::interpret_ir},
    ir::IndexedQuery,
    schema::error::InvalidSchemaError,
};

use crate::schema::{GRAPH_SCHEMA, NEIGHBORS_QUERY};

const ENTITY_HALF_BITS: u32 = 16;
static PARSED_GRAPH_SCHEMA: OnceLock<Result<Schema, TrustfallUpstreamDiagnostic>> = OnceLock::new();
static PARSED_NEIGHBORS_QUERY: OnceLock<Result<Arc<IndexedQuery>, TrustfallUpstreamDiagnostic>> =
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

    use nudox_index_graph_vector::{GraphEdge, GraphRow, ProjectionId};
    use nudox_index_vocab::IndexSnapshotId;
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
