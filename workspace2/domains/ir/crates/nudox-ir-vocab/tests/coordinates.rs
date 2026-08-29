use core::mem::{align_of, size_of};
use nudox_ir_vocab::{DenseId, Entity, EntityId, TypeId};

#[test]
fn coordinate_layout_is_exact_and_raw_position_is_named() {
    const RAW_POSITION: u32 = 7;

    assert_eq!(size_of::<DenseId<Entity>>(), size_of::<u32>());
    assert_eq!(align_of::<DenseId<Entity>>(), align_of::<u32>());
    assert_eq!(size_of::<EntityId>(), size_of::<u32>());
    assert_eq!(size_of::<TypeId>(), size_of::<u32>());
    assert_eq!(EntityId::new(RAW_POSITION).raw, RAW_POSITION);
}
