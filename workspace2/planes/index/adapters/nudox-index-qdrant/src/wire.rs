//! Qdrant's JSON wire contract, isolated from adapter admission and transport policy.

use std::fmt;

use arrayvec::ArrayVec;
use serde::{
    Deserialize, Serialize, Serializer,
    ser::{SerializeSeq, SerializeStruct},
};

use super::{
    CollectionField, ENTITY_PAYLOAD_KEY, METRIC_PAYLOAD_KEY, MODEL_PAYLOAD_KEY,
    MalformedResponseCause, ModelId, PARTITION_PAYLOAD_KEY, PayloadField, PayloadMismatchCause,
    PhysicalPointId, PreparedIdentity, PreparedPoint, QdrantDataKey, QdrantError, QdrantHit,
    RequestPhase, SEGMENT_PAYLOAD_KEY, SNAPSHOT_PAYLOAD_KEY, VectorAuthority, projected_score,
};
use nudox_index_graph_vector::{Metric as VectorMetric, PartitionId};
use nudox_index_vocab::{IndexSnapshotId, VectorSegmentId};
use nudox_ir_vocab::EntityId;

/// Decodes one JSON response into its endpoint-specific DTO.
pub(super) fn decode<'body, T: Deserialize<'body>>(
    phase: RequestPhase,
    body: &'body str,
) -> Result<T, QdrantError> {
    serde_json::from_str(body).map_err(|source| QdrantError::Decode { phase, source })
}

/// Verifies collection create/delete's boolean acknowledgement contract.
pub(super) fn parse_boolean_ack(phase: RequestPhase, body: &str) -> Result<(), QdrantError> {
    if decode::<BooleanAcknowledgement>(phase, body)?.result {
        Ok(())
    } else {
        Err(QdrantError::MalformedResponse {
            phase,
            cause: MalformedResponseCause::RejectedAcknowledgement,
        })
    }
}

/// Verifies a wait-bound point or payload-index operation reached `completed`.
pub(super) fn parse_completed_ack(phase: RequestPhase, body: &str) -> Result<(), QdrantError> {
    if decode::<OperationAcknowledgement>(phase, body)?
        .result
        .status
        == OperationStatus::Completed
    {
        Ok(())
    } else {
        Err(QdrantError::MalformedResponse {
            phase,
            cause: MalformedResponseCause::RejectedAcknowledgement,
        })
    }
}

/// The collection metadata that participates in this adapter's contract.
pub(super) struct CollectionMetadata {
    pub(super) dimension: u64,
    pub(super) metric: CollectionMetric,
    pub(super) replication_factor: u64,
    pub(super) write_consistency_factor: u64,
}

/// Decodes the typed collection configuration response.
pub(super) fn collection_metadata(
    phase: RequestPhase,
    body: &str,
) -> Result<CollectionMetadata, QdrantError> {
    let response: CollectionResponse = decode(phase, body)?;
    let params = response.result.config.params;
    Ok(CollectionMetadata {
        dimension: params.vectors.size,
        metric: params.vectors.distance,
        replication_factor: params.replication_factor,
        write_consistency_factor: params.write_consistency_factor,
    })
}

/// Verifies the required typed payload index schemas.
pub(super) fn verify_payload_indexes(
    phase: RequestPhase,
    body: &str,
    expected: &[(PayloadField, PayloadDataType)],
) -> Result<(), QdrantError> {
    let response: CollectionResponse = decode(phase, body)?;
    for (field, expected_type) in expected {
        let name = payload_field_name(*field);
        let observed = response
            .result
            .payload_schema
            .get(name)
            .map(|entry| entry.data_type);
        if observed != Some(*expected_type) {
            return Err(QdrantError::CollectionMismatch {
                phase,
                field: CollectionField::PayloadIndex(*field),
            });
        }
    }
    Ok(())
}

/// Decodes bounded retrieve results.
pub(super) fn retrieve_points(
    phase: RequestPhase,
    body: &str,
) -> Result<Vec<WirePoint<'_>>, QdrantError> {
    Ok(decode::<RetrieveResponse>(phase, body)?.result)
}

