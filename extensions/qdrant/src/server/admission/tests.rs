//! Defines admission tests behavior for `backend-extension-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the admission tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use super::{prepare_keys, reject_identity_pair};
use crate::server::contract::{PhysicalPointId, QdrantAdmissionError, QdrantDataKey};
use server_index_graph_vector::{Metric, ModelId, PartitionId, VectorAuthority};
use server_index_vocabulary::{IndexSnapshotId, VectorSegmentId};

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
fn collision_checker_rejects_same_physical_id_for_distinct_keys() {
    let first = QdrantDataKey::new(
        authority(1),
        segment(1),
        PartitionId::new(1),
        backend_semantic::ir::EntityId::new(1),
    );
    let second = QdrantDataKey::new(
        authority(1),
        segment(1),
        PartitionId::new(2),
        backend_semantic::ir::EntityId::new(3),
    );
    let physical_id = PhysicalPointId(42);
    assert_eq!(
        reject_identity_pair(0, first, physical_id, 1, second, physical_id),
        Err(QdrantAdmissionError::PhysicalIdCollision {
            physical_id,
            first: first.into(),
            second: second.into(),
        })
    );
}

#[test]
fn prepared_identity_derives_each_physical_coordinate_from_its_key() {
    let key = QdrantDataKey::new(
        authority(2),
        segment(2),
        PartitionId::new(4),
        backend_semantic::ir::EntityId::new(8),
    );
    let expected = PhysicalPointId::for_key(key);
    let prepared = prepare_keys(key.authority, &[key]);
    assert!(prepared.is_ok());
    let Ok(prepared) = prepared else {
        return;
    };
    assert_eq!(
        prepared.first().map(|point| point.physical_id),
        Some(expected)
    );
}
