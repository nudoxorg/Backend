//! Shared offline test doubles: in-memory trace store, vector CAS, vector
//! store, a whitespace token counter, and delta/facet builders.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heart::{ContentHash, SymbolId};
use vector_core::JinaCodeV2;
use vector_core::key::{ChangedSymbol, SymbolDelta, SymbolPartHashes};
use vector_core::recipe::{EmbedFacetsBuf, TokenCounter};
use vector_core::store::{PointId, SearchFilter, SearchHit, StoreCapabilities, StoreError, VectorPoint, VectorStore};
use vector_embed::stage::{TraceStore, VectorCas};

// ── L1: in-memory stage traces ───────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct MemTraces {
	rows: Arc<Mutex<HashSet<(String, ContentHash, ContentHash)>>>,
}

impl MemTraces {
	pub fn new() -> Self { Self::default() }

	pub fn len(&self) -> usize { self.rows.lock().unwrap().len() }
}

#[async_trait]
impl TraceStore for MemTraces {
	async fn has(&self, stage_id: &str, input: &ContentHash, tool: &ContentHash) -> bool {
		self.rows.lock().unwrap().contains(&(stage_id.to_owned(), *input, *tool))
	}

	async fn put(&self, stage_id: &str, input: &ContentHash, tool: &ContentHash) {
		self.rows.lock().unwrap().insert((stage_id.to_owned(), *input, *tool));
	}
}

// ── L2: in-memory vector CAS ─────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct MemCas {
	blobs: Arc<Mutex<HashMap<ContentHash, Vec<f32>>>>,
}

impl MemCas {
	pub fn new() -> Self { Self::default() }

	pub fn insert(&self, key: ContentHash, vector: Vec<f32>) {
		self.blobs.lock().unwrap().insert(key, vector);
	}

	pub fn len(&self) -> usize { self.blobs.lock().unwrap().len() }
}

#[async_trait]
impl VectorCas for MemCas {
	async fn get(&self, key: &ContentHash) -> Option<Vec<f32>> {
		self.blobs.lock().unwrap().get(key).cloned()
	}

	async fn put(&self, key: &ContentHash, vector: &[f32]) {
		self.blobs.lock().unwrap().insert(*key, vector.to_vec());
	}
}

// ── In-memory vector store with counters ─────────────────────────────────────

#[derive(Default)]
pub struct MemStoreState {
	pub points: HashMap<PointId, VectorPoint<JinaCodeV2>>,
	pub upserted: usize,
	pub deleted: Vec<PointId>,
}

#[derive(Clone, Default)]
pub struct MemStore {
	pub state: Arc<Mutex<MemStoreState>>,
}

impl MemStore {
	pub fn new() -> Self { Self::default() }

	pub fn upserted(&self) -> usize { self.state.lock().unwrap().upserted }

	pub fn deleted(&self) -> Vec<PointId> { self.state.lock().unwrap().deleted.clone() }

	pub fn point_count(&self) -> usize { self.state.lock().unwrap().points.len() }
}

#[async_trait]
impl VectorStore<JinaCodeV2> for MemStore {
	async fn upsert(&self, points: Vec<VectorPoint<JinaCodeV2>>) -> Result<(), StoreError> {
		let mut state = self.state.lock().unwrap();
		state.upserted += points.len();
		for point in points {
			state.points.insert(point.id, point);
		}
		Ok(())
	}

	async fn delete(&self, ids: &[PointId]) -> Result<(), StoreError> {
		let mut state = self.state.lock().unwrap();
		for id in ids {
			state.points.remove(id);
			state.deleted.push(*id);
		}
		Ok(())
	}

	async fn search(&self, _request: vector_core::store::SearchRequest<JinaCodeV2>) -> Result<Vec<SearchHit>, StoreError> {
		Ok(vec![])
	}

	async fn count(&self, _filter: Option<&SearchFilter>) -> Result<u64, StoreError> {
		Ok(self.state.lock().unwrap().points.len() as u64)
	}

	async fn flush(&self) -> Result<(), StoreError> { Ok(()) }

	async fn compact(&self) -> Result<(), StoreError> { Ok(()) }

	fn capabilities(&self) -> StoreCapabilities {
		StoreCapabilities {
			filtered_search: false,
			disk_resident: false,
			exact_count: true,
			backend: "mem-test-store".into(),
		}
	}
}

// ── Token counting without a model ───────────────────────────────────────────

/// Whitespace "tokens" — enough for recipe plumbing in offline tests.
pub struct WhitespaceCounter;

impl TokenCounter for WhitespaceCounter {
	fn count(&self, text: &str) -> usize { text.split_whitespace().count() }

	fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
		text.split_whitespace().take(max_tokens).collect::<Vec<_>>().join(" ")
	}
}

// ── Corpus builders ──────────────────────────────────────────────────────────

/// One synthetic symbol: id, its part hashes, and its embed facets.
pub struct TestSymbol {
	pub id: SymbolId,
	pub parts: SymbolPartHashes,
	pub facets: EmbedFacetsBuf,
}

pub fn hash(label: &str) -> ContentHash { ContentHash::of_bytes(label.as_bytes()) }

pub fn parts(sig: &str, body: &str, doc: &str) -> SymbolPartHashes {
	SymbolPartHashes {
		sig: hash(sig),
		body: hash(body),
		doc: hash(doc),
		refs: hash("refs"),
	}
}

pub fn symbol(index: usize) -> TestSymbol {
	let moniker = format!("pkg::module::sym_{index}");
	TestSymbol {
		id: SymbolId::new_random(),
		parts: parts(&format!("sig-{index}"), &format!("body-{index}"), &format!("doc-{index}")),
		facets: facets(&moniker, &format!("fn sym_{index}()"), "original doc", "body tokens here"),
	}
}

pub fn facets(moniker: &str, sig: &str, doc: &str, body: &str) -> EmbedFacetsBuf {
	EmbedFacetsBuf {
		language: heart::Language::Rust,
		package_stem: "pkg".to_owned(),
		kind: "function".to_owned(),
		moniker: moniker.to_owned(),
		path: moniker.to_owned(),
		name: moniker.split("::").last().unwrap_or(moniker).to_owned(),
		signature: sig.to_owned(),
		doc: doc.to_owned(),
		body_tokens: body.to_owned(),
	}
}

/// A facet lookup over a corpus slice.
pub fn facet_lookup(corpus: &[TestSymbol]) -> impl Fn(&SymbolId) -> Option<EmbedFacetsBuf> + '_ {
	move |id| corpus.iter().find(|s| s.id == *id).map(|s| s.facets.clone())
}

pub fn delta_added(corpus: &[TestSymbol]) -> SymbolDelta {
	SymbolDelta {
		added: corpus.iter().map(|s| (s.id, s.parts.clone())).collect(),
		removed: Vec::new(),
		changed: Vec::new(),
	}
}

pub fn delta_changed(symbol: &TestSymbol, old: SymbolPartHashes) -> SymbolDelta {
	SymbolDelta {
		added: Vec::new(),
		removed: Vec::new(),
		changed: vec![ChangedSymbol { id: symbol.id, old, new: symbol.parts.clone() }],
	}
}

pub fn delta_removed(ids: Vec<SymbolId>) -> SymbolDelta {
	SymbolDelta { added: Vec::new(), removed: ids, changed: Vec::new() }
}
