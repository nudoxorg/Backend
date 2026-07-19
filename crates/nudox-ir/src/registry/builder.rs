use std::marker::PhantomData;

use crate::{
    entry::{Entry, Node},
    kind::{EntryKind, KindDiscriminant},
    package::PackageId,
    symbol::Symbol,
};

use super::{
    EntryIdx, EntryLink, ErasedUniqueId, RawEntryIdx, RegistryResolver, StoredEntry, UniqueId,
};

pub struct EntryBuilder<R> {
    kind: KindDiscriminant,
    tree: EntryTree,
    links: Vec<EntryLink>,
    _p: PhantomData<R>,
}

impl<R> EntryBuilder<R>
where
    R: RegistryResolver,
{
    /// Create a new child entry and recieve an EntryIdx for it
    ///
    /// This is the public API for building entries.
    pub fn create<T>(
        &mut self,
        id: R::EntryId,
        sym: Symbol,
        build: impl FnOnce(&mut Self) -> T,
    ) -> EntryIdx<T>
    where
        T: EntryKind,
    {
        let built = Self::builder()
            .id(self.unique_id(id))
            .idx(self.next_idx())
            .build(sym, self.idx(), build);

        let idx = built.tree.idx.typed();

        self.tree.push(built.tree);
        self.links.extend(built.links);

        idx
    }

    pub fn create_ref<T>(&mut self, id: R::EntryId, sym: Symbol, idx: EntryIdx<T>) -> EntryIdx<T>
    where
        T: EntryKind,
    {
        let entry_idx = self.next_idx();

        self.tree.push(EntryTree::resolved(
            entry_idx,
            StoredEntry::resolved(
                self.unique_id(id).upcast(),
                Entry::reference(sym, Node::build(Some(self.idx()), []), idx.raw()),
            ),
        ));

        entry_idx.typed()
    }

    /// Emits a link between the currently-being-built entry and another entry.
    pub fn link<T>(&mut self, idx: EntryIdx<T>)
    where
        T: EntryKind,
    {
        let link = EntryLink::new((self.idx(), self.kind), (idx.into(), T::discriminant()));
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

impl<R> EntryBuilder<R>
where
    R: RegistryResolver,
{
    fn unique_id(&self, entry: R::EntryId) -> UniqueId<R::EntryId> {
        UniqueId::new(self.pkg(), entry)
    }
}

impl<R> EntryBuilder<R> {
    fn pkg(&self) -> PackageId {
        self.tree.entry.id().package()
    }

    fn idx(&self) -> RawEntryIdx {
        self.tree.idx
    }

    fn next_idx(&self) -> RawEntryIdx {
        self.idx().increment(self.tree.count())
    }
}

#[bon::bon]
impl<R> EntryBuilder<R>
where
    R: RegistryResolver,
{
    #[builder(finish_fn(name = finish, vis = ""))]
    pub(super) fn new(id: UniqueId<R::EntryId>, idx: RawEntryIdx) -> Self {
        EntryBuilder {
            kind: KindDiscriminant::Module, // replaced in build
            tree: EntryTree::new(id.upcast(), idx),
            links: Vec::new(),
            _p: PhantomData,
        }
    }
}

impl<R, S> EntryBuilderBuilder<R, S>
where
    R: RegistryResolver,
    S: entry_builder_builder::IsComplete,
{
    pub fn build<T>(
        self,

        sym: Symbol,
        parent: impl Into<Option<RawEntryIdx>>,
        build: impl FnOnce(&mut EntryBuilder<R>) -> T,
    ) -> BuildResult
    where
        T: EntryKind,
    {
        let mut builder = EntryBuilder {
            kind: T::discriminant(),
            ..self.finish()
        };

        let kind = build(&mut builder).into_kind();

        let EntryBuilder { tree, links, .. } = builder;

        let node = Node::build(parent.into(), tree.children());

        tree.init(Entry::new(sym, node, kind));

        BuildResult { tree, links }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct EntryTree {
    idx: RawEntryIdx,
    entry: StoredEntry,
    children: Vec<EntryTree>,
}

impl EntryTree {
    pub(super) fn iter(self) -> impl Iterator<Item = StoredEntry> {
        std::iter::once(self.entry)
            .chain(Box::new(self.children.into_iter().flat_map(Self::iter))
                as Box<dyn Iterator<Item = _>>)
    }
}

impl EntryTree {
    fn new(id: ErasedUniqueId, idx: RawEntryIdx) -> Self {
        Self {
            idx,
            entry: StoredEntry::deferred(id),
            children: Vec::new(),
        }
    }

    fn resolved(idx: RawEntryIdx, entry: StoredEntry) -> EntryTree {
        Self {
            idx,
            entry,
            children: Vec::new(),
        }
    }

    fn push(&mut self, tree: EntryTree) {
        self.children.push(tree);
    }

    fn children(&self) -> impl Iterator<Item = RawEntryIdx> {
        self.children.iter().map(|c| c.idx)
    }

    fn count(&self) -> usize {
        self.children.iter().map(EntryTree::count).sum::<usize>() + 1
    }

    fn init(&self, entry: Entry) {
        self.entry.init(entry);
    }
}

pub(super) struct BuildResult {
    pub(super) tree: EntryTree,
    pub(super) links: Vec<EntryLink>,
}

#[cfg(test)]
mod tests {
    use crate::{package::PackageId, registry::EntryId, test_helpers::*};

    use super::*;

    fn uid(id: usize) -> UniqueId<usize> {
        UniqueId::new(PackageId::path("/"), id)
    }

    fn stored_entry(id: UniqueId<impl EntryId>, entry: Entry) -> StoredEntry {
        StoredEntry::resolved(id.upcast(), entry)
    }

    fn build<T>(
        index: RawEntryIdx,
        id: UniqueId<<DummyRegistryResolver as RegistryResolver>::EntryId>,
        sym: &str,
        build: impl FnOnce(&mut EntryBuilder<DummyRegistryResolver>) -> T,
    ) -> BuildResult
    where
        T: EntryKind,
    {
        EntryBuilder::builder()
            .id(id)
            .idx(index)
            .build(dummy_symbol(sym), None, build)
    }

    #[test]
    fn built_entries_single_entry_indices() {
        let built = build(idx(0x100), uid(0), "mod42", |_| Module);

        assert_eq!(built.links, []);

        assert_eq!(built.tree.idx, idx(0x100));

        itertools::assert_equal(built.tree.iter(), [stored_entry(
            uid(0),
            entry("mod42", n::root([]), Module),
        )]);
    }

    #[test]
    fn built_entries_with_children() {
        let built = build(idx(0x10), uid(0), "struct67", |b| {
            let fields = ["field1", "field2", "field3"]
                .into_iter()
                .enumerate()
                .map(|(idx, field)| b.create(idx + 1, dummy_symbol(field), |_| Field {}))
                .collect();

            Record { fields }
        });

        itertools::assert_equal(built.tree.iter(), [
            stored_entry(
                uid(0),
                entry(
                    "struct67",
                    n::root([idx(0x11), idx(0x12), idx(0x13)]),
                    Record {
                        fields: list![idx(0x11), idx(0x12), idx(0x13)],
                    },
                ),
            ),
            stored_entry(uid(1), entry("field1", n::leaf(idx(0x10)), Field {})),
            stored_entry(uid(2), entry("field2", n::leaf(idx(0x10)), Field {})),
            stored_entry(uid(3), entry("field3", n::leaf(idx(0x10)), Field {})),
        ]);
    }

    #[test]
    fn links_emitted_correctly() {
        let built = build(idx(0), uid(0), "root", |b| {
            let struct_idx_1 = b.create(1, dummy_symbol("struct1"), |b| {
                let f1 = b.create(2, dummy_symbol("f1"), |_| Field {});
                b.link(f1);

                Record { fields: list![f1] }
            });

            let struct_idx_2 = b.create(3, dummy_symbol("struct2"), |_| Record { fields: list![] });

            b.link(struct_idx_1);
            b.link(struct_idx_2);

            b.link_between(struct_idx_1, struct_idx_2);

            Module
        });

        itertools::assert_equal(built.links, [
            EntryLink::typed(idx::<Record>(1), idx::<Field>(2)),
            EntryLink::typed(idx::<Module>(0), idx::<Record>(1)),
            EntryLink::typed(idx::<Module>(0), idx::<Record>(3)),
            EntryLink::typed(idx::<Record>(1), idx::<Record>(3)),
        ]);
    }
}
