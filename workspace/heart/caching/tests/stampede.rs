//! Behavioural tests for the stampede-resistant cache: the herd collapses to one
//! compute, distinct keys stay independent, hits don't recompute, a failed leader
//! doesn't poison the key, and TTL jitter stays in bounds. All exercise the public
//! API only, so they double as usage examples.

use std::{
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	time::Duration,
};

use caching::{StampedeCache, jittered};

/// A non-`Clone` error, to prove the coalescer/cache never require `E: Clone` —
/// the property that makes the embedding cache avoid moka's `try_get_with`.
#[derive(Debug)]
struct NonCloneErr(#[allow(dead_code)] String);

#[tokio::test]
async fn coalesces_concurrent_misses_to_one_compute() {
	let cache: StampedeCache<u64, u64> = StampedeCache::new(1_000, Duration::from_secs(60));
	let calls = Arc::new(AtomicUsize::new(0));

	// Fire many concurrent loads on the *same* key. Exactly one should compute.
	let mut handles = Vec::new();
	for _ in 0..64 {
		let cache = cache.clone();
		let calls = calls.clone();
		handles.push(tokio::spawn(async move {
			cache
				.get_or_load(7u64, move || {
					let calls = calls.clone();
					async move {
						calls.fetch_add(1, Ordering::SeqCst);
						// Hold the key long enough that the herd piles up behind it.
						tokio::time::sleep(Duration::from_millis(50)).await;
						Ok::<u64, NonCloneErr>(42)
					}
				})
				.await
				.unwrap()
		}));
	}

	for h in handles {
		assert_eq!(h.await.unwrap(), 42);
	}
	assert_eq!(calls.load(Ordering::SeqCst), 1, "the herd must collapse to one compute");
}

#[tokio::test]
async fn distinct_keys_compute_independently() {
	let cache: StampedeCache<u64, u64> = StampedeCache::new(1_000, Duration::from_secs(60));
	let calls = Arc::new(AtomicUsize::new(0));

	let mut handles = Vec::new();
	for k in 0..8u64 {
		let cache = cache.clone();
		let calls = calls.clone();
		handles.push(tokio::spawn(async move {
			cache
				.get_or_load(k, move || {
					let calls = calls.clone();
					async move {
						calls.fetch_add(1, Ordering::SeqCst);
						Ok::<u64, NonCloneErr>(k * 10)
					}
				})
				.await
				.unwrap()
		}));
	}
	for (i, h) in handles.into_iter().enumerate() {
		assert_eq!(h.await.unwrap(), i as u64 * 10);
	}
	assert_eq!(calls.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn hit_serves_without_recompute() {
	let cache: StampedeCache<u64, u64> = StampedeCache::new(1_000, Duration::from_secs(3600));
	let calls = Arc::new(AtomicUsize::new(0));
	let compute = {
		let calls = calls.clone();
		move || {
			let calls = calls.clone();
			async move {
				calls.fetch_add(1, Ordering::SeqCst);
				Ok::<u64, NonCloneErr>(99)
			}
		}
	};

	assert_eq!(cache.get_or_load(1, compute.clone()).await.unwrap(), 99);
	assert_eq!(cache.get_or_load(1, compute.clone()).await.unwrap(), 99);
	// Long TTL, so the second call is a pure hit with no background refresh.
	assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn leader_failure_lets_a_waiter_retry() {
	let cache: StampedeCache<u64, u64> = StampedeCache::new(1_000, Duration::from_secs(60));
	let attempt = Arc::new(AtomicUsize::new(0));

	// First compute fails, later ones succeed. A failed leader must not poison
	// the key: some caller retries and everyone ends up with the value.
	let mut handles = Vec::new();
	for _ in 0..16 {
		let cache = cache.clone();
		let attempt = attempt.clone();
		handles.push(tokio::spawn(async move {
			cache
				.get_or_load(5u64, move || {
					let attempt = attempt.clone();
					async move {
						let n = attempt.fetch_add(1, Ordering::SeqCst);
						tokio::time::sleep(Duration::from_millis(20)).await;
						if n == 0 {
							Err(NonCloneErr("first attempt fails".into()))
						} else {
							Ok::<u64, NonCloneErr>(1234)
						}
					}
				})
				.await
		}));
	}
	let mut ok = 0;
	let mut err = 0;
	for h in handles {
		match h.await.unwrap() {
			Ok(v) => {
				assert_eq!(v, 1234);
				ok += 1;
			},
			Err(_) => err += 1,
		}
	}
	assert!(ok >= 1, "at least one caller must observe the eventual success");
	assert!(err <= 1, "only the original leader should see the failure");
}

#[test]
fn jitter_stays_in_bounds() {
	let base = Duration::from_secs(100);
	for _ in 0..1_000 {
		let j = jittered(base, 0.2);
		assert!(j >= Duration::from_secs(80) && j <= Duration::from_secs(120));
	}
	assert_eq!(jittered(base, 0.0), base);
}
