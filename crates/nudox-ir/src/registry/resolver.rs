use std::error::Error;

use crate::entry::Entry;

use super::{DeserContext, EntryId, UniqueId};

pub trait RegistryResolver: Sized + 'static {
    /// A type that can be used to uniquely identify an Entry within a package.
    /// Should be constructable based on information available within the IR of
    /// a package that is consuming an external package's entry as the target.
    type EntryId: EntryId;

    type Error: Error;

    fn load_unique_id(
        &self,
        id: &UniqueId<Self::EntryId>,
        cx: DeserContext<'_>,
    ) -> impl Future<Output = Result<Entry, Self::Error>>;
}
