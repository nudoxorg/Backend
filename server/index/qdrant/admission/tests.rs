//! Defines admission tests behavior for `server-index-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the admission tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use super::{prepare_ir_column, prepare_keys, reject_identity_pair};
use crate::contract::{PhysicalPointId, QdrantAdmissionError, QdrantDataKey};
use allocation_counter::{AllocationInfo, measure};
use compiler_ir::{
    BorrowedTree, EntityId, EntityVersion, IrBuilder, ItemKind, PayloadHash, StableEntityId,
    TreeItemInput, Visibility,
};
use server_index_graph_vector::{IrVectorColumn, Metric, ModelId, PartitionId, VectorAuthority};
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
fn canonical_ir_column_enters_qdrant_without_point_or_segment_projection()
-> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let versions = [EntityVersion {
        stable: StableEntityId::from_raw([1; 16]),
        payload: PayloadHash::from_raw([2; 16]),
    }];
    let items = [TreeItemInput {
        name: b"entity",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        typescript: None,
    }];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let ir = builder.finish()?;
    let authority = authority(7);
    let ordinals = [0];
    let coordinates = [11, -4];
    let column =
        IrVectorColumn::try_new(&ir, authority, PartitionId::new(3), &ordinals, &coordinates)
            .expect("aligned vector column");
    let mut prepared = None;
    let allocations = measure(|| {
        prepared = Some(prepare_ir_column(authority, segment(9), column));
    });
    assert_eq!(allocations, AllocationInfo::default());
    let prepared = prepared
        .expect("measurement executed")
        .expect("prepared IR lane");
    assert_eq!(prepared.len(), 1);
    assert_eq!(prepared[0].key.entity, EntityId::new(0));
    assert_eq!(prepared[0].coordinates, coordinates);
    Ok(())
}

#[test]
fn collision_checker_rejects_same_physical_id_for_distinct_keys() {
    let first = QdrantDataKey::new(
        authority(1),
        segment(1),
        PartitionId::new(1),
        EntityId::new(1),
    );
    let second = QdrantDataKey::new(
        authority(1),
        segment(1),
        PartitionId::new(2),
        EntityId::new(3),
    );
    let physical_id = crate::contract::PhysicalPointId(42);
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
        EntityId::new(8),
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
