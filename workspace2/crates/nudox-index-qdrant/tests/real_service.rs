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
    MalformedResponseCause, QdrantBlockingAdapter, QdrantDataKey, QdrantError,
};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;

fn authority() -> VectorAuthority {
    VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(b"qdrant-real-service-snapshot"),
        ModelId::new([0x5a; 16]),
        2,
        Metric::SquaredEuclidean,
    )
}

fn unique_collection() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("nudox_it_{}_{}", std::process::id(), nanos)
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
