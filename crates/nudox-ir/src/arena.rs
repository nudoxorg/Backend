mod build;
mod entry;
mod idx;
mod node;
mod typed;

pub use self::{build::EntryBuilder, entry::Entry, idx::EntryIdx, node::Node, typed::TypedEntry};
use crate::{kind::EntryKind, symbol::Symbol};

#[derive(Default)]
pub struct EntryArena {
	entries: Vec<Entry>,
}

impl EntryArena {
	pub const fn new() -> Self { EntryArena { entries: Vec::new() } }

	pub fn create_top_level<T>(&mut self, _sym: Symbol, _builder: impl FnOnce(&mut EntryBuilder) -> T)
	where
		T: EntryKind,
	{
	}
}

impl<T: EntryKind> std::ops::Index<EntryIdx<T>> for EntryArena {
	type Output = TypedEntry<T>;

	fn index(&self, index: EntryIdx<T>) -> &Self::Output {
		TypedEntry::new(&self.entries[index.index()])
	}
}

#[cfg(test)]
mod tests {
	use ecow::EcoVec;

	use super::*;
	use crate::{kind::Kind, module::Module, test_helpers::*};

	#[test]
	fn create_top_level_module() {
		let mut arena = EntryArena::new();

		arena.create_top_level(dummy_symbol("module"), |_| Module {});

		assert_eq!(arena.entries, [Entry {
			node: Node { parent: None, children: EcoVec::new() },
			sym:  dummy_symbol("module"),
			kind: Kind::Module(Module {}),
		}]);
	}
}
