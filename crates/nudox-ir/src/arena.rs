mod build;
mod entry;
mod idx;
mod node;
mod typed;

use std::collections::HashSet;

pub use self::{build::EntryBuilder, entry::Entry, idx::{EntryIdx, RawEntryIdx}, node::Node, typed::TypedEntry};
use crate::{kind::{EntryKind, KindDiscriminant}, symbol::Symbol};

#[derive(Default)]
pub struct EntryArena {
	entries: Vec<Entry>,
	links:   HashSet<EntryLink>,
}

// TODO: figure out how to make this cleanly two-way?
//       or decide if we support directed edges
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EntryLink {
	a: (RawEntryIdx, KindDiscriminant),
	b: (RawEntryIdx, KindDiscriminant),
}

impl EntryArena {
	pub fn new() -> Self { EntryArena { entries: Vec::new(), links: HashSet::new() } }

	#[expect(private_bounds)]
	pub fn create_top_level<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut EntryBuilder) -> T)
	where
		T: EntryKind,
	{
		let (entries, links) = EntryBuilder::build(self.len(), sym, None, build);

		self.entries.extend(entries.iter());
		self.links.extend(links.iter());
	}

	pub fn len(&self) -> usize { self.entries.len() }
}

impl<T: EntryKind> std::ops::Index<EntryIdx<T>> for EntryArena {
	type Output = TypedEntry<T>;

	fn index(&self, index: EntryIdx<T>) -> &Self::Output {
		TypedEntry::new(&self.entries[index.index()])
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{kind::Kind, module::Module, test_helpers::*};

	#[test]
	fn create_top_level_module() {
		let mut arena = EntryArena::new();

		arena.create_top_level(dummy_symbol("module"), |_| Module {});

		assert_eq!(arena.entries, [Entry {
			node: Node { parent: None, children: Vec::new() },
			sym:  dummy_symbol("module"),
			kind: Kind::Module(Module {}),
		}]);

		arena.create_top_level(dummy_symbol("module_2"), |_| Module {});

		assert_eq!(arena.entries, [
			Entry {
				node: Node { parent: None, children: Vec::new() },
				sym:  dummy_symbol("module"),
				kind: Kind::Module(Module {}),
			},
			Entry {
				node: Node { parent: None, children: Vec::new() },
				sym:  dummy_symbol("module_2"),
				kind: Kind::Module(Module {}),
			}
		])
	}
}
