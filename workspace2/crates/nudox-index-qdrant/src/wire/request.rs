//! Typed Qdrant request records and borrowed serialization.

use std::fmt;

use arrayvec::ArrayVec;
use serde::{
    Serialize, Serializer,
    ser::{SerializeSeq, SerializeStruct},
};

use super::super::{
    admission::PreparedIdentity,
    contract::{
        ENTITY_PAYLOAD_KEY, METRIC_PAYLOAD_KEY, MODEL_PAYLOAD_KEY, PARTITION_PAYLOAD_KEY,
        PayloadField, PayloadIndexKind, QdrantDataKey, SEGMENT_PAYLOAD_KEY, SNAPSHOT_PAYLOAD_KEY,
    },
    limits::{MAX_BATCH_POINTS, MAX_QUERY_SEGMENTS, QUERY_SCAN_LIMIT},
};
use nudox_index_graph_vector::{
    Metric as VectorMetric, ModelId, VectorAuthority, VectorSegmentDescriptor,
};
use nudox_index_vocab::{IndexSnapshotId, VectorSegmentId};

/// Qdrant's closed collection-level distance vocabulary.
#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
pub(crate) enum CollectionMetric {
    /// Squared Euclidean distance.
    Euclid,
    /// Dot-product similarity.
    Dot,
}

impl From<VectorMetric> for CollectionMetric {
    fn from(metric: VectorMetric) -> Self {
        match metric {
            VectorMetric::SquaredEuclidean => Self::Euclid,
            VectorMetric::NegativeDotProduct => Self::Dot,
        }
    }
}

impl From<CollectionMetric> for VectorMetric {
    fn from(metric: CollectionMetric) -> Self {
        match metric {
            CollectionMetric::Euclid => Self::SquaredEuclidean,
            CollectionMetric::Dot => Self::NegativeDotProduct,
        }
    }
}

/// Stable metric name stored in every immutable point payload.
#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum PayloadMetric {
    SquaredEuclidean,
    NegativeDotProduct,
}

impl From<VectorMetric> for PayloadMetric {
    fn from(metric: VectorMetric) -> Self {
        match metric {
            VectorMetric::SquaredEuclidean => Self::SquaredEuclidean,
            VectorMetric::NegativeDotProduct => Self::NegativeDotProduct,
        }
    }
}

/// The complete descriptor needed to create and verify one payload index.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PayloadIndexDescriptor {
    pub(crate) field: PayloadField,
    pub(crate) wire_name: &'static str,
    pub(crate) schema: PayloadIndexKind,
}

impl PayloadIndexDescriptor {
    pub(crate) const fn new(
        field: PayloadField,
        wire_name: &'static str,
        schema: PayloadIndexKind,
    ) -> Self {
        Self {
            field,
            wire_name,
            schema,
        }
    }
}

/// Typed request body for collection creation.
#[derive(Serialize)]
pub(crate) struct CollectionRequest {
    vectors: VectorConfig,
    replication_factor: u8,
    write_consistency_factor: u8,
}

#[derive(Serialize)]
struct VectorConfig {
    size: u16,
    distance: CollectionMetric,
}

impl CollectionRequest {
    pub(crate) fn new(authority: VectorAuthority) -> Self {
        Self {
            vectors: VectorConfig {
                size: authority.dimension,
                distance: authority.metric.into(),
            },
            replication_factor: 1,
            write_consistency_factor: 1,
        }
    }
}

/// Typed request body for payload-index creation.
#[derive(Serialize)]
pub(crate) struct PayloadIndexRequest {
    field_name: PayloadField,
    field_schema: PayloadIndexKind,
}

impl PayloadIndexRequest {
    pub(crate) const fn new(field: PayloadField, schema: PayloadIndexKind) -> Self {
        Self {
            field_name: field,
            field_schema: schema,
        }
    }
}

/// Typed request body for a point upsert.
#[derive(Serialize)]
pub(crate) struct UpsertRequest<'coordinates> {
    points: ArrayVec<UpsertPoint<'coordinates>, MAX_BATCH_POINTS>,
}

#[derive(Serialize)]
struct UpsertPoint<'coordinates> {
    id: u64,
    vector: Coordinates<'coordinates>,
    payload: IdentityPayload,
}

impl<'coordinates> UpsertRequest<'coordinates> {
    pub(crate) fn from_points(
        points: &[super::super::admission::PreparedPoint<'coordinates>],
    ) -> Self {
        Self {
            points: points
                .iter()
                .map(|point| UpsertPoint {
                    id: point.physical_id.0,
                    vector: Coordinates(point.coordinates),
                    payload: IdentityPayload::from_key(point.key),
                })
                .collect(),
        }
    }
}

/// Typed request body for retrieve operations.
#[derive(Serialize)]
pub(crate) struct RetrieveRequest {
    ids: ArrayVec<u64, MAX_BATCH_POINTS>,
    with_payload: bool,
    with_vector: bool,
}

impl RetrieveRequest {
    pub(crate) fn new(keys: &[impl PreparedIdentity], with_vector: bool) -> Self {
        Self {
            ids: keys.iter().map(|key| key.physical_id().0).collect(),
            with_payload: true,
            with_vector,
        }
    }
}

/// Typed request body for delete operations.
#[derive(Serialize)]
pub(crate) struct DeleteRequest {
    points: ArrayVec<u64, MAX_BATCH_POINTS>,
}

impl DeleteRequest {
    pub(crate) fn new(keys: &[impl PreparedIdentity]) -> Self {
        Self {
            points: keys.iter().map(|key| key.physical_id().0).collect(),
        }
    }
}

/// Typed request body for exact vector queries.
#[derive(Serialize)]
pub(crate) struct QueryRequest<'coordinates> {
    query: Coordinates<'coordinates>,
    limit: usize,
    with_payload: bool,
    with_vector: bool,
    params: ExactParams,
    filter: IdentityFilter,
}

