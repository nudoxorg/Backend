//! Optional observability hooks — keep the sandbox crate metrics-free.
//!
//! The server registers a Prometheus adapter; tests use a counting observer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::error::KillReason;
use crate::profiles::ProducerProfile;

/// Events emitted by sandboxed runs and worker pools.
pub trait SandboxObserver: Send + Sync {
	/// A sandboxed process finished successfully (exit 0 or non-zero).
	fn job_finished(&self, _profile: Option<ProducerProfile>, _wall: Duration, _peak_mem: Option<u64>) {}

	/// A sandboxed process was killed by a resource ceiling.
	fn job_killed(&self, _profile: Option<ProducerProfile>, _reason: KillReason) {}

	/// Backend denied a run (policy).
	fn denied(&self, _reason: &str) {}

	/// Worker pool restarted a slot (crash / watermark).
	fn worker_restart(&self, _reason: &str) {}

	/// Content-addressed parse cache hit.
	fn cache_hit(&self) {}

	/// Content-addressed parse cache miss.
	fn cache_miss(&self) {}
}

/// No-op observer (default).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullObserver;

impl SandboxObserver for NullObserver {}

/// Atomic counters for tests / simple ops dashboards.
#[derive(Debug, Default)]
pub struct CountingObserver {
	/// Successful or non-zero completions.
	pub finished: AtomicU64,
	/// Resource kills.
	pub killed: AtomicU64,
	/// Policy denials.
	pub denied: AtomicU64,
	/// Worker restarts.
	pub worker_restarts: AtomicU64,
	/// Cache hits.
	pub cache_hits: AtomicU64,
	/// Cache misses.
	pub cache_misses: AtomicU64,
}

impl SandboxObserver for CountingObserver {
	fn job_finished(&self, _: Option<ProducerProfile>, _: Duration, _: Option<u64>) {
		self.finished.fetch_add(1, Ordering::Relaxed);
	}
	fn job_killed(&self, _: Option<ProducerProfile>, _: KillReason) {
		self.killed.fetch_add(1, Ordering::Relaxed);
	}
	fn denied(&self, _: &str) {
		self.denied.fetch_add(1, Ordering::Relaxed);
	}
	fn worker_restart(&self, _: &str) {
		self.worker_restarts.fetch_add(1, Ordering::Relaxed);
	}
	fn cache_hit(&self) {
		self.cache_hits.fetch_add(1, Ordering::Relaxed);
	}
	fn cache_miss(&self) {
		self.cache_misses.fetch_add(1, Ordering::Relaxed);
	}
}

/// Process-wide observer slot.
static OBSERVER: std::sync::OnceLock<Arc<dyn SandboxObserver>> = std::sync::OnceLock::new();

/// Install the process-wide observer (first call wins).
pub fn install(observer: Arc<dyn SandboxObserver>) {
	let _ = OBSERVER.set(observer);
}

/// Borrow the process-wide observer (null if unset).
pub fn global() -> Arc<dyn SandboxObserver> {
	OBSERVER
		.get()
		.cloned()
		.unwrap_or_else(|| Arc::new(NullObserver))
}
