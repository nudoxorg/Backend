use std::{marker::PhantomData, ops::Deref};

use crate::kind::EntryKind;

use super::Entry;

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
    pub(crate) fn new(entry: &Entry) -> &Self {
        // Safety: `TypedEntry` is `repr(transparent)` and there are no possibilities
        // for UB as all operations are checked before accessing the inner Kind
        // variant regardless
        unsafe { &*std::ptr::from_ref(entry).cast() }
    }
}
