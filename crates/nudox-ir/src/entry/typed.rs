use std::marker::PhantomData;

use super::{Entry, EntryInner};
use crate::{kind::{EntryKind, Kind}, registry::RegistryResolver, symbol::Symbol};

#[repr(transparent)]
pub struct TypedEntry<T> {
	inner: Entry,
	_p:    PhantomData<T>,
}

impl<T> TypedEntry<T> {
	/// the raw (untyped) inner entry
	pub fn entry(&self) -> &Entry { &self.inner }

	/// the entry's symbol
	pub fn sym(&self) -> &Symbol { &self.inner.sym }
}

impl<T: EntryKind> TypedEntry<T> {
	pub(crate) fn new(entry: &Entry) -> &Self {
		// Safety: `TypedEntry` is `repr(transparent)` and there are no possibilities
		// for UB as all operations are checked before accessing the inner Kind
		// variant regardless
		unsafe { &*std::ptr::from_ref(entry).cast() }
	}

	/// the entry's raw kind enum
	pub fn kind<'a>(&'a self, r: &'a impl RegistryResolver) -> &'a Kind {
		match &self.inner.kind {
			EntryInner::Owned(kind) => &kind,
			EntryInner::Reference(idx) => r.resolve(idx.typed::<T>()).kind(r),
		}
	}

	// TODO: do we want to impl Deref and make this more like a smart pointer?
	pub fn get<'a>(&'a self, r: &'a impl RegistryResolver) -> &'a T {
		self.kind(r).variant_as_dyn().downcast_ref().expect("using TypedEntry with incorrect type")
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_helpers::*;

	#[test]
	fn get_allows_typed_access() {
		let entry = entry("test_sym", n::root(vec![]), Module);

		let entry = TypedEntry::new(&entry);

		let _module: &Module = entry.get(&DummyRegistry);
	}
}
