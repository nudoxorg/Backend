//! `EmbedGate` — session concurrency cap + idle unload (09c §1.6, §4.3, §8.3).
//!
//! Two jobs, one type:
//!
//! 1. **Concurrency cap.** ORT sessions are not free-threaded; concurrent
//!    `Run` on one session is a known footgun. The gate owns a semaphore
//!    (default **1** permit — one session, serialized use).
//! 2. **Idle unload.** The loaded embedder holds ~160–300 MB RSS (weights +
//!    arena). That budget is separate from the vector-store RSS budget, and
//!    must drop to **0 when idle** (09c §4.5). The gate tracks last use and a
//!    background task drops the embedder after `unload_idle` (default 120 s =
//!    `semantic.unload_idle_ms`); the next [`EmbedGate::acquire`] lazily
//!    reloads via the factory closure.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use crate::vector::core::EmbedError;

/// Async factory producing a fresh embedder (e.g. `FastembedOrt::load`).
pub type EmbedderFactory<E> =
	Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<E, EmbedError>> + Send>> + Send + Sync>;

/// Gate configuration (09c §8.3).
#[derive(Debug, Clone)]
pub struct GateConfig {
	/// Concurrent sessions allowed. Default 1: one ORT session, serialized.
	pub max_sessions: usize,
	/// Unload the embedder after this much idle time.
	pub unload_idle: Duration,
}

impl Default for GateConfig {
	fn default() -> Self {
		Self { max_sessions: 1, unload_idle: Duration::from_millis(120_000) }
	}
}

/// Load state, for UI ("embedder: loaded / idle").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
	/// Weights are resident in memory.
	Loaded,
	/// Nothing loaded; next use pays the (re)load cost.
	Idle,
}

/// The embedder lifecycle gate. Construct with [`EmbedGate::new`], which also
/// spawns the idle-unload task (tied to the gate's lifetime via `Weak`).
pub struct EmbedGate<E> {
	factory: EmbedderFactory<E>,
	slot: Mutex<Option<Arc<E>>>,
	permits: Arc<Semaphore>,
	max_sessions: usize,
	unload_idle: Duration,
	last_used: std::sync::Mutex<Instant>,
	loaded: AtomicBool,
	loads: std::sync::atomic::AtomicU64,
}

impl<E: Send + Sync + 'static> EmbedGate<E> {
	/// Build the gate and spawn its idle-unload watchdog on the current
	/// tokio runtime. The watchdog holds only a `Weak`; dropping the last
	/// `Arc<EmbedGate>` ends it.
	pub fn new(config: GateConfig, factory: EmbedderFactory<E>) -> Arc<Self> {
		let gate = Arc::new(Self {
			factory,
			slot: Mutex::new(None),
			permits: Arc::new(Semaphore::new(config.max_sessions)),
			max_sessions: config.max_sessions,
			unload_idle: config.unload_idle,
			last_used: std::sync::Mutex::new(Instant::now()),
			loaded: AtomicBool::new(false),
			loads: std::sync::atomic::AtomicU64::new(0),
		});
		tokio::spawn(idle_watchdog(Arc::downgrade(&gate)));
		gate
	}

	/// Acquire a session permit and the (lazily loaded) embedder.
	///
	/// Blocks while `max_sessions` guards are outstanding — this is the
	/// backpressure that keeps exactly one ORT session running.
	pub async fn acquire(self: &Arc<Self>) -> Result<GateGuard<E>, EmbedError> {
		let permit = Arc::clone(&self.permits)
			.acquire_owned()
			.await
			.map_err(|_| super::backend_error("embed gate closed"))?;

		let embedder = {
			let mut slot = self.slot.lock().await;
			match &*slot {
				Some(embedder) => Arc::clone(embedder),
				None => {
					tracing::info!("embed gate: loading embedder");
					let embedder = Arc::new((self.factory)().await?);
					*slot = Some(Arc::clone(&embedder));
					self.loaded.store(true, Ordering::Release);
					self.loads.fetch_add(1, Ordering::Relaxed);
					embedder
				}
			}
		};

		self.touch();
		Ok(GateGuard { embedder, gate: Arc::clone(self), _permit: permit })
	}

	/// Current load state, for status UI.
	pub fn state(&self) -> LoadState {
		if self.loaded.load(Ordering::Acquire) { LoadState::Loaded } else { LoadState::Idle }
	}

	/// How many times the factory has been invoked (load + reloads).
	pub fn load_count(&self) -> u64 { self.loads.load(Ordering::Relaxed) }

	/// Drop the loaded embedder now (e.g. on memory pressure). In-flight
	/// guards keep their `Arc<E>` alive until they drop; new acquires reload.
	pub async fn unload(&self) {
		let mut slot = self.slot.lock().await;
		if slot.take().is_some() {
			self.loaded.store(false, Ordering::Release);
			tracing::info!("embed gate: embedder unloaded");
		}
	}

	fn touch(&self) {
		*self.last_used.lock().expect("last_used poisoned") = Instant::now();
	}

	fn idle_for(&self) -> Duration {
		self.last_used.lock().expect("last_used poisoned").elapsed()
	}
}

