//! Typed Qdrant response records, validation, and identity decoding.

use arrayvec::ArrayVec;
use serde::Deserialize;

use super::super::{
    contract::{
        CollectionField, MalformedResponseCause, PayloadField, PayloadMismatchCause,
        PhysicalPointId, QdrantDataKey, QdrantError, QdrantHit, RequestPhase,
    },
    limits::MAX_BATCH_POINTS,
    scoring::projected_score,
};
use super::request::{CollectionMetric, PayloadDataType, PayloadIndexDescriptor};
use nudox_index_graph_vector::{Metric as VectorMetric, ModelId, PartitionId, VectorAuthority};
use nudox_index_vocab::{IndexSnapshotId, VectorSegmentId};
use nudox_ir_vocab::EntityId;

/// Decodes one JSON response into its endpoint-specific DTO.
pub(crate) fn decode<'body, T: Deserialize<'body>>(
    phase: RequestPhase,
    body: &'body str,
) -> Result<T, QdrantError> {
    serde_json::from_str(body).map_err(|source| QdrantError::Decode { phase, source })
}

/// Verifies collection create/delete's boolean acknowledgement contract.
pub(crate) fn parse_boolean_ack(phase: RequestPhase, body: &str) -> Result<(), QdrantError> {
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
pub(crate) fn parse_completed_ack(phase: RequestPhase, body: &str) -> Result<(), QdrantError> {
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
pub(crate) struct CollectionMetadata {
    pub(crate) dimension: u64,
    pub(crate) metric: CollectionMetric,
    pub(crate) replication_factor: u64,
    pub(crate) write_consistency_factor: u64,
}

/// Decodes the typed collection configuration response.
pub(crate) fn collection_metadata(
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
pub(crate) fn verify_payload_indexes(
    phase: RequestPhase,
    body: &str,
    expected: &[PayloadIndexDescriptor],
) -> Result<(), QdrantError> {
    let response: CollectionResponse = decode(phase, body)?;
    for descriptor in expected {
        let observed = response
            .result
            .payload_schema
            .get(descriptor.wire_name)
            .map(|entry| entry.data_type);
        if observed != Some(descriptor.schema) {
            return Err(QdrantError::CollectionMismatch {
                phase,
                field: CollectionField::PayloadIndex(descriptor.field),
            });
        }
    }
    Ok(())
}

/// Decodes bounded retrieve results.
pub(crate) fn retrieve_points(
    phase: RequestPhase,
    body: &str,
) -> Result<Vec<WirePoint<'_>>, QdrantError> {
    Ok(decode::<RetrieveResponse>(phase, body)?.result)
}

/// Parses, proves, and locally reranks one query response.
pub(crate) fn parse_query_hits(
    authority: VectorAuthority,
    selected: &[PartitionId],
    query_coordinates: &[i16],
    body: &str,
) -> Result<ArrayVec<QdrantHit, MAX_BATCH_POINTS>, QdrantError> {
    let phase = RequestPhase::QueryPoints;
    let points = decode::<QueryResponse>(phase, body)?.result.points;
    if points.len() > MAX_BATCH_POINTS {
        return Err(QdrantError::ProjectionCapacity {
            maximum: MAX_BATCH_POINTS,
            observed: points.len(),
        });
    }
    let mut hits: ArrayVec<QdrantHit, MAX_BATCH_POINTS> = ArrayVec::new();
    for point in &points {
        let physical_id = PhysicalPointId(point.id);
        let key = decode_identity(point, phase, authority.dimension)?;
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
        let coordinates =
            decode_vector(point, phase, physical_id, usize::from(authority.dimension))?;
        hits.try_push(QdrantHit {
            authority,
            segment: key.segment,
            partition: key.partition,
            entity: key.entity,
            score: projected_score(authority.metric, query_coordinates, coordinates),
            physical_id,
        })
        .map_err(|_| QdrantError::ProjectionCapacity {
            maximum: MAX_BATCH_POINTS,
            observed: points.len(),
        })?;
    }
    Ok(hits)
}

/// Decodes a point payload into its complete semantic identity.
pub(crate) fn decode_identity(
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
pub(crate) fn decode_vector<'point>(
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
    // serde_json owns this bounded response list while decoding the body. The adapter immediately
    // validates the length and moves readbacks into an inline ArrayVec before any caller sees it.
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
    // The same decode boundary is the only Vec in the query path; parse_query_hits bounds it before
    // moving fixed-size hits into an inline ArrayVec for sorting and publication.
    #[serde(borrow)]
    points: Vec<WirePoint<'body>>,
}

#[derive(Deserialize)]
pub(crate) struct WirePoint<'body> {
    pub(crate) id: u64,
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
        let high = hex_nibble(pair[0]).ok_or(QdrantError::InvalidFieldRange { phase, field })?;
        let low = hex_nibble(pair[1]).ok_or(QdrantError::InvalidFieldRange { phase, field })?;
        *output = (high << 4) | low;
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
