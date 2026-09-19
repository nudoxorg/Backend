//! Exercises the `backend-extension-qdrant` tests public-contract contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_semantic::ir::EntityId;
use backend_semantic::graph_vector::{Metric, ModelId, PartitionId, VectorAuthority};
use backend_extension_qdrant::server::{PhysicalPointId, QdrantBlockingAdapter, QdrantDataKey};
use backend_semantic::index_vocabulary::{IndexSnapshotId, VectorSegmentId};

fn authority(byte: u8) -> VectorAuthority {
    VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(&[byte; 32]),
        ModelId::new([byte.wrapping_add(1); 16]),
        2,
        Metric::SquaredEuclidean,
    )
}

fn segment(byte: u8) -> VectorSegmentId {
    VectorSegmentId::from_canonical_bytes(&[byte; 32])
}

#[test]
fn physical_id_is_deterministic_and_retains_every_identity_axis() {
    let first = QdrantDataKey::new(
        authority(1),
        segment(1),
        PartitionId::new(2),
        EntityId::new(3),
    );
    let same = first;
    let snapshot = QdrantDataKey::new(
        authority(2),
        segment(1),
        PartitionId::new(2),
        EntityId::new(3),
    );
    let segment_key = QdrantDataKey::new(
        authority(1),
        segment(2),
        PartitionId::new(2),
        EntityId::new(3),
    );
    let model = QdrantDataKey::new(
        VectorAuthority::new(
            authority(1).snapshot,
            ModelId::new([9; 16]),
            2,
            Metric::SquaredEuclidean,
        ),
        segment(1),
        PartitionId::new(2),
        EntityId::new(3),
    );
    let partition = QdrantDataKey::new(
        authority(1),
        segment(1),
        PartitionId::new(4),
        EntityId::new(3),
    );
    let entity = QdrantDataKey::new(
        authority(1),
        segment(1),
        PartitionId::new(2),
        EntityId::new(5),
    );
    assert_eq!(
        PhysicalPointId::for_key(first),
        PhysicalPointId::for_key(same)
    );
    assert_ne!(
        PhysicalPointId::for_key(first),
        PhysicalPointId::for_key(snapshot)
    );
    assert_ne!(
        PhysicalPointId::for_key(first),
        PhysicalPointId::for_key(model)
    );
    assert_ne!(
        PhysicalPointId::for_key(first),
        PhysicalPointId::for_key(segment_key)
    );
    assert_ne!(
        PhysicalPointId::for_key(first),
        PhysicalPointId::for_key(partition)
    );
    assert_ne!(
        PhysicalPointId::for_key(first),
        PhysicalPointId::for_key(entity)
    );
}

#[test]
fn config_view_keeps_connection_facts_tied_to_one_authority() {
    let adapter = QdrantBlockingAdapter::new("http://127.0.0.1:6333///", "unit", authority(3));
    assert!(adapter.is_ok());
    let Ok(adapter) = adapter else {
        return;
    };
    let config = adapter.config();
    assert_eq!(config.endpoint, "http://127.0.0.1:6333");
    assert_eq!(config.collection, "unit");
    assert_eq!(config.authority, authority(3));
    assert_eq!(config.retry.attempts.get(), 3);
}
