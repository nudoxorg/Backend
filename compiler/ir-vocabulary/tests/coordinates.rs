//! Layout proof for the typed dense coordinates shared by IR producers and consumers.
//! Const construction must remain available because schemas embed coordinates at compile time.
//! The phantom owner must add no size or alignment beyond the underlying `u32` position.
use compiler_ir_vocabulary::{DenseId, Entity, EntityId, TypeId};
use core::mem::{align_of, size_of};

const RAW_POSITION: u32 = 7;
const CONST_ENTITY: EntityId = EntityId::new(RAW_POSITION);

#[test]
/// Proves every owner-tagged coordinate retains the exact layout of its raw position.
fn coordinate_layout_and_const_construction_are_exact() {
    assert_eq!(size_of::<DenseId<Entity>>(), size_of::<u32>());
    assert_eq!(align_of::<DenseId<Entity>>(), align_of::<u32>());
    assert_eq!(size_of::<EntityId>(), size_of::<u32>());
    assert_eq!(size_of::<TypeId>(), size_of::<u32>());
    assert_eq!(CONST_ENTITY.raw, RAW_POSITION);
}