/// Parses, proves, and locally reranks one query response.
pub(super) fn parse_query_hits(
    authority: VectorAuthority,
    selected: &[PartitionId],
    query_coordinates: &[i16],
    body: &str,
) -> Result<Vec<QdrantHit>, QdrantError> {
    let phase = RequestPhase::QueryPoints;
    let points = decode::<QueryResponse>(phase, body)?.result.points;
    if points.len() > super::MAX_BATCH_POINTS {
        return Err(QdrantError::ProjectionCapacity {
            maximum: super::MAX_BATCH_POINTS,
            observed: points.len(),
        });
    }
    let mut hits = Vec::with_capacity(points.len());
    for point in &points {
        let physical_id = PhysicalPointId(point.id);
        let key = decode_identity(point, phase, authority.dimension())?;
        validate_query_scope(phase, physical_id, key, authority, selected)?;
        reject_remote_physical_identity(phase, physical_id, key)?;
        if let Some(first) = hits.iter().find(|hit: &&QdrantHit| {
            hit.authority == key.authority
                && hit.partition == key.partition
                && hit.entity == key.entity
        }) {
            return Err(QdrantError::DuplicateSemanticPoint {
                phase,
                key: key.into(),
                first_physical_id: first.physical_id,
                second_physical_id: physical_id,
            });
        }
        if hits
            .iter()
            .any(|hit: &QdrantHit| hit.physical_id == physical_id)
        {
            return Err(QdrantError::MalformedResponse {
                phase,
                cause: MalformedResponseCause::PhysicalIdAliased,
            });
        }
        let coordinates = decode_vector(
            point,
            phase,
            physical_id,
            usize::from(authority.dimension()),
        )?;
        hits.push(QdrantHit {
            authority,
            segment: key.segment,
            partition: key.partition,
            entity: key.entity,
            score: projected_score(authority.metric(), query_coordinates, coordinates),
            physical_id,
        });
    }
    Ok(hits)
}

/// Decodes a point payload into its complete semantic identity.
pub(super) fn decode_identity(
    point: &WirePoint<'_>,
    phase: RequestPhase,
    dimension: u16,
) -> Result<QdrantDataKey, QdrantError> {
    let snapshot = decode_snapshot(point.payload.snapshot, phase)?;
    let model = decode_model(point.payload.model, phase)?;
    let segment = decode_segment(point.payload.segment, phase)?;
    let metric = decode_metric(point.payload.metric, phase)?;
    let partition = decode_partition(point.payload.partition, phase)?;
    let entity = decode_entity(point.payload.entity, phase)?;
    Ok(QdrantDataKey::new(
        VectorAuthority::new(snapshot, model, dimension, metric),
        segment,
        partition,
        entity,
    ))
}

/// Decodes a required point vector and checks its exact dimension.
pub(super) fn decode_vector<'point>(
    point: &'point WirePoint<'_>,
    phase: RequestPhase,
    physical_id: PhysicalPointId,
    expected_dimension: usize,
) -> Result<&'point [f64], QdrantError> {
    let Some(vector) = point.vector.as_ref() else {
        return Err(QdrantError::MalformedResponse {
            phase,
            cause: MalformedResponseCause::MissingVector,
        });
    };
    if vector.len() != expected_dimension {
        return Err(QdrantError::VectorMismatch { phase, physical_id });
    }
    Ok(vector)
}

/// Qdrant's closed collection-level distance vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum CollectionMetric {
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

/// Stable metric name stored in every immutable point payload.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

impl From<PayloadMetric> for VectorMetric {
    fn from(metric: PayloadMetric) -> Self {
        match metric {
            PayloadMetric::SquaredEuclidean => Self::SquaredEuclidean,
            PayloadMetric::NegativeDotProduct => Self::NegativeDotProduct,
        }
    }
}

/// Qdrant's payload-index schema vocabulary used by this adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum PayloadDataType {
    Keyword,
    Integer,
}

