use crate::traits::package::Package;

#[allow(dead_code)]
pub trait Registry: Send + Sync {
	type Pkg: Package;
	type Error: std::error::Error + Send + Sync;

	/// Search all available packages for a specific query and return any hits.
	/// No hits are represented as an empty vec.
	fn search_packages(&self, query: &str) -> impl std::future::Future<Output = Result<Vec<Self::Pkg>, Self::Error>> + Send;

	/// Find a specific package by ID.
	fn get_package_by_id(&self, id: u64) -> impl std::future::Future<Output = Result<Self::Pkg, Self::Error>> + Send;

	/// Find all packages that share a specific name.
	fn get_packages_by_name(&self, name: &str) -> impl std::future::Future<Output = Result<Vec<Self::Pkg>, Self::Error>> + Send;

	/// Return the package reflecting the language reference.
	fn get_reference(&self) -> impl std::future::Future<Output = Self::Pkg> + Send;
}
