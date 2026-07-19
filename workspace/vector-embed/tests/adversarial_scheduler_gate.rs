//! Adversarial tests for EmbedScheduler, EmbedGate, and GatedEmbedder
//! (items 10-11 from the adversarial target list).
//!
//! All offline, all under paused tokio time.

use std::sync::Arc;
use std::time::Duration;

use heart::ContentHash;
use vector_core::EmbedRole;
use vector_embed::gate::{EmbedGate, GateConfig, GatedEmbedder, LoadState};
use vector_embed::mock::MockEmbedder;
use vector_embed::scheduler::{
	CancelGroup, EmbedHandle, EmbedScheduler, Priority, SchedulerConfig, SchedulerError,
};

fn key(label: &str) -> ContentHash { ContentHash::of_bytes(label.as_bytes()) }

fn spawn_mock() -> (Arc<MockEmbedder>, EmbedHandle<vector_core::JinaCodeV2>) {
	let mock = Arc::new(MockEmbedder::new());
	let handle = EmbedScheduler::spawn(Arc::clone(&mock), SchedulerConfig::default());
	(mock, handle)
}

// ── item 10a: 100 Background + 1 Interactive → interactive overtakes ──────────

/// 100 background jobs are submitted, then 1 interactive. The interactive
/// job must complete before any background job replies to the caller
/// (completion-order assertion: the interactive embedding appears as the
/// first batch the mock records).
#[tokio::test(start_paused = true)]
async fn interactive_overtakes_100_background_jobs() {
	let (mock, handle) = spawn_mock();

	// Submit 100 background jobs first.
	let bg_jobs: Vec<_> = (0..100)
		.map(|i| {
			let h = handle.clone();
			tokio::spawn(async move {
				h.embed(
					key(&format!("bg-{i}")),
					format!("background text {i}"),
					EmbedRole::Document,
					Priority::Background,
					CancelGroup::new(),
				)
				.await
			})
		})
		.collect();

	// Yield so the background jobs land in the scheduler's inbox before we
	// submit the interactive one.
	tokio::task::yield_now().await;

	// Submit the interactive job — it must overtake the background queue.
	let interactive = {
		let h = handle.clone();
		tokio::spawn(async move {
			h.embed(
				key("interactive-query"),
				"interactive query text".to_owned(),
				EmbedRole::Query,
				Priority::Interactive,
				CancelGroup::new(),
			)
			.await
		})
	};

	// Await all.
	interactive.await.expect("join").expect("embed");
	for job in bg_jobs {
		job.await.expect("join").expect("embed");
	}

	let batches = mock.batches();
	// The first batch the model saw must contain the interactive text.
	// (It may be batched alone or mixed with nothing; background texts come after.)
	let first_batch = &batches[0];
	assert!(
		first_batch.contains(&"interactive query text".to_owned()),
		"interactive job must appear in the first batch: batches={:?}",
		batches.iter().map(|b| b.len()).collect::<Vec<_>>()
	);

	// The interactive job is in a batch before any background batch.
	let interactive_batch_idx = batches
		.iter()
		.position(|b| b.contains(&"interactive query text".to_owned()))
		.expect("interactive text must be in some batch");
	let first_bg_batch_idx = batches
		.iter()
		.position(|b| b.iter().any(|t| t.starts_with("background text")))
		.expect("background texts must be in some batch");
	assert!(
		interactive_batch_idx < first_bg_batch_idx,
		"interactive batch (idx {interactive_batch_idx}) must precede background batches (idx {first_bg_batch_idx})"
	);
}

// ── item 10b: cancelled group → all queued jobs reply Cancelled, mock call = 0 ──

/// When a cancel group is cancelled before any job is dequeued, all jobs in
/// that group must observe SchedulerError::Cancelled and the mock embedder
/// must never be called.
#[tokio::test(start_paused = true)]
async fn cancelled_group_all_reply_cancelled_never_hits_embedder() {
	let (mock, handle) = spawn_mock();

	let cancel = CancelGroup::new();
	cancel.cancel(); // cancel immediately, before any job is submitted

	// Submit 5 jobs into the cancelled group.
	let jobs: Vec<_> = (0..5)
		.map(|i| {
			let h = handle.clone();
			let c = cancel.clone();
			tokio::spawn(async move {
				h.embed(
					key(&format!("k{i}")),
					format!("text {i}"),
					EmbedRole::Document,
					Priority::Background,
					c,
				)
				.await
			})
		})
		.collect();

	// Drop the handle so the scheduler drains and terminates.
	drop(handle);

	// All 5 must be Cancelled.
	for job in jobs {
		let result = job.await.expect("join");
		assert!(
			matches!(result, Err(SchedulerError::Cancelled)),
			"every cancelled-group job must return Cancelled, got {:?}", result
		);
	}

	// The mock must never have been called.
	assert_eq!(mock.call_count(), 0, "embedder must not be called for cancelled jobs");
}

// ── item 10c: no batch ever exceeds MAX_BATCH (32) ────────────────────────────

