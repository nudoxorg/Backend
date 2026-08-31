use std::{
    env,
    net::TcpListener,
    time::{SystemTime, UNIX_EPOCH},
};

use nudox_index_graph_vector::{
    Metric, ModelId, PartitionId, ValidatedVectorSegment, VectorAuthority, VectorPoint,
    VectorQueryError, VectorSegmentError, exact_vector_query,
};
use nudox_index_qdrant::{
    CollectionField, CollectionValue, MalformedResponseCause, QdrantBlockingAdapter, QdrantDataKey,
    QdrantError, RequestPhase,
};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;

fn authority_for(metric: Metric, seed: u8) -> VectorAuthority {
    VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(&[seed; 32]),
        ModelId::new([seed; 16]),
        2,
        metric,
    )
}

fn authority() -> VectorAuthority {
    authority_for(Metric::SquaredEuclidean, 0x5a)
}

fn unique_collection() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("nudox_it_{}_{}", std::process::id(), nanos)
}

fn configured_collection() -> Option<String> {
    env::var("QDRANT_TEST_COLLECTION")
        .ok()
        .filter(|collection| !collection.is_empty())
}

#[derive(Debug, thiserror::Error)]
enum JourneyError {
    #[error("Qdrant journey failed: {0}")]
    Qdrant(#[from] QdrantError),
    #[error("graph-vector oracle rejected the differential query: {0:?}")]
    Graph(VectorQueryError),
    #[error("graph-vector oracle rejected a canonical segment: {0:?}")]
    Segment(VectorSegmentError),
}

impl From<VectorQueryError> for JourneyError {
    fn from(error: VectorQueryError) -> Self {
        Self::Graph(error)
    }
}

impl From<VectorSegmentError> for JourneyError {
    fn from(error: VectorSegmentError) -> Self {
        Self::Segment(error)
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the live differential keeps complete graph-vector rejection evidence"
)]
fn journey(adapter: &QdrantBlockingAdapter) -> Result<(), JourneyError> {
    adapter.ensure_collection()?;
    let authority = adapter.config().authority;
    let first_coordinates = [1_i16, 0];
    let second_coordinates = [0_i16, 2];
    let first_points = [VectorPoint::new(EntityId::new(4), &first_coordinates)];
    let second_points = [VectorPoint::new(EntityId::new(7), &second_coordinates)];
    let segments = [
        ValidatedVectorSegment::try_new(authority, PartitionId::new(1), &first_points)?,
        ValidatedVectorSegment::try_new(authority, PartitionId::new(2), &second_points)?,
    ];
    let receipt = adapter.upsert(&segments)?;
    assert_eq!(receipt.attempted, 2);
    assert_eq!(receipt.verified, 2);

    let keys = [
        QdrantDataKey::new(
            authority,
            segments[0].id,
            segments[0].partition,
            EntityId::new(4),
        ),
        QdrantDataKey::new(
            authority,
            segments[1].id,
            segments[1].partition,
            EntityId::new(7),
        ),
    ];
    let mut readback = [None, None];
    assert_eq!(adapter.readback(&keys, &mut readback)?, 2);
    let Some(first_readback) = readback.first().and_then(Option::as_ref) else {
        return Err(JourneyError::Qdrant(QdrantError::MalformedResponse {
            phase: nudox_index_qdrant::RequestPhase::ReadPoints,
            cause: MalformedResponseCause::ReadbackOutputIndex,
        }));
    };
    assert_eq!(first_readback.key, keys[0]);
    assert_eq!(first_readback.coordinates.as_ref(), &[1.0, 0.0]);

    let mut remote = [None, None];
    let remote_count = adapter.query(
        &[segments[0].descriptor(), segments[1].descriptor()],
        &[0, 0],
        2,
        &mut remote,
    )?;
    assert_eq!(remote_count.count, 2);
    assert_eq!(remote[0].map(|hit| hit.entity), Some(EntityId::new(4)));
    assert_eq!(remote[1].map(|hit| hit.entity), Some(EntityId::new(7)));

    let mut local = [None, None];
    let local_count = exact_vector_query(
        authority,
        &[PartitionId::new(1), PartitionId::new(2)],
        &segments,
        &[0, 0],
        2,
        &mut local,
    )?;
    assert_eq!(local_count.written, 2);
    assert_eq!(
        local.map(|hit| hit.map(|hit| hit.entity)),
        remote.map(|hit| hit.map(|hit| hit.entity))
    );
    assert_eq!(
        local.map(|hit| hit.map(|hit| hit.score)),
        [Some(1), Some(4)]
    );
    assert_eq!(
        remote.map(|hit| hit.map(|hit| hit.score)),
        [Some(1.0), Some(4.0)]
    );

    let repeat = adapter.upsert(&segments)?;
    assert_eq!(repeat.attempted, 2);
    assert_eq!(repeat.verified, 2);
    let conflicting_coordinates = [2_i16, 0];
    let conflicting_points = [VectorPoint::new(EntityId::new(4), &conflicting_coordinates)];
    let conflicting_segment =
        ValidatedVectorSegment::try_new(authority, PartitionId::new(1), &conflicting_points)?;
    assert_ne!(conflicting_segment.id, segments[0].id);
    let mut unchanged = [None];
    assert_eq!(adapter.readback(&[keys[0]], &mut unchanged)?, 1);
    assert_eq!(
        unchanged[0]
            .as_ref()
            .map(|point| point.coordinates.as_ref()),
        Some([1.0, 0.0].as_slice())
    );
    let deleted = adapter.delete(&[keys[0]])?;
    assert_eq!(deleted.attempted, 1);
    assert_eq!(deleted.verified, 1);

    let mut after_delete = [None, None];
    let count = adapter.query(
        &[segments[0].descriptor(), segments[1].descriptor()],
        &[0, 0],
        2,
        &mut after_delete,
    )?;
    assert_eq!(count.count, 1);
    Ok(())
}

#[allow(
    clippy::result_large_err,
    reason = "the live metric matrix retains complete adapter and graph-vector failures"
)]
fn metric_matrix_case(endpoint: &str, metric: Metric) -> Result<(), JourneyError> {
    let collection = unique_collection();
    let primary_authority = authority_for(metric, 0x31);
    let foreign_authority = authority_for(metric, 0x42);
    let primary = QdrantBlockingAdapter::new(endpoint, &collection, primary_authority)?;
    let foreign = QdrantBlockingAdapter::new(endpoint, &collection, foreign_authority)?;
    let operation = (|| -> Result<(), JourneyError> {
        primary.ensure_collection()?;
        foreign.ensure_collection()?;

        let conflicting_metric = match metric {
            Metric::SquaredEuclidean => Metric::NegativeDotProduct,
            Metric::NegativeDotProduct => Metric::SquaredEuclidean,
        };
        let collection_metric_mismatch = QdrantBlockingAdapter::new(
            endpoint,
            &collection,
            VectorAuthority::new(
                primary_authority.snapshot,
                primary_authority.model,
                primary_authority.dimension,
                conflicting_metric,
            ),
        )?;
        assert!(matches!(
            collection_metric_mismatch.ensure_collection(),
            Err(QdrantError::CollectionMismatch {
                phase: RequestPhase::ReadCollection,
                field: CollectionField::VectorMetric,
                expected: CollectionValue::Metric(expected),
                observed: CollectionValue::Metric(observed),
            }) if expected == conflicting_metric && observed == primary_authority.metric
        ));

        let first_coordinates = [1_i16, 0];
        let second_coordinates = [0_i16, 1];
        let foreign_coordinates = [2_i16, 0];
        let primary_first_points = [VectorPoint::new(EntityId::new(9), &first_coordinates)];
        let primary_second_points = [VectorPoint::new(EntityId::new(3), &second_coordinates)];
        let foreign_points = [VectorPoint::new(EntityId::new(99), &foreign_coordinates)];
        let primary_segments = [
            ValidatedVectorSegment::try_new(
                primary_authority,
                PartitionId::new(1),
                &primary_first_points,
            )?,
            ValidatedVectorSegment::try_new(
                primary_authority,
                PartitionId::new(2),
                &primary_second_points,
            )?,
        ];
        let foreign_segments = [ValidatedVectorSegment::try_new(
            foreign_authority,
            PartitionId::new(1),
            &foreign_points,
        )?];
        assert_eq!(primary.upsert(&primary_segments)?.verified, 2);
        assert_eq!(foreign.upsert(&foreign_segments)?.verified, 1);

        let query = match metric {
            Metric::SquaredEuclidean => [0_i16, 0],
            Metric::NegativeDotProduct => [1_i16, 1],
        };
        let mut remote = [None, None];
        assert_eq!(
            primary
                .query(
                    &[
                        primary_segments[0].descriptor(),
                        primary_segments[1].descriptor(),
                    ],
                    &query,
                    2,
                    &mut remote,
                )?
                .count,
            2
        );
        assert_eq!(remote[0].map(|hit| hit.entity), Some(EntityId::new(3)));
        assert_eq!(remote[1].map(|hit| hit.entity), Some(EntityId::new(9)));
        assert!(
            remote
                .iter()
                .flatten()
                .all(|hit| hit.authority == primary_authority)
        );

        let mut local = [None, None];
        assert_eq!(
            exact_vector_query(
                primary_authority,
                &[PartitionId::new(1), PartitionId::new(2)],
                &primary_segments,
                &query,
                2,
                &mut local,
            )?
            .written,
            2
        );
        assert_eq!(
            local.map(|hit| hit.map(|hit| hit.entity)),
            remote.map(|hit| hit.map(|hit| hit.entity))
        );

        let mut foreign_remote = [None, None];
        assert_eq!(
            foreign
                .query(
                    &[foreign_segments[0].descriptor()],
                    &query,
                    2,
                    &mut foreign_remote,
                )?
                .count,
            1
        );
        assert_eq!(
            foreign_remote[0].map(|hit| hit.entity),
            Some(EntityId::new(99))
        );
        assert!(
            foreign_remote
                .iter()
                .flatten()
                .all(|hit| hit.authority == foreign_authority)
        );

        let primary_keys = [
            QdrantDataKey::new(
                primary_authority,
                primary_segments[0].id,
                primary_segments[0].partition,
                EntityId::new(9),
            ),
            QdrantDataKey::new(
                primary_authority,
                primary_segments[1].id,
                primary_segments[1].partition,
                EntityId::new(3),
            ),
        ];
        assert_eq!(primary.upsert(&primary_segments)?.verified, 2);
        assert_eq!(primary.delete(&[primary_keys[0]])?.verified, 1);
        let mut after_delete = [None, None];
        assert_eq!(
            primary
                .query(
                    &[
                        primary_segments[0].descriptor(),
                        primary_segments[1].descriptor(),
                    ],
                    &query,
                    2,
                    &mut after_delete,
                )?
                .count,
            1
        );
        assert_eq!(
            after_delete[0].map(|hit| hit.entity),
            Some(EntityId::new(3))
        );
        assert_eq!(primary.delete(&[primary_keys[0]])?.verified, 1);
        assert_eq!(primary.upsert(&[primary_segments[0]])?.verified, 1);
        let mut restored = [None];
        assert_eq!(primary.readback(&[primary_keys[0]], &mut restored)?, 1);
        assert_eq!(
            restored[0].as_ref().map(|point| point.coordinates.as_ref()),
            Some([1.0, 0.0].as_slice())
        );
        Ok(())
    })();
    let cleanup = primary.delete_collection();
    operation?;
    cleanup?;
    Ok(())
}

