use std::{
    env,
    net::TcpListener,
    time::{SystemTime, UNIX_EPOCH},
};

use nudox_index_graph_vector::{Metric, ModelId, PartitionId, VectorAuthority};
use nudox_index_qdrant::{QdrantBlockingAdapter, QdrantError, QdrantPoint};
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

fn journey(adapter: &QdrantBlockingAdapter) -> Result<(), QdrantError> {
    adapter.ensure_collection()?;
    let authority = adapter.authority();
    let first_coordinates = [1_i16, 0];
    let second_coordinates = [0_i16, 2];
    let first = QdrantPoint::new(
        authority,
        PartitionId::new(1),
        EntityId::new(4),
        &first_coordinates,
    );
    let second = QdrantPoint::new(
        authority,
        PartitionId::new(2),
        EntityId::new(7),
        &second_coordinates,
    );
    let points = [first, second];
    let receipt = adapter.upsert(&points)?;
    assert_eq!(receipt.attempted(), 2);
    assert_eq!(receipt.verified(), 2);

    let keys = [first.key(), second.key()];
    let mut readback = [None, None];
    assert_eq!(adapter.readback(&keys, &mut readback)?, 2);
    let Some(first_readback) = readback.first().and_then(Option::as_ref) else {
        return Err(QdrantError::MalformedResponse {
            phase: nudox_index_qdrant::RequestPhase::ReadPoints,
            detail: "first readback",
        });
    };
    assert_eq!(first_readback.key(), first.key());
    assert_eq!(first_readback.coordinates(), &[1.0, 0.0]);

    let mut remote = [None, None];
    let remote_outcome = adapter.query(
        &[PartitionId::new(1), PartitionId::new(2)],
        &[0, 0],
        2,
        &mut remote,
    )?;
    assert_eq!(remote_outcome.written(), 2);
    assert!(remote_outcome.terminal().is_complete());
    assert_eq!(remote[0].map(|hit| hit.entity()), Some(EntityId::new(4)));
    assert_eq!(remote[1].map(|hit| hit.entity()), Some(EntityId::new(7)));

    let mut local = [None, None];
    let local_outcome = adapter.local_brute_force(
        &[PartitionId::new(1), PartitionId::new(2)],
        &points,
        &[0, 0],
        2,
        &mut local,
    )?;
    assert_eq!(local_outcome.written(), 2);
    assert_eq!(
        local.map(|hit| hit.map(|hit| hit.entity())),
        remote.map(|hit| hit.map(|hit| hit.entity()))
    );
    assert_eq!(
        local.map(|hit| hit.map(|hit| hit.score())),
        remote.map(|hit| hit.map(|hit| hit.score()))
    );

    let repeat = adapter.upsert(&points)?;
    assert_eq!(repeat.attempted(), 2);
    assert_eq!(repeat.verified(), 2);
    let deleted = adapter.delete(&[first.key()])?;
    assert_eq!(deleted.attempted(), 1);
    assert_eq!(deleted.verified(), 1);

    let mut after_delete = [None, None];
    let outcome = adapter.query(
        &[PartitionId::new(1), PartitionId::new(2)],
        &[0, 0],
        2,
        &mut after_delete,
    )?;
    assert_eq!(outcome.written(), 1);
    assert_eq!(outcome.terminal().missing_len(), 1);
    assert_eq!(outcome.terminal().missing_at(0), Some(PartitionId::new(1)));
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
    let attempts = adapter.retry_policy().attempts().get();
    let result = adapter.ensure_collection();
    assert!(matches!(
        result,
        Err(QdrantError::Transport {
            phase: nudox_index_qdrant::RequestPhase::CreateCollection,
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
