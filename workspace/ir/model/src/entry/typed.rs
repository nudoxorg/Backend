//! `TypedEntry<T>`, a kind-typed view over an `Entry`.
use std::{marker::PhantomData, ops::Deref};

use crate::kind::EntryKind;

use super::{Entry, EntryInner};

/// A typed view over an [`Entry`].
///
/// `TypedEntry<T>` is a wrapper around `Entry` with a phantom type parameter.
/// It derefs to `Entry`, so all entry methods are available without unwrapping.
#[repr(transparent)]
pub struct TypedEntry<T> {
    inner: Entry,
    _p: PhantomData<T>,
}

impl<T> Deref for TypedEntry<T> {
    type Target = Entry;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: EntryKind> TypedEntry<T> {
    /// Constructs a `&TypedEntry<T>` from a raw `&Entry` WITHOUT verifying the
    /// kind discriminant.  Callers must have already confirmed that
    /// `entry.kind()` is `EntryInner::Owned(k)` where
    /// `k.discriminant() == T::discriminant()`.
    ///
    /// # Safety
    ///
    /// `TypedEntry<T>` is `repr(transparent)` over `Entry`.  The only
    /// unsoundness would be calling methods that assume the specific `T`
    /// variant; `body()` does that check via `Any::downcast_ref`, which
    /// panics if the invariant is somehow violated, so the repr cast itself
    /// is always memory-safe.
    pub(crate) fn new_unchecked(entry: &Entry) -> &Self {
        // Safety: see doc comment above.
        unsafe { &*std::ptr::from_ref(entry).cast() }
    }

    /// Returns a reference to the typed body of this entry.
    ///
    /// # Panics
    ///
    /// Panics if the `TypedEntry<T>` invariant is violated (i.e. the entry
    /// was constructed without the matching discriminant check).  This is
    /// unreachable in practice because `Entry::downcast` ensures correctness.
    pub fn body(&self) -> &T {
        self.inner
            .kind()
            .as_owned_kind()
            .expect("TypedEntry invariant violated: entry is a Reference, not Owned")
            .variant_as_dyn()
            .downcast_ref::<T>()
            .expect("TypedEntry kind invariant violated: discriminant mismatch in body()")
    }
}

impl Entry {
    /// Returns `Some(&TypedEntry<T>)` when this entry is an owned entry whose
    /// kind matches `T`, or `None` otherwise (including `Reference` entries).
    pub fn downcast<T: EntryKind>(&self) -> Option<&TypedEntry<T>> {
        match self.kind() {
            EntryInner::Owned(k) if k.discriminant() == T::discriminant() => {
                Some(TypedEntry::new_unchecked(self))
            }
            _ => None,
        }
    }
}
