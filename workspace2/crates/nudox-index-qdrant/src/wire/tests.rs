use super::*;
use crate::wire::request::{IdentityFilter, IdentityPayload};
use crate::{
    contract::{
        CollectionField, CollectionValue, ENTITY_PAYLOAD_KEY, METRIC_PAYLOAD_KEY,
        MODEL_PAYLOAD_KEY, MalformedResponseCause, PARTITION_PAYLOAD_KEY, PayloadEncodingCause,
        PayloadField, PayloadIndexKind, PhysicalPointId, QdrantDataKey, QdrantError, RequestPhase,
        SEGMENT_PAYLOAD_KEY, SNAPSHOT_PAYLOAD_KEY,
    },
    limits::{
        CREATE_PAYLOAD_INDEX_PATH, DELETE_POINTS_PATH, QUERY_POINTS_PATH, QUERY_SCAN_LIMIT,
        READ_POINTS_PATH, UPSERT_POINTS_PATH,
    },
    scoring::projected_score,
};
use nudox_index_graph_vector::{
    Metric, ModelId, PartitionId, ValidatedVectorSegment, VectorAuthority, VectorPoint,
    VectorSegmentError, exact_vector_query,
};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;

fn authority(byte: u8) -> VectorAuthority {
    VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(&[byte; 32]),
        ModelId::new([byte.wrapping_add(1); 16]),
        2,
        Metric::SquaredEuclidean,
    )
}

fn encoded_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, thiserror::Error)]
enum WireTestError {
    #[error("vector fixture failed validation: {cause:?}")]
    Segment { cause: Box<VectorSegmentError> },
    #[error("typed query fixture failed serialization")]
    Encode(#[from] serde_json::Error),
}

impl From<VectorSegmentError> for WireTestError {
    fn from(cause: VectorSegmentError) -> Self {
        Self::Segment {
            cause: Box::new(cause),
        }
    }
}

#[derive(serde::Serialize)]
struct QueryFixture<Points> {
    result: QueryResultFixture<Points>,
}

#[derive(serde::Serialize)]
struct QueryResultFixture<Points> {
    points: Points,
}

#[derive(serde::Serialize)]
struct PointFixture<Payload> {
    id: u64,
    payload: Payload,
    vector: [f64; 2],
}

#[derive(serde::Serialize)]
struct MetricPayloadFixture {
    #[serde(rename = "nudox_snapshot")]
    snapshot: String,
    #[serde(rename = "nudox_model")]
    model: String,
    #[serde(rename = "nudox_segment")]
    segment: String,
    #[serde(rename = "nudox_metric")]
    metric: &'static str,
    #[serde(rename = "nudox_partition")]
    partition: u16,
    #[serde(rename = "nudox_entity")]
    entity: u32,
}

fn query_fixture<Points: serde::Serialize>(points: Points) -> Result<String, serde_json::Error> {
    serde_json::to_string(&QueryFixture {
        result: QueryResultFixture { points },
    })
}

#[test]
fn payload_index_mismatch_retains_typed_expected_and_observed_schema() {
    let expected = [PayloadIndexDescriptor::new(
        PayloadField::Snapshot,
        SNAPSHOT_PAYLOAD_KEY,
        PayloadIndexKind::Keyword,
    )];
    let body = r#"{
        "result": {
            "config": {
                "params": {
                    "vectors": {"size": 2, "distance": "Euclid"},
                    "replication_factor": 1,
                    "write_consistency_factor": 1
                }
            },
            "payload_schema": {
                "nudox_snapshot": {"data_type": "integer"},
                "foreign_open_key": {"data_type": "keyword"}
            }
        }
    }"#;

    assert!(matches!(
        verify_payload_indexes(RequestPhase::ReadCollection, body, &expected),
        Err(QdrantError::CollectionMismatch {
            phase: RequestPhase::ReadCollection,
            field: CollectionField::PayloadIndex(PayloadField::Snapshot),
            expected: CollectionValue::PayloadIndex(PayloadIndexKind::Keyword),
            observed: CollectionValue::PayloadIndex(PayloadIndexKind::Integer),
        })
    ));
}

