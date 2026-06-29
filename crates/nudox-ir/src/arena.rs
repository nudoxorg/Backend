mod entry;
mod idx;
mod node;
mod typed;

pub use self::{entry::Entry, idx::EntryIdx, node::Node, typed::TypedEntry};
use crate::kind::EntryKind;

#[derive(Default)]
pub struct EntryArena {
	entries: Vec<Entry>,
}

impl EntryArena {
	pub const fn new() -> Self { EntryArena { entries: Vec::new() } }

	// TODO: implement builder API surface
}

impl<T: EntryKind> std::ops::Index<EntryIdx<T>> for EntryArena {
	type Output = TypedEntry<T>;

	fn index(&self, index: EntryIdx<T>) -> &Self::Output {
		// Safety: creating a EntryIdx<T> upholds that it points to an entry of the
		// correct type (T)
		unsafe { TypedEntry::new(&self.entries[index.index()]) }
	}
}
