use std::collections::HashSet;

use super::{EntryBuilder, EntryIdx, RawEntryIdx};
use crate::{entry::Entry, kind::{EntryKind, KindDiscriminant}, symbol::Symbol};

#[derive(Default)]
pub struct EntryArena {
	package: usize,
	entries: Vec<Entry>,
	links:   HashSet<EntryLink>,
}

// TODO: figure out how to make this cleanly two-way?
//       or decide if we support directed edges
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EntryLink {
	pub(super) a: (RawEntryIdx, KindDiscriminant),
	pub(super) b: (RawEntryIdx, KindDiscriminant),
}

impl EntryArena {
	#[cfg_attr(not(test), expect(unused))]
	pub(crate) fn new(package: usize) -> Self {
		EntryArena { package, entries: Vec::new(), links: HashSet::new() }
	}

	pub fn create_top_level<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut EntryBuilder) -> T)
	where
		T: EntryKind,
	{
		let (entries, links) =
			EntryBuilder::build(EntryIdx::new(self.package, self.len()), sym, None, build);

		self.entries.extend(entries.iter());
		self.links.extend(links.iter());
	}

	pub fn len(&self) -> usize { self.entries.len() }

	pub(crate) fn resolve(&self, index: usize) -> &Entry { &self.entries[index] }

	#[cfg_attr(not(test), expect(unused))]
	pub(crate) fn iter(&self) -> impl Iterator<Item = &Entry> { self.entries.iter() }
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{entry::Node, kind::Kind, module::Module, test_helpers::*};

	#[test]
	fn create_top_level_module() {
		let mut arena = EntryArena::new(0);

		arena.create_top_level(dummy_symbol("module"), |_| Module {});

		assert_eq!(arena.entries, [Entry::new(
			dummy_symbol("module"),
			Node { parent: None, children: Vec::new() },
			Kind::Module(Module {})
		)]);

		arena.create_top_level(dummy_symbol("module_2"), |_| Module {});

		assert_eq!(arena.entries, [
			Entry::new(
				dummy_symbol("module"),
				Node { parent: None, children: Vec::new() },
				Kind::Module(Module {})
			),
			Entry::new(
				dummy_symbol("module_2"),
				Node { parent: None, children: Vec::new() },
				Kind::Module(Module {})
			)
		])
	}
}
