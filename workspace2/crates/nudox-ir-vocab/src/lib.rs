#![no_std]

use core::marker::PhantomData;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Entity {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Type {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Atom {}

/// ```compile_fail
/// use nudox_ir_vocab::{EntityId, TypeId};
/// fn entity_only(_: EntityId) {}
/// entity_only(TypeId::new(1));
/// ```
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DenseId<Owner> {
    /// Dense position inside this owner's coordinate space.
    pub raw: u32,
    owner: PhantomData<fn() -> Owner>,
}

impl<Owner> DenseId<Owner> {
    pub const fn new(raw: u32) -> Self {
        Self {
            raw,
            owner: PhantomData,
        }
    }
}

pub type EntityId = DenseId<Entity>;
pub type TypeId = DenseId<Type>;
pub type AtomId = DenseId<Atom>;
