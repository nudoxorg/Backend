use std::hash::Hash;

use crate::{
    entry::{Entry, Node, Symbol},
    id::UniqueId,
    index::{EntryIndex, UntypedEntryIndex},
    kind::EntryKind,
};

use super::IrPackage;

pub struct EntryBuilder<'a, Id: Eq + Hash> {
    ir: &'a mut IrPackage<Id>,
    idx: UntypedEntryIndex,
    children: &'a mut Vec<UntypedEntryIndex>,
}

impl<Id: Eq + Hash> EntryBuilder<'_, Id> {
    /// Creates a new entry and returns an `EntryIndex` that points to it.
    pub fn create<T>(
        &mut self,
        id: Id,
        sym: Symbol,
        build: impl FnOnce(EntryBuilder<'_, Id>) -> T,
    ) -> EntryIndex<T>
    where
        T: EntryKind,
    {
        let idx = EntryBuilder::build(self.ir, sym, Some((id, self.idx)), build);

        self.children.push(idx);

        idx.typed()
    }

    /// Creates a reference/re-export entry that points to another entry,
    /// and returns an `EntryIndex` that points to the re-export
    #[doc(alias = "create_reexport")]
    pub fn create_ref<T>(&mut self, id: Id, sym: Symbol, other: EntryIndex<T>) -> EntryIndex<T>
    where
        T: EntryKind,
    {
        let idx = self.ir.info.create_export(id);

        self.ir.entries.push((
            idx,
            Entry::reference(sym, Node::build(self.idx, []), other.raw()),
        ));

        idx.typed()
    }

    /// Returns an `EntryIndex` that refers to the entry that corresponds to
    /// the given `Id`
    ///
    /// Passing an `Id` that is not actually created within the package will
    /// lead to creating an invalid `PackageIr`. This will be caught at IR
    /// validation time and **will panic**.
    pub fn index_of<T>(&mut self, id: Id) -> EntryIndex<T>
    where
        T: EntryKind,
    {
        self.ir.info.create_export(id).typed()
    }

    /// Returns an `EntryIndex` that refers to the entry that corresponds to
    /// the given `UniqueId`.
    ///
    /// Passing a `UniqueId` that is not actually existant will result in an
    /// error when trying to resolve the `EntryIndex`.
    pub fn index_of_import<T>(&mut self, id: UniqueId<Id>) -> EntryIndex<T>
    where
        T: EntryKind,
    {
        self.ir.info.create_import(id).typed()
    }

    pub(super) fn build<T>(
        ir: &mut IrPackage<Id>,
        sym: Symbol,
        info: Option<(Id, UntypedEntryIndex)>,
        build: impl FnOnce(EntryBuilder<'_, Id>) -> T,
    ) -> UntypedEntryIndex
    where
        T: EntryKind,
    {
        let (id, parent) = info.unzip();

        let mut children = Vec::new();

        let idx = id
            .map(|id| ir.info.create_export(id))
            .unwrap_or(ir.info.root_export());

        let kind = build(EntryBuilder {
            ir,
            idx,
            children: &mut children,
        });

        let node = Node::build(parent, children);
        let entry = Entry::new(sym, node, kind.into_kind());

        ir.entries.push((idx, entry));

        idx
    }
}
