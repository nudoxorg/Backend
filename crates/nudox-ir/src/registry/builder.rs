use super::{DeferredIdx, EntryIdx, EntryLink, RawEntryIdx};

use crate::{entry::{Entry, Node}, kind::{EntryKind, KindDiscriminant}, package::PackageId, registry::RegistryState, symbol::Symbol};

pub struct EntryBuilder {
	sym: Symbol,

	kind:      KindDiscriminant,
	entry_idx: RawEntryIdx,

	parent: Option<RawEntryIdx>,

	// deferred_idx: DeferredIdx,
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
		let (entries, links) =
			Self::builder().sym(sym).entry_idx(self.next_entry_idx()).parent(self.entry_idx).build(build);

		let idx = self.next_entry_idx().typed();

		self.entries.extend(entries);
		self.links.extend(links.iter());

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
	fn next_entry_idx(&self) -> RawEntryIdx { self.entry_idx.inc_arena_idx(self.entries.len()) }
}

#[bon::bon]
impl EntryBuilder {
	#[builder(finish_fn(name = finish, vis = ""))]
	pub(super) fn new(
		sym: Symbol,
		entry_idx: RawEntryIdx,
		parent: Option<RawEntryIdx>,
		// deferred_idx: DeferredIdx,
	) -> Self {
		EntryBuilder {
			sym,
			kind: KindDiscriminant::Module, // replaced when `build` is called
			entry_idx,
			parent,
			// deferred_idx,
			entries: Vec::new(),
			links: Vec::new(),
		}
	}
}

impl<S: entry_builder_builder::IsComplete> EntryBuilderBuilder<S> {
	pub fn build<T>(self, build: impl FnOnce(&mut EntryBuilder) -> T) -> (Vec<Entry>, Vec<EntryLink>)
	where
		T: EntryKind,
	{
		let mut builder = self.finish();

		builder.kind = T::discriminant();

		let kind = build(&mut builder).into_kind();

		let EntryBuilder { sym, parent, entry_idx, entries, links, .. } = builder;

		let node = Node::build(parent, (1..=entries.len()).map(|i| entry_idx.inc_arena_idx(i)));

		let entries = std::iter::once(Entry::new(sym, node, kind)).chain(entries).collect();

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
		todo!()
		// EntryBuilder::build(dummy_symbol(sym), index, None,
		// &RegistryState::new(), build)
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
				fields: list![idx(0x11), idx(0x12), idx(0x13)],
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
