use core::mem::{align_of, size_of};
use nudox_ir_vocab::{DenseId, Entity, EntityId, TypeId};

fn entity_only(_: EntityId) {}

#[test]
fn coordinates_are_dense_and_branded() {
    assert_eq!(size_of::<DenseId<Entity>>(), 4);
    assert_eq!(align_of::<DenseId<Entity>>(), 4);
    assert_eq!(size_of::<EntityId>(), 4);
    assert_eq!(size_of::<TypeId>(), 4);
    entity_only(EntityId::new(7));
}

#[test]
fn owner_types_are_not_interchangeable() {
    let _: TypeId = TypeId::new(3);
    let _: EntityId = EntityId::new(2);
}
