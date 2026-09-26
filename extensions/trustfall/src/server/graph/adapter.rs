//! Resolves borrowed graph and semantic-image vertices for Trustfall.

use super::*;

/// One Trustfall vertex in the borrowed graph projection.
#[derive(Clone, Copy, Debug)]
pub(super) enum GraphVertex {
    /// A source entity that starts neighbor resolution.
    Entity(EntityId),
    /// One outgoing neighbor of a source entity.
    Neighbor {
        /// Canonical neighboring entity.
        entity: EntityId,
        /// Partition that supplied this edge.
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

/// In-memory Trustfall adapter over one borrowed graph view.
#[derive(Debug)]
pub(super) struct BorrowedGraphAdapter<'view, 'schema> {
    view: &'view ValidatedGraphView<'view>,
    schema: &'schema Schema,
}

impl<'view, 'schema> BorrowedGraphAdapter<'view, 'schema> {
    /// Borrows one validated graph view and the process-owned schema.
    pub(super) const fn new(
        view: &'view ValidatedGraphView<'view>,
        schema: &'schema Schema,
    ) -> Self {
        Self { view, schema }
    }
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

/// One Trustfall vertex in the borrowed semantic-image projection.
#[derive(Clone, Copy, Debug)]
pub(super) enum SemanticVertex {
    /// A canonical entity that starts outgoing-link resolution.
    Entity(EntityId),
    /// One local outgoing link from a source entity.
    Link {
        /// Canonical neighboring entity.
        entity: EntityId,
        /// Semantic relation represented by the edge.
        kind: LinkKind,
        /// Compiler evidence retained for this link.
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

/// In-memory Trustfall adapter over one borrowed semantic image.
pub(super) struct BorrowedSemanticAdapter<'view, 'bytes> {
    image: &'view SemanticImageView<'bytes>,
}

impl<'view, 'bytes> BorrowedSemanticAdapter<'view, 'bytes> {
    /// Borrows one validated semantic image.
    pub(super) const fn new(image: &'view SemanticImageView<'bytes>) -> Self {
        Self { image }
    }
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
