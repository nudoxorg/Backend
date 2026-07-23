use super::PackageId;

#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UniqueId<T> {
    package: PackageId,
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
