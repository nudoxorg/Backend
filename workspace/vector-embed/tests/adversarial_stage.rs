//! Adversarial tests for EmbedStage (items 9 from the adversarial target list).
//!
//! All offline. Uses MockEmbedder + in-memory trait impls from support/.
//! Tests pin exact counts and panic-proof behaviours.

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heart::{ContentHash, SymbolId};
use support::*;
use vector_core::JinaCodeV2;
use vector_core::store::{PointId, SearchFilter, SearchHit, StoreCapabilities, StoreError, VectorPoint, VectorStore};
use vector_embed::mock::MockEmbedder;
use vector_embed::scheduler::{CancelGroup, EmbedScheduler, SchedulerConfig};
use vector_embed::stage::{EmbedStage, StageConfig, StageError, TraceStore, VectorCas};

// ── helpers ──────────────────────────────────────────────────────────────────

struct Harness {
	mock: Arc<MockEmbedder>,
	traces: MemTraces,
	cas: MemCas,
	store: MemStore,
	stage: EmbedStage<MemStore>,
}

fn harness() -> Harness {
	let mock = Arc::new(MockEmbedder::new());
	let traces = MemTraces::new();
	let cas = MemCas::new();
	let store = MemStore::new();
	let handle = EmbedScheduler::spawn(Arc::clone(&mock), SchedulerConfig::default());
	let stage = EmbedStage::new(
		store.clone(),
		Box::new(traces.clone()),
		Box::new(cas.clone()),
		handle,
		Box::new(WhitespaceCounter),
		StageConfig::default(),
	);
	Harness { mock, traces, cas, store, stage }
}

// ── FailingStore: upsert always returns an I/O error ─────────────────────────

#[derive(Clone, Default)]
struct FailingStore {
	/// Number of times upsert was called before failing.
	calls: Arc<Mutex<usize>>,
	/// Fail after this many successful upserts (0 = fail immediately).
	fail_after: usize,
	inner: MemStore,
}

impl FailingStore {
	fn new(fail_after: usize) -> Self {
		Self { calls: Arc::new(Mutex::new(0)), fail_after, inner: MemStore::new() }
	}
}

#[async_trait]
impl VectorStore<JinaCodeV2> for FailingStore {
	async fn upsert(&self, points: Vec<VectorPoint<JinaCodeV2>>) -> Result<(), StoreError> {
		let mut calls = self.calls.lock().unwrap();
		if *calls >= self.fail_after {
			return Err(StoreError::Backend("injected failure".to_owned()));
		}
		*calls += 1;
		self.inner.upsert(points).await
	}

	async fn delete(&self, ids: &[PointId]) -> Result<(), StoreError> {
		self.inner.delete(ids).await
	}

	async fn search(&self, _req: vector_core::store::SearchRequest<JinaCodeV2>) -> Result<Vec<SearchHit>, StoreError> {
		Ok(vec![])
	}

	async fn count(&self, _filter: Option<&SearchFilter>) -> Result<u64, StoreError> {
		Ok(0)
	}

	async fn flush(&self) -> Result<(), StoreError> { Ok(()) }

	async fn compact(&self) -> Result<(), StoreError> { Ok(()) }

	fn capabilities(&self) -> StoreCapabilities {
		StoreCapabilities {
			filtered_search: false,
			disk_resident: false,
			exact_count: true,
			backend: "failing-test-store".into(),
		}
	}
}

// ── 9a: upsert failure on chunk N leaves ZERO traces for that chunk ───────────

/// When upsert fails, no trace must be written for ANY symbol in that chunk
/// (traces are written AFTER upsert; a crash mid-run re-does the tail on next
/// seal — 09b §16.9).
#[tokio::test(start_paused = true)]
async fn upsert_failure_leaves_no_traces_for_that_chunk() {
	let mock = Arc::new(MockEmbedder::new());
	let traces = MemTraces::new();
	let cas = MemCas::new();
	// Store fails immediately on any upsert.
	let store = FailingStore::new(0);
	let handle = EmbedScheduler::spawn(Arc::clone(&mock), SchedulerConfig::default());
	let stage = EmbedStage::new(
		store,
		Box::new(traces.clone()),
		Box::new(cas.clone()),
		handle,
		Box::new(WhitespaceCounter),
		StageConfig::default(),
	);

	let corpus: Vec<TestSymbol> = (0..3).map(symbol).collect();
	let result = stage.run(&delta_added(&corpus), &facet_lookup(&corpus), CancelGroup::new()).await;

	// Stage must fail (store error propagates).
	assert!(result.is_err(), "upsert failure must propagate to stage run");
	// Zero traces for the failed chunk — the durability ordering requires this.
	assert_eq!(traces.len(), 0, "no traces must be written when upsert fails");
}

// ── 9b: all-trace-hit → infer==0 && upsert==0 && skip_trace==|delta| ─────────

