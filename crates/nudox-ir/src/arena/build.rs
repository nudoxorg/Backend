use super::{Entry, EntryIdx, Node, RawEntryIdx};
use crate::{kind::EntryKind, symbol::Symbol};

pub struct EntryBuilder {
	sym:      Symbol,
	idx:      RawEntryIdx,
	next_idx: usize,
	parent:   Option<RawEntryIdx>,
	children: Vec<BuiltEntries>,
}

impl EntryBuilder {
	/// Create a new child entry and recieve an EntryIdx for it
	///
	/// This is the public API for building entries.
	pub fn create<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut Self) -> T) -> EntryIdx<T>
	where
		T: EntryKind,
	{
		let child = Self::new(self.next_idx, sym, Some(self.idx));

		let entries = child.build(build);

		let idx = entries.idx;

		self.next_idx += entries.count();
		self.children.push(entries);

		EntryIdx::new(idx.index())
	}
}

impl EntryBuilder {
	pub(super) fn root(index: usize, sym: Symbol) -> Self { Self::new(index, sym, None) }

	fn new(index: usize, sym: Symbol, parent: Option<RawEntryIdx>) -> Self {
		EntryBuilder {
			sym,
			idx: RawEntryIdx::new(index),
			next_idx: index + 1,
			parent,
			children: Vec::new(),
		}
	}

	pub(super) fn build<T>(mut self, build: impl FnOnce(&mut Self) -> T) -> BuiltEntries
	where
		T: EntryKind,
	{
		let kind = build(&mut self).into_kind();

		let EntryBuilder { idx, sym, parent, children, .. } = self;

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

	/// returns the number of entries in this tree
	fn count(&self) -> usize { self.children.iter().map(Self::count).sum::<usize>() + 1 }
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{kind::Kind, module::Module, record::{Field, Record}, test_helpers::dummy_symbol};

	#[test]
	fn built_entries_single_entry_indices() {
		let built = EntryBuilder::root(0x100, dummy_symbol("mod42")).build(|_| Module {});

		itertools::assert_equal(built.enumerate(), [(RawEntryIdx::new(0x100), Entry {
			node: Node { parent: None, children: Vec::new() },
			sym:  dummy_symbol("mod42"),
			kind: Kind::Module(Module {}),
		})]);
	}

	#[test]
	fn built_entries_with_children() {
		let built = EntryBuilder::root(0x10, dummy_symbol("struct67")).build(|b| {
			let fields = ["field1", "field2", "field3"]
				.map(dummy_symbol)
				.map(|field| b.create(field, |_| Field {}))
				.into_iter()
				.collect();

			Record { fields }
		});

		itertools::assert_equal(built.enumerate(), [
			(RawEntryIdx::new(0x10), Entry {
				node: Node {
					parent:   None,
					children: vec![RawEntryIdx::new(0x11), RawEntryIdx::new(0x12), RawEntryIdx::new(0x13)],
				},
				sym:  dummy_symbol("struct67"),
				kind: Kind::Record(Record {
					fields: vec![EntryIdx::new(0x11), EntryIdx::new(0x12), EntryIdx::new(0x13)],
				}),
			}),
			(RawEntryIdx::new(0x11), Entry {
				node: Node { parent: Some(RawEntryIdx::new(0x10)), children: Vec::new() },
				sym:  dummy_symbol("field1"),
				kind: Kind::Field(Field {}),
			}),
			(RawEntryIdx::new(0x12), Entry {
				node: Node { parent: Some(RawEntryIdx::new(0x10)), children: Vec::new() },
				sym:  dummy_symbol("field2"),
				kind: Kind::Field(Field {}),
			}),
			(RawEntryIdx::new(0x13), Entry {
				node: Node { parent: Some(RawEntryIdx::new(0x10)), children: Vec::new() },
				sym:  dummy_symbol("field3"),
				kind: Kind::Field(Field {}),
			}),
		]);
	}
}
