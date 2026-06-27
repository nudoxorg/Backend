mod entry;
mod idx;
mod typed;

use nonmax::NonMaxUsize;

use self::typed::TypedEntry;
pub use self::{entry::Entry, idx::EntryIdx};
use crate::{kind::EntryKind, symbol::Symbol};

pub struct EntryArena {
	entries: Vec<Entry>,
}

impl EntryArena {
	pub fn insert<K: EntryKind>(&mut self, sym: Symbol, kind: K) -> EntryIdx<K> {
		let entry = Entry { sym, kind: EntryKind::into_kind(kind) };

		let index = self.insert_raw(entry);

		EntryIdx::new(index)
	}

	fn insert_raw(&mut self, entry: Entry) -> NonMaxUsize {
		// this never fails as Vec::len is never greater than isize::MAX. in fact, we
		// could even use new_unchecked, but Vec::len has an intrinsic that ensures that
		// len is <= T::MAX_SLICE_LEN which is < usize::MAX for all cases, therefore the
		// unwrap should be optimized out and there isn't a reason to introduce unsafe
		let index = NonMaxUsize::new(self.entries.len()).unwrap();

		self.entries.push(entry);

		index
	}
}

impl<T: EntryKind> std::ops::Index<EntryIdx<T>> for EntryArena {
	type Output = TypedEntry<T>;

	fn index(&self, index: EntryIdx<T>) -> &Self::Output {
		// Safety: creating a EntryIdx uphold that
		unsafe { TypedEntry::new(&self.entries[index.index()]) }
	}
}