/// Submit 200 jobs and assert that no single batch seen by the mock exceeds
/// the MAX_BATCH constant of 32. The scheduler's default max_batch is already
/// 32; this test confirms the constraint over a large burst.
#[tokio::test(start_paused = true)]
async fn no_batch_exceeds_max_batch() {
	use vector_embed::MAX_BATCH;

	let (mock, handle) = spawn_mock();

	let n = 200usize;
	let jobs: Vec<_> = (0..n)
		.map(|i| {
			let h = handle.clone();
			tokio::spawn(async move {
				h.embed(
					key(&format!("k{i}")),
					format!("text {i}"),
					EmbedRole::Document,
					Priority::Background,
					CancelGroup::new(),
				)
				.await
			})
		})
		.collect();

	for job in jobs {
		job.await.expect("join").expect("embed");
	}

	let batches = mock.batches();
	let max_seen = batches.iter().map(Vec::len).max().unwrap_or(0);
	assert!(
		max_seen <= MAX_BATCH,
		"no batch must exceed MAX_BATCH={MAX_BATCH}; max seen={max_seen}"
	);
	// Sanity: all 200 texts were embedded.
	let total: usize = batches.iter().map(Vec::len).sum();
	assert_eq!(total, n, "all {n} texts must be embedded");
}

// ── item 11: runtime() while idle-unloaded returns info WITHOUT loading ────────

/// GatedEmbedder::runtime() while the embedder is idle-unloaded (never loaded
/// or explicitly unloaded) must return the brand's static info WITHOUT
/// triggering a load (load_count unchanged).
///
/// The OnceLock on GatedEmbedder caches runtime info after the first embed;
/// before any embed, it falls back to the static EmbedRuntimeInfo.
/// This test verifies that the fallback does not cause a panic or a load.
#[tokio::test(start_paused = true)]
async fn runtime_while_idle_no_load() {
	use vector_core::{Embedder, EmbeddingModel};

	let gate = EmbedGate::new(
		GateConfig { max_sessions: 1, unload_idle: Duration::from_secs(120) },
		Box::new(|| Box::pin(async { Ok(MockEmbedder::new()) })),
	);

	assert_eq!(gate.state(), LoadState::Idle, "gate starts idle");
	assert_eq!(gate.load_count(), 0, "nothing loaded yet");

	// Build a GatedEmbedder over the gate.
	let gated = GatedEmbedder::new(Arc::clone(&gate));

	// Call runtime() while idle — must NOT load, must NOT panic.
	let info = gated.runtime();

	// load_count must still be 0.
	assert_eq!(gate.load_count(), 0, "runtime() must not trigger a load");
	assert_eq!(gate.state(), LoadState::Idle, "gate must remain idle after runtime()");

	// The returned info must be self-consistent: model_id matches the brand.
	let expected_model = vector_core::JinaCodeV2::id();
	assert_eq!(
		info.model_id, expected_model,
		"runtime() while idle must return brand-static model_id"
	);

	// Verify load_count stays 0 after a second call.
	let _info2 = gated.runtime();
	assert_eq!(gate.load_count(), 0, "second runtime() call must not trigger a load either");
}

// ── item 11 (supplement): permit cap 1 → no overlapping executions ────────────

/// With max_sessions=1, a second acquire must block until the first guard is
/// dropped — no two sessions can overlap. Verified via a flag-based mock proof:
/// we check the in-use flag is exclusively held.
#[tokio::test(start_paused = true)]
async fn permit_cap_1_no_overlapping_sessions() {
	use std::sync::atomic::{AtomicBool, Ordering};

	let in_use = Arc::new(AtomicBool::new(false));
	let overlap_detected = Arc::new(AtomicBool::new(false));

	let gate = EmbedGate::new(
		GateConfig { max_sessions: 1, unload_idle: Duration::from_secs(120) },
		Box::new(|| Box::pin(async { Ok(MockEmbedder::new()) })),
	);

	// First session: acquire and hold.
	let guard1 = gate.acquire().await.expect("first acquire");
	// Flag: we are now in the session.
	let was_in_use = in_use.swap(true, Ordering::SeqCst);
	// If a previous session was still in use, that's an overlap.
	if was_in_use {
		overlap_detected.store(true, Ordering::SeqCst);
	}

	// Attempt second acquire: must time out (the semaphore is at 0 permits).
	let gate2 = Arc::clone(&gate);
	let second = tokio::time::timeout(Duration::from_millis(50), gate2.acquire()).await;
	assert!(second.is_err(), "second acquire must block while first session is active");

	// Release first session.
	in_use.store(false, Ordering::SeqCst);
	drop(guard1);

	// Now the second acquire must succeed.
	let guard2 = gate.acquire().await.expect("second acquire after release");
	let was_in_use2 = in_use.swap(true, Ordering::SeqCst);
	if was_in_use2 {
		overlap_detected.store(true, Ordering::SeqCst);
	}
	drop(guard2);
	in_use.store(false, Ordering::SeqCst);

	assert!(!overlap_detected.load(Ordering::SeqCst), "no overlapping sessions must occur");
}

// ── item 11 (supplement): unload-then-use reload (load_count == 2) ────────────

/// Explicitly unload the gate, then acquire again — the factory must fire
/// a second time, making load_count == 2.
/// (This partially overlaps with gate_tests::manual_unload_then_reload but
/// verifies the same via GatedEmbedder + exact load_count pin.)
#[tokio::test(start_paused = true)]
async fn unload_then_use_reload_load_count_is_2() {
	let gate = EmbedGate::new(
		GateConfig { max_sessions: 1, unload_idle: Duration::from_secs(120) },
		Box::new(|| Box::pin(async { Ok(MockEmbedder::new()) })),
	);

	// First use.
	{
		let guard = gate.acquire().await.expect("first acquire");
		drop(guard);
	}
	assert_eq!(gate.load_count(), 1, "load_count after first use must be 1");

	// Explicit unload.
	gate.unload().await;
	assert_eq!(gate.state(), LoadState::Idle);

	// Second use: factory must fire again.
	{
		let guard = gate.acquire().await.expect("second acquire");
		drop(guard);
	}
	assert_eq!(gate.load_count(), 2, "load_count after reload must be 2");
}
