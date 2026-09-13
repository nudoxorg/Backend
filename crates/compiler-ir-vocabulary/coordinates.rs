//! Dense, type-separated coordinates for canonical entities, types, and atoms inside one IR fragment.
//! Coordinates remain plain `u32` values in memory while their owner markers prevent lane confusion.
//! Pooled-list spans and their validated projections share the same dense discipline.

use core::{fmt, marker::PhantomData, num::TryFromIntError};

use backend_version::{ContentId, IrFragmentDomain};

/// Marker for the entity coordinate space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Entity {}

/// Marker for the semantic-type coordinate space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Type {}

/// Marker for the interned-atom coordinate space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Atom {}

/// Marker for a recursive semantic-product coordinate in one fragment.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Product {}

/// Marker for a pooled list of semantic-product children.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProductChildren {}

/// Dense fragment-local coordinate whose marker prevents cross-space substitution.
/// ```compile_fail
/// use compiler_ir_vocabulary::{EntityId, TypeId};
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

impl<Owner> fmt::Debug for DenseId<Owner> {
    /// Formats only the meaningful compact coordinate, never its marker.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Id").field(&self.raw).finish()
    }
}

impl<Owner> DenseId<Owner> {
    /// Creates a coordinate from a caller-proved position in the matching owner space.
    pub const fn new(raw: u32) -> Self {
        Self {
            raw,
            owner: PhantomData,
        }
    }

    /// Widens the compact coordinate for direct slice indexing.
    #[must_use]
    #[allow(
        clippy::as_conversions,
        reason = "the crate rejects address spaces narrower than its u32 coordinates"
    )]
    pub const fn index(self) -> usize {
        self.raw as usize
    }

    /// Converts a native index without truncating its high bits.
    pub fn try_from_index(index: usize) -> Result<Self, TryFromIntError> {
        u32::try_from(index).map(Self::new)
    }
}

/// Dense coordinate of an entity row inside one IR fragment.
pub type EntityId = DenseId<Entity>;
/// Dense coordinate of a semantic-type row inside one IR fragment.
pub type TypeId = DenseId<Type>;
/// Dense coordinate of an interned atom inside one IR fragment.
pub type AtomId = DenseId<Atom>;
/// Dense coordinate of a recursive semantic product inside one IR fragment.
pub type ProductId = DenseId<Product>;
/// Dense coordinate inside an arbitrary pooled-owner space.
pub type ListId<Owner> = DenseId<Owner>;
/// Dense coordinate of one product's pooled child-list row.
pub type ProductListId = ListId<ProductChildren>;

/// A borrowed semantic atom. The caller retains the bytes until canonical
/// emission copies the selected value into its output region.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SemanticAtom<'bytes> {
    /// Exact atom bytes retained by the caller.
    pub bytes: &'bytes [u8],
}

impl AsRef<[u8]> for SemanticAtom<'_> {
    /// Borrows the exact atom bytes without a copy.
    fn as_ref(&self) -> &[u8] {
        self.bytes
    }
}

/// The central typed authority that gives an external product ordinal meaning.
pub type ExternalFragmentId = ContentId<IrFragmentDomain>;

/// A kind-owned coordinate inside one external fragment authority.
/// ```compile_fail
/// use compiler_ir_vocabulary::{EntityId, ExternalEntityRef, ExternalFragmentId};
/// fn local_only(_: EntityId) {}
/// fn external_target() -> ExternalEntityRef {
///     let authority = ExternalFragmentId::from_canonical_bytes(b"remote-fragment");
///     compiler_ir_vocabulary::ExternalCoordinate::bind(authority, 3)
/// }
/// local_only(external_target());
/// ```
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExternalCoordinate<ExpectedKind> {
    /// Typed fragment authority that owns `ordinal`.
    pub fragment: ExternalFragmentId,
    /// Dense position inside the external fragment's kind-owned space.
    pub ordinal: u32,
    expected_kind: PhantomData<fn() -> ExpectedKind>,
}

impl<ExpectedKind> ExternalCoordinate<ExpectedKind> {
    /// Binds one external ordinal to its fragment authority and expected kind.
    #[must_use]
    pub const fn bind(fragment: ExternalFragmentId, ordinal: u32) -> Self {
        Self {
            fragment,
            ordinal,
            expected_kind: PhantomData,
        }
    }
}

/// External authority for an entity coordinate.
pub type ExternalEntityRef = ExternalCoordinate<Entity>;
/// External authority for a semantic-type coordinate.
pub type ExternalTypeRef = ExternalCoordinate<Type>;
/// External authority for a recursive semantic-product coordinate.
pub type ExternalProductRef = ExternalCoordinate<Product>;

/// A product-kind-owned range in a caller-owned pooled child lane.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListSpan<Owner> {
    /// First pooled position owned by this list.
    pub start: u32,
    /// Pooled position count owned by this list.
    pub length: u32,
    owner: PhantomData<fn() -> Owner>,
}

impl<Owner> ListSpan<Owner> {
    /// Creates a span from a caller-proved pooled range.
    #[must_use]
    pub const fn new(start: u32, length: u32) -> Self {
        Self {
            start,
            length,
            owner: PhantomData,
        }
    }
}

/// Exact pooled-list rejection retaining every operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PooledListError {
    /// The requested span is outside the caller's pooled lane.
    OutOfBounds {
        /// Requested first position.
        start: u32,
        /// Requested length.
        length: u32,
        /// Complete pool length.
        pool_length: usize,
    },
}
