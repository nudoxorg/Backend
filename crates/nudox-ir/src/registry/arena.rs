use super::ArenaIdx;
use crate::entry::Entry;

pub(super) struct EntryArena {
	entries: Vec<Entry>,
}

impl EntryArena {
	pub(super) fn new(entries: Vec<Entry>) -> Self { EntryArena { entries } }

	pub(super) fn entry(&self, index: ArenaIdx) -> &Entry { &self.entries[index.index()] }

	pub(super) fn iter(&self) -> impl Iterator<Item = &Entry> { self.entries.iter() }
}
