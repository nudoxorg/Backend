use std::error::Error;

use crate::entry::Entry;

use super::{EntryId, RegistryState, UniqueId};

pub trait RegistryResolver: 'static {
    /// A type that can be used to uniquely identify an Entry within a package.
    /// Should be constructable based on information available within the IR of
    /// a package that is consuming an external package's entry as the target.
    type EntryId: EntryId;

    type Error: Error;

    fn load_unique_id(
        &self,
        id: &UniqueId<Self::EntryId>,
        state: &RegistryState,
    ) -> impl Future<Output = Result<Entry, Self::Error>>;
}