#[test]
fn identity_payload_and_filter_include_full_authority() {
    let authority = authority(7);
    let coordinates = [1_i16, 2];
    let points = [VectorPoint::new(EntityId::new(13), &coordinates)];
    let Ok(segment) = ValidatedVectorSegment::try_new(authority, PartitionId::new(11), &points)
    else {
        return;
    };
    let key = QdrantDataKey::new(
        authority,
        segment.id,
        PartitionId::new(11),
        EntityId::new(13),
    );
    let payload = serde_json::to_value(IdentityPayload::from_key(key));
    assert!(payload.is_ok());
    let Ok(payload) = payload else {
        return;
    };
    assert_eq!(
        payload.get(SNAPSHOT_PAYLOAD_KEY),
        Some(&serde_json::Value::from(encoded_hex(
            key.authority.snapshot.as_ref(),
        )))
    );
    assert_eq!(
        payload.get(MODEL_PAYLOAD_KEY),
        Some(&serde_json::Value::from(encoded_hex(
            key.authority.model.as_ref(),
        )))
    );
    assert_eq!(
        payload.get(SEGMENT_PAYLOAD_KEY),
        Some(&serde_json::Value::from(encoded_hex(key.segment.as_ref())))
    );
    assert_eq!(
        payload.get(METRIC_PAYLOAD_KEY),
        Some(&serde_json::Value::from("squared_euclidean"))
    );
    assert_eq!(
        payload.get(PARTITION_PAYLOAD_KEY),
        Some(&serde_json::Value::from(11_u16))
    );
    assert_eq!(
        payload.get(ENTITY_PAYLOAD_KEY),
        Some(&serde_json::Value::from(13_u32))
    );
    let filter = serde_json::to_value(IdentityFilter::new(key.authority, &[segment.descriptor()]));
    assert!(filter.is_ok());
    let Ok(filter) = filter else {
        return;
    };
    let must = filter.get("must").and_then(serde_json::Value::as_array);
    assert!(must.is_some_and(|must| must.len() == 4));
    assert_eq!(
        must.and_then(|must| must.get(3))
            .and_then(|condition| condition.get("key")),
        Some(&serde_json::Value::from(SEGMENT_PAYLOAD_KEY))
    );
}

#[test]
fn request_shapes_are_batched_and_idempotent() {
    let coordinates = [1_i16, -2];
    let authority = authority(3);
    let points = [VectorPoint::new(EntityId::new(5), &coordinates)];
    let Ok(segment) = ValidatedVectorSegment::try_new(authority, PartitionId::new(1), &points)
    else {
        return;
    };
    let Ok(prepared) = crate::admission::prepare_segments(authority, &[segment]) else {
        return;
    };
    let Some(point) = prepared.first().copied() else {
        return;
    };
    let upsert = serde_json::to_value(UpsertRequest::from_points(&prepared));
    assert!(upsert.is_ok());
    let Ok(upsert) = upsert else {
        return;
    };
    assert_eq!(
        upsert
            .get("points")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        upsert
            .get("points")
            .and_then(serde_json::Value::as_array)
            .and_then(|points| points.first())
            .and_then(|point| point.get("id")),
        Some(&serde_json::Value::from(point.physical_id.0))
    );
    let keys = [crate::admission::PreparedKey {
        index: 0,
        key: point.key,
        physical_id: point.physical_id,
    }];
    let delete = serde_json::to_value(DeleteRequest::new(&keys));
    assert!(delete.is_ok());
    let Ok(delete) = delete else {
        return;
    };
    assert_eq!(
        delete
            .get("points")
            .and_then(serde_json::Value::as_array)
            .and_then(|points| points.first()),
        Some(&serde_json::Value::from(point.physical_id.0))
    );
    let retrieve = serde_json::to_value(RetrieveRequest::new(&keys, true));
    assert!(retrieve.is_ok());
    let Ok(retrieve) = retrieve else {
        return;
    };
    assert_eq!(
        retrieve.get("with_vector"),
        Some(&serde_json::Value::from(true))
    );
    let query = serde_json::to_value(QueryRequest::new(
        authority,
        &[segment.descriptor()],
        &[0, 0],
    ));
    assert!(query.is_ok());
    let Ok(query) = query else {
        return;
    };
    assert_eq!(
        query.get("limit"),
        Some(&serde_json::Value::from(QUERY_SCAN_LIMIT))
    );
    assert_eq!(
        query.get("with_vector"),
        Some(&serde_json::Value::from(true))
    );
    assert_eq!(
        query.get("params").and_then(|params| params.get("exact")),
        Some(&serde_json::Value::from(true))
    );
}

