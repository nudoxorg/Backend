use super::PackageId;

#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UniqueId<T> {
    package: PackageId,
    entry: Option<T>,
}

impl<T> UniqueId<T> {
    pub fn new(package: PackageId, entry: T) -> Self {
        UniqueId {
            package,
            entry: Some(entry),
        }
    }

    pub fn root(package: PackageId) -> Self {
        UniqueId {
            package,
            entry: None,
        }
    }

    pub fn package(&self) -> PackageId {
        PackageId::clone(&self.package)
    }

    pub fn entry(&self) -> Option<&T> {
        self.entry.as_ref()
    }
}
