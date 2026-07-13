use std::{any::Any, marker::PhantomData};

use super::{Entry, EntryInner};
use crate::{kind::{EntryKind, Kind}, registry::Registry, symbol::Symbol};

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

#[expect(private_bounds)]
impl<T: EntryKind> TypedEntry<T> {
	pub(crate) fn new(entry: &Entry) -> &Self {
		// Safety: `TypedEntry` is `repr(transparent)` and there are no possibilities
		// for UB as all operations are checked before accessing the inner Kind
		// variant regardless
		unsafe { &*std::ptr::from_ref(entry).cast() }
	}

	/// the entry's raw kind enum
	pub fn kind<'a>(&'a self, r: &'a impl Registry) -> &'a Kind {
		match &self.inner.kind {
			EntryInner::Owned(kind) => &kind,
			EntryInner::Reference(idx) => r.resolve(idx.typed::<T>()).kind(r),
		}
	}

	// TODO: do we want to impl Deref and make this more like a smart pointer?
	pub fn get<'a>(&'a self, r: &'a impl Registry) -> &'a T {
		kind_as_dyn_any(self.kind(r)).downcast_ref().expect("using TypedEntry with incorrect type")
	}
}

// helper function to get a &dyn Any from the inner variant of a Kind as &dyn
// Any, needed because variant_as_dyn returns &dyn EntryKind which doesn't
// implicitly downcast to &dyn Any for some reason?
fn kind_as_dyn_any(kind: &Kind) -> &dyn Any { kind.variant_as_dyn() }

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{entry::{Entry, Node}, kind::Kind, module::Module, test_helpers::*};

	#[test]
	fn get_allows_typed_access() {
		let entry = Entry::new(
			dummy_symbol("test_sym"),
			Node { parent: None, children: Vec::new() },
			Kind::Module(Module {}),
		);

		let entry = TypedEntry::new(&entry);

		let _module: &Module = entry.get(&DummyRegistry);
	}
}