/// When every symbol in the delta is already covered by a stage-trace row,
/// the stage does zero model calls, zero upserts, and skip_trace == |delta|.
#[tokio::test(start_paused = true)]
async fn all_trace_hit_zero_work() {
	let h = harness();
	let corpus: Vec<TestSymbol> = (0..7).map(symbol).collect();
	let delta = delta_added(&corpus);

	// First run: cold ingest — writes traces for all 7.
	let cold = h.stage.run(&delta, &facet_lookup(&corpus), CancelGroup::new()).await.unwrap();
	assert_eq!(cold.infer_count, 7);

	let calls_before = h.mock.call_count();
	let upserted_before = h.store.upserted();

	// Second run: every symbol hits L1.
	let report = h.stage.run(&delta, &facet_lookup(&corpus), CancelGroup::new()).await.unwrap();
	assert_eq!(report.infer_count, 0, "infer==0 on all-trace-hit");
	assert_eq!(report.upsert_count, 0, "upsert==0 on all-trace-hit");
	assert_eq!(report.skip_trace, 7, "skip_trace == |delta|");
	assert_eq!(h.mock.call_count(), calls_before, "model not called");
	assert_eq!(h.store.upserted(), upserted_before, "store not touched");
}

// ── 9c: CAS poisoned with wrong-dim → DimensionMismatch, no trace written ────

/// A CAS blob of the wrong dimension (not 768) must produce
/// EmbedError::DimensionMismatch and leave NO trace row.
#[tokio::test(start_paused = true)]
async fn cas_wrong_dim_gives_dimension_mismatch_no_trace() {
	use vector_core::EmbeddingModel;
	use vector_embed::stage::StageConfig;
	use vector_core::key as vkey;
	use vector_core::recipe::VectorName;
	use vector_core::model::Metric;

	let mock = Arc::new(MockEmbedder::new());
	let traces = MemTraces::new();
	let cas = MemCas::new();
	let store = MemStore::new();
	let handle = EmbedScheduler::spawn(Arc::clone(&mock), SchedulerConfig::default());
	let stage = EmbedStage::new(
		store.clone(),
		Box::new(traces.clone()),
		Box::new(cas.clone()),
		handle,
		Box::new(WhitespaceCounter),
		StageConfig::default(),
	);

	let sym = symbol(0);
	let config = StageConfig::default();

	// Compute the embed key the stage will look up.
	let embed_key = vkey::embed_key(
		&config.model_id,
		VectorName::Sym,
		JinaCodeV2::DIMENSIONS,
		Metric::Cosine,
		&sym.parts,
		sym.facets.moniker.as_bytes(),
		&sym.facets.kind,
	);

	// Poison the CAS with a wrong-dimension blob (only 3 floats, not 768).
	cas.insert(embed_key, vec![0.1f32, 0.2, 0.3]);

	let corpus = vec![sym];
	let result = stage.run(&delta_added(&corpus), &facet_lookup(&corpus), CancelGroup::new()).await;

	// Must fail with an embed (dimension mismatch) error.
	assert!(result.is_err(), "wrong-dim CAS blob must cause stage failure");
	match result.unwrap_err() {
		StageError::Embed(vector_core::EmbedError::DimensionMismatch { expected, got }) => {
			assert_eq!(expected, JinaCodeV2::DIMENSIONS);
			assert_eq!(got, 3);
		}
		other => panic!("expected StageError::Embed(DimensionMismatch), got {other:?}"),
	}

	// No trace written for the failed symbol.
	assert_eq!(traces.len(), 0, "no trace must be written when CAS blob has wrong dim");
}

// ── 9d: same symbol in added AND changed → pin exact behavior ─────────────────

/// If the same SymbolId appears in both `added` and `changed`, the stage
/// processes it twice (once per occurrence). Both are present in `work`;
/// this test pins the exact count behavior: 2 embeddings, 2 upserts.
///
/// NOTE: this is a "malformed delta" scenario — the producer should never
/// emit the same id in both lists. But the stage must not crash or panic;
/// it just does the double work.
#[tokio::test(start_paused = true)]
async fn symbol_in_added_and_changed_pinned_behavior() {
	use vector_core::key::ChangedSymbol;
	use vector_core::key::SymbolDelta;

	let h = harness();
	let sym = symbol(0);
	let old_parts = parts("sig-old", "body-old", "doc-old");

	// Construct a delta where sym appears in both added and changed.
	let delta = SymbolDelta {
		added: vec![(sym.id, sym.parts.clone())],
		removed: Vec::new(),
		changed: vec![ChangedSymbol { id: sym.id, old: old_parts, new: sym.parts.clone() }],
	};

	let corpus = vec![sym];
	let report = h.stage.run(&delta, &facet_lookup(&corpus), CancelGroup::new()).await.unwrap();

	// Pin exact behavior: both occurrences land in the same chunk (2 < MAX_BATCH=32).
	// Phase 1 classifies all items before any trace is written, so both the `added`
	// entry and the `changed` entry (same embed_key) miss L1 and go to need_infer.
	// Phase 3 upserts both and writes 1 trace (second insert is idempotent in MemStore).
	// Total: infer==2, upsert==2, skip_trace==0.
	assert_eq!(report.skip_trace, 0, "no L1 hits within one chunk before traces are written");
	assert_eq!(report.infer_count, 2, "both occurrences embed independently within the chunk");
	assert_eq!(report.upsert_count, 2, "both occurrences upsert (idempotent under same id)");
}

