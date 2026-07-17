#![expect(unused)]

use std::any::Any;

use triomphe::Arc;
use unsize::{CoerceUnsize, Coercion};

use crate::package::PackageId;

use super::{EntryId, RawEntryIdx};

pub(super) enum DeferredEntry {
    Resolved(RawEntryIdx),
    Deferred(DeferredId),
}

pub(super) struct DeferredId {
    pub(super) package: PackageId,
    entry: Arc<dyn Any>,
}

impl DeferredId {
    pub(super) fn new(package: PackageId, entry: impl EntryId) -> Self {
        let entry = Arc::new(entry).unsize(Coercion!(to dyn Any));

        DeferredId { package, entry }
    }

    pub fn entry<T: EntryId>(&self) -> &T {
        self.entry
            .downcast_ref()
            .expect("incorrect EntryId downcast target")
    }
}
