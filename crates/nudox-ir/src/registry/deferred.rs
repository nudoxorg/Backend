#![expect(unused)]

use std::any::Any;

use crate::{package::PackageId, registry::EntryId};

pub(super) struct DeferredId {
    pub(super) package: PackageId,
    entry: Box<dyn DynEntryId>,
}

impl DeferredId {
    pub(super) fn new(package: PackageId, entry: impl EntryId) -> Self {
        DeferredId {
            package,
            entry: Box::new(entry),
        }
    }
}

trait DynEntryId: Any {}

impl<T: EntryId> DynEntryId for T {}