/// Typed request body for collection creation.
#[derive(Serialize)]
pub(super) struct CollectionRequest {
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
    pub(super) fn new(authority: VectorAuthority) -> Self {
        Self {
            vectors: VectorConfig {
                size: authority.dimension(),
                distance: authority.metric().into(),
            },
            replication_factor: 1,
            write_consistency_factor: 1,
        }
    }
}

/// Typed request body for payload-index creation.
#[derive(Serialize)]
pub(super) struct PayloadIndexRequest {
    field_name: &'static str,
    field_schema: PayloadDataType,
}

impl PayloadIndexRequest {
    pub(super) const fn new(field: PayloadField, schema: PayloadDataType) -> Self {
        Self {
            field_name: payload_field_name(field),
            field_schema: schema,
        }
    }
}

/// Typed request body for a point upsert.
#[derive(Serialize)]
pub(super) struct UpsertRequest<'coordinates> {
    points: ArrayVec<UpsertPoint<'coordinates>, { super::MAX_BATCH_POINTS }>,
}

#[derive(Serialize)]
struct UpsertPoint<'coordinates> {
    id: u64,
    vector: Coordinates<'coordinates>,
    payload: IdentityPayload,
}

impl<'coordinates> UpsertRequest<'coordinates> {
    pub(super) fn from_points(points: &[PreparedPoint<'coordinates>]) -> Self {
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
pub(super) struct RetrieveRequest {
    ids: ArrayVec<u64, { super::MAX_BATCH_POINTS }>,
    with_payload: bool,
    with_vector: bool,
}

impl RetrieveRequest {
    pub(super) fn new(keys: &[impl PreparedIdentity], with_vector: bool) -> Self {
        Self {
            ids: keys.iter().map(|key| key.physical_id().0).collect(),
            with_payload: true,
            with_vector,
        }
    }
}

/// Typed request body for delete operations.
#[derive(Serialize)]
pub(super) struct DeleteRequest {
    points: ArrayVec<u64, { super::MAX_BATCH_POINTS }>,
}

impl DeleteRequest {
    pub(super) fn new(keys: &[impl PreparedIdentity]) -> Self {
        Self {
            points: keys.iter().map(|key| key.physical_id().0).collect(),
        }
    }
}

/// Typed request body for exact vector queries.
#[derive(Serialize)]
pub(super) struct QueryRequest<'coordinates> {
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
    pub(super) fn new(
        authority: VectorAuthority,
        selected: &[PartitionId],
        coordinates: &'coordinates [i16],
    ) -> Self {
        Self {
            query: Coordinates(coordinates),
            limit: super::QUERY_SCAN_LIMIT,
            with_payload: true,
            with_vector: true,
            params: ExactParams { exact: true },
            filter: IdentityFilter::new(authority, selected),
        }
    }
}

pub(super) struct IdentityPayload {
    key: QdrantDataKey,
}

impl IdentityPayload {
    pub(super) const fn from_key(key: QdrantDataKey) -> Self {
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
            &Hex(self.key.authority.snapshot().as_ref()),
        )?;
        payload.serialize_field(MODEL_PAYLOAD_KEY, &Hex(self.key.authority.model().as_ref()))?;
        payload.serialize_field(SEGMENT_PAYLOAD_KEY, &Hex(self.key.segment.as_ref()))?;
        payload.serialize_field(
            METRIC_PAYLOAD_KEY,
            &PayloadMetric::from(self.key.authority.metric()),
        )?;
        payload.serialize_field(PARTITION_PAYLOAD_KEY, &self.key.partition.raw)?;
        payload.serialize_field(ENTITY_PAYLOAD_KEY, &self.key.entity.raw)?;
        payload.end()
    }
}

#[derive(Serialize)]
pub(super) struct IdentityFilter {
    must: ArrayVec<MatchCondition, 4>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_should: Option<MinimumShould>,
}

#[derive(Serialize)]
struct MinimumShould {
    conditions: ArrayVec<MatchCondition, { super::MAX_QUERY_PARTITIONS }>,
    min_count: u8,
}

#[derive(Serialize)]
struct MatchCondition {
    key: &'static str,
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
    Integer(u16),
}

impl IdentityFilter {
    pub(super) fn new(authority: VectorAuthority, selected: &[PartitionId]) -> Self {
        let mut must = ArrayVec::new();
        must.push(MatchCondition::new(
            SNAPSHOT_PAYLOAD_KEY,
            MatchScalar::Snapshot(authority.snapshot()),
        ));
        must.push(MatchCondition::new(
            MODEL_PAYLOAD_KEY,
            MatchScalar::Model(authority.model()),
        ));
        must.push(MatchCondition::new(
            METRIC_PAYLOAD_KEY,
            MatchScalar::Metric(authority.metric().into()),
        ));
        let min_should = match selected {
            [partition] => {
                must.push(MatchCondition::partition(*partition));
                None
            }
            [] => None,
            _ => Some(MinimumShould {
                conditions: selected
                    .iter()
                    .copied()
                    .map(MatchCondition::partition)
                    .collect(),
                min_count: 1,
            }),
        };
        Self { must, min_should }
    }
}

impl MatchCondition {
    const fn new(key: &'static str, value: MatchScalar) -> Self {
        Self {
            key,
            r#match: MatchValue { value },
        }
    }