#[test]
fn unavailable_endpoint_preserves_transport_phase_and_retry_bound() {
    let Ok(listener) = TcpListener::bind("127.0.0.1:0") else {
        return;
    };
    let Ok(address) = listener.local_addr() else {
        return;
    };
    drop(listener);
    let endpoint = format!("http://127.0.0.1:{}", address.port());
    let Ok(adapter) = QdrantBlockingAdapter::new(&endpoint, "nudox_outage", authority()) else {
        return;
    };
    let attempts = adapter.config().retry.attempts.get();
    let result = adapter.ensure_collection();
    assert!(matches!(
        result,
        Err(QdrantError::Transport {
            phase: nudox_index_qdrant::RequestPhase::ReadCollection,
            attempts: observed,
            ..
        }) if observed == attempts
    ));
}

#[test]
#[ignore = "requires a Qdrant service at QDRANT_URL"]
fn real_qdrant_service_metric_matrix_isolates_authority_and_stabilizes_ties() {
    let Ok(endpoint) = env::var("QDRANT_URL") else {
        return;
    };
    for metric in [Metric::SquaredEuclidean, Metric::NegativeDotProduct] {
        let result = metric_matrix_case(&endpoint, metric);
        assert!(
            result.is_ok(),
            "metric matrix failed for {metric:?}: {result:?}"
        );
    }
}