#[test]
fn replica_lag_requires_all_reads_and_strong_ordered_writes() {
    assert_eq!(READ_POINTS_PATH, "/points?consistency=all");
    assert_eq!(QUERY_POINTS_PATH, "/points/query?consistency=all");
    assert_eq!(UPSERT_POINTS_PATH, "/points?wait=true&ordering=strong");
    assert_eq!(
        DELETE_POINTS_PATH,
        "/points/delete?wait=true&ordering=strong"
    );
    assert_eq!(
        CREATE_PAYLOAD_INDEX_PATH,
        "/index?wait=true&ordering=strong"
    );
    let collection = serde_json::to_value(CollectionRequest::new(authority(3)));
    assert!(collection.is_ok());
    let Ok(collection) = collection else {
        return;
    };
    assert_eq!(
        collection.get("replication_factor"),
        Some(&serde_json::Value::from(1))
    );
    assert_eq!(
        collection.get("write_consistency_factor"),
        Some(&serde_json::Value::from(1))
    );
}

#[test]
fn backend_coordinates_reconstruct_graph_vector_scores() {
    let authority = authority(9);
    let query = [1_i16, -2];
    let coordinates = [4_i16, 2];
    let points = [VectorPoint::new(EntityId::new(7), &coordinates)];
    let segment = ValidatedVectorSegment::try_new(authority, PartitionId::new(0), &points);
    assert!(segment.is_ok());
    let Ok(segment) = segment else {
        return;
    };
    let mut output = [None];
    let result = exact_vector_query(
        authority,
        &[PartitionId::new(0)],
        std::slice::from_ref(&segment),
        &query,
        1,
        &mut output,
    );
    assert!(result.is_ok());
    let Ok(result) = result else {
        return;
    };
    assert_eq!(result.written, 1);
    let Some(local) = output[0] else {
        return;
    };
    assert_eq!(
        projected_score(Metric::SquaredEuclidean, &query, &[4.0_f64, 2.0]),
        25.0
    );
    assert_eq!(local.score, 25);
}

#[test]
fn query_rejects_alias_physical_id_and_duplicate_semantic_key() -> Result<(), WireTestError> {
    let authority = authority(6);
    let first_coordinates = [1_i16, 2];
    let second_coordinates = [2_i16, 3];
    let points = [
        VectorPoint::new(EntityId::new(7), &first_coordinates),
        VectorPoint::new(EntityId::new(8), &second_coordinates),
    ];
    let segment = ValidatedVectorSegment::try_new(authority, PartitionId::new(2), &points)?;
    let key = QdrantDataKey::new(authority, segment.id, segment.partition, EntityId::new(7));
    let canonical_id = PhysicalPointId::for_key(key);
    let alias_id = PhysicalPointId(canonical_id.0.wrapping_add(1));
    let point = |physical_id: PhysicalPointId| PointFixture {
        id: physical_id.0,
        payload: IdentityPayload::from_key(key),
        vector: [1.0, 2.0],
    };
    let alias_body = query_fixture([point(alias_id)])?;
    assert!(matches!(
        parse_query_candidates(authority, &[segment.descriptor()], &[0, 0], &alias_body),
        Err(QdrantError::PhysicalIdentityMismatch {
            phase: RequestPhase::QueryPoints,
            key: observed_key,
            observed_physical_id,
            expected_physical_id,
        }) if *observed_key == key
            && observed_physical_id == alias_id
            && expected_physical_id == canonical_id
    ));

    let duplicate_body = query_fixture([point(canonical_id), point(canonical_id)])?;
    assert!(matches!(
        parse_query_candidates(authority, &[segment.descriptor()], &[0, 0], &duplicate_body),
        Err(QdrantError::DuplicateSemanticPoint {
            phase: RequestPhase::QueryPoints,
            key: observed_key,
            first_physical_id,
            second_physical_id,
        }) if *observed_key == key
            && first_physical_id == canonical_id
            && second_physical_id == canonical_id
    ));
    Ok(())
}

