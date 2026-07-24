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

    pub(super) fn new(id: PackageId) -> Self {
        Self {
            id,
            exports: IndexSet::new(),
            imports: IndexSet::new(),
        }
    }

    pub(super) fn root_export(&self) -> UntypedEntryIndex {
        UntypedEntryIndex::export(0)
    }

    pub(super) fn create_export(&mut self, id: Id) -> UntypedEntryIndex {
        let (idx, _) = self.exports.insert_full(id);

        UntypedEntryIndex::export(idx + 1)
    }

    pub(super) fn export_idx_to_id(&self, idx: UntypedEntryIndex) -> Option<&Id> {
        assert!(idx.is_export());

        match idx.export_index() {
            0 => None,
            i => Some(&self.exports[i - 1]),
        }
    }

    pub(super) fn create_import(&mut self, id: UniqueId<Id>) -> UntypedEntryIndex {
        let (idx, _) = self.imports.insert_full(id);

        UntypedEntryIndex::import(idx)
    }

    #[cfg(test)]
    pub(crate) fn export_id_to_idx(&self, id: &Id) -> Option<UntypedEntryIndex> {
        self.exports
            .get_index_of(id)
            .map(|idx| UntypedEntryIndex::export(idx + 1))
    }
}
