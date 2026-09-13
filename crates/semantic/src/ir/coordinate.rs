//! Strongly typed dense coordinates shared by every in-memory and wire IR lane.

use core::marker::PhantomData;

pub use crate::ir_vocabulary::{
    Atom as AtomSpace, AtomId, DenseId, Entity, EntityId, Type, TypeId,
};

/// Marker for an atom proven to contain UTF-8 documentation text.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Text {}

/// Marker for an interned list whose elements have type `T`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct List<T>(PhantomData<fn() -> T>);

/// Dense coordinate of a UTF-8 atom. The backing bytes live in the atom arena.
pub type TextId = DenseId<Text>;
/// Dense coordinate of an interned typed list.
pub type ListId<T> = DenseId<List<T>>;
