//! Observability boundary (DAEMON-PLAN §2.5).
//!
//! Metrics are emitted at the `run_producer` boundary only — not bolted onto
//! supervisor internals. The [`ForgeObserver`] is owned by the `ForgeRuntime`
//! and injected; there is no process-global observer.
//!
//! The server registers a Prometheus adapter; tests use [`CountingObserver`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::error::KillReason;
use crate::profiles::ProducerProfile;

/// Events observed at the `run_producer` boundary.
///
/// Owned by the `ForgeRuntime`; two runtimes carry independent observers so the
/// multi-tenant smoke test can assert distinct counts.
pub trait ForgeObserver: Send + Sync {
	/// A content-addressed producer cache hit.
	fn cache_hit(&self) {}

	/// A content-addressed producer cache miss (a run followed).
	fn cache_miss(&self) {}

	/// A producer run finished (exit 0 or non-zero — not a resource kill).
	fn producer_finished(&self, _profile: Option<ProducerProfile>, _wall: Duration, _peak_mem: Option<u64>) {}

	/// A producer run was killed by a resource ceiling.
	fn producer_killed(&self, _profile: Option<ProducerProfile>, _reason: KillReason) {}

	/// A worker pool restarted a slot (crash / watermark).
	fn worker_restart(&self, _reason: &str) {}
}

/// No-op observer (default).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullObserver;

impl ForgeObserver for NullObserver {}

/// Atomic counters for tests / simple ops dashboards.
#[derive(Debug, Default)]
pub struct CountingObserver {
	/// Cache hits.
	pub cache_hits: AtomicU64,
	/// Cache misses.
	pub cache_misses: AtomicU64,
	/// Successful or non-zero completions.
	pub finished: AtomicU64,
	/// Resource kills.
	pub killed: AtomicU64,
	/// Worker restarts.
	pub worker_restarts: AtomicU64,
}

impl ForgeObserver for CountingObserver {
	fn cache_hit(&self) {
		self.cache_hits.fetch_add(1, Ordering::Relaxed);
	}
	fn cache_miss(&self) {
		self.cache_misses.fetch_add(1, Ordering::Relaxed);
	}
	fn producer_finished(&self, _: Option<ProducerProfile>, _: Duration, _: Option<u64>) {
		self.finished.fetch_add(1, Ordering::Relaxed);
	}
	fn producer_killed(&self, _: Option<ProducerProfile>, _: KillReason) {
		self.killed.fetch_add(1, Ordering::Relaxed);
	}
	fn worker_restart(&self, _: &str) {
		self.worker_restarts.fetch_add(1, Ordering::Relaxed);
	}
}
