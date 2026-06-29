use std::{any::Any, marker::PhantomData};

use super::Entry;
use crate::{kind::{EntryKind, Kind}, symbol::Symbol};

#[repr(transparent)]
pub struct TypedEntry<T> {
	inner: Entry,
	_p:    PhantomData<T>,
}

impl<T: EntryKind> TypedEntry<T> {
	pub(super) unsafe fn new(entry: &Entry) -> &Self {
		debug_assert!(kind_as_dyn_any(&entry.kind).is::<T>(), "creating invalid TypedEntry");

		// Safety: the caller upholds invariant that the kind of the entry is `T`, and
		// `TypedEntry` is `repr(transparent)` so this is safe
		unsafe { &*std::ptr::from_ref(entry).cast() }
	}

	pub fn sym(&self) -> &Symbol { &self.inner.sym }

	pub fn kind(&self) -> &T {
		let dyn_kind = kind_as_dyn_any(&self.inner.kind);

		// guard UB from broken invariants in debug mode
		debug_assert!(dyn_kind.is::<T>(), "TypedEntry broken invariant");

		// Safety: this is upheld as an invariant of `TypedEntry`
		unsafe { dyn_kind.downcast_unchecked_ref() }
	}
}

fn kind_as_dyn_any(kind: &Kind) -> &dyn Any { kind.as_dyn() }
