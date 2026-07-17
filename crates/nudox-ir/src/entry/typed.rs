use std::{marker::PhantomData, ops::Deref};

use crate::{
    kind::EntryKind,
    registry::{Registry, RegistryResolver},
};

use super::Entry;

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

    pub fn typed<'r>(&'r self, r: &'r Registry<impl RegistryResolver>) -> &'r T {
        self.kind(r)
            .variant_as_dyn()
            .downcast_ref()
            .expect("using TypedEntry with incorrect type")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::*;

    #[test]
    fn get_allows_typed_access() {
        let registry = dummy_registry();

        let entry = entry("test_sym", n::root(vec![]), Module);

        let entry = TypedEntry::new(&entry);

        let _module: &Module = entry.typed(&registry);
    }
}