#[test]
fn typed_decoder_rejects_unknown_metric_and_malformed_query_shapes() -> Result<(), WireTestError> {
    let authority = authority(12);
    let coordinates = [1_i16, 2];
    let points = [VectorPoint::new(EntityId::new(7), &coordinates)];
    let segment = ValidatedVectorSegment::try_new(authority, PartitionId::new(2), &points)?;
    let key = QdrantDataKey::new(authority, segment.id, segment.partition, EntityId::new(7));
    let point_id = PhysicalPointId::for_key(key);
    let invalid_metric = MetricPayloadFixture {
        snapshot: encoded_hex(key.authority.snapshot.as_ref()),
        model: encoded_hex(key.authority.model.as_ref()),
        segment: encoded_hex(key.segment.as_ref()),
        metric: "cosine",
        partition: key.partition.raw,
        entity: key.entity.raw,
    };
    let unknown_metric = query_fixture([PointFixture {
        id: point_id.0,
        payload: invalid_metric,
        vector: [1.0, 2.0],
    }])?;
    assert!(matches!(
        parse_query_candidates(authority, &[segment.descriptor()], &[0, 0], &unknown_metric),
        Err(QdrantError::MalformedResponse {
            phase: RequestPhase::QueryPoints,
            cause: MalformedResponseCause::UnknownMetric(observed),
        }) if observed.0 == "cosine"
    ));

    let short_snapshot = query_fixture([PointFixture {
        id: point_id.0,
        payload: MetricPayloadFixture {
            snapshot: String::from("00"),
            model: encoded_hex(key.authority.model.as_ref()),
            segment: encoded_hex(key.segment.as_ref()),
            metric: "squared_euclidean",
            partition: key.partition.raw,
            entity: key.entity.raw,
        },
        vector: [1.0, 2.0],
    }])?;
    assert!(matches!(
        parse_query_candidates(authority, &[segment.descriptor()], &[0, 0], &short_snapshot),
        Err(QdrantError::InvalidFieldEncoding {
            phase: RequestPhase::QueryPoints,
            field: PayloadField::Snapshot,
            cause: PayloadEncodingCause::HexLength {
                expected: 64,
                observed: 2,
            },
        })
    ));

    let invalid_snapshot_digit = format!("z{}", "0".repeat(63));
    let invalid_snapshot = query_fixture([PointFixture {
        id: point_id.0,
        payload: MetricPayloadFixture {
            snapshot: invalid_snapshot_digit,
            model: encoded_hex(key.authority.model.as_ref()),
            segment: encoded_hex(key.segment.as_ref()),
            metric: "squared_euclidean",
            partition: key.partition.raw,
            entity: key.entity.raw,
        },
        vector: [1.0, 2.0],
    }])?;
    assert!(matches!(
        parse_query_candidates(authority, &[segment.descriptor()], &[0, 0], &invalid_snapshot),
        Err(QdrantError::InvalidFieldEncoding {
            phase: RequestPhase::QueryPoints,
            field: PayloadField::Snapshot,
            cause: PayloadEncodingCause::HexDigit {
                index: 0,
                observed: b'z',
            },
        })
    ));

    let missing_payload = r#"{\"result\":{\"points\":[{\"id\":1,\"vector\":[1.0,2.0]}]}}"#;
    assert!(matches!(
        parse_query_candidates(authority, &[], &[0, 0], missing_payload),
        Err(QdrantError::Decode {
            phase: RequestPhase::QueryPoints,
            ..
        })
    ));

    let points_not_array = r#"{\"result\":{\"points\":{}}}"#;
    assert!(matches!(
        parse_query_candidates(authority, &[], &[0, 0], points_not_array),
        Err(QdrantError::Decode {
            phase: RequestPhase::QueryPoints,
            ..
        })
    ));
    Ok(())
}