/// A live session: semaphore permit + a handle on the loaded embedder.
/// Refreshes the gate's last-used clock on drop.
pub struct GateGuard<E: Send + Sync + 'static> {
	embedder: Arc<E>,
	gate: Arc<EmbedGate<E>>,
	_permit: OwnedSemaphorePermit,
}

impl<E: Send + Sync + 'static> std::ops::Deref for GateGuard<E> {
	type Target = E;
	fn deref(&self) -> &E { &self.embedder }
}

impl<E: Send + Sync + 'static> Drop for GateGuard<E> {
	fn drop(&mut self) { self.gate.touch(); }
}

/// [`Embedder`] adapter over a gate: every call acquires a session (loading
/// the embedder if idle-unloaded) and releases it afterwards. This is what
/// the [`super::scheduler::EmbedScheduler`] is normally built over, so the
/// embedder can unload between bursts of work.
pub struct GatedEmbedder<E: Send + Sync + 'static> {
	gate: Arc<EmbedGate<E>>,
	/// The inner embedder's runtime info, captured on first use — so
	/// [`crate::vector::core::Embedder::runtime`] (sync) never has to reach through
	/// the async slot and never panics while the embedder is idle-unloaded.
	runtime_cache: std::sync::OnceLock<crate::vector::core::EmbedRuntimeInfo>,
}

impl<E: Send + Sync + 'static> GatedEmbedder<E> {
	pub fn new(gate: Arc<EmbedGate<E>>) -> Self {
		Self { gate, runtime_cache: std::sync::OnceLock::new() }
	}

	/// The underlying gate (for state UI / manual unload).
	pub fn gate(&self) -> &Arc<EmbedGate<E>> { &self.gate }
}

#[async_trait::async_trait]
impl<E> crate::vector::core::Embedder for GatedEmbedder<E>
where
	E: crate::vector::core::Embedder + Send + Sync + 'static,
{
	type Model = E::Model;

	async fn embed(
		&self,
		text: &str,
		role: crate::vector::core::EmbedRole,
	) -> Result<crate::vector::core::Embedding<Self::Model>, EmbedError> {
		let session = self.gate.acquire().await?;
		let _ = self.runtime_cache.set(session.runtime());
		session.embed(text, role).await
	}

	async fn embed_batch(
		&self,
		texts: &[&str],
		role: crate::vector::core::EmbedRole,
	) -> Result<Vec<crate::vector::core::Embedding<Self::Model>>, EmbedError> {
		let session = self.gate.acquire().await?;
		let _ = self.runtime_cache.set(session.runtime());
		session.embed_batch(texts, role).await
	}

	fn runtime(&self) -> crate::vector::core::EmbedRuntimeInfo {
		// Cached on first use. Before any embed has run (and hence before the
		// inner embedder ever existed), fall back to a best-effort live read;
		// if the slot is unloaded/contended, report the brand's static
		// contract — a status probe must never panic or force a model load.
		if let Some(info) = self.runtime_cache.get() {
			return info.clone();
		}
		if let Ok(guard) = self.gate.slot.try_lock() {
			if let Some(embedder) = &*guard {
				let info = embedder.runtime();
				let _ = self.runtime_cache.set(info.clone());
				return info;
			}
		}
		crate::vector::core::EmbedRuntimeInfo {
			model_id: <Self::Model as crate::vector::core::EmbeddingModel>::id(),
			accel: crate::vector::core::AccelKind::Cpu,
			durable_canonical: true,
			max_batch: super::MAX_BATCH,
			max_seq_len: super::MAX_SEQ_LEN,
			weights_sha256: None,
			ort_package_id: super::ORT_PACKAGE_ID.into(),
		}
	}
}

/// Periodically drops the embedder once it has sat idle past `unload_idle`
/// with no outstanding guards. Uses `tokio::time` so paused-clock tests work.
async fn idle_watchdog<E: Send + Sync + 'static>(gate: std::sync::Weak<EmbedGate<E>>) {
	loop {
		let period = match gate.upgrade() {
			Some(gate) => (gate.unload_idle / 4).max(Duration::from_millis(50)),
			None => return,
		};
		tokio::time::sleep(period).await;

		let Some(gate) = gate.upgrade() else { return };
		let all_permits_free = gate.permits.available_permits() == gate.max_sessions;
		if gate.state() == LoadState::Loaded
			&& all_permits_free
			&& gate.idle_for() >= gate.unload_idle
		{
			gate.unload().await;
		}
	}
}
