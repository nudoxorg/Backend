use std::hash::Hash;

use indexmap::IndexSet;

use crate::{
    id::{PackageId, UniqueId},
    index::UntypedEntryIndex,
};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct PackageInfo<Id: Eq + Hash> {
    id: PackageId,
    exports: IndexSet<Id>,
    imports: IndexSet<UniqueId<Id>>,
}

impl<Id: Eq + Hash> PackageInfo<Id> {
    pub fn id(&self) -> PackageId {
        PackageId::clone(&self.id)
    }

    // TODO: this should really get a rethink
    pub(crate) fn iters(
        self,
    ) -> (
        impl Iterator<Item = UniqueId<Id>>,
        impl Iterator<Item = Option<Id>>,
    ) {
        let imports = self.imports.into_iter();
        let exports = std::iter::once(None).chain(self.exports.into_iter().map(Some));

        (imports, exports)
    }

    // Widened to pub(crate) so that `lower::Lowering` (a top-level module) can
    // construct a PackageInfo without going through EntryBuilder::build.
    pub(crate) fn new(id: PackageId) -> Self {
        Self {
            id,
            exports: IndexSet::new(),
            imports: IndexSet::new(),
        }
    }

    // Widened to pub(crate) so that `lower::Lowering` can obtain the root index.
    pub(crate) fn root_export(&self) -> UntypedEntryIndex {
        UntypedEntryIndex::export(0)
    }

    // Widened to pub(crate) so that `lower::Lowering` can intern producer IDs.
    pub(crate) fn create_export(&mut self, id: Id) -> UntypedEntryIndex {
        let (idx, _) = self.exports.insert_full(id);

        UntypedEntryIndex::export(idx + 1)
    }

    // Widened to pub(crate) — needed for error reporting in `lower::Lowering`.
    pub(crate) fn export_idx_to_id(&self, idx: UntypedEntryIndex) -> Option<&Id> {
        assert!(idx.is_export());

        match idx.export_index() {
            0 => None,
            i => Some(&self.exports[i - 1]),
        }
    }

    // `create_import` is deliberately gone. A cross-package reference is now a
    // self-describing `Ref::Foreign` (see `crate::foreign`), so nothing mints an
    // import index any more and `Ref::Local` names exactly one thing: an export
    // index that `seal` rewrites.
    //
    // The `imports` field and `iters()` are retained, permanently empty, for the
    // `crate::registry` resolution plane — a second, fully unused import-resolution
    // design that `nudox-{store,engine,graph}` and every producer ignore. Deleting
    // that plane is the correct follow-up and is deliberately not folded into this
    // change; see the note on `Ref::Foreign`.

    #[cfg(test)]
    pub(crate) fn export_id_to_idx(&self, id: &Id) -> Option<UntypedEntryIndex> {
        self.exports
            .get_index_of(id)
            .map(|idx| UntypedEntryIndex::export(idx + 1))
    }
}
