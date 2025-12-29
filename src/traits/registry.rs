use crate::traits::package::Package;

pub trait Registry {
    type Error;

    /// Search all available packages for a specific query and return any hits.
    /// No hits are represented as an empty array.
    /// This operation is async and can fail due to network requests.
    async fn search_packages(&self, query: &str) -> Result<Vec<Box<dyn Package>>, Self::Error>;

    /// Find a specific package by UUID.
    /// This operation is async and can fail (e.g., if the package is not found).
    async fn get_package_by_uuid(&self, uuid: u64) -> Result<Box<dyn Package>, Self::Error>;

    /// Find all packages that share a specific name.
    /// This operation is async and can fail.
    async fn get_packages_by_name(&self, name: &str) -> Result<Vec<Box<dyn Package>>, Self::Error>;

    /// Return the package reflecting the language reference.
    /// This operation is not fallible as it largely fills in known information.
    async fn get_reference(&self) -> Box<dyn Package>;
}
