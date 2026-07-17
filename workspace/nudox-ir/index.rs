//! Arena index types: `ArenaIdx`, `PackageIdx`, typed `EntryIdx<T>`, and
//! the sealed `EntryKind` trait that gates typed retrieval in [`crate::registry`].
//!
//! # Design rationale
//!
//! The arena-local `ArenaIdx` and the cross-package `PackageIdx` together form
//! `EntryIdx<T>`, a *typed* handle that encodes both *which* package's arena and
//! *which* slot within it. The phantom `T` is the kind marker (e.g. `ModuleMarker`)
//! and is always `fn() -> T` to keep the handle invariant and `Copy`.
//!
//! The `EntryKind` sealed trait ensures only well-formed kind markers (defined in
//! this crate) can be used as `T`, preventing accidental use of foreign types.

use core::marker::PhantomData;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public module sealed — kind.rs accesses it via `crate::index::sealed`
// ---------------------------------------------------------------------------

/// Sealing module. Prevents external crates from implementing [`EntryKind`].
pub(crate) mod sealed {
    /// Marker trait; not visible outside this crate.
    pub trait Sealed {}
}

// ---------------------------------------------------------------------------
// ArenaIdx
// ---------------------------------------------------------------------------

/// A zero-based index into a per-package [`crate::entry::EntryArena`].
///
/// Arena indices are only valid for the specific arena they were obtained from;
/// they are **not** stable across arena reconstructions or serialization.
/// Cross-session identity is carried by [`nudox_change::IntroId`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct ArenaIdx(pub u32);

// ---------------------------------------------------------------------------
// PackageIdx
// ---------------------------------------------------------------------------

/// Index of a package in a multi-package registry session.
///
/// Assigned at load time; not stable across process restarts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PackageIdx(pub u32);

// ---------------------------------------------------------------------------
// EntryIdx<T>
// ---------------------------------------------------------------------------

/// A typed, compound handle into a specific package's arena slot.
///
/// The phantom type `T` must implement [`EntryKind`]; it tags the expected kind
/// of the pointed-to entry (e.g. `ModuleMarker`, `FunctionMarker`). The handle
/// is `Copy` without requiring `T: Copy`.
pub struct EntryIdx<T: ?Sized> {
    /// Which package's arena this index refers to.
    pub package_idx: PackageIdx,
    /// The slot within that arena.
    pub arena_idx: ArenaIdx,
    /// Variance: `fn() -> T` keeps the idx invariant in `T`.
    pub(crate) _p: PhantomData<fn() -> T>,
}

impl<T: ?Sized> Clone for EntryIdx<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for EntryIdx<T> {}

impl<T: ?Sized> PartialEq for EntryIdx<T> {
    fn eq(&self, other: &Self) -> bool {
        self.package_idx == other.package_idx && self.arena_idx == other.arena_idx
    }
}

impl<T: ?Sized> Eq for EntryIdx<T> {}

impl<T: ?Sized> core::hash::Hash for EntryIdx<T> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.package_idx.hash(state);
        self.arena_idx.hash(state);
    }
}

impl<T: ?Sized> core::fmt::Debug for EntryIdx<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EntryIdx")
            .field("pkg", &self.package_idx)
            .field("arena", &self.arena_idx)
            .finish()
    }
}

impl<T: ?Sized> EntryIdx<T> {
    /// Construct a new typed handle. Caller must ensure the slot is populated
    /// with an entry of kind `T`.
    #[inline]
    pub fn new(package_idx: PackageIdx, arena_idx: ArenaIdx) -> Self {
        Self { package_idx, arena_idx, _p: PhantomData }
    }

    /// Erase the kind marker, yielding a [`RawEntryIdx`].
    #[inline]
    pub fn erase(self) -> RawEntryIdx {
        EntryIdx { package_idx: self.package_idx, arena_idx: self.arena_idx, _p: PhantomData }
    }
}

// ---------------------------------------------------------------------------
// UntypedMarker / RawEntryIdx
// ---------------------------------------------------------------------------

/// Kind marker for untyped (erased) entry handles.
///
/// Used wherever a kind-generic `EntryIdx` is needed (parent/child links,
/// `Node` children, etc.).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct UntypedMarker;

impl sealed::Sealed for UntypedMarker {}
impl EntryKind for UntypedMarker {}

/// An entry index with no kind constraint — valid for any slot.
///
/// Produced by [`EntryIdx::erase`] or constructed directly for parent/child
/// pointers that are kind-agnostic.
pub type RawEntryIdx = EntryIdx<UntypedMarker>;

// ---------------------------------------------------------------------------
// StrId
// ---------------------------------------------------------------------------

/// A handle into the per-arena string interner.
///
/// Compact (4 bytes) and `Copy`; use [`crate::entry::StringInterner::resolve`]
/// to recover the `&str`. IDs are not stable across arenas.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct StrId(pub u32);

// ---------------------------------------------------------------------------
// LinkId
// ---------------------------------------------------------------------------

/// A process-local handle to a link record in the in-memory link table.
///
/// Not content-addressed; used only in session-local data structures.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct LinkId(pub u32);

// ---------------------------------------------------------------------------
// TypeFingerprintId
// ---------------------------------------------------------------------------

/// The first 4 bytes (LE u32) of `blake3("nudox.tyskel.v1" || skeleton_bytes)`.
///
/// Used as a cheap structural type-equality filter before comparing full skeletons.
/// See [`crate::skeleton::type_fingerprint`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct TypeFingerprintId(pub u32);

// ---------------------------------------------------------------------------
// EntryKind sealed trait
// ---------------------------------------------------------------------------

/// Sealed marker trait for kind-phantom types.
///
/// Only types defined in this crate (the `*Marker` structs in `kind.rs` plus
/// [`UntypedMarker`]) implement this trait.  External crates cannot add new
/// implementations, preventing accidental misuse of [`EntryIdx`].
pub trait EntryKind: sealed::Sealed {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_idx_copy_and_eq() {
        let a = ArenaIdx(42);
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn entry_idx_erase_round_trip() {
        let idx: EntryIdx<UntypedMarker> = EntryIdx::new(PackageIdx(1), ArenaIdx(5));
        let raw = idx.erase();
        assert_eq!(raw.package_idx, PackageIdx(1));
        assert_eq!(raw.arena_idx, ArenaIdx(5));
    }
}