    fn partition(partition: PartitionId) -> Self {
        Self {
            key: PARTITION_PAYLOAD_KEY,
            r#match: MatchValue {
                value: MatchScalar::Integer(partition.raw),
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
            Self::Integer(value) => value.serialize(serializer),
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

#[derive(Deserialize)]
struct BooleanAcknowledgement {
    result: bool,
}

#[derive(Deserialize)]
struct OperationAcknowledgement {
    result: OperationResult,
}

#[derive(Deserialize)]
struct OperationResult {
    status: OperationStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum OperationStatus {
    Acknowledged,
    Completed,
}

#[derive(Deserialize)]
struct CollectionResponse {
    result: CollectionResult,
}

#[derive(Deserialize)]
struct CollectionResult {
    config: CollectionConfig,
    #[serde(default)]
    payload_schema: std::collections::BTreeMap<String, PayloadSchema>,
}

#[derive(Deserialize)]
struct CollectionConfig {
    params: CollectionParameters,
}

#[derive(Deserialize)]
struct CollectionParameters {
    vectors: CollectionVectors,
    replication_factor: u64,
    write_consistency_factor: u64,
}

#[derive(Deserialize)]
struct CollectionVectors {
    size: u64,
    distance: CollectionMetric,
}

#[derive(Deserialize)]
struct PayloadSchema {
    data_type: PayloadDataType,
}

#[derive(Deserialize)]
struct RetrieveResponse<'body> {
    #[serde(borrow)]
    result: Vec<WirePoint<'body>>,
}

#[derive(Deserialize)]
struct QueryResponse<'body> {
    #[serde(borrow)]
    result: QueryResult<'body>,
}

#[derive(Deserialize)]
struct QueryResult<'body> {
    #[serde(borrow)]
    points: Vec<WirePoint<'body>>,
}

#[derive(Deserialize)]
pub(super) struct WirePoint<'body> {
    pub(super) id: u64,
    #[serde(borrow)]
    payload: WirePayload<'body>,
    #[serde(default)]
    vector: Option<Vec<f64>>,
}

#[derive(Deserialize)]
struct WirePayload<'body> {
    #[serde(rename = "nudox_snapshot")]
    snapshot: &'body str,
    #[serde(rename = "nudox_model")]
    model: &'body str,
    #[serde(rename = "nudox_segment")]
    segment: &'body str,
    #[serde(rename = "nudox_metric")]
    metric: &'body str,
    #[serde(rename = "nudox_partition")]
    partition: u64,
    #[serde(rename = "nudox_entity")]
    entity: u64,
}

fn validate_query_scope(
    phase: RequestPhase,
    physical_id: PhysicalPointId,
    key: QdrantDataKey,
    authority: VectorAuthority,
    selected: &[PartitionId],
) -> Result<(), QdrantError> {
    let cause = if key.authority != authority {
        Some(PayloadMismatchCause::Authority)
    } else if !selected.is_empty() && !selected.contains(&key.partition) {
        Some(PayloadMismatchCause::PartitionSelection)
    } else {
        None
    };
    match cause {
        Some(cause) => Err(QdrantError::PayloadMismatch {
            phase,
            physical_id,
            cause,
        }),
        None => Ok(()),
    }
}

