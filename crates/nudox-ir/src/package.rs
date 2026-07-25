mod builder;
mod info;

#[cfg(test)]
mod tests;

use std::hash::Hash;

use crate::{
    entry::{Entry, Symbol},
    index::UntypedEntryIndex,
    kinds::Module,
};

pub use crate::id::{PackageId, PackageIdView};

pub use self::{builder::EntryBuilder, info::PackageInfo};

pub struct IrPackage<Id: Eq + Hash> {
    info: PackageInfo<Id>,
    entries: Vec<(UntypedEntryIndex, Entry)>,
}

impl<Id: Eq + Hash> IrPackage<Id> {
    pub fn build(id: PackageId, sym: Symbol, build: impl FnOnce(EntryBuilder<Id>)) -> Self {
        let mut pkg = IrPackage {
            info: PackageInfo::new(id),
            entries: Vec::new(),
        };

        EntryBuilder::build(&mut pkg, sym, None, |b| {
            build(b);
            Module
        });

        debug_assert!(pkg.is_valid(), "built invalid package");

        pkg
    }

    pub fn info(&self) -> &PackageInfo<Id> {
        &self.info
    }

    pub fn iter(&self) -> impl Iterator<Item = (Option<&Id>, &Entry)> {
        self.entries
            .iter()
            .map(|(idx, entry)| (self.info.export_idx_to_id(*idx), entry))
    }

    fn is_valid(&self) -> bool {
        // TODO: do some validation passes to make sure that the generated IR
        // doesn't have any issues we can catch
        true
    }
}
