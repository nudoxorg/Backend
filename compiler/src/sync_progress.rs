use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PackageSyncStatus {
	#[default]
	Idle,
	Queued,
	Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageSyncPhase {
	Resolving,
	Fetching,
	Materializing,
	GeneratingIr,
	UploadingSchema,
	UploadingDocuments,
	Embedding,
	UploadingVectors,
}

#[derive(Clone)]
pub struct ProgressReporter {
	callback: Arc<dyn Fn(PackageSyncPhase, Option<String>) + Send + Sync>,
}

impl ProgressReporter {
	pub fn new<F>(callback: F) -> Self
	where
		F: Fn(PackageSyncPhase, Option<String>) + Send + Sync + 'static,
	{
		Self { callback: Arc::new(callback) }
	}

	/// A reporter that discards every update. Lets call sites take
	/// `&ProgressReporter` unconditionally instead of threading
	/// `Option<&ProgressReporter>` and guarding each emission with `if let Some`.
	pub fn silent() -> Self { Self { callback: Arc::new(|_, _| {}) } }

	pub fn phase(&self, phase: PackageSyncPhase) { self.phase_with_detail(phase, None); }

	pub fn phase_with_detail(&self, phase: PackageSyncPhase, detail: impl Into<Option<String>>) {
		(self.callback)(phase, detail.into());
	}
}
