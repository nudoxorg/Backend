use core::mem::{align_of, size_of};
use nudox_ir_vocab::{DenseId, Entity, EntityId, TypeId};

const RAW_POSITION: u32 = 7;
const CONST_ENTITY: EntityId = EntityId::new(RAW_POSITION);

#[test]
fn coordinate_layout_and_const_construction_are_exact() {
    assert_eq!(size_of::<DenseId<Entity>>(), size_of::<u32>());
    assert_eq!(align_of::<DenseId<Entity>>(), align_of::<u32>());
    assert_eq!(size_of::<EntityId>(), size_of::<u32>());
    assert_eq!(size_of::<TypeId>(), size_of::<u32>());
    assert_eq!(CONST_ENTITY.raw, RAW_POSITION);
}
