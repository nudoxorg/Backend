//! `EntryIndex`/`Ref`, the one reference type across the IR lifecycle.
use std::{fmt, hash, marker::PhantomData, num::NonZeroUsize};

use triomphe::Arc;

use crate::{
    change::{IntroId, StableRef},
    foreign::ForeignKey,
    kind::EntryKind,
};

// FIXME: NonZeroUsize handling needs to be carefully considered wrt the bit
// checks we do, and needs plenty of tests

/// A (possibly typed) index into a store of [`Entry`s](crate::entry::Entry)
///
/// `EntryIndex` is the primary way entries reference each other.
///
/// # Internals
///
/// An index can be in one of three states, distinguished by reserved bits in
/// the underlying `usize`:
///
/// | State        | MSB | MSB-1 | Meaning |
/// |--------------|-----|-------|---------|
/// | **Resolved** | 0   | —     | A runtime index into an in-memory entry store (a [`Registry`](crate::registry::Registry)). |
/// | **Export**   | 1   | 0     | A static index into a package's export table. Embedded in serialized IR files. |
/// | **Import**   | 1   | 1     | A static index into a package's import table. Also used in serialized IR. |
///
/// Resolved indices are ephemeral; export/import indices are stable across
/// serialization round-trips (within their
/// [`PackageInfo`](crate::package::PackageInfo)) context.
///
/// # Typed vs untyped
///
/// [`EntryIndex<T>`] carries a type parameter that implements [`Indexable`].
/// When `T` is a concrete [`EntryKind`] like `Record` or `Function`, the index
/// is *typed* and can be used with
/// [`Registry::resolve_typed_entry`](crate::registry::Registry).
///
/// [`UntypedEntryIndex`] (aliased as `EntryIndex<UntypedMarker>`) erases the
/// kind. It is used internally for parent/child links in the IR tree so that
/// nodes can point to entries of any kind.
#[repr(transparent)]
pub struct EntryIndex<T: Indexable> {
    index: NonZeroUsize,
    _p: PhantomData<fn() -> T>,
}

pub type UntypedEntryIndex = EntryIndex<private::UntypedMarker>;

pub trait Indexable: private::Sealed {}

impl<T: EntryKind> Indexable for T {}
impl Indexable for private::UntypedMarker {}

mod private {
    pub struct UntypedMarker;

    pub trait Sealed {}

    impl<T: super::EntryKind> Sealed for T {}

    impl Sealed for UntypedMarker {}
}

// we reserve 2 bits for serialized indices.
// - the MSB is always set to 1 to indicate that we are a serializable index
// - the second MSB is set to 0 for a local index and 1 for an import index
const INDEX_SER_RESERVED_BITS: u32 = 2;
const INDEX_SER_AVAILABLE_MASK: usize = !(usize::MAX << (usize::BITS - INDEX_SER_RESERVED_BITS));

// MSB indicates if we are a serialized index
const IS_SERIALIZED_MASK: usize = 1 << (usize::BITS - 1);

// second MSB indicates an import if it is set
const IS_IMPORT_MASK: usize = 1 << (usize::BITS - 2);

// we reserve the MSB for resolved indices. it should always be 0 for resolved
// indices, and a value of 1 indicates an unresolved index and should panic
const INDEX_RES_RESERVED_BITS: u32 = 1;
const INDEX_RES_AVAILABLE_MASK: usize = !(usize::MAX << (usize::BITS - INDEX_RES_RESERVED_BITS));

impl<T: Indexable> EntryIndex<T> {
    pub(super) fn resolved(index: usize) -> Self {
        debug_assert_eq!(
            index & INDEX_RES_AVAILABLE_MASK,
            index,
            "creating resolved index with unavailable bits"
        );

        Self::build(index)
    }

    pub(super) fn export(index: usize) -> Self {
        debug_assert_eq!(
            index & INDEX_SER_AVAILABLE_MASK,
            index,
            "creating export index with unavailable bits"
        );

        Self::build(index | IS_SERIALIZED_MASK)
    }

    // There is deliberately no `import` constructor. A cross-package reference
    // is a self-describing `Ref::Foreign` (see `crate::foreign`), not an index
    // into an arena `seal` drops. Without a way to mint one, `is_import` is
    // permanently false and `Ref::Local` names exactly one thing: an export
    // index that `seal` rewrites. Re-adding it would make the original defect
    // representable again.

