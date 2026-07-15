use crate::{kind::{EntryKind, KindDiscriminant}, registry::RawEntryIdx};

use super::EntryIdx;

pub type LinkVertex = (RawEntryIdx, KindDiscriminant);

// TODO: figure out how to make this cleanly two-way?
//       or decide if we support directed edges
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntryLink {
	inner: [(RawEntryIdx, KindDiscriminant); 2],
}

impl EntryLink {
	pub(crate) fn new(a: LinkVertex, b: LinkVertex) -> Self {
		let mut inner = [a, b];
		inner.sort_by_key(|&(idx, _)| idx);
		EntryLink { inner }
	}

	pub(crate) fn typed<T, U>(a: EntryIdx<T>, b: EntryIdx<U>) -> Self
	where
		T: EntryKind,
		U: EntryKind,
	{
		Self::new((a.raw(), T::discriminant()), (b.raw(), U::discriminant()))
	}
}
