#![cfg(feature = "embed")]
//! EmbedStage acceptance tests (09b §16) — fully offline via MockEmbedder.

mod support;

use std::sync::Arc;

use support::*;
use registry::vector::{EmbeddingModel, JinaCodeV2, Metric};
use registry::vector::key as vkey;
use registry::vector::recipe::VectorName;
use registry::vector::embed::mock::{MockEmbedder, deterministic_unit_vector};
use registry::vector::embed::scheduler::{CancelGroup, EmbedScheduler, SchedulerConfig};
use registry::vector::embed::stage::{EmbedStage, Error, StageConfig, TraceStore};
use registry::vector::embed::stage;

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

/// The stage-identical embed key for a test symbol.
fn expected_key(symbol: &TestSymbol) -> heart::ContentHash {
	vkey::embed_key(
		&JinaCodeV2::id(),
		VectorName::Sym,
		JinaCodeV2::DIMENSIONS,
		Metric::Cosine,
		&symbol.parts,
		symbol.facets.moniker.as_bytes(),
		&symbol.facets.kind,
	)
}

/// 09b §16.4 acceptance: a doc-only edit of one symbol in a 100-symbol corpus
/// produces exactly one inference and one upsert.
#[tokio::test(start_paused = true)]
async fn doc_only_edit_infers_exactly_once() {
	let h = harness();
	let mut corpus: Vec<TestSymbol> = (0..100).map(symbol).collect();

	// Gen N-1: cold index of all 100 symbols.
	let cold = h
		.stage
		.run(&delta_added(&corpus), &facet_lookup(&corpus), CancelGroup::new())
		.await
		.expect("cold index");
	assert_eq!(cold.infer_count, 100);
	assert_eq!(cold.upsert_count, 100);
	assert_eq!(h.store.point_count(), 100);

	// Gen N: symbol 42's doc comment changes; nothing else does.
	let old_parts = corpus[42].parts.clone();
	corpus[42].parts = parts("sig-42", "body-42", "doc-42-EDITED");
	corpus[42].facets = facets("pkg::module::sym_42", "fn sym_42()", "edited doc", "body tokens here");
	let delta = delta_changed(&corpus[42], old_parts);

	let embeds_before = h.mock.text_count();
	let report = h
		.stage
		.run(&delta, &facet_lookup(&corpus), CancelGroup::new())
		.await
		.expect("delta run");

	// THE acceptance assertion.
	assert_eq!(report.infer_count, 1, "exactly one model inference");
	assert_eq!(report.upsert_count, 1, "exactly one store upsert");
	assert_eq!(report.skip_trace, 0);
	assert_eq!(report.skip_cas, 0);
	assert_eq!(h.mock.text_count() - embeds_before, 1, "unchanged symbols never embedded");
	assert_eq!(h.store.point_count(), 100, "same point set, one vector replaced");
}

/// L1: an identical delta re-run hits stage traces and does zero work.
#[tokio::test(start_paused = true)]
async fn trace_hit_skips_infer_and_upsert() {
	let h = harness();
	let corpus: Vec<TestSymbol> = (0..5).map(symbol).collect();
	let delta = delta_added(&corpus);

	h.stage.run(&delta, &facet_lookup(&corpus), CancelGroup::new()).await.expect("first run");
	assert_eq!(h.traces.len(), 5);

	let rerun = h
		.stage
		.run(&delta, &facet_lookup(&corpus), CancelGroup::new())
		.await
		.expect("second run");
	assert_eq!(rerun.infer_count, 0);
	assert_eq!(rerun.upsert_count, 0);
	assert_eq!(rerun.skip_trace, 5);
	assert_eq!(h.mock.text_count(), 5, "no extra model calls on the rerun");
}

/// L2: a CAS blob for the embed_key is upserted without inference, and the
/// trace is written so the next run is an L1 skip.
#[tokio::test(start_paused = true)]
async fn cas_hit_upserts_without_infer() {
	let h = harness();
	let corpus = vec![symbol(0)];
	let key = expected_key(&corpus[0]);
	h.cas.insert(key, deterministic_unit_vector("some prior run"));

	let report = h
		.stage
		.run(&delta_added(&corpus), &facet_lookup(&corpus), CancelGroup::new())
		.await
		.expect("run");

	assert_eq!(report.infer_count, 0);
	assert_eq!(report.skip_cas, 1);
	assert_eq!(report.upsert_count, 1);
	assert_eq!(h.mock.call_count(), 0, "model never invoked");
	assert_eq!(h.store.point_count(), 1);
	assert!(h.traces.has(stage::STAGE_ID, &key, &tool_digest()).await, "trace written for CAS hit");
}

fn tool_digest() -> heart::ContentHash {
	let config = StageConfig::default();
	vkey::tool_digest(
		&config.model_id,
		&config.ort_package_id,
		config.weights_sha256.as_ref(),
		JinaCodeV2::DIMENSIONS,
	)
}

/// Removed symbols are deleted from the store and never reach the embedder.
#[tokio::test(start_paused = true)]
async fn removed_symbols_deleted_never_embedded() {
	let h = harness();
	let corpus: Vec<TestSymbol> = (0..3).map(symbol).collect();
	h.stage
		.run(&delta_added(&corpus), &facet_lookup(&corpus), CancelGroup::new())
		.await
		.expect("cold index");
	let embeds_before = h.mock.text_count();

	let removed: Vec<_> = corpus.iter().map(|s| s.id).collect();
	let report = h
		.stage
		.run(&delta_removed(removed.clone()), &facet_lookup(&corpus), CancelGroup::new())
		.await
		.expect("removal run");

	assert_eq!(report.deleted, 3);
	assert_eq!(report.infer_count, 0);
	assert_eq!(report.upsert_count, 0);
	assert_eq!(h.mock.text_count(), embeds_before, "removed symbols never embedded");
	assert_eq!(h.store.point_count(), 0);
	assert_eq!(h.store.deleted().len(), 3);
}

/// A pre-cancelled run does no embedding work and reports cancellation.
#[tokio::test(start_paused = true)]
async fn cancelled_run_stops_before_embedding() {
	let h = harness();
	let corpus: Vec<TestSymbol> = (0..4).map(symbol).collect();
	let cancel = CancelGroup::new();
	cancel.cancel();

	let result = h.stage.run(&delta_added(&corpus), &facet_lookup(&corpus), cancel).await;
	assert!(matches!(result, Err(Error::Cancelled)));
	assert_eq!(h.mock.call_count(), 0);
	assert_eq!(h.store.upserted(), 0);
}

/// Symbols the facet lookup cannot resolve are skipped and counted, not fatal.
#[tokio::test(start_paused = true)]
async fn missing_facets_skipped_not_fatal() {
	let h = harness();
	let known = vec![symbol(0)];
	let unknown = symbol(1);

	let mut delta = delta_added(&known);
	delta.added.push((unknown.id, unknown.parts.clone()));

	let report = h
		.stage
		.run(&delta, &facet_lookup(&known), CancelGroup::new())
		.await
		.expect("run survives missing facets");
	assert_eq!(report.infer_count, 1);
	assert_eq!(report.missing_facets, 1);
}
