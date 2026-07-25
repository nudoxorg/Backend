use super::PackageId;

/// A globally-unique identifier for an entry, scoped to its package.
///
/// `UniqueId` combines a [`PackageId`] with an entry-level identifier `T`
/// (typically a path, string, or numeric id) chosen by the tool that
/// constructed the IR.
///
/// When `entry` is `None`, the id refers to the *root* entry of a package
/// (the top-level module or namespace). Use [`UniqueId::root`] to construct
/// this. All other entries are created with [`UniqueId::new`].
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UniqueId<T> {
    /// The package that owns the entry.
    package: PackageId,

    /// The entry's identifier within its package. `None` indicates the root.
    entry: Option<T>,
}

impl<T> UniqueId<T> {
    pub fn new(package: PackageId, entry: T) -> Self {
        UniqueId::build(package, Some(entry))
    }

    pub fn root(package: PackageId) -> Self {
        UniqueId::build(package, None)
    }

    pub fn package(&self) -> PackageId {
        PackageId::clone(&self.package)
    }

    pub fn entry(&self) -> Option<&T> {
        self.entry.as_ref()
    }

    pub(crate) fn build(package: PackageId, entry: Option<T>) -> UniqueId<T> {
        UniqueId { package, entry }
    }
}
