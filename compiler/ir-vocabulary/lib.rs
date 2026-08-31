//! Dense, type-separated coordinates for canonical entities, types, and atoms inside one IR fragment.
//! Coordinates remain plain `u32` values in memory while their owner markers prevent lane confusion.
//! This crate contains no format policy, allowing producers and index consumers to share it cheaply.
#![no_std]

use core::marker::PhantomData;

/// Marker for the entity coordinate space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Entity {}

/// Marker for the semantic-type coordinate space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Type {}

/// Marker for the interned-atom coordinate space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Atom {}

/// Dense fragment-local coordinate whose marker prevents cross-space substitution.
/// ```compile_fail
/// use compiler_ir_vocabulary::{EntityId, TypeId};
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
    /// Creates a coordinate from a caller-proved position in the matching owner space.
    pub const fn new(raw: u32) -> Self {
        Self {
            raw,
            owner: PhantomData,
        }
    }
}

/// Dense coordinate of an entity row inside one IR fragment.
pub type EntityId = DenseId<Entity>;
/// Dense coordinate of a semantic-type row inside one IR fragment.
pub type TypeId = DenseId<Type>;
/// Dense coordinate of an interned atom inside one IR fragment.
pub type AtomId = DenseId<Atom>;
