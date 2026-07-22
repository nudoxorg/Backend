use std::{error::Error, hash::Hash};

use crate::entry::Entry;

use crate::{
    id::{PackageId, UniqueId},
    package::PackageInfo,
};

pub trait RegistryResolver: Sized + 'static {
    /// A type that can be used to uniquely identify an Entry within a package.
    /// Should be constructable based on information available within the IR of
    /// a package that is consuming an external package's entry as the target.
    type EntryId: Eq + Hash;

    type Error: Error;

    fn load_unique_id(
        &self,
        id: &UniqueId<Self::EntryId>,
    ) -> impl Future<Output = Result<Entry, Self::Error>>;

    fn load_package_info(
        &self,
        id: PackageId,
    ) -> impl Future<Output = Result<PackageInfo<Self::EntryId>, Self::Error>>;
}
