use super::{EntryIdx, EntryLink, RawEntryIdx};

use crate::{entry::{Entry, Node}, kind::{EntryKind, KindDiscriminant}, symbol::Symbol};

pub struct EntryBuilder {
	kind: KindDiscriminant,

	idx:      RawEntryIdx,
	next_idx: RawEntryIdx,

	entries: Vec<Entry>,
	links:   Vec<EntryLink>,
}

impl EntryBuilder {
	/// Create a new child entry and recieve an EntryIdx for it
	///
	/// This is the public API for building entries.
	pub fn create<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut Self) -> T) -> EntryIdx<T>
	where
		T: EntryKind,
	{
		let (entries, links) = Self::build(sym, self.next_idx, Some(self.idx), build);

		let idx = self.next_idx.typed();

		self.next_idx = self.next_idx.inc_arena_idx(entries.len());

		self.entries.extend(entries);
		self.links.extend(links.iter());

		idx
	}

	/// Emits a link between the currently-being-built entry and another entry.
	pub fn link<T>(&mut self, idx: EntryIdx<T>)
	where
		T: EntryKind,
	{
		let link = EntryLink::new((self.idx, self.kind), (idx.into(), T::discriminant()));
		self.links.push(link);
	}

	/// Emits a link between two entry
	pub fn link_between<T, U>(&mut self, a: EntryIdx<T>, b: EntryIdx<U>)
	where
		T: EntryKind,
		U: EntryKind,
	{
		self.links.push(EntryLink::typed(a, b));
	}
}

impl EntryBuilder {
	pub(super) fn build<T>(
		sym: Symbol,
		idx: RawEntryIdx,
		parent: Option<RawEntryIdx>,
		build: impl FnOnce(&mut Self) -> T,
	) -> (Vec<Entry>, Vec<EntryLink>)
	where
		T: EntryKind,
	{
		let mut this = EntryBuilder {
			kind: T::discriminant(),
			idx,
			next_idx: idx.inc_arena_idx(1),
			entries: Vec::new(),
			links: Vec::new(),
		};

		let kind = build(&mut this).into_kind();

		let EntryBuilder { mut entries, links, .. } = this;

		let node = Node::build(parent, (1..=entries.len()).map(|i| idx.inc_arena_idx(i)).collect());

		entries.insert(0, Entry::new(sym, node, kind));

		(entries, links)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::test_helpers::*;

	fn build<T>(
		index: RawEntryIdx,
		sym: &str,
		build: impl FnOnce(&mut EntryBuilder) -> T,
	) -> (Vec<Entry>, Vec<EntryLink>)
	where
		T: EntryKind,
	{
		EntryBuilder::build(dummy_symbol(sym), index, None, build)
	}

	#[test]
	fn built_entries_single_entry_indices() {
		let (built, _links) = build(idx(0x100), "mod42", |_| Module);

		itertools::assert_equal(built, [entry("mod42", n::root(vec![]), Module)]);
	}

	#[test]
	fn built_entries_with_children() {
		let (built, _links) = build(idx(0x10), "struct67", |b| {
			let fields = ["field1", "field2", "field3"]
				.map(dummy_symbol)
				.map(|field| b.create(field, |_| Field {}))
				.into_iter()
				.collect();

			Record { fields }
		});

		itertools::assert_equal(built, [
			entry("struct67", n::root(vec![idx(0x11), idx(0x12), idx(0x13)]), Record {
				fields: vec![idx(0x11), idx(0x12), idx(0x13)],
			}),
			entry("field1", n::leaf(idx(0x10)), Field {}),
			entry("field2", n::leaf(idx(0x10)), Field {}),
			entry("field3", n::leaf(idx(0x10)), Field {}),
		]);
	}

	#[test]
	fn links_emitted_correctly() {
		let (_entries, links) = build(idx(0), "root", |b| {
			let struct_idx_1 = b.create(dummy_symbol("struct1"), |b| {
				let f1 = b.create(dummy_symbol("f1"), |_| Field {});
				b.link(f1);

				Record { fields: vec![f1] }
			});

			let struct_idx_2 = b.create(dummy_symbol("struct2"), |_| Record { fields: vec![] });

			b.link(struct_idx_1);
			b.link(struct_idx_2);

			b.link_between(struct_idx_1, struct_idx_2);

			Module
		});

		itertools::assert_equal(links, [
			EntryLink::typed(idx::<Record>(1), idx::<Field>(2)),
			EntryLink::typed(idx::<Module>(0), idx::<Record>(1)),
			EntryLink::typed(idx::<Module>(0), idx::<Record>(3)),
			EntryLink::typed(idx::<Record>(1), idx::<Record>(3)),
		]);
	}
}
