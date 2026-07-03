use std::{any::Any, marker::PhantomData};

use super::Entry;
use crate::{kind::{EntryKind, Kind}, symbol::Symbol};

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

	/// the entry's raw kind enum
	pub fn kind(&self) -> &Kind { &self.inner.kind }
}

impl<T: EntryKind> TypedEntry<T> {
	/// # Safety
	///
	/// the caller must guarentee that the variant of Kind is type `T`.
	pub(super) unsafe fn new(entry: &Entry) -> &Self {
		debug_assert!(kind_as_dyn_any(&entry.kind).is::<T>(), "creating invalid TypedEntry");

		// Safety: the caller upholds invariant that the kind of the entry is `T`, and
		// `TypedEntry` is `repr(transparent)` so this is safe
		unsafe { &*std::ptr::from_ref(entry).cast() }
	}

	// TODO: do we want to impl Deref and make this more like a smart pointer?
	pub fn get(&self) -> &T {
		kind_as_dyn_any(&self.inner.kind).downcast_ref().expect("using TypedEntry with incorrect type")
	}
}

// helper function to get a &dyn Any from the inner variant of a Kind as &dyn
// Any, needed because variant_as_dyn returns &dyn EntryKind which doesn't
// implicitly downcast to &dyn Any for some reason?
//
// TODO: ensure that this works as expected through testing, since it could be
// doing <&dyn EntryKind as &dyn Any> instead of trait upcasting
fn kind_as_dyn_any(kind: &Kind) -> &dyn Any { kind.variant_as_dyn() }
