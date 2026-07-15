use super::ArenaIdx;
use crate::entry::Entry;

#[derive(Default)]
pub struct EntryArena {
	entries: Vec<Entry>,
}

impl EntryArena {
	#[cfg_attr(not(test), expect(unused))]
	pub(super) fn new() -> Self { EntryArena { entries: Vec::new() } }

	// pub fn create_top_level<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut
	// EntryBuilder) -> T) where
	// 	T: EntryKind,
	// {
	// 	let (entries, links) =
	// 		EntryBuilder::build(EntryIdx::new(self.package, self.len()), sym, None,
	// build);

	// 	self.entries.extend(entries.iter());
	// 	self.links.extend(links.iter());
	// }

	pub fn len(&self) -> usize { self.entries.len() }

	pub(crate) fn resolve(&self, index: ArenaIdx) -> &Entry { &self.entries[index.index()] }

	#[cfg_attr(not(test), expect(unused))]
	pub(crate) fn iter(&self) -> impl Iterator<Item = &Entry> { self.entries.iter() }
}

#[cfg(test)]
mod tests {
	// use super::*;
	// use crate::{module::Module, test_helpers::*};

	// #[test]
	// fn create_top_level_module() {
	// 	let mut arena = EntryArena::new();

	// 	arena.create_top_level(dummy_symbol("module"), |_| Module);

	// 	assert_eq!(arena.entries, [entry("module", n::root(vec![]), Module)]);

	// 	arena.create_top_level(dummy_symbol("module_2"), |_| Module);

	// 	assert_eq!(arena.entries, [
	// 		entry("module", n::root(vec![]), Module),
	// 		entry("module_2", n::root(vec![]), Module)
	// 	])
	// }
}