#[derive(Serialize)]
struct ExactParams {
    exact: bool,
}

impl<'coordinates> QueryRequest<'coordinates> {
    pub(crate) fn new(
        authority: VectorAuthority,
        selected: &[VectorSegmentDescriptor],
        coordinates: &'coordinates [i16],
    ) -> Self {
        Self {
            query: Coordinates(coordinates),
            limit: QUERY_SCAN_LIMIT,
            with_payload: true,
            with_vector: true,
            params: ExactParams { exact: true },
            filter: IdentityFilter::new(authority, selected),
        }
    }
}

pub(crate) struct IdentityPayload {
    key: QdrantDataKey,
}

impl IdentityPayload {
    pub(crate) const fn from_key(key: QdrantDataKey) -> Self {
        Self { key }
    }
}

impl Serialize for IdentityPayload {
    fn serialize<Output>(&self, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        let mut payload = serializer.serialize_struct("IdentityPayload", 6)?;
        payload.serialize_field(
            SNAPSHOT_PAYLOAD_KEY,
            &Hex(self.key.authority.snapshot.as_ref()),
        )?;
        payload.serialize_field(MODEL_PAYLOAD_KEY, &Hex(self.key.authority.model.as_ref()))?;
        payload.serialize_field(SEGMENT_PAYLOAD_KEY, &Hex(self.key.segment.as_ref()))?;
        payload.serialize_field(
            METRIC_PAYLOAD_KEY,
            &PayloadMetric::from(self.key.authority.metric),
        )?;
        payload.serialize_field(PARTITION_PAYLOAD_KEY, &self.key.partition.raw)?;
        payload.serialize_field(ENTITY_PAYLOAD_KEY, &self.key.entity.raw)?;
        payload.end()
    }
}

#[derive(Serialize)]
pub(crate) struct IdentityFilter {
    must: ArrayVec<MatchCondition, 4>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_should: Option<MinimumShould>,
}

#[derive(Serialize)]
struct MinimumShould {
    conditions: ArrayVec<MatchCondition, MAX_QUERY_SEGMENTS>,
    min_count: u8,
}

#[derive(Serialize)]
struct MatchCondition {
    key: PayloadField,
    r#match: MatchValue,
}

#[derive(Serialize)]
struct MatchValue {
    value: MatchScalar,
}

enum MatchScalar {
    Snapshot(IndexSnapshotId),
    Model(ModelId),
    Metric(PayloadMetric),
    Segment(VectorSegmentId),
}

impl IdentityFilter {
    pub(crate) fn new(authority: VectorAuthority, selected: &[VectorSegmentDescriptor]) -> Self {
        let mut must = ArrayVec::new();
        must.push(MatchCondition::new(
            PayloadField::Snapshot,
            MatchScalar::Snapshot(authority.snapshot),
        ));
        must.push(MatchCondition::new(
            PayloadField::Model,
            MatchScalar::Model(authority.model),
        ));
        must.push(MatchCondition::new(
            PayloadField::Metric,
            MatchScalar::Metric(authority.metric.into()),
        ));
        let min_should = match selected {
            [segment] => {
                must.push(MatchCondition::segment(segment.id));
                None
            }
            [] => None,
            _ => Some(MinimumShould {
                conditions: selected
                    .iter()
                    .copied()
                    .map(|segment| MatchCondition::segment(segment.id))
                    .collect(),
                min_count: 1,
            }),
        };
        Self { must, min_should }
    }
}

impl MatchCondition {
    const fn new(key: PayloadField, value: MatchScalar) -> Self {
        Self {
            key,
            r#match: MatchValue { value },
        }
    }

    fn segment(segment: VectorSegmentId) -> Self {
        Self {
            key: PayloadField::Segment,
            r#match: MatchValue {
                value: MatchScalar::Segment(segment),
            },
        }
    }
}

impl Serialize for MatchScalar {
    fn serialize<Output>(&self, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        match self {
            Self::Snapshot(snapshot) => serializer.collect_str(&Hex(snapshot.as_ref())),
            Self::Model(model) => serializer.collect_str(&Hex(model.as_ref())),
            Self::Metric(metric) => metric.serialize(serializer),
            Self::Segment(segment) => serializer.collect_str(&Hex(segment.as_ref())),
        }
    }
}

#[derive(Clone, Copy)]
struct Coordinates<'coordinates>(&'coordinates [i16]);

impl Serialize for Coordinates<'_> {
    fn serialize<Output>(&self, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for coordinate in self.0 {
            sequence.serialize_element(&f64::from(*coordinate))?;
        }
        sequence.end()
    }
}

struct Hex<'bytes>(&'bytes [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for Hex<'_> {
    fn serialize<Output>(&self, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        serializer.collect_str(self)
    }
}
