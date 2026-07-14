//! A stampede-resistant cache built on moka's L1 with three layers of herd
//! protection stacked on top:
//!
//! 1. **Coalescing** ([`crate::single_flight`]) — on a miss, one leader computes
//!    while concurrent callers park and then read the leader's result. `N`
//!    simultaneous misses cost one compute.
//! 2. **Probabilistic early recomputation** (XFetch, Vattani et al. 2015,
//!    *"Optimal Probabilistic Cache Stampede Prevention"*) — as an entry nears
//!    expiry, each read has a small, cost-weighted chance of refreshing it
//!    *early*, in the background. Expensive entries (large `cost`) refresh
//!    sooner and the randomness spreads refreshes out, so a popular key never
//!    has a single hard expiry instant that every request slams at once.
//! 3. **Stale-while-revalidate** — that early refresh happens on a spawned task
//!    while the current (still-valid) value is served immediately, so a refresh
//!    never adds latency to the request that triggered it.
//!
//! moka provides the bounded, concurrent, TinyLFU-admission L1; this type adds
//! the timing intelligence. A disk-backed L2 (à la a brotli `TempCache`) is a
//! deliberate follow-up — it needs a vendored `foyer`/`cacache` and is out of
//! scope here; the `cost`/`Entry` bookkeeping is already L2-ready.

use std::{
	future::Future,
	hash::Hash,
	sync::Arc,
	time::{Duration, Instant},
};

use moka::future::Cache;

use crate::single_flight::{SingleFlight, Ticket};

/// A cached value plus the timing metadata that drives probabilistic refresh.
#[derive(Clone)]
struct Entry<V> {
	value:       V,
	/// When this value was produced (monotonic).
	computed_at: Instant,
	/// How long the producing compute took — the XFetch `delta`. Costlier
	/// entries are refreshed earlier to hide their longer recompute latency.
	cost:        Duration,
}

/// A clone-cheap handle to a stampede-resistant cache keyed by `K` holding `V`.
///
/// Cloning shares the underlying moka store and in-flight set (both are already
/// `Arc`-internally), so the whole thing can be moved into spawned refresh tasks.
pub struct StampedeCache<K, V>
where
	K: Eq + Hash + Send + Sync + Clone + 'static,
	V: Clone + Send + Sync + 'static,
{
	cache:    Cache<K, Entry<V>>,
	inflight: Arc<SingleFlight<K>>,
	/// Soft time-to-live: the age at which XFetch's refresh probability reaches
	/// certainty. moka's own eviction TTL is set slightly longer (see [`new`]) so
	/// a value stays *servable* through its refresh window (stale-while-revalidate).
	ttl:      Duration,
	/// XFetch aggressiveness. `>1.0` refreshes earlier (favours freshness),
	/// `<1.0` later (favours fewer recomputes). `1.0` is the paper's optimum.
	beta:     f64,
}

impl<K, V> Clone for StampedeCache<K, V>
where
	K: Eq + Hash + Send + Sync + Clone + 'static,
	V: Clone + Send + Sync + 'static,
{
	fn clone(&self) -> Self {
		Self {
			cache:    self.cache.clone(),
			inflight: self.inflight.clone(),
			ttl:      self.ttl,
			beta:     self.beta,
		}
	}
}

