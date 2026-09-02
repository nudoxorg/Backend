//! Strongly typed dense coordinates shared by every in-memory and wire IR lane.

use core::{fmt, marker::PhantomData, num::TryFromIntError};

/// Marker for the entity coordinate space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Entity {}

/// Marker for the semantic-type coordinate space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Type {}

/// Marker for the interned-atom coordinate space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AtomSpace {}

/// Marker for an atom proven to contain UTF-8 documentation text.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Text {}

/// Marker for an interned list whose elements have type `T`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct List<T>(PhantomData<fn() -> T>);

/// Dense arena coordinate whose marker prevents cross-space substitution.
///
/// ```compile_fail
/// use compiler_ir::{EntityId, TypeId};
/// fn entity_only(_: EntityId) {}
/// entity_only(TypeId::new(1));
/// ```
#[repr(transparent)]
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DenseId<Owner> {
    /// Dense position inside this owner's coordinate space.
    pub raw: u32,
    owner: PhantomData<fn() -> Owner>,
}

impl<Owner> DenseId<Owner> {
    /// Creates a coordinate from a caller-proved position in the matching arena.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self {
            raw,
            owner: PhantomData,
        }
    }

    /// Returns the coordinate as an indexing value on this host.
    #[must_use]
    #[allow(
        clippy::as_conversions,
        reason = "u32 is admitted on every supported host"
    )]
    pub const fn index(self) -> usize {
        self.raw as usize
    }

    /// Converts a host index without truncation.
    pub fn try_from_index(index: usize) -> Result<Self, TryFromIntError> {
        u32::try_from(index).map(Self::new)
    }
}

impl<Owner> fmt::Debug for DenseId<Owner> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Id").field(&self.raw).finish()
    }
}

/// Dense coordinate of an entity row.
pub type EntityId = DenseId<Entity>;
/// Dense coordinate of an interned semantic type.
pub type TypeId = DenseId<Type>;
/// Dense coordinate of an interned atom.
pub type AtomId = DenseId<AtomSpace>;
/// Dense coordinate of a UTF-8 atom. The backing bytes live in the atom arena.
pub type TextId = DenseId<Text>;
/// Dense coordinate of an interned typed list.
pub type ListId<T> = DenseId<List<T>>;
