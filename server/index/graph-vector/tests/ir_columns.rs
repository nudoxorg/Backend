//! Proves vector ranking borrows entity-aligned canonical IR columns directly.

use allocation_counter::{AllocationInfo, measure};
use compiler_ir::{
    BorrowedTree, EntityVersion, IrBuilder, ItemKind, PayloadHash, StableEntityId, TreeItemInput,
    Visibility,
};
use server_index_graph_vector::{
    IrVectorColumn, Metric, ModelId, PartitionId, VectorAuthority, exact_ir_vector_query,
};
use server_index_vocabulary::IndexSnapshotId;

fn version(byte: u8) -> EntityVersion {
    EntityVersion {
        stable: StableEntityId::from_raw([byte; 16]),
        payload: PayloadHash::from_raw([byte; 16]),
    }
}

#[test]
fn aligned_ir_vectors_need_no_point_projection() -> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let versions = [version(1), version(2), version(3)];
    let items =
        [b"alpha".as_slice(), b"beta".as_slice(), b"gamma".as_slice()].map(|name| TreeItemInput {
            name,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        });
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let ir = builder.finish()?;
    let authority = VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(b"aligned-ir-vector"),
        ModelId::new([7; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let ordinals = [1, u32::MAX, 0];
    let coordinates = [0, 0, 10, 10];
    let column =
        IrVectorColumn::try_new(&ir, authority, PartitionId::new(0), &ordinals, &coordinates)
            .expect("aligned vector column");
    let mut output = [None; 2];
    let mut result = None;
    let allocations = measure(|| {
        result = Some(exact_ir_vector_query(column, &[1, 1], 2, &mut output));
    });
    assert_eq!(allocations, AllocationInfo::default());
    let result = result
        .expect("measurement executed")
        .expect("direct vector ranking");
    assert_eq!(result.written, 2);
    assert_eq!(output[0].map(|hit| hit.entity.raw), Some(2));
    assert_eq!(output[1].map(|hit| hit.entity.raw), Some(0));
    Ok(())
}
