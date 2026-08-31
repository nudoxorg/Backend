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
    frontend::parse, interpreter::execution::interpret_ir, ir::IndexedQuery,
};

use crate::schema::{GRAPH_SCHEMA, NEIGHBORS_QUERY};

const ENTITY_HALF_BITS: u32 = 16;
static PARSED_GRAPH_SCHEMA: OnceLock<Option<Schema>> = OnceLock::new();
static PARSED_NEIGHBORS_QUERY: OnceLock<Option<Arc<IndexedQuery>>> = OnceLock::new();

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

/// Fixed Trustfall stage whose library error type is intentionally private upstream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustfallStaticPhase {
    /// Parsing the crate-owned static schema.
    Schema,
    /// Parsing the crate-owned fixed query operation.
    Query,
    /// Binding crate-owned typed query variables.
    Arguments,
}

/// Exact failure while executing the fixed synchronous Trustfall operation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TrustfallGraphError {
    /// Trustfall rejected one crate-owned static grammar stage.
    ///
    /// Trustfall 0.8 keeps these concrete error types private, so no dynamic or stringly source is
    /// admitted into the public terminal. The stage is closed and the grammar is covered by the
    /// adapter-invariant test.
    #[error("Trustfall rejected the crate-owned static {phase:?} grammar")]
    StaticGrammar {
        /// Closed static grammar stage rejected by Trustfall.
        phase: TrustfallStaticPhase,
    },
    /// The caller's output cannot preserve every result from the immutable graph view.
    #[error("Trustfall graph output has {available} slots, required {required}")]
    InsufficientOutput {
        /// Complete graph fact count required by the selected source.
        required: usize,
        /// Caller-provided output capacity.
        available: usize,
    },
    /// A fixed Trustfall output field was absent or outside its declared integer grammar.
    #[error("Trustfall graph output field {field:?} was not an unsigned sixteen-bit integer")]
    InvalidOutputField {
        /// Declared field whose observed dynamic value was rejected.
        field: TrustfallOutputField,
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
            Err(_rejected) => {
                return Err(TrustfallGraphError::StaticGrammar {
                    phase: TrustfallStaticPhase::Arguments,
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
    PARSED_GRAPH_SCHEMA
        .get_or_init(|| Schema::parse(GRAPH_SCHEMA).ok())
        .as_ref()
        .ok_or(TrustfallGraphError::StaticGrammar {
            phase: TrustfallStaticPhase::Schema,
        })
}

fn parsed_query(schema: &Schema) -> Result<Arc<IndexedQuery>, TrustfallGraphError> {
    PARSED_NEIGHBORS_QUERY
        .get_or_init(|| parse(schema, NEIGHBORS_QUERY).ok())
        .as_ref()
        .map(Arc::clone)
        .ok_or(TrustfallGraphError::StaticGrammar {
            phase: TrustfallStaticPhase::Query,
        })
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
    row.get(name)
        .and_then(FieldValue::as_i64)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or(TrustfallGraphError::InvalidOutputField { field })
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
}