    fn build(index: usize) -> Self {
        assert_ne!(index, usize::MAX);

        EntryIndex {
            index: NonZeroUsize::new(index + 1).unwrap(),
            _p: PhantomData,
        }
    }

    pub(super) fn resolved_index(self) -> usize {
        debug_assert!(self.is_resolved(), "using unresolved EntryIndex");

        self.raw_index()
    }

    pub(super) fn import_index(self) -> usize {
        debug_assert!(self.is_import(), "using non-import EntryIndex");

        self.raw_index() & !(IS_SERIALIZED_MASK | IS_IMPORT_MASK)
    }

    pub(super) fn export_index(self) -> usize {
        debug_assert!(self.is_export(), "using non-export EntryIndex");

        self.raw_index() & !IS_SERIALIZED_MASK
    }

    pub(super) fn is_resolved(self) -> bool {
        // return if the serialized bit is _NOT_ set
        self.raw_index() & IS_SERIALIZED_MASK == 0
    }

    pub(super) fn is_serialize(self) -> bool {
        // return if the serialized bit IS set
        self.raw_index() & IS_SERIALIZED_MASK == IS_SERIALIZED_MASK
    }

    pub(super) fn is_export(self) -> bool {
        debug_assert!(self.is_serialize());

        self.raw_index() & IS_IMPORT_MASK == 0
    }

    pub(super) fn is_import(self) -> bool {
        debug_assert!(self.is_serialize());

        self.raw_index() & IS_IMPORT_MASK == IS_IMPORT_MASK
    }

    pub fn raw(self) -> UntypedEntryIndex {
        self.cast()
    }

    pub fn typed<U>(self) -> EntryIndex<U>
    where
        U: EntryKind,
    {
        self.cast()
    }

    pub(super) fn cast<U: Indexable>(self) -> EntryIndex<U> {
        EntryIndex {
            index: self.index,
            _p: PhantomData,
        }
    }

    fn raw_index(self) -> usize {
        self.index.get() - 1
    }
}

impl<T: Indexable> serde::Serialize for EntryIndex<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::Error;

        if !self.is_serialize() {
            return Err(S::Error::custom(
                "tried to serialize an EntryIdx that is not marked for serialization",
            ));
        }

        self.index.serialize(serializer)
    }
}

impl<'de, T: Indexable> serde::Deserialize<'de> for EntryIndex<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let index = <_>::deserialize(deserializer)?;

        let index = EntryIndex {
            index,
            _p: PhantomData,
        };

        if !index.is_serialize() {
            return Err(D::Error::custom(
                "tried to deserialize an EntryIdx that is not marked for serialization",
            ));
        }

        Ok(index)
    }
}

impl<T: Indexable> Clone for EntryIndex<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Indexable> Copy for EntryIndex<T> {}

impl<T: Indexable> fmt::Debug for EntryIndex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, index) = if self.is_resolved() {
            ("resolved", self.resolved_index())
        } else if self.is_import() {
            ("import", self.import_index())
        } else if self.is_export() {
            ("export", self.export_index())
        } else {
            unreachable!()
        };

        f.debug_struct("EntryIdx")
            .field("kind", &kind)
            .field("index", &index)
            .finish()
    }
}

impl<T: Indexable> PartialEq for EntryIndex<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl<T: Indexable> Eq for EntryIndex<T> {}

impl<T: Indexable> PartialOrd for EntryIndex<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Indexable> Ord for EntryIndex<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Ord::cmp(&self.index, &other.index)
    }
}

impl<T: Indexable> hash::Hash for EntryIndex<T> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}

// ── Ref: the ONE reference to another entry, across its lifecycle ───────────
//
// `Local` while building (arena-local index) → `Intro` after sealing
// (same-package content id) → `Foreign` for a cross-package target. This single
// type is used everywhere an entry points at another: kind bodies (fields,
// params, variants), `Node` tree edges, and `Type` nominal references. `seal`
// lowers every `Local` to `Intro` in one pass via the `Visitor`.