const RESTART_FIRST_COORDINATES: [i16; 2] = [1, 0];
const RESTART_SECOND_COORDINATES: [i16; 2] = [0, 2];

#[test]
#[ignore = "launcher provisions QDRANT_TEST_COLLECTION and QDRANT_URL"]
fn real_qdrant_service_prepare_restart_fixture() {
    let (Some(endpoint), Some(collection)) = (env::var("QDRANT_URL").ok(), configured_collection())
    else {
        return;
    };
    let adapter = QdrantBlockingAdapter::new(&endpoint, &collection, authority());
    assert!(adapter.is_ok(), "adapter construction failed");
    let Ok(adapter) = adapter else {
        return;
    };
    let first_points = [VectorPoint::new(
        EntityId::new(101),
        &RESTART_FIRST_COORDINATES,
    )];
    let second_points = [VectorPoint::new(
        EntityId::new(102),
        &RESTART_SECOND_COORDINATES,
    )];
    let segments = [
        ValidatedVectorSegment::try_new(
            adapter.config().authority,
            PartitionId::new(11),
            &first_points,
        ),
        ValidatedVectorSegment::try_new(
            adapter.config().authority,
            PartitionId::new(12),
            &second_points,
        ),
    ];
    assert!(
        segments.iter().all(Result::is_ok),
        "restart fixture invalid"
    );
    let [first, second] = segments;
    let (Ok(first), Ok(second)) = (first, second) else {
        return;
    };
    let prepared = adapter
        .ensure_collection()
        .and_then(|()| adapter.upsert(&[first, second]));
    assert!(
        matches!(prepared, Ok(receipt) if receipt.verified == 2),
        "restart fixture preparation failed: {prepared:?}"
    );
}