// ── 9e: cancellation mid-run → Cancelled, no traces for cancelled tail ────────

/// A cancel group activated between two chunks: the first chunk completes
/// (with traces), the second chunk is never started (no traces for its symbols).
///
/// The stage processes `MAX_BATCH` items per chunk. We build a corpus of
/// 2·MAX_BATCH symbols so we get at least two chunks.
#[tokio::test(start_paused = true)]
async fn cancellation_mid_run_no_traces_for_tail() {
	use vector_embed::MAX_BATCH;

	// Use 2×MAX_BATCH so we get two chunks.
	let n = MAX_BATCH * 2;
	let corpus: Vec<TestSymbol> = (0..n).map(symbol).collect();

	// A TraceStore that cancels the CancelGroup after the first put.
	struct CancelOnFirstTrace {
		inner: MemTraces,
		cancel: CancelGroup,
		count: Arc<Mutex<usize>>,
	}

	#[async_trait]
	impl TraceStore for CancelOnFirstTrace {
		async fn has(&self, stage_id: &str, input: &ContentHash, tool: &ContentHash) -> bool {
			self.inner.has(stage_id, input, tool).await
		}
		async fn put(&self, stage_id: &str, input: &ContentHash, tool: &ContentHash) {
			self.inner.put(stage_id, input, tool).await;
			let mut c = self.count.lock().unwrap();
			*c += 1;
			// Cancel after the first full chunk's traces are written.
			if *c >= MAX_BATCH {
				self.cancel.cancel();
			}
		}
	}

	let mock = Arc::new(MockEmbedder::new());
	let cancel = CancelGroup::new();
	let cancel_trace_store = CancelOnFirstTrace {
		inner: MemTraces::new(),
		cancel: cancel.clone(),
		count: Arc::new(Mutex::new(0)),
	};
	let inner_traces = cancel_trace_store.inner.clone();
	let cas = MemCas::new();
	let store = MemStore::new();
	let handle = EmbedScheduler::spawn(Arc::clone(&mock), SchedulerConfig::default());
	let stage = EmbedStage::new(
		store,
		Box::new(cancel_trace_store),
		Box::new(cas),
		handle,
		Box::new(WhitespaceCounter),
		StageConfig::default(),
	);

	let result = stage.run(&delta_added(&corpus), &facet_lookup(&corpus), cancel).await;

	// Must report Cancelled.
	assert!(matches!(result, Err(StageError::Cancelled)),
		"stage must return Cancelled when CancelGroup is set: {:?}", result);

	// Traces must be <= MAX_BATCH (first chunk only; second chunk cancelled).
	let trace_count = inner_traces.len();
	assert!(trace_count <= MAX_BATCH,
		"traces for cancelled tail must not exist: got {trace_count}, expected <= {MAX_BATCH}");
	assert!(trace_count > 0, "first chunk must have completed before cancellation");
}

// ── 9f: removed ids never reach facets_for (panicking closure proof) ──────────

/// The stage must NEVER call facets_for for removed symbols. We pass a
/// closure that panics on any call — if the stage accidentally routes a
/// removed symbol through the facet lookup, the test panics (proof by
/// absence of panic = invariant holds).
#[tokio::test(start_paused = true)]
async fn removed_ids_never_reach_facets_for() {
	let h = harness();
	let corpus: Vec<TestSymbol> = (0..3).map(symbol).collect();

	// Cold index so the points exist.
	h.stage.run(&delta_added(&corpus), &facet_lookup(&corpus), CancelGroup::new()).await.unwrap();

	let removed_ids: Vec<SymbolId> = corpus.iter().map(|s| s.id).collect();
	let delta = delta_removed(removed_ids);

	// A facets_for that panics if called — proves removed ids never enter it.
	let panic_lookup = |_id: &SymbolId| -> Option<_> {
		panic!("facets_for must NEVER be called for removed symbols");
		#[allow(unreachable_code)]
		None::<vector_core::recipe::EmbedFacetsBuf>
	};

	let report = h.stage.run(&delta, &panic_lookup, CancelGroup::new()).await.unwrap();

	// If we reach here, no panic occurred — the invariant holds.
	assert_eq!(report.deleted, 3, "all removed symbols must be deleted from store");
	assert_eq!(report.infer_count, 0, "no inference for removed symbols");
}
