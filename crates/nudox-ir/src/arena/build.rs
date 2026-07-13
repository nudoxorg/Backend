use super::{Entry, EntryIdx, EntryLink, Node, RawEntryIdx};
use crate::{kind::{EntryKind, KindDiscriminant}, symbol::Symbol};

pub struct EntryBuilder {
	sym:      Symbol,
	idx:      RawEntryIdx,
	next_idx: usize,
	parent:   Option<RawEntryIdx>,
	children: Vec<BuiltEntries>,
	kind:     KindDiscriminant,
	links:    Vec<EntryLink>,
}

impl EntryBuilder {
	/// Create a new child entry and recieve an EntryIdx for it
	///
	/// This is the public API for building entries.
	#[expect(private_bounds)]
	pub fn create<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut Self) -> T) -> EntryIdx<T>
	where
		T: EntryKind,
	{
		let (entries, links) = Self::build(self.next_idx, sym, Some(self.idx), build);

		let idx = entries.idx;

		self.next_idx += entries.count();
		self.children.push(entries);
		self.links.extend(links.iter());

		EntryIdx::new(idx.index())
	}

	/// Emits a link between the currently-being-built entry and another entity.
	#[expect(private_bounds)]
	pub fn link<T>(&mut self, idx: EntryIdx<T>)
	where
		T: EntryKind,
	{
		self.links.push(EntryLink { a: (self.idx, self.kind), b: (idx.into(), T::discriminant()) });
	}

	/// Emits a link between two entities
	#[expect(private_bounds)]
	pub fn link_between<T, U>(&mut self, a: EntryIdx<T>, b: EntryIdx<U>)
	where
		T: EntryKind,
		U: EntryKind,
	{
		self
			.links
			.push(EntryLink { a: (a.into(), T::discriminant()), b: (b.into(), U::discriminant()) });
	}
}

impl EntryBuilder {
	pub(super) fn build<T>(
		index: usize,
		sym: Symbol,
		parent: Option<RawEntryIdx>,
		build: impl FnOnce(&mut Self) -> T,
	) -> (BuiltEntries, BuiltLinks)
	where
		T: EntryKind,
	{
		let mut this = EntryBuilder {
			sym,
			idx: RawEntryIdx::new(index),
			next_idx: index + 1,
			parent,
			children: Vec::new(),
			kind: T::discriminant(),
			links: Vec::new(),
		};

		let kind = build(&mut this).into_kind();

		let EntryBuilder { idx, sym, parent, children, .. } = this;

		let node = Node { parent, children: children.iter().map(|e| e.idx).collect() };

		let entries = BuiltEntries { idx, entry: Entry { node, sym, kind }, children };
		let links = BuiltLinks { links: this.links };

		(entries, links)
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

pub(super) struct BuiltLinks {
	links: Vec<EntryLink>,
}
impl BuiltLinks {
	pub(super) fn iter(self) -> impl Iterator<Item = EntryLink> { self.links.into_iter() }
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{kind::Kind, module::Module, record::{Field, Record}, test_helpers::dummy_symbol};

	fn build<T>(
		index: usize,
		sym: &str,
		build: impl FnOnce(&mut EntryBuilder) -> T,
	) -> (BuiltEntries, BuiltLinks)
	where
		T: EntryKind,
	{
		EntryBuilder::build(index, dummy_symbol(sym), None, build)
	}

	#[test]
	fn built_entries_single_entry_indices() {
		let (built, _links) = build(0x100, "mod42", |_| Module {});

		itertools::assert_equal(built.enumerate(), [(RawEntryIdx::new(0x100), Entry {
			node: Node { parent: None, children: Vec::new() },
			sym:  dummy_symbol("mod42"),
			kind: Kind::Module(Module {}),
		})]);
	}

	#[test]
	fn built_entries_with_children() {
		let (built, _links) = build(0x10, "struct67", |b| {
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

	#[test]
	fn links_emitted_correctly() {
		let (_entries, links) = build(0, "root", |b| {
			let struct_idx_1: EntryIdx<Record> = b.create(dummy_symbol("struct1"), |b| {
				let f1 = b.create(dummy_symbol("f1"), |_| Field {});
				b.link(f1);

				Record { fields: vec![f1] }
			});

			let struct_idx_2: EntryIdx<Record> =
				b.create(dummy_symbol("struct2"), |_| Record { fields: vec![] });

			b.link(struct_idx_1);
			b.link(struct_idx_2);

			b.link_between(struct_idx_1, struct_idx_2);

			Module {}
		});

		itertools::assert_equal(links.iter(), [
			EntryLink {
				a: (RawEntryIdx::new(1), KindDiscriminant::Record),
				b: (RawEntryIdx::new(2), KindDiscriminant::Field),
			},
			EntryLink {
				a: (RawEntryIdx::new(0), KindDiscriminant::Module),
				b: (RawEntryIdx::new(1), KindDiscriminant::Record),
			},
			EntryLink {
				a: (RawEntryIdx::new(0), KindDiscriminant::Module),
				b: (RawEntryIdx::new(3), KindDiscriminant::Record),
			},
			EntryLink {
				a: (RawEntryIdx::new(1), KindDiscriminant::Record),
				b: (RawEntryIdx::new(3), KindDiscriminant::Record),
			},
		]);
	}
}
