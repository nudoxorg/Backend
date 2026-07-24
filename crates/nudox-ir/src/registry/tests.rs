use std::hash::Hash;

use rustc_hash::FxHashMap;

use crate::{id::PackageId, package::PackageInfo};

use super::*;

struct InMemoryResolver<Id: Eq + Hash> {
    entries: FxHashMap<UniqueId<Id>, Entry>,
    packages: FxHashMap<PackageId, PackageInfo<Id>>,
}

#[derive(Debug, thiserror::Error)]
#[error("not found")]
struct InMemoryPackageResolverErrorNotFound;

impl<Id: Eq + Hash + 'static> RegistryResolver for InMemoryResolver<Id> {
    type EntryId = Id;
    type Error = InMemoryPackageResolverErrorNotFound;

    async fn load_unique_id(&mut self, id: &UniqueId<Self::EntryId>) -> Result<Entry, Self::Error> {
        self.entries
            .remove(id)
            .ok_or(InMemoryPackageResolverErrorNotFound)
    }

    async fn load_package_info(
        &self,
        id: PackageId,
    ) -> Result<PackageInfo<Self::EntryId>, Self::Error> {
        self.packages
            .remove(&id)
            .ok_or(InMemoryPackageResolverErrorNotFound)
    }
}