/// A reference to another entry, resolved to whatever stage the IR is at.
///
/// Std trait impls are hand-written (no spurious `T: Trait` bound) because `T`
/// is only a phantom marker — mirroring [`EntryIndex`].
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub enum Ref<T: Indexable> {
    /// Arena-local, build-time only.
    ///
    /// Post-seal this variant names exactly one thing — an export index that
    /// `seal` rewrote — so a `Local` surviving into a sealed table is provably
    /// a seal bug and is reported as [`SealReport::unmapped_local`]. It used to
    /// name *two* things, the second being an index into an import arena `seal`
    /// dropped; that ambiguity is what shipped `?` to the GUI for three years
    /// of language frontends.
    ///
    /// [`SealReport::unmapped_local`]: crate::package::SealReport::unmapped_local
    Local(EntryIndex<T>),
    /// Same-package, content-addressed (post-seal).
    Intro(IntroId),
    /// A cross-package target: always **named**, sometimes **linked**.
    ///
    /// `key` is always present — that is what makes the reference renderable
    /// without a corpus and re-linkable later, and it is what identity and
    /// content hashing encode. `target` is `Some` once some
    /// [`ForeignResolver`](crate::foreign::ForeignResolver) supplied the sealed
    /// identity; `None` means "named but not linked", which the GUI shows as
    /// un-clickable text rather than a dead hyperlink.
    ///
    /// Neither state is dangling — which is the whole point, since the
    /// `Local(import_index)` it replaces was.
    Foreign {
        key: Arc<ForeignKey>,
        target: Option<StableRef>,
    },
}

/// A kind-erased [`Ref`] — the currency of the [`crate::visitor::Visitor`].
pub type RawRef = Ref<private::UntypedMarker>;

impl<T: Indexable> Ref<T> {
    /// The arena-local index, if this is still a build-time reference.
    pub fn as_local(&self) -> Option<EntryIndex<T>> {
        match self {
            Ref::Local(idx) => Some(*idx),
            _ => None,
        }
    }

    /// The cross-package key and its resolved target, if this names another
    /// package.
    ///
    /// Exists so consumers stop re-deriving the same `match` (and stop writing
    /// `_ =>` arms that silently swallow the case — three separate files did).
    pub fn as_foreign(&self) -> Option<(&ForeignKey, Option<&StableRef>)> {
        match self {
            Ref::Foreign { key, target } => Some((key, target.as_ref())),
            Ref::Local(_) | Ref::Intro(_) => None,
        }
    }

    /// Erase the kind marker, by value.
    pub fn into_raw(self) -> RawRef {
        match self {
            Ref::Local(i) => Ref::Local(i.raw()),
            Ref::Intro(i) => Ref::Intro(i),
            Ref::Foreign { key, target } => Ref::Foreign { key, target },
        }
    }
}

/// Erase the kind marker. Safety: `Ref<T>` layout is independent of the
/// phantom `T` (only `Local` carries it, as a `repr(transparent)` index).
pub(crate) fn erase_mut<T: Indexable>(r: &mut Ref<T>) -> &mut RawRef {
    unsafe { &mut *std::ptr::from_mut(r).cast() }
}

impl<T: Indexable> From<EntryIndex<T>> for Ref<T> {
    fn from(idx: EntryIndex<T>) -> Self {
        Ref::Local(idx)
    }
}

impl<T: Indexable> Clone for Ref<T> {
    fn clone(&self) -> Self {
        match self {
            Ref::Local(i) => Ref::Local(*i),
            Ref::Intro(i) => Ref::Intro(*i),
            Ref::Foreign { key, target } => Ref::Foreign {
                key: key.clone(),
                target: target.clone(),
            },
        }
    }
}

impl<T: Indexable> PartialEq for Ref<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Ref::Local(a), Ref::Local(b)) => a == b,
            (Ref::Intro(a), Ref::Intro(b)) => a == b,
            (
                Ref::Foreign {
                    key: ka,
                    target: ta,
                },
                Ref::Foreign {
                    key: kb,
                    target: tb,
                },
            ) => ka == kb && ta == tb,
            _ => false,
        }
    }
}

impl<T: Indexable> Eq for Ref<T> {}

impl<T: Indexable> hash::Hash for Ref<T> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Ref::Local(i) => i.hash(state),
            Ref::Intro(i) => i.hash(state),
            Ref::Foreign { key, target } => {
                key.hash(state);
                target.hash(state);
            }
        }
    }
}

impl<T: Indexable> fmt::Debug for Ref<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Local(i) => f.debug_tuple("Local").field(i).finish(),
            Ref::Intro(i) => f.debug_tuple("Intro").field(i).finish(),
            Ref::Foreign { key, target } => f
                .debug_struct("Foreign")
                .field("key", key)
                .field("target", target)
                .finish(),
        }
    }
}
