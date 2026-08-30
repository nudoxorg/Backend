//! Qdrant's JSON wire contract, isolated from adapter admission and transport policy.

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    CollectionField, ENTITY_PAYLOAD_KEY, METRIC_PAYLOAD_KEY, MODEL_PAYLOAD_KEY,
    MalformedResponseCause, ModelId, PARTITION_PAYLOAD_KEY, PayloadField, PayloadMismatchCause,
    PhysicalPointId, PreparedIdentity, PreparedPoint, QdrantDataKey, QdrantError, QdrantHit,
    RequestPhase, SNAPSHOT_PAYLOAD_KEY, VectorAuthority, hex, metric_name, metric_payload_name,
    projected_score,
};
use nudox_index_graph_vector::{Metric as VectorMetric, PartitionId};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;

/// Decodes one JSON response into its endpoint-specific DTO.
pub(super) fn decode<T: DeserializeOwned>(
    phase: RequestPhase,
    body: &str,
) -> Result<T, QdrantError> {
    serde_json::from_str(body).map_err(|source| QdrantError::Decode { phase, source })
}

/// Verifies the minimal acknowledgement response contract.
pub(super) fn parse_ack(phase: RequestPhase, body: &str) -> Result<(), QdrantError> {
    let _: Acknowledgement = decode(phase, body)?;
    Ok(())
}