impl<K, V> StampedeCache<K, V>
where
	K: Eq + Hash + Send + Sync + Clone + 'static,
	V: Clone + Send + Sync + 'static,
{
	/// Build a cache holding up to `capacity` entries with soft TTL `ttl`.
	///
	/// The moka eviction TTL is set to `ttl` plus a grace window so entries
	/// remain servable while a background refresh is in flight. `beta` defaults
	/// to `1.0` (the XFetch optimum) via [`StampedeCache::with_beta`].
	pub fn new(capacity: u64, ttl: Duration) -> Self {
		// A grace window of one extra TTL keeps values servable during refresh;
		// XFetch almost always refreshes well before the soft TTL anyway.
		let hard = ttl.saturating_mul(2);
		let cache = Cache::builder().max_capacity(capacity).time_to_live(hard).build();
		Self { cache, inflight: Arc::new(SingleFlight::new()), ttl, beta: 1.0 }
	}

	/// Override the XFetch `beta`. Higher = refresh earlier (fresher, more
	/// recomputes); lower = later (staler, fewer recomputes).
	pub fn with_beta(mut self, beta: f64) -> Self {
		self.beta = beta.max(0.0);
		self
	}

	/// Coalesced read-through. On a hit, returns the cached value (spawning a
	/// probabilistic background refresh via [`Self::maybe_refresh`] if the entry
	/// is nearing expiry). On a miss, exactly one caller computes while the rest
	/// park and then read the freshly stored value.
	///
	/// `compute` is `Fn` (not `FnOnce`) because a waiter whose leader *failed*
	/// re-races and may itself become the leader — failures never poison the key
	/// and never get shared, so non-`Clone` error types pass straight through.
	pub async fn get_or_load<E, F, Fut>(&self, key: K, compute: F) -> Result<V, E>
	where
		F: Fn() -> Fut + Send + Sync + Clone + 'static,
		Fut: Future<Output = Result<V, E>> + Send,
		E: Send + 'static,
	{
		loop {
			if let Some(entry) = self.cache.get(&key).await {
				self.maybe_refresh(&key, &entry, &compute);
				return Ok(entry.value);
			}

			match self.inflight.enter(key.clone()) {
				Ticket::Leader(_guard) => {
					let started = Instant::now();
					let result = compute().await;
					if let Ok(value) = &result {
						let cost = started.elapsed();
						self.store(key.clone(), value.clone(), cost).await;
					}
					// `_guard` drops here: key released, waiters woken. A failed
					// compute stores nothing, so a waiter re-races and retries.
					return result;
				},
				Ticket::Waiter(notify) => {
					// Lost-wakeup-safe: register interest *before* re-checking.
					let notified = notify.notified();
					tokio::pin!(notified);
					notified.as_mut().enable();
					if self.cache.get(&key).await.is_some() {
						continue; // leader stored while we were parking
					}
					if !self.inflight.is_inflight(&key) {
						continue; // leader already gone (likely failed) → re-race
					}
					notified.await;
					// Loop: read the value the leader just stored, or re-race.
				},
			}
		}
	}

	/// The XFetch decision (Vattani et al.): refresh early with probability that
	/// rises as the entry ages, scaled by its recompute cost. Returns `true` when
	/// `age + cost * beta * (-ln U) >= ttl` for a uniform `U ∈ (0, 1]` — so a
	/// costlier entry, or an unlucky draw, triggers a refresh before hard expiry.
	fn should_refresh(&self, entry: &Entry<V>) -> bool {
		let age = entry.computed_at.elapsed();
		let u: f64 = rand::random::<f64>().max(f64::MIN_POSITIVE); // avoid ln(0)
		let xfetch = entry.cost.as_secs_f64() * self.beta * -u.ln();
		age.as_secs_f64() + xfetch >= self.ttl.as_secs_f64()
	}

	/// If `entry` is in its XFetch window and no refresh is already in flight,
	/// spawn a coalesced background recompute. The current value keeps being
	/// served in the meantime (stale-while-revalidate).
	fn maybe_refresh<F, Fut, E>(&self, key: &K, entry: &Entry<V>, compute: &F)
	where
		F: Fn() -> Fut + Send + Sync + Clone + 'static,
		Fut: Future<Output = Result<V, E>> + Send,
		E: Send + 'static,
	{
		if !self.should_refresh(entry) {
			return;
		}
		if self.inflight.is_inflight(key) {
			return; // a refresh (or a miss-fill) is already running
		}
		let this = self.clone();
		let key = key.clone();
		let compute = compute.clone();
		tokio::spawn(async move {
			match this.inflight.enter(key.clone()) {
				Ticket::Leader(_guard) => {
					let started = Instant::now();
					if let Ok(value) = compute().await {
						this.store(key, value, started.elapsed()).await;
					}
				},
				// Someone else grabbed the refresh between our check and the
				// spawn — nothing to do.
				Ticket::Waiter(_) => {},
			}
		});
	}

	async fn store(&self, key: K, value: V, cost: Duration) {
		self.cache
			.insert(key, Entry { value, computed_at: Instant::now(), cost })
			.await;
	}

	/// Peek without computing. Returns the stored value on a hit, `None` on a miss.
	pub async fn get(&self, key: &K) -> Option<V> {
		self.cache.get(key).await.map(|e| e.value)
	}

	/// Insert `value` for `key` with an assumed `cost` (used to seed XFetch for
	/// values produced outside [`Self::get_or_load`], e.g. warm-fill on startup).
	pub async fn insert(&self, key: K, value: V, cost: Duration) {
		self.store(key, value, cost).await;
	}

	/// Drop `key` from the cache. The next read recomputes. Use for explicit,
	/// event-driven invalidation (e.g. an outbox generation bump).
	pub async fn invalidate(&self, key: &K) { self.cache.invalidate(key).await; }

	/// Best-effort entry count (moka's approximate size).
	pub fn entry_count(&self) -> u64 { self.cache.entry_count() }
}
