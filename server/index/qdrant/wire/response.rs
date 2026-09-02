//! Defines wire response behavior for `server-index-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the wire response invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Typed Qdrant response records, validation, and identity decoding.

use arrayvec::ArrayVec;
use serde::Deserialize;

use super::super::{
    contract::{
        AuthorityMismatchEvidence, CollectionField, CollectionValue, MalformedResponseCause,
        PayloadEncodingCause, PayloadField, PayloadIndexKind, PayloadMismatchCause,
        PhysicalPointId, QdrantCandidate, QdrantDataKey, QdrantError, RejectedMetric, RequestPhase,
    },
    limits::{MAX_BATCH_POINTS, MAX_QUERY_SEGMENTS},
    scoring::projected_score,
};
use super::request::{CollectionMetric, PayloadIndexDescriptor};
use compiler_ir_vocabulary::EntityId;
use server_index_graph_vector::{
    Metric as VectorMetric, ModelId, PartitionId, VectorAuthority, VectorSegmentDescriptor,
};
use server_index_vocabulary::{IndexSnapshotId, VectorSegmentId};

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
        let observed = response.result.payload_schema.kind(descriptor.field);
        if observed != Some(descriptor.schema) {
            return Err(QdrantError::CollectionMismatch {
                phase,
                field: CollectionField::PayloadIndex(descriptor.field),
                expected: CollectionValue::PayloadIndex(descriptor.schema),
                observed: observed.map_or(
                    CollectionValue::MissingPayloadIndex,
                    CollectionValue::PayloadIndex,
                ),
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
pub(crate) fn parse_query_candidates(
    authority: VectorAuthority,
    selected: &[VectorSegmentDescriptor],
    query_coordinates: &[i16],
    body: &str,
) -> Result<ArrayVec<QdrantCandidate, MAX_BATCH_POINTS>, QdrantError> {
    let phase = RequestPhase::QueryPoints;
    let points = decode::<QueryResponse>(phase, body)?.result.points;
    if points.len() > MAX_BATCH_POINTS {
        return Err(QdrantError::ProjectionCapacity {
            maximum: MAX_BATCH_POINTS,
            observed: points.len(),
        });
    }
    let mut hits: ArrayVec<QdrantCandidate, MAX_BATCH_POINTS> = ArrayVec::new();
    let mut segment_counts = [0_u8; MAX_QUERY_SEGMENTS];
    for point in &points {
        let physical_id = PhysicalPointId(point.id);
        let key = decode_identity(point, phase, authority.dimension)?;
        observe_query_scope(
            phase,
            physical_id,
            key,
            authority,
            selected,
            &mut segment_counts,
        )?;
        reject_remote_physical_identity(phase, physical_id, key)?;
        if let Some(first) = hits.iter().find(|hit: &&QdrantCandidate| {
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
            .any(|hit: &QdrantCandidate| hit.physical_id == physical_id)
        {
            return Err(QdrantError::MalformedResponse {
                phase,
                cause: MalformedResponseCause::PhysicalIdAliased,
            });
        }
        let coordinates =
            decode_vector(point, phase, physical_id, usize::from(authority.dimension))?;
        if let Err(_rejected) = hits.try_push(QdrantCandidate {
            authority,
            segment: key.segment,
            partition: key.partition,
            entity: key.entity,
            score: projected_score(authority.metric, query_coordinates, coordinates),
            physical_id,
        }) {
            return Err(QdrantError::ProjectionCapacity {
                maximum: MAX_BATCH_POINTS,
                observed: points.len(),
            });
        }
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
    payload_schema: PayloadSchemaMap,
}

/// Qdrant owns an open payload-schema object. Serde ignores external keys while this fixed record
/// retains only fields that participate in the adapter's authority contract.
#[derive(Default, Deserialize)]
struct PayloadSchemaMap {
    #[serde(rename = "server_snapshot")]
    snapshot: Option<PayloadSchema>,
    #[serde(rename = "server_model")]
    model: Option<PayloadSchema>,
    #[serde(rename = "server_segment")]
    segment: Option<PayloadSchema>,
    #[serde(rename = "server_metric")]
    metric: Option<PayloadSchema>,
    #[serde(rename = "server_partition")]
    partition: Option<PayloadSchema>,
    #[serde(rename = "server_entity")]
    entity: Option<PayloadSchema>,
}

impl PayloadSchemaMap {
    fn kind(&self, field: PayloadField) -> Option<PayloadIndexKind> {
        match field {
            PayloadField::Snapshot => self.snapshot,
            PayloadField::Model => self.model,
            PayloadField::Segment => self.segment,
            PayloadField::Metric => self.metric,
            PayloadField::Partition => self.partition,
            PayloadField::Entity => self.entity,
        }
        .map(|entry| entry.data_type)
    }
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

#[derive(Clone, Copy, Deserialize)]
struct PayloadSchema {
    data_type: PayloadIndexKind,
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
    // serde_json must own both this response list and each numeric vector array: neither can borrow
    // the response string. parse_query_candidates bounds the list before moving fixed-size hits into an
    // inline ArrayVec, and checks each owned vector against the authority dimension.
    #[serde(borrow)]
    points: Vec<WirePoint<'body>>,
}

#[derive(Deserialize)]
pub(crate) struct WirePoint<'body> {
    pub(crate) id: u64,
    #[serde(borrow)]
    payload: WirePayload<'body>,
    // Numeric JSON values are not borrowable; this allocation is released after score/readback
    // conversion and is bounded by the response byte limit plus the authority dimension check.
    #[serde(default)]
    vector: Option<Vec<f64>>,
}

#[derive(Deserialize)]
struct WirePayload<'body> {
    #[serde(rename = "server_snapshot")]
    snapshot: &'body str,
    #[serde(rename = "server_model")]
    model: &'body str,
    #[serde(rename = "server_segment")]
    segment: &'body str,
    #[serde(rename = "server_metric")]
    metric: &'body str,
    #[serde(rename = "server_partition")]
    partition: u64,
    #[serde(rename = "server_entity")]
    entity: u64,
}

fn observe_query_scope(
    phase: RequestPhase,
    physical_id: PhysicalPointId,
    key: QdrantDataKey,
    authority: VectorAuthority,
    selected: &[VectorSegmentDescriptor],
    segment_counts: &mut [u8; MAX_QUERY_SEGMENTS],
) -> Result<(), QdrantError> {
    if key.authority != authority {
        return Err(QdrantError::PayloadMismatch {
            phase,
            physical_id,
            cause: PayloadMismatchCause::Authority(Box::new(AuthorityMismatchEvidence {
                expected: authority,
                observed: key.authority,
            })),
        });
    }
    let Some((descriptor, observed)) = selected
        .iter()
        .copied()
        .zip(segment_counts)
        .find(|(descriptor, _)| descriptor.id == key.segment)
    else {
        let mut selection = [None; MAX_QUERY_SEGMENTS];
        for (slot, descriptor) in selection.iter_mut().zip(selected.iter().copied()) {
            *slot = Some(descriptor.id);
        }
        return Err(QdrantError::PayloadMismatch {
            phase,
            physical_id,
            cause: PayloadMismatchCause::SegmentSelection {
                selected: Box::new(selection),
                observed: key.segment,
            },
        });
    };
    if descriptor.partition != key.partition {
        return Err(QdrantError::PayloadMismatch {
            phase,
            physical_id,
            cause: PayloadMismatchCause::SegmentPartition {
                segment: key.segment,
                expected: descriptor.partition,
                observed: key.partition,
            },
        });
    }
    *observed += 1;
    if *observed > descriptor.point_count {
        return Err(QdrantError::SegmentCardinalityExceeded {
            phase,
            segment: descriptor.id,
            maximum: descriptor.point_count,
            observed: *observed,
        });
    }
    Ok(())
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
            cause: MalformedResponseCause::UnknownMetric(RejectedMetric(value.to_owned())),
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
        return Err(QdrantError::InvalidFieldEncoding {
            phase,
            field,
            cause: PayloadEncodingCause::HexWidthOverflow { bytes: BYTES },
        });
    };
    if value.len() != expected_digits {
        return Err(QdrantError::InvalidFieldEncoding {
            phase,
            field,
            cause: PayloadEncodingCause::HexLength {
                expected: expected_digits,
                observed: value.len(),
            },
        });
    }
    let mut bytes = [0_u8; BYTES];
    for (byte_index, (output, pair)) in bytes
        .iter_mut()
        .zip(value.as_bytes().chunks_exact(2))
        .enumerate()
    {
        let high = hex_nibble(pair[0]).ok_or(QdrantError::InvalidFieldEncoding {
            phase,
            field,
            cause: PayloadEncodingCause::HexDigit {
                index: byte_index * 2,
                observed: pair[0],
            },
        })?;
        let low = hex_nibble(pair[1]).ok_or(QdrantError::InvalidFieldEncoding {
            phase,
            field,
            cause: PayloadEncodingCause::HexDigit {
                index: byte_index * 2 + 1,
                observed: pair[1],
            },
        })?;
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