fn reject_remote_physical_identity(
    phase: RequestPhase,
    observed_physical_id: PhysicalPointId,
    key: QdrantDataKey,
) -> Result<(), QdrantError> {
    let expected_physical_id = PhysicalPointId::for_key(key);
    if observed_physical_id == expected_physical_id {
        return Ok(());
    }
    Err(QdrantError::PhysicalIdentityMismatch {
        phase,
        key: key.into(),
        observed_physical_id,
        expected_physical_id,
    })
}

fn decode_snapshot(value: &str, phase: RequestPhase) -> Result<IndexSnapshotId, QdrantError> {
    let bytes = decode_hex::<32>(value, PayloadField::Snapshot, phase)?;
    IndexSnapshotId::try_from(bytes).map_err(|source| QdrantError::SnapshotDecode { phase, source })
}

fn decode_model(value: &str, phase: RequestPhase) -> Result<ModelId, QdrantError> {
    let bytes = decode_hex::<16>(value, PayloadField::Model, phase)?;
    Ok(ModelId::new(bytes))
}

fn decode_segment(value: &str, phase: RequestPhase) -> Result<VectorSegmentId, QdrantError> {
    let bytes = decode_hex::<32>(value, PayloadField::Segment, phase)?;
    VectorSegmentId::try_from(bytes).map_err(|source| QdrantError::SegmentDecode { phase, source })
}

fn decode_metric(value: &str, phase: RequestPhase) -> Result<VectorMetric, QdrantError> {
    match value {
        "squared_euclidean" => Ok(VectorMetric::SquaredEuclidean),
        "negative_dot_product" => Ok(VectorMetric::NegativeDotProduct),
        _ => Err(QdrantError::MalformedResponse {
            phase,
            cause: MalformedResponseCause::UnknownMetric,
        }),
    }
}

fn decode_partition(value: u64, phase: RequestPhase) -> Result<PartitionId, QdrantError> {
    let raw = u16::try_from(value).map_err(|source| QdrantError::IntegerRange {
        phase,
        field: PayloadField::Partition,
        observed: value,
        source,
    })?;
    Ok(PartitionId::new(raw))
}

fn decode_entity(value: u64, phase: RequestPhase) -> Result<EntityId, QdrantError> {
    let raw = u32::try_from(value).map_err(|source| QdrantError::IntegerRange {
        phase,
        field: PayloadField::Entity,
        observed: value,
        source,
    })?;
    Ok(EntityId::new(raw))
}

fn decode_hex<const BYTES: usize>(
    value: &str,
    field: PayloadField,
    phase: RequestPhase,
) -> Result<[u8; BYTES], QdrantError> {
    let Some(expected_digits) = BYTES.checked_mul(2) else {
        return Err(QdrantError::InvalidFieldRange { phase, field });
    };
    if value.len() != expected_digits {
        return Err(QdrantError::InvalidFieldRange { phase, field });
    }
    let mut bytes = [0_u8; BYTES];
    for (output, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let high =
            super::hex_nibble(pair[0]).ok_or(QdrantError::InvalidFieldRange { phase, field })?;
        let low =
            super::hex_nibble(pair[1]).ok_or(QdrantError::InvalidFieldRange { phase, field })?;
        *output = (high << 4) | low;
    }
    Ok(bytes)
}

const fn payload_field_name(field: PayloadField) -> &'static str {
    match field {
        PayloadField::Snapshot => SNAPSHOT_PAYLOAD_KEY,
        PayloadField::Model => MODEL_PAYLOAD_KEY,
        PayloadField::Segment => SEGMENT_PAYLOAD_KEY,
        PayloadField::Metric => METRIC_PAYLOAD_KEY,
        PayloadField::Partition => PARTITION_PAYLOAD_KEY,
        PayloadField::Entity => ENTITY_PAYLOAD_KEY,
    }
}
