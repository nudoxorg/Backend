use std::sync::Arc;

use crate::{http::error::AppError, ingest::IngestTargets};

use super::{AddPackageOutcome, LocalRegistry, NewPackageRequest, PackageSnapshot};

#[allow(async_fn_in_trait)]
pub trait Registry: Send + Sync + 'static {
	async fn package_count(&self) -> usize;
	async fn list_packages(&self) -> Vec<PackageSnapshot>;
	async fn get_package(&self, id: u64) -> Result<PackageSnapshot, AppError>;
	async fn add_package(self: &Arc<Self>, request: NewPackageRequest) -> Result<AddPackageOutcome, AppError>;
	async fn sync_package(self: &Arc<Self>, id: u64) -> Result<PackageSnapshot, AppError>;
	async fn run_monitor(self: Arc<Self>);
	fn with_targets(self, targets: IngestTargets) -> Self where Self: Sized;
}

impl Registry for LocalRegistry {
	async fn package_count(&self) -> usize { self.package_count().await }

	async fn list_packages(&self) -> Vec<PackageSnapshot> { self.list_packages().await }

	async fn get_package(&self, id: u64) -> Result<PackageSnapshot, AppError> {
		self.get_package(id).await
	}

	async fn add_package(
		self: &Arc<Self>,
		request: NewPackageRequest,
	) -> Result<AddPackageOutcome, AppError> {
		self.add_package(request).await
	}

	async fn sync_package(self: &Arc<Self>, id: u64) -> Result<PackageSnapshot, AppError> {
		self.sync_package(id).await
	}

	async fn run_monitor(self: Arc<Self>) { self.run_monitor().await }

	fn with_targets(self, targets: IngestTargets) -> Self { self.with_targets(targets) }
}
