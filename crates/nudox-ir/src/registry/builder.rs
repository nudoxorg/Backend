use super::{DeferredId, DeferredIdx, EntryIdx, EntryLink, RawEntryIdx};

use crate::{entry::{Entry, Node}, kind::{EntryKind, KindDiscriminant}, symbol::Symbol};

pub struct EntryBuilder {
	sym: Symbol,

	kind:      KindDiscriminant,
	entry_idx: RawEntryIdx,

	parent: Option<RawEntryIdx>,

	deferred_start: DeferredIdx,

	entries:  Vec<Entry>,
	links:    Vec<EntryLink>,
	deferred: Vec<DeferredId>,
}

impl EntryBuilder {
	/// Create a new child entry and recieve an EntryIdx for it
	///
	/// This is the public API for building entries.
	pub fn create<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut Self) -> T) -> EntryIdx<T>
	where
		T: EntryKind,
	{
		let (idx, entries, links, deferred) = Self::builder()
			.sym(sym)
			.parent(self.entry_idx)
			.entry_idx(self.next_entry_idx())
			.deferred_start(self.next_deferred_idx())
			.build(build);

		self.entries.extend(entries);
		self.links.extend(links);
		self.deferred.extend(deferred);

		idx
	}

	/// Emits a link between the currently-being-built entry and another entry.
	pub fn link<T>(&mut self, idx: EntryIdx<T>)
	where
		T: EntryKind,
	{
		let link = EntryLink::new((self.entry_idx, self.kind), (idx.into(), T::discriminant()));
		self.links.push(link);
	}

	/// Emits links between the currently-being-built entry and the entries
	/// provided
	pub fn link_many<T>(&mut self, entries: impl IntoIterator<Item = EntryIdx<T>>)
	where
		T: EntryKind,
	{
		for idx in entries {
			self.link(idx);
		}
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
	fn next_entry_idx(&self) -> RawEntryIdx {
		self.entry_idx.inc_arena_idx(self.entries.len() + 1) // offset to not reuse entry_idx for children
	}

	fn next_deferred_idx(&self) -> DeferredIdx { self.deferred_start.increment(self.deferred.len()) }
}

#[bon::bon]
impl EntryBuilder {
	#[builder(finish_fn(name = finish, vis = ""))]
	pub(super) fn new(
		sym: Symbol,
		parent: Option<RawEntryIdx>,
		entry_idx: RawEntryIdx,
		deferred_start: DeferredIdx,
	) -> Self {
		EntryBuilder {
			sym,
			entry_idx,
			parent,
			deferred_start,

			kind: KindDiscriminant::Module, // replaced when `build` is called

			entries: Vec::new(),
			links: Vec::new(),
			deferred: Vec::new(),
		}
	}
}

impl<S: entry_builder_builder::IsComplete> EntryBuilderBuilder<S> {
	pub fn build<T>(
		self,
		build: impl FnOnce(&mut EntryBuilder) -> T,
	) -> (EntryIdx<T>, Vec<Entry>, Vec<EntryLink>, Vec<DeferredId>)
	where
		T: EntryKind,
	{
		let mut builder = self.finish();

		builder.kind = T::discriminant();

		let kind = build(&mut builder).into_kind();

		let EntryBuilder { sym, parent, entry_idx, entries, links, deferred, .. } = builder;

		let node = Node::build(parent, (1..=entries.len()).map(|i| entry_idx.inc_arena_idx(i)));

		let entries = std::iter::once(Entry::new(sym, node, kind)).chain(entries).collect();

		(entry_idx.typed(), entries, links, deferred)
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
	) -> (EntryIdx<T>, Vec<Entry>, Vec<EntryLink>, Vec<DeferredId>)
	where
		T: EntryKind,
	{
		EntryBuilder::builder()
			.sym(dummy_symbol(sym))
			.entry_idx(index)
			.deferred_start(DeferredIdx::new(0))
			.build(build)
	}

	#[test]
	fn built_entries_single_entry_indices() {
		let (_idx, entries, _links, _deferred) = build(idx(0x100), "mod42", |_| Module);

		itertools::assert_equal(entries, [entry("mod42", n::root(vec![]), Module)]);
	}

	#[test]
	fn built_entries_with_children() {
		let (_idx, entries, _links, _deferred) = build(idx(0x10), "struct67", |b| {
			let fields = ["field1", "field2", "field3"]
				.map(dummy_symbol)
				.map(|field| b.create(field, |_| Field {}))
				.into_iter()
				.collect();

			Record { fields }
		});

		itertools::assert_equal(entries, [
			entry("struct67", n::root(vec![idx(0x11), idx(0x12), idx(0x13)]), Record {
				fields: list![idx(0x11), idx(0x12), idx(0x13)],
			}),
			entry("field1", n::leaf(idx(0x10)), Field {}),
			entry("field2", n::leaf(idx(0x10)), Field {}),
			entry("field3", n::leaf(idx(0x10)), Field {}),
		]);
	}

	#[test]
	fn links_emitted_correctly() {
		let (_idx, _entries, links, _deferred) = build(idx(0), "root", |b| {
			let struct_idx_1 = b.create(dummy_symbol("struct1"), |b| {
				let f1 = b.create(dummy_symbol("f1"), |_| Field {});
				b.link(f1);

				Record { fields: list![f1] }
			});

			let struct_idx_2 = b.create(dummy_symbol("struct2"), |_| Record { fields: list![] });

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