/// The collection metadata that participates in this adapter's contract.
pub(super) struct CollectionMetadata {
    pub(super) dimension: u64,
    pub(super) metric: String,
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
    expected: &[(PayloadField, &'static str)],
) -> Result<(), QdrantError> {
    let response: CollectionResponse = decode(phase, body)?;
    for (field, expected_type) in expected {
        let name = payload_field_name(*field);
        let observed = response
            .result
            .payload_schema
            .get(name)
            .map(|entry| entry.data_type.as_str());
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
) -> Result<Vec<WirePoint>, QdrantError> {
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
            hit.authority == key.authority()
                && hit.partition == key.partition()
                && hit.entity == key.entity()
        }) {
            return Err(QdrantError::DuplicateSemanticPoint {
                phase,
                key,
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
            partition: key.partition(),
            entity: key.entity(),
            score: projected_score(authority.metric(), query_coordinates, &coordinates),
            physical_id,
        });
    }
    Ok(hits)
}

/// Decodes a point payload into its complete semantic identity.
pub(super) fn decode_identity(
    point: &WirePoint,
    phase: RequestPhase,
    dimension: u16,
) -> Result<QdrantDataKey, QdrantError> {
    let snapshot = decode_snapshot(&point.payload.snapshot, phase)?;
    let model = decode_model(&point.payload.model, phase)?;
    let metric = decode_metric(&point.payload.metric, phase)?;
    let partition = decode_partition(point.payload.partition, phase)?;
    let entity = decode_entity(point.payload.entity, phase)?;
    Ok(QdrantDataKey::new(
        VectorAuthority::new(snapshot, model, dimension, metric),
        partition,
        entity,
    ))
}

/// Decodes a required point vector and checks its exact dimension.
pub(super) fn decode_vector(
    point: &WirePoint,
    phase: RequestPhase,
    physical_id: PhysicalPointId,
    expected_dimension: usize,
) -> Result<Vec<f64>, QdrantError> {
    let Some(vector) = point.vector.as_ref() else {
        return Err(QdrantError::MalformedResponse {
            phase,
            cause: MalformedResponseCause::MissingVector,
        });
    };
    if vector.len() != expected_dimension {
        return Err(QdrantError::VectorMismatch { phase, physical_id });
    }
    Ok(vector.clone())
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
    distance: &'static str,
}

impl CollectionRequest {
    pub(super) fn new(authority: VectorAuthority) -> Self {
        Self {
            vectors: VectorConfig {
                size: authority.dimension(),
                distance: metric_name(authority.metric()),
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
    field_schema: &'static str,
}

impl PayloadIndexRequest {
    pub(super) const fn new(field: PayloadField, schema: &'static str) -> Self {
        Self {
            field_name: payload_field_name(field),
            field_schema: schema,
        }
    }
}

/// Typed request body for a point upsert.
#[derive(Serialize)]
pub(super) struct UpsertRequest {
    points: Vec<UpsertPoint>,
}

#[derive(Serialize)]
struct UpsertPoint {
    id: u64,
    vector: Vec<f64>,
    payload: IdentityPayload,
}

impl UpsertRequest {
    pub(super) fn from_points(points: &[PreparedPoint<'_>]) -> Self {
        Self {
            points: points
                .iter()
                .map(|point| UpsertPoint {
                    id: point.physical_id.raw(),
                    vector: point
                        .coordinates
                        .iter()
                        .map(|coordinate| f64::from(*coordinate))
                        .collect(),
                    payload: IdentityPayload::from_key(point.key),
                })
                .collect(),
        }
    }
}

/// Typed request body for retrieve operations.
#[derive(Serialize)]
pub(super) struct RetrieveRequest {
    ids: Vec<u64>,
    with_payload: bool,
    with_vector: bool,
}

impl RetrieveRequest {
    pub(super) fn new(keys: &[impl PreparedIdentity], with_vector: bool) -> Self {
        Self {
            ids: keys.iter().map(|key| key.physical_id().raw()).collect(),
            with_payload: true,
            with_vector,
        }
    }
}

/// Typed request body for delete operations.
#[derive(Serialize)]
pub(super) struct DeleteRequest {
    points: Vec<u64>,
}

impl DeleteRequest {
    pub(super) fn new(keys: &[impl PreparedIdentity]) -> Self {
        Self {
            points: keys.iter().map(|key| key.physical_id().raw()).collect(),
        }
    }
}

/// Typed request body for exact vector queries.
#[derive(Serialize)]
pub(super) struct QueryRequest {
    query: Vec<f64>,
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

impl QueryRequest {
    pub(super) fn new(
        authority: VectorAuthority,
        selected: &[PartitionId],
        coordinates: &[i16],
    ) -> Self {
        Self {
            query: coordinates
                .iter()
                .map(|coordinate| f64::from(*coordinate))
                .collect(),
            limit: super::QUERY_SCAN_LIMIT,
            with_payload: true,
            with_vector: true,
            params: ExactParams { exact: true },
            filter: IdentityFilter::new(authority, selected),
        }
    }
}

#[derive(Serialize)]
pub(super) struct IdentityPayload {
    #[serde(rename = "nudox_snapshot")]
    snapshot: String,
    #[serde(rename = "nudox_model")]
    model: String,
    #[serde(rename = "nudox_metric")]
    metric: &'static str,
    #[serde(rename = "nudox_partition")]
    partition: u16,
    #[serde(rename = "nudox_entity")]
    entity: u32,
}

impl IdentityPayload {
    pub(super) fn from_key(key: QdrantDataKey) -> Self {
        Self {
            snapshot: hex(key.snapshot().as_ref()),
            model: hex(key.model().as_ref()),
            metric: metric_payload_name(key.metric()),
            partition: key.partition().raw,
            entity: key.entity().raw,
        }
    }
}

#[derive(Serialize)]
pub(super) struct IdentityFilter {
    must: Vec<MatchCondition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_should: Option<MinimumShould>,
}

#[derive(Serialize)]
struct MinimumShould {
    conditions: Vec<MatchCondition>,
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

#[derive(Serialize)]
#[serde(untagged)]
enum MatchScalar {
    Text(String),
    Integer(u16),
}

impl IdentityFilter {
    pub(super) fn new(authority: VectorAuthority, selected: &[PartitionId]) -> Self {
        let mut must = vec![
            MatchCondition::text(SNAPSHOT_PAYLOAD_KEY, hex(authority.snapshot().as_ref())),
            MatchCondition::text(MODEL_PAYLOAD_KEY, hex(authority.model().as_ref())),
            MatchCondition::text(
                METRIC_PAYLOAD_KEY,
                metric_payload_name(authority.metric()).to_owned(),
            ),
        ];
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
    fn text(key: &'static str, value: String) -> Self {
        Self {
            key,
            r#match: MatchValue {
                value: MatchScalar::Text(value),
            },
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

#[derive(Deserialize)]
struct Acknowledgement {
    #[serde(rename = "result")]
    _result: serde::de::IgnoredAny,
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
    distance: String,
}

#[derive(Deserialize)]
struct PayloadSchema {
    data_type: String,
}

#[derive(Deserialize)]
struct RetrieveResponse {
    result: Vec<WirePoint>,
}

#[derive(Deserialize)]
struct QueryResponse {
    result: QueryResult,
}

#[derive(Deserialize)]
struct QueryResult {
    points: Vec<WirePoint>,
}

#[derive(Deserialize)]
pub(super) struct WirePoint {
    pub(super) id: u64,
    payload: WirePayload,
    #[serde(default)]
    vector: Option<Vec<f64>>,
}

#[derive(Deserialize)]
struct WirePayload {
    #[serde(rename = "nudox_snapshot")]
    snapshot: String,
    #[serde(rename = "nudox_model")]
    model: String,
    #[serde(rename = "nudox_metric")]
    metric: String,
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
    let cause = if key.authority() != authority {
        Some(PayloadMismatchCause::Authority)
    } else if !selected.is_empty() && !selected.contains(&key.partition()) {
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
        key,
        observed_physical_id,
        expected_physical_id,
    })
}

fn decode_snapshot(value: &str, phase: RequestPhase) -> Result<IndexSnapshotId, QdrantError> {
    let bytes = decode_hex(value, PayloadField::Snapshot, 32, phase)?;
    IndexSnapshotId::try_from(bytes.as_slice())
        .map_err(|source| QdrantError::SnapshotDecode { phase, source })
}

fn decode_model(value: &str, phase: RequestPhase) -> Result<ModelId, QdrantError> {
    let bytes = decode_hex(value, PayloadField::Model, 16, phase)?;
    let bytes: [u8; 16] = bytes
        .as_slice()
        .try_into()
        .map_err(|source| QdrantError::ModelDecode { phase, source })?;
    Ok(ModelId::new(bytes))
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

fn decode_hex(
    value: &str,
    field: PayloadField,
    expected_bytes: usize,
    phase: RequestPhase,
) -> Result<Vec<u8>, QdrantError> {
    if value.len() != expected_bytes * 2 {
        return Err(QdrantError::InvalidFieldRange { phase, field });
    }
    let mut bytes = Vec::with_capacity(expected_bytes);
    for pair in value.as_bytes().chunks_exact(2) {
        let high =
            super::hex_nibble(pair[0]).ok_or(QdrantError::InvalidFieldRange { phase, field })?;
        let low =
            super::hex_nibble(pair[1]).ok_or(QdrantError::InvalidFieldRange { phase, field })?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

const fn payload_field_name(field: PayloadField) -> &'static str {
    match field {
        PayloadField::Snapshot => SNAPSHOT_PAYLOAD_KEY,
        PayloadField::Model => MODEL_PAYLOAD_KEY,
        PayloadField::Metric => METRIC_PAYLOAD_KEY,
        PayloadField::Partition => PARTITION_PAYLOAD_KEY,
        PayloadField::Entity => ENTITY_PAYLOAD_KEY,
    }
}
