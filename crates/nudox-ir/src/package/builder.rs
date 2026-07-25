use std::hash::Hash;

use crate::{
    entry::{Entry, Node, Symbol},
    id::UniqueId,
    index::{EntryIndex, UntypedEntryIndex},
    kind::EntryKind,
};

use super::IrPackage;

/// A scoped builder for constructing entries within an [`IrPackage`].
///
/// `EntryBuilder` is the workhorse of IR construction. Each instance
/// represents a single entry in the tree; calling [`create`](Self::create) on
/// it adds a child entry and records the parent-child relationship.
///
/// # Automatic tree wiring
///
/// When you call `builder.create(id, sym, |child| { … })`, the builder:
///
/// 1. Allocates an export index for the new entry.
/// 2. Invokes the closure, passing a new `EntryBuilder` for the child.
/// 3. The child builder collects any grandchildren added inside the closure.
/// 4. The IR's tree structure is updated, linking parents and children.
///
/// This means you never manually set parent or child fields.
///
/// # Cross-package references
///
/// Use [`index_of`](Self::index_of) to reference entries within the same
/// package, and [`index_of_import`](Self::index_of_import) to reference entries
/// in other packages (by their [`UniqueId`]).
pub struct EntryBuilder<'a, Id: Eq + Hash> {
    ir: &'a mut IrPackage<Id>,
    idx: UntypedEntryIndex,
    children: &'a mut Vec<UntypedEntryIndex>,
}

impl<Id: Eq + Hash> EntryBuilder<'_, Id> {
    /// Create a new child entry and return its typed index.
    ///
    /// The `build` closure receives a fresh `EntryBuilder` for the new entry,
    /// and must return a value that implements [`EntryKind`]---typically one
    /// of the kind builders like `Record::builder().build()`.
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

    /// Create a re-export entry pointing to an existing entry.
    ///
    /// The returned index refers to the *re-export*, not the original.
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

    /// Get a typed index for an entry already created (or about to be created)
    /// within this package.
    ///
    /// # Panics
    ///
    /// Panics at IR validation time if `id` was never created.
    pub fn index_of<T>(&mut self, id: Id) -> EntryIndex<T>
    where
        T: EntryKind,
    {
        self.ir.info.create_export(id).typed()
    }

    /// Get an import index for a cross-package entry reference.
    ///
    /// The returned index will be remapped at registry resolution time.
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

        let idx = id.map_or(ir.info.root_export(), |id| ir.info.create_export(id));

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
