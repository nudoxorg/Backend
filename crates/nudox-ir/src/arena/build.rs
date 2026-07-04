use super::{Entry, Node, RawEntryIdx};
use crate::{kind::EntryKind, symbol::Symbol};

pub struct EntryBuilder {
	sym:      Symbol,
	idx:      usize,
	parent:   Option<RawEntryIdx>,
	children: Vec<BuiltEntries>,
}

impl EntryBuilder {
	pub(super) fn root(idx: usize, sym: Symbol) -> Self {
		EntryBuilder { idx, sym, parent: None, children: Vec::new() }
	}

	pub(super) fn build<T>(mut self, build: impl FnOnce(&mut Self) -> T) -> BuiltEntries
	where
		T: EntryKind,
	{
		let kind = build(&mut self).into_kind();

		let EntryBuilder { idx, sym, parent, children } = self;

		let idx = RawEntryIdx::new(idx);
		let node = Node { parent, children: children.iter().map(|e| e.idx).collect() };

		BuiltEntries { idx, entry: Entry { node, sym, kind }, children }
	}
}

pub(super) struct BuiltEntries {
	idx:      RawEntryIdx,
	entry:    Entry,
	children: Vec<BuiltEntries>,
}

impl BuiltEntries {
	pub(super) fn iter(self) -> impl Iterator<Item = Entry> {
		use std::iter::*;

		let first = once(self.entry);

		let rest: Box<dyn Iterator<Item = Entry>> =
			Box::new(self.children.into_iter().flat_map(BuiltEntries::iter));

		chain(first, rest)
	}

	/// helper to return the `Entry` alongside the corresponding `RawEntryIdx`
	///
	/// used for tests to ensure that the `RawEntryIdx`'s that are created are
	/// accurate w.r.t. the initial provided index when building `BuiltEntries`
	#[cfg(test)]
	pub(super) fn enumerate(self) -> impl Iterator<Item = (RawEntryIdx, Entry)> {
		use std::iter::*;

		let first = once((self.idx, self.entry));

		let rest: Box<dyn Iterator<Item = _>> =
			Box::new(self.children.into_iter().flat_map(BuiltEntries::enumerate));

		chain(first, rest)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{kind::Kind, module::Module, test_helpers::dummy_symbol};

	#[test]
	fn built_entries_single_entry_indices() {
		let built = EntryBuilder::root(0x100, dummy_symbol("mod42")).build(|_| Module {});

		itertools::assert_equal(built.enumerate(), [(RawEntryIdx::new(0x100), Entry {
			node: Node { parent: None, children: Vec::new() },
			sym:  dummy_symbol("mod42"),
			kind: Kind::Module(Module {}),
		})]);
	}
}
