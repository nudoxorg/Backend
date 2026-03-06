use crate::traits::package::Package;

#[allow(dead_code)]
pub trait Registry: Send + Sync {
	type Pkg: Package;
	type Error: std::error::Error + Send + Sync;

	/// Search all available packages for a specific query and return any hits.
	/// No hits are represented as an empty vec.
	async fn search_packages(&self, query: &str) -> Result<Vec<Self::Pkg>, Self::Error>;

	/// Find a specific package by ID.
	async fn get_package_by_id(&self, id: u64) -> Result<Self::Pkg, Self::Error>;

	/// Find all packages that share a specific name.
	async fn get_packages_by_name(&self, name: &str) -> Result<Vec<Self::Pkg>, Self::Error>;

	/// Return the package reflecting the language reference.
	async fn get_reference(&self) -> Self::Pkg;
}
