//! Cooperative cancellation for cage runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Shared cancellation flag for in-flight cage work.
///
/// Cheap to clone. Linux cages honor cancel by writing `cgroup.kill` when a
/// cgroup is owned; worker slots kill the child process tree the same way.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
	inner: Arc<AtomicBool>,
}

impl CancelToken {
	/// Fresh token (not cancelled).
	pub fn new() -> Self {
		Self {
			inner: Arc::new(AtomicBool::new(false)),
		}
	}

	/// Token that is never cancelled (shared static-friendly handle).
	pub fn never() -> Self {
		// Distinct instance; never call cancel on it.
		Self::new()
	}

	/// Request cancellation. Idempotent.
	pub fn cancel(&self) {
		self.inner.store(true, Ordering::SeqCst);
	}

	/// Whether cancellation has been requested.
	pub fn is_cancelled(&self) -> bool {
		self.inner.load(Ordering::SeqCst)
	}
}