#[test]
#[ignore = "launcher provisions QDRANT_TEST_COLLECTION and QDRANT_URL"]
fn real_qdrant_service_reports_transport_during_launcher_outage() {
    let (Some(endpoint), Some(collection)) = (env::var("QDRANT_URL").ok(), configured_collection())
    else {
        return;
    };
    let adapter = QdrantBlockingAdapter::new(&endpoint, &collection, authority());
    assert!(adapter.is_ok(), "adapter construction failed");
    let Ok(adapter) = adapter else {
        return;
    };
    let attempts = adapter.config().retry.attempts.get();
    assert!(matches!(
        adapter.ensure_collection(),
        Err(QdrantError::Transport {
            phase: RequestPhase::ReadCollection,
            attempts: observed,
            ..
        }) if observed == attempts
    ));
}

#[test]
#[ignore = "launcher provisions QDRANT_TEST_COLLECTION and QDRANT_URL"]
fn real_qdrant_service_verifies_restart_fixture_and_cleans_up() {
    let (Some(endpoint), Some(collection)) = (env::var("QDRANT_URL").ok(), configured_collection())
    else {
        return;
    };
    let adapter = QdrantBlockingAdapter::new(&endpoint, &collection, authority());
    assert!(adapter.is_ok(), "adapter construction failed");
    let Ok(adapter) = adapter else {
        return;
    };
    let first_points = [VectorPoint::new(
        EntityId::new(101),
        &RESTART_FIRST_COORDINATES,
    )];
    let second_points = [VectorPoint::new(
        EntityId::new(102),
        &RESTART_SECOND_COORDINATES,
    )];
    let segments = [
        ValidatedVectorSegment::try_new(
            adapter.config().authority,
            PartitionId::new(11),
            &first_points,
        ),
        ValidatedVectorSegment::try_new(
            adapter.config().authority,
            PartitionId::new(12),
            &second_points,
        ),
    ];
    assert!(
        segments.iter().all(Result::is_ok),
        "restart fixture invalid"
    );
    let [first, second] = segments;
    let (Ok(first), Ok(second)) = (first, second) else {
        return;
    };
    let keys = [
        QdrantDataKey::new(
            first.authority,
            first.id,
            first.partition,
            EntityId::new(101),
        ),
        QdrantDataKey::new(
            second.authority,
            second.id,
            second.partition,
            EntityId::new(102),
        ),
    ];
    let operation = (|| -> Result<(), QdrantError> {
        adapter.ensure_collection()?;
        let mut readback = [None, None];
        assert_eq!(adapter.readback(&keys, &mut readback)?, 2);
        assert!(readback.iter().all(Option::is_some));
        let mut remote = [None, None];
        assert_eq!(
            adapter
                .query(
                    &[first.descriptor(), second.descriptor()],
                    &[0, 0],
                    2,
                    &mut remote,
                )?
                .count,
            2
        );
        assert_eq!(adapter.delete(&keys)?.verified, 2);
        Ok(())
    })();
    let cleanup = adapter.delete_collection();
    assert!(cleanup.is_ok(), "collection cleanup failed: {cleanup:?}");
    assert!(
        operation.is_ok(),
        "restart fixture verification failed: {operation:?}"
    );
}

#[test]
#[ignore = "requires a Qdrant service at QDRANT_URL"]
fn real_qdrant_service_upload_readback_filter_query_delete_and_recovery() {
    let Ok(endpoint) = env::var("QDRANT_URL") else {
        return;
    };
    let collection = unique_collection();
    let adapter = QdrantBlockingAdapter::new(&endpoint, &collection, authority());
    assert!(adapter.is_ok());
    let Ok(adapter) = adapter else {
        return;
    };
    let result = journey(&adapter);
    let cleanup = adapter.delete_collection();
    assert!(cleanup.is_ok(), "collection cleanup failed: {cleanup:?}");
    assert!(result.is_ok(), "real-service journey failed: {result:?}");
}
