//! Feature-gated protocol bridge for the frozen external search benchmark.
//!
//! This module builds and queries the production `DiscoverySearchIndex`; it
//! deliberately does not contain a second tokenizer or ranking implementation.

use super::{
    DiscoverySearchIndex, DiscoverySearchKey, DiscoverySearchSource, ForgeSearchDocument,
    SearchStandingEvidence,
};
use crate::discovery::DiscoveryStore;
use backend_engine::{ForgeCoordinate, ProductPackageCoordinate, registry};
use registry::{
    CARGO_SPARSE_INDEX, CONAN_CENTER, CratesSparseMetadata, DiscoveryAdvisory, DiscoveryBatch,
    DiscoveryCompleteness, DiscoveryCursor, DiscoveryFacet, DiscoveryFact, DiscoveryMetadata,
    DiscoveryObservedAt, DiscoveryPackageRetraction, DiscoverySourceEvent, DiscoverySourceIdentity,
    DiscoveryStanding, DiscoveryTimestamp, GO_MODULE_PROXY, MAVEN_CENTRAL,
    MAX_DISCOVERY_PAGE_ITEMS, NPM_REGISTRY, NUGET_V3, PYPI_SIMPLE_API, RegistryEcosystem,
    RegistryEndpoint,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use tantivy::{Directory, HasLen};

const CORPUS_FILE: &str = "corpus.jsonl";
const JOURNAL_FILE: &str = "catalog.journal";
const ADAPTER_MANIFEST_FILE: &str = "adapter-manifest.json";
const SNAPSHOT_OBSERVED_AT_MS: u64 = 1_790_578_687_000;
const FULL_RANKED_PAGE: usize = 256;

/// Live production-ranker session used by the standalone benchmark process.
pub struct BenchmarkIndex {
    root: PathBuf,
    store: DiscoveryStore,
    search: DiscoverySearchIndex,
    documents: BTreeMap<String, BenchmarkDocument>,
    canonical_document_ids: BTreeMap<(DiscoverySearchSource, ProductPackageCoordinate), String>,
    update_sequence: u64,
}

#[derive(Clone)]
struct BenchmarkDocument {
    row: Value,
    source: Option<DiscoverySearchSource>,
    registry_source: Option<DiscoverySourceIdentity>,
    coordinate: Option<ProductPackageCoordinate>,
    forge_document: Option<ForgeSearchDocument>,
    skip_reason: Option<&'static str>,
}

/// Creates the durable frozen-input journal and exercises a cold build of the
/// production Tantivy projection.
/// Rows without an upstream ordering event are admitted as explicit snapshot
/// observations; the local build time is never used as source event evidence.
///
/// # Errors
/// Returns an error if the corpus is malformed, contains an invalid supported
/// registry coordinate, or cannot be committed to a new index directory.
pub fn build(corpus_path: &Path, index_root: &Path) -> Result<Value, String> {
    for child in [CORPUS_FILE, JOURNAL_FILE, ADAPTER_MANIFEST_FILE] {
        if index_root.join(child).exists() {
            return Err(format!(
                "refusing to overwrite existing adapter index file: {}",
                index_root.join(child).display()
            ));
        }
    }
    fs::create_dir_all(index_root).map_err(|error| format!("create index directory: {error}"))?;
    let corpus_bytes = fs::read(corpus_path).map_err(|error| format!("read corpus: {error}"))?;
    let rows = parse_corpus(&corpus_bytes)?;
    fs::write(index_root.join(CORPUS_FILE), &corpus_bytes)
        .map_err(|error| format!("persist frozen corpus: {error}"))?;

    let (indexed_documents, skipped_documents, journal_batches, index_bytes) = {
        let mut store = DiscoveryStore::open(index_root.join(JOURNAL_FILE))
            .map_err(|error| format!("open benchmark discovery journal: {error:?}"))?;
        let mut documents = BTreeMap::new();
        let mut batches = BTreeMap::<DiscoverySourceIdentity, Vec<DiscoveryFact>>::new();
        let mut forge_documents = Vec::new();
        let mut skipped = Vec::new();
        for row in rows {
            let document_id = string_at(&row, &["document_id"])?.to_owned();
            let document = parse_document(row)?;
            if let (Some(source), Some(coordinate)) =
                (document.registry_source, document.coordinate.clone())
            {
                let fact = fact_from_row(&document.row, source, coordinate)?;
                batches.entry(source).or_default().push(fact);
            }
            if let Some(forge_document) = document.forge_document.clone() {
                forge_documents.push(forge_document);
            }
            if let Some(reason) = document.skip_reason {
                skipped.push(json!({"document_id": document_id, "reason": reason}));
            }
            documents.insert(document_id, document);
        }
        let mut journal_batches = 0_usize;
        for (source, facts) in batches {
            let mut previous_cursor = DiscoveryCursor::default();
            let mut expected_base_sequence = store.sequence(source).unwrap_or(0);
            for (batch_index, chunk) in facts.chunks(MAX_DISCOVERY_PAGE_ITEMS).enumerate() {
                let next_cursor = snapshot_cursor(source, batch_index.saturating_add(1))?;
                store
                    .commit(DiscoveryBatch {
                        source,
                        expected_base_sequence,
                        previous_cursor,
                        next_cursor: next_cursor.clone(),
                        source_high_watermark: next_cursor.clone(),
                        caught_up: false,
                        observed_at: DiscoveryObservedAt::from_unix_millis(SNAPSHOT_OBSERVED_AT_MS),
                        completeness: DiscoveryCompleteness::Windowed,
                        facts: chunk.to_vec(),
                        package_retractions: Vec::new(),
                    })
                    .map_err(|error| format!("commit frozen source rows: {error:?}"))?;
                expected_base_sequence = expected_base_sequence
                    .checked_add(1)
                    .ok_or_else(|| "benchmark discovery sequence overflow".to_owned())?;
                previous_cursor = next_cursor;
                journal_batches = journal_batches.saturating_add(1);
            }
        }
        let search = DiscoverySearchIndex::open_with_forge(&store, &forge_documents)
            .map_err(|error| format!("build production Tantivy projection: {error}"))?;
        let index_bytes = tantivy_projection_bytes(&search)?;
        (
            documents
                .values()
                .filter(|document| document.source.is_some())
                .count(),
            skipped,
            journal_batches,
            index_bytes,
        )
    };

    let corpus_blake3 = backend_engine::blake3::hash(&corpus_bytes)
        .to_hex()
        .to_string();
    let manifest = json!({
        "schema_version": 1,
        "ranker": "backend-local-service::builtin::discovery_search::DiscoverySearchIndex",
        "corpus_blake3": corpus_blake3,
        "indexed_documents": indexed_documents,
        "journal_batches": journal_batches,
        "skipped_documents": skipped_documents,
        "tantivy_projection_bytes": index_bytes,
        "tantivy_projection_storage": "in-memory managed-directory bytes; source journal and copied corpus are measured separately",
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("encode adapter manifest: {error}"))?;
    fs::write(index_root.join(ADAPTER_MANIFEST_FILE), manifest_bytes)
        .map_err(|error| format!("write adapter manifest: {error}"))?;
    let persistent_directory_bytes = directory_bytes(index_root)?;
    Ok(json!({
        "ranker": "backend-local-service::builtin::discovery_search::DiscoverySearchIndex",
        "indexed_documents": indexed_documents,
        "journal_batches": journal_batches,
        "skipped_documents": skipped_documents,
        "index_bytes": index_bytes,
        "tantivy_projection_bytes": index_bytes,
        "persistent_index_directory_bytes": persistent_directory_bytes,
        "corpus_blake3": corpus_blake3,
    }))
}

/// Opens a previously built benchmark index and rebuilds the production
/// Tantivy projection from its persisted source journal.
///
/// # Errors
/// Returns an error if the persisted corpus, manifest, or journal is invalid.
pub fn open(index_root: &Path) -> Result<BenchmarkIndex, String> {
    let corpus_path = index_root.join(CORPUS_FILE);
    let corpus_bytes =
        fs::read(&corpus_path).map_err(|error| format!("read persisted corpus: {error}"))?;
    let manifest_bytes = fs::read(index_root.join(ADAPTER_MANIFEST_FILE))
        .map_err(|error| format!("read adapter manifest: {error}"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("decode adapter manifest: {error}"))?;
    let observed_hash = backend_engine::blake3::hash(&corpus_bytes)
        .to_hex()
        .to_string();
    if manifest.get("corpus_blake3").and_then(Value::as_str) != Some(observed_hash.as_str()) {
        return Err("persisted corpus does not match the adapter manifest".to_owned());
    }
    let rows = parse_corpus(&corpus_bytes)?;
    let mut documents = BTreeMap::new();
    for row in rows {
        let document_id = string_at(&row, &["document_id"])?.to_owned();
        documents.insert(document_id, parse_document(row)?);
    }
    let forge_documents = documents
        .values()
        .filter_map(|document| document.forge_document.clone())
        .collect::<Vec<_>>();
    let mut canonical_document_ids = BTreeMap::new();
    for (document_id, document) in &documents {
        let (Some(source), Some(coordinate)) = (&document.source, &document.coordinate) else {
            continue;
        };
        let key = (source.clone(), coordinate.clone());
        if canonical_document_ids
            .insert(key, document_id.clone())
            .is_some()
        {
            return Err(format!(
                "multiple benchmark document ids map to one source-coordinate: {document_id}"
            ));
        }
    }
    let store = DiscoveryStore::open(index_root.join(JOURNAL_FILE))
        .map_err(|error| format!("open benchmark discovery journal: {error:?}"))?;
    let mut update_sequence = 0_u64;
    for document in documents.values_mut() {
        let (Some(source), Some(coordinate)) = (document.registry_source, &document.coordinate)
        else {
            continue;
        };
        let cursor = store.cursor(source);
        update_sequence = update_sequence.max(benchmark_update_sequence(&cursor));
        if let Some(fact) = store.fact(source, coordinate.as_str()) {
            // The journal is authoritative after incremental updates. Restore
            // every typed metadata facet into the serving row so a reopened
            // adapter does not replay an older corpus value on its next update.
            sync_document_metadata_fields(&mut document.row, &fact.metadata)?;
        }
    }
    let search = DiscoverySearchIndex::open_with_forge(&store, &forge_documents)
        .map_err(|error| format!("rebuild production Tantivy projection: {error}"))?;
    Ok(BenchmarkIndex {
        root: index_root.to_path_buf(),
        store,
        search,
        documents,
        canonical_document_ids,
        update_sequence,
    })
}

impl BenchmarkIndex {
    /// Executes one request through the current production discovery ranker.
    ///
    /// # Errors
    /// Returns an error for malformed search requests or ranker failures.
    pub fn search_request(&self, request: &Value) -> Result<Value, String> {
        if request.get("op").and_then(Value::as_str) == Some("search-groups") {
            return self.search_groups_request(request);
        }
        if request.get("op").and_then(Value::as_str) != Some("search") {
            return Err("search request must set op=search".to_owned());
        }
        let query_id = string_at(request, &["query_id"])?;
        let text = string_at(request, &["query"])?;
        let corpus = string_at(request, &["corpus"])?;
        let limit = request
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(50, |value| usize::try_from(value).unwrap_or(usize::MAX))
            .min(FULL_RANKED_PAGE);
        if limit == 0 {
            let mut response = json!({"query_id": query_id, "document_ids": []});
            if let Some(request_id) = request.get("request_id") {
                response["request_id"] = request_id.clone();
            }
            return Ok(response);
        }
        let scope = request
            .pointer("/scope/ecosystems")
            .and_then(Value::as_array)
            .ok_or_else(|| "search request scope.ecosystems must be an array".to_owned())?
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        let exclude_yanked = request
            .get("exclude_yanked")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let include_standing = request
            .get("include_standing")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut standing_by_document_id = serde_json::Map::new();
        let mut scoped_ecosystems = scope
            .iter()
            .filter_map(|name| search_ecosystem(name))
            .collect::<Vec<_>>();
        scoped_ecosystems.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        scoped_ecosystems.dedup();
        let mut hits = Vec::new();
        for ecosystem in scoped_ecosystems {
            let page = self
                .search
                .search_with_store(
                    &self.store,
                    super::DiscoverySearchRequest {
                        text,
                        ecosystem: Some(ecosystem),
                    },
                    FULL_RANKED_PAGE,
                )
                .map_err(|error| format!("production package search failed: {error}"))?;
            hits.extend(page.hits);
        }
        hits.sort_by(|left, right| {
            (
                left.evidence.rank(),
                &left.continuation_after.after_sort_key,
            )
                .cmp(&(
                    right.evidence.rank(),
                    &right.continuation_after.after_sort_key,
                ))
        });
        let mut seen_keys = BTreeSet::new();
        let mut document_ids = Vec::new();
        for hit in hits {
            let standing = standing_name(hit.standing);
            let key: DiscoverySearchKey = hit.key;
            if !seen_keys.insert((key.source.clone(), key.coordinate.clone())) {
                continue;
            }
            let Some(document_id) = self
                .canonical_document_ids
                .get(&(key.source.clone(), key.coordinate.clone()))
            else {
                continue;
            };
            let Some(document) = self.documents.get(document_id) else {
                continue;
            };
            let row_corpus = if document.row.get("data_class").and_then(Value::as_str)
                == Some("adversarial-fixture")
            {
                "adversarial"
            } else {
                "primary"
            };
            let ecosystem = document
                .row
                .get("ecosystem")
                .and_then(Value::as_str)
                .unwrap_or("");
            if row_corpus != corpus || !scope.contains(ecosystem) {
                continue;
            }
            if exclude_yanked
                && document
                    .row
                    .pointer("/fields/yanked")
                    .is_some_and(facet_is_yanked)
            {
                continue;
            }
            document_ids.push(document_id.clone());
            if include_standing {
                standing_by_document_id.insert(document_id.clone(), json!(standing));
            }
            if document_ids.len() >= limit {
                break;
            }
        }
        let mut response = json!({"query_id": query_id, "document_ids": document_ids});
        if include_standing {
            response["standing_by_document_id"] = Value::Object(standing_by_document_id);
        }
        if let Some(request_id) = request.get("request_id") {
            response["request_id"] = request_id.clone();
        }
        Ok(response)
    }

    /// Runs the production source-scoped package-lineage query used by the
    /// benchmark holdout. Each returned group includes the canonical release
    /// ids that matched inside that lineage. Requests may set
    /// `measure_owner_time: true` to add `owner_search_nanos`, timing the
    /// in-process ranker separately from JSONL and process overhead.
    ///
    /// # Errors
    /// Returns an error for malformed requests or a grouped-search failure.
    pub fn search_groups_request(&self, request: &Value) -> Result<Value, String> {
        if request.get("op").and_then(Value::as_str) != Some("search-groups") {
            return Err("group search request must set op=search-groups".to_owned());
        }
        let query_id = string_at(request, &["query_id"])?;
        let text = string_at(request, &["query"])?;
        let corpus = string_at(request, &["corpus"])?;
        let ecosystems = request
            .pointer("/scope/ecosystems")
            .and_then(Value::as_array)
            .ok_or_else(|| "search request scope.ecosystems must be an array".to_owned())?;
        if ecosystems.len() != 1 {
            return Err("group search holdout requires exactly one ecosystem".to_owned());
        }
        let ecosystem_name = ecosystems[0]
            .as_str()
            .ok_or_else(|| "search request ecosystem must be a string".to_owned())?;
        let ecosystem = ecosystem(ecosystem_name)
            .ok_or_else(|| format!("unsupported group-search ecosystem: {ecosystem_name}"))?;
        let limit = request
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(50, |value| usize::try_from(value).unwrap_or(usize::MAX))
            .min(FULL_RANKED_PAGE);
        let cursor = request
            .get("cursor")
            .cloned()
            .map(serde_json::from_value::<super::DiscoverySearchCursor>)
            .transpose()
            .map_err(|error| format!("decode lineage search cursor: {error}"))?;
        // A long-lived adapter process can report owner-only ranker latency
        // alongside the response. A caller timing a fresh process around this
        // request can then compare both measurements on the same saved index.
        let measure_owner_time = request
            .get("measure_owner_time")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let owner_search_started = measure_owner_time.then(std::time::Instant::now);
        let page = self
            .search
            .search_groups_after(
                super::DiscoverySearchRequest {
                    text,
                    ecosystem: Some(ecosystem),
                },
                limit,
                cursor.as_ref(),
            )
            .map_err(|error| format!("production grouped package search failed: {error}"))?;
        let owner_search_nanos = owner_search_started
            .map(|started| u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        let mut groups = Vec::new();
        for group in page.groups {
            let mut document_ids = Vec::new();
            for key in group.matched_releases {
                let Some(document_id) = self
                    .canonical_document_ids
                    .get(&(key.source.clone(), key.coordinate.clone()))
                else {
                    continue;
                };
                let Some(document) = self.documents.get(document_id) else {
                    continue;
                };
                let row_corpus = if document.row.get("data_class").and_then(Value::as_str)
                    == Some("adversarial-fixture")
                {
                    "adversarial"
                } else {
                    "primary"
                };
                if row_corpus == corpus {
                    document_ids.push(document_id.clone());
                }
            }
            if document_ids.is_empty() {
                continue;
            }
            groups.push(json!({
                "ecosystem": group.ecosystem.as_str(),
                "lineage": group.lineage,
                "source_id": super::hex(&group.source.id()),
                "document_ids": document_ids,
                "more_releases": group.more_releases,
            }));
        }
        let mut response = json!({
            "query_id": query_id,
            "groups": groups,
            "posting_candidates": page.posting_candidates,
            "index_documents_visited": page.index_documents_visited,
            "facet_releases_examined": page.facet_releases_examined,
            "next_cursor": page.next_cursor,
        });
        if let Some(request_id) = request.get("request_id") {
            response["request_id"] = request_id.clone();
        }
        if let Some(owner_search_nanos) = owner_search_nanos {
            response["owner_search_nanos"] = json!(owner_search_nanos);
        }
        Ok(response)
    }

    /// Applies a package-level NPM deletion event through the authoritative
    /// discovery journal, then keeps the production search projection in sync.
    /// This hook exists only under the benchmark feature for retraction tests.
    /// It uses a synthetic NPM event and labels that provenance in its reply.
    ///
    /// # Errors
    /// Returns an error when the source package is absent or the journal rejects
    /// the retraction.
    pub fn retract_package_request(&mut self, request: &Value) -> Result<Value, String> {
        if request.get("op").and_then(Value::as_str) != Some("retract-package") {
            return Err("retraction request must set op=retract-package".to_owned());
        }
        let package_name = string_at(request, &["package_name"])?.to_owned();
        let source = self
            .documents
            .values()
            .find(|document| {
                document.registry_source.is_some_and(|source| {
                    source.ecosystem() == RegistryEcosystem::Npm
                        && document.row.get("name").and_then(Value::as_str)
                            == Some(package_name.as_str())
                })
            })
            .and_then(|document| document.registry_source)
            .ok_or_else(|| format!("unknown NPM benchmark package: {package_name}"))?;
        let release_count = self
            .documents
            .values()
            .filter(|document| {
                document.registry_source == Some(source)
                    && document.row.get("name").and_then(Value::as_str)
                        == Some(package_name.as_str())
            })
            .count();
        let next_update_sequence = self
            .update_sequence
            .checked_add(1)
            .ok_or_else(|| "benchmark update cursor overflow".to_owned())?;
        let source_event_sequence = self
            .documents
            .values()
            .filter(|document| document.registry_source == Some(source))
            .filter_map(|document| {
                let coordinate = document.coordinate.as_ref()?;
                let fact = self.store.fact(source, coordinate.as_str())?;
                match &fact.source_event {
                    DiscoverySourceEvent::NpmChange { sequence, .. } => Some(*sequence),
                    DiscoverySourceEvent::Snapshot | DiscoverySourceEvent::Unordered => None,
                    DiscoverySourceEvent::NugetCatalog { .. } => None,
                }
            })
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| "NPM source event sequence overflow".to_owned())?;
        let observed_at = self
            .store
            .latest_observed_at(source)
            .unwrap_or(SNAPSHOT_OBSERVED_AT_MS)
            .checked_add(1)
            .ok_or_else(|| "benchmark observation timestamp overflow".to_owned())?;
        let next_cursor = DiscoveryCursor::new(
            format!("z-benchmark-update-{next_update_sequence:016}").into_bytes(),
        )
        .map_err(|error| format!("create retraction cursor: {error}"))?;
        let previous_cursor = self.store.cursor(source);
        let expected_base_sequence = self.store.sequence(source).unwrap_or(0);
        let batch = DiscoveryBatch {
            source,
            expected_base_sequence,
            previous_cursor,
            next_cursor: next_cursor.clone(),
            source_high_watermark: next_cursor,
            caught_up: false,
            observed_at: DiscoveryObservedAt::from_unix_millis(observed_at),
            completeness: DiscoveryCompleteness::Windowed,
            facts: Vec::new(),
            package_retractions: vec![DiscoveryPackageRetraction {
                package_name: package_name.clone(),
                source_event_time: format!("{source_event_sequence:020}"),
                source_event: DiscoverySourceEvent::NpmChange {
                    sequence: source_event_sequence,
                    revision: None,
                    change_proof: [0xA7; 32],
                },
                proof: [0xA7; 32],
            }],
        };
        batch
            .admit()
            .map_err(|error| format!("admit NPM package retraction: {error:?}"))?;
        self.store
            .commit(batch)
            .map_err(|error| format!("commit NPM package retraction: {error:?}"))?;
        self.update_sequence = next_update_sequence;
        self.search
            .sync(&self.store)
            .map_err(|error| format!("synchronize production Tantivy postings: {error}"))?;
        Ok(json!({
            "op": "retract-package",
            "package_name": package_name,
            "updated_releases": release_count,
            "event_provenance": "benchmark_only_synthetic",
        }))
    }

    /// Applies one or more document-field mutations through the discovery
    /// journal and synchronizes the production incremental Tantivy projection.
    /// A missing upstream event remains a Snapshot. Typed NPM and NuGet events
    /// require their source ordering fields and exact raw event-time spelling.
    /// The request permits one document per source so every batch can be
    /// validated before any source journal is changed.
    ///
    /// # Errors
    /// Returns an error for unknown documents, invalid facets, or journal/index
    /// synchronization failures.
    pub fn update_request(&mut self, request: &Value) -> Result<Value, String> {
        if request.get("op").and_then(Value::as_str) != Some("update") {
            return Err("update request must set op=update".to_owned());
        }
        let updates = request
            .get("updates")
            .and_then(Value::as_array)
            .ok_or_else(|| "update request must include an updates array".to_owned())?;
        let mut updated_documents = BTreeSet::new();
        let mut updated_sources = BTreeSet::new();
        let mut next_update_sequence = self.update_sequence;
        let mut planned = Vec::with_capacity(updates.len());
        for update in updates {
            let document_id = string_at(update, &["document_id"])?.to_owned();
            if !updated_documents.insert(document_id.clone()) {
                return Err(format!("update request repeats document: {document_id}"));
            }
            let mut document = self
                .documents
                .get(&document_id)
                .cloned()
                .ok_or_else(|| format!("unknown update document: {document_id}"))?;
            let source = document
                .registry_source
                .ok_or_else(|| format!("document is not searchable: {document_id}"))?;
            if !updated_sources.insert(source) {
                return Err(format!(
                    "update request may contain at most one document per source: {document_id}"
                ));
            }
            let coordinate = document
                .coordinate
                .clone()
                .ok_or_else(|| format!("document has no registry coordinate: {document_id}"))?;
            let field_updates = update
                .get("fields")
                .and_then(Value::as_object)
                .ok_or_else(|| "update fields must be a JSON object".to_owned())?;
            let existing = self
                .store
                .fact(source, coordinate.as_str())
                .cloned()
                .ok_or_else(|| {
                    format!("document is absent from the discovery journal: {document_id}")
                })?;
            let source_event = update_source_event(update, source)?;
            let fields = document
                .row
                .get_mut("fields")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| "indexed document fields must be an object".to_owned())?;
            for (field, value) in field_updates {
                fields.insert(field.clone(), value.clone());
            }
            let latest_observed_at = self
                .store
                .latest_observed_at(source)
                .unwrap_or(existing.observed_at.as_unix_millis());
            let observed_at = latest_observed_at
                .checked_add(1)
                .ok_or_else(|| "benchmark observation timestamp overflow".to_owned())?;
            let (event, event_time) = source_event;
            let mut incoming =
                fact_from_row_with_event(&document.row, source, coordinate, event, event_time)?;
            incoming.proof = existing.proof;
            incoming.observed_at = DiscoveryObservedAt::from_unix_millis(observed_at);
            validate_benchmark_transition(&existing, &incoming)?;
            let previous_cursor = self.store.cursor(source);
            let expected_base_sequence = self.store.sequence(source).unwrap_or(0);
            next_update_sequence = next_update_sequence
                .checked_add(1)
                .ok_or_else(|| "benchmark update cursor overflow".to_owned())?;
            let next_cursor = DiscoveryCursor::new(
                format!("z-benchmark-update-{next_update_sequence:016}").into_bytes(),
            )
            .map_err(|error| format!("create update cursor: {error}"))?;
            let batch = DiscoveryBatch {
                source,
                expected_base_sequence,
                previous_cursor,
                next_cursor: next_cursor.clone(),
                source_high_watermark: next_cursor,
                caught_up: false,
                observed_at: DiscoveryObservedAt::from_unix_millis(observed_at),
                completeness: DiscoveryCompleteness::Windowed,
                facts: vec![incoming],
                package_retractions: Vec::new(),
            };
            batch
                .admit()
                .map_err(|error| format!("commit document update: {error:?}"))?;
            planned.push((document_id, document, batch, next_update_sequence));
        }

        for (document_id, document, batch, cursor_sequence) in planned {
            self.store
                .commit(batch)
                .map_err(|error| format!("commit document update: {error:?}"))?;
            self.update_sequence = cursor_sequence;
            self.documents.insert(document_id, document);
            self.search
                .sync(&self.store)
                .map_err(|error| format!("synchronize production Tantivy postings: {error}"))?;
        }
        Ok(json!({"op": "update", "updated": updated_documents.len()}))
    }

    /// Returns logical byte lengths of the actual in-memory Tantivy managed
    /// files represented by the current ranker.
    ///
    /// # Errors
    /// Returns an error if Tantivy cannot enumerate/read its managed files.
    pub fn tantivy_projection_bytes(&self) -> Result<u64, String> {
        tantivy_projection_bytes(&self.search)
    }

    /// Returns the persistent journal and copied-corpus directory byte count.
    ///
    /// # Errors
    /// Returns an error if the index directory cannot be traversed.
    pub fn persistent_directory_bytes(&self) -> Result<u64, String> {
        directory_bytes(&self.root)
    }
}

/// Reads JSONL commands from standard input and writes one result per line.
///
/// # Errors
/// Returns an I/O or request error; protocol errors are emitted as JSON error
/// lines so the benchmark harness can fail the sample explicitly.
pub fn serve(index_root: &Path, default_limit: usize) -> Result<(), String> {
    let mut index = open(index_root)?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = input
            .read_line(&mut line)
            .map_err(|error| format!("read adapter request: {error}"))?;
        if bytes_read == 0 {
            return Ok(());
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => match request.get("op").and_then(Value::as_str) {
                Some("search") => {
                    let mut with_default = request.clone();
                    if with_default.get("limit").is_none() {
                        with_default["limit"] = json!(default_limit);
                    }
                    index.search_request(&with_default)
                }
                Some("search-groups") => index.search_groups_request(&request),
                Some("retract-package") => index.retract_package_request(&request),
                Some("update") => index.update_request(&request),
                _ => Err(
                    "adapter operation must be search, search-groups, update, or retract-package"
                        .to_owned(),
                ),
            },
            Err(error) => Err(format!("decode adapter request: {error}")),
        };
        let value = match response {
            Ok(value) => value,
            Err(error) => json!({"error": error}),
        };
        serde_json::to_writer(&mut output, &value)
            .map_err(|error| format!("encode adapter response: {error}"))?;
        output
            .write_all(b"\n")
            .and_then(|()| output.flush())
            .map_err(|error| format!("write adapter response: {error}"))?;
    }
}

/// Reads search requests concurrently against one shared production index.
///
/// This is used only for the labeled scale lane. It keeps one in-memory index
/// and allows concurrent read guards; field updates take an exclusive guard.
///
/// # Errors
/// Returns an I/O or worker failure; per-request failures are emitted as JSON
/// error lines so the scale driver can fail its sample explicitly.
pub fn serve_parallel(
    index_root: &Path,
    default_limit: usize,
    workers: usize,
) -> Result<(), String> {
    if workers == 0 || workers > 64 {
        return Err("parallel reader count must be between 1 and 64".to_owned());
    }
    let index = Arc::new(RwLock::new(open(index_root)?));
    let queue = Arc::new((
        Mutex::new(VecDeque::<Option<String>>::new()),
        Condvar::new(),
    ));
    let output = Arc::new(Mutex::new(std::io::stdout()));
    let mut worker_threads = Vec::with_capacity(workers);
    for _ in 0..workers {
        let index = Arc::clone(&index);
        let queue = Arc::clone(&queue);
        let output = Arc::clone(&output);
        worker_threads.push(thread::spawn(move || -> Result<(), String> {
            loop {
                let line = {
                    let (pending, ready) = &*queue;
                    let mut pending = pending
                        .lock()
                        .map_err(|_| "parallel request queue was poisoned".to_owned())?;
                    while pending.is_empty() {
                        pending = ready
                            .wait(pending)
                            .map_err(|_| "parallel request queue was poisoned".to_owned())?;
                    }
                    pending
                        .pop_front()
                        .ok_or_else(|| "parallel request queue lost its front item".to_owned())?
                };
                let Some(line) = line else {
                    return Ok(());
                };
                let response = match serde_json::from_str::<Value>(&line) {
                    Ok(request) => match request.get("op").and_then(Value::as_str) {
                        Some("search") => {
                            let mut with_default = request.clone();
                            if with_default.get("limit").is_none() {
                                with_default["limit"] = json!(default_limit);
                            }
                            index
                                .read()
                                .map_err(|_| "parallel benchmark index was poisoned".to_owned())?
                                .search_request(&with_default)
                        }
                        Some("update") => index
                            .write()
                            .map_err(|_| "parallel benchmark index was poisoned".to_owned())?
                            .update_request(&request),
                        _ => Err("adapter operation must be search or update".to_owned()),
                    },
                    Err(error) => Err(format!("decode adapter request: {error}")),
                };
                let value = match response {
                    Ok(value) => value,
                    Err(error) => json!({"error": error}),
                };
                let mut output = output
                    .lock()
                    .map_err(|_| "parallel adapter output was poisoned".to_owned())?;
                serde_json::to_writer(&mut *output, &value)
                    .map_err(|error| format!("encode adapter response: {error}"))?;
                output
                    .write_all(b"\n")
                    .and_then(|()| output.flush())
                    .map_err(|error| format!("write adapter response: {error}"))?;
            }
        }));
    }

    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = input
            .read_line(&mut line)
            .map_err(|error| format!("read adapter request: {error}"))?;
        if bytes_read == 0 {
            break;
        }
        let (pending, ready) = &*queue;
        pending
            .lock()
            .map_err(|_| "parallel request queue was poisoned".to_owned())?
            .push_back(Some(line.clone()));
        ready.notify_one();
    }
    let (pending, ready) = &*queue;
    {
        let mut pending = pending
            .lock()
            .map_err(|_| "parallel request queue was poisoned".to_owned())?;
        pending.extend((0..workers).map(|_| None));
    }
    ready.notify_all();
    for worker in worker_threads {
        worker
            .join()
            .map_err(|_| "parallel benchmark worker panicked".to_owned())??;
    }
    Ok(())
}

fn parse_corpus(bytes: &[u8]) -> Result<Vec<Value>, String> {
    let mut rows = Vec::new();
    for (line_number, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let row: Value = serde_json::from_slice(line)
            .map_err(|error| format!("corpus line {}: {error}", line_number + 1))?;
        if !row.is_object() {
            return Err(format!(
                "corpus line {} is not a JSON object",
                line_number + 1
            ));
        }
        rows.push(row);
    }
    rows.sort_by(|left, right| {
        left.get("document_id")
            .and_then(Value::as_str)
            .cmp(&right.get("document_id").and_then(Value::as_str))
    });
    Ok(rows)
}

fn parse_document(row: Value) -> Result<BenchmarkDocument, String> {
    let source_kind = row.get("source_kind").and_then(Value::as_str).unwrap_or("");
    if source_kind == "forge" {
        let name = string_at(&row, &["name"])?;
        let version = string_at(&row, &["version"])?;
        let source_coordinate = string_at(&row, &["forge_source_coordinate"])?;
        let source_coordinate = ForgeCoordinate::parse(source_coordinate.to_owned())
            .map_err(|error| format!("parse pinned forge coordinate: {error:?}"))?;
        let coordinate =
            ProductPackageCoordinate::parse(format!("pkg:generic/{name}@{version}"))
                .map_err(|error| format!("parse pinned forge package coordinate: {error:?}"))?;
        let mut generic_terms = Vec::new();
        if let Some(archive_url) = row.get("archive_url").and_then(Value::as_str) {
            generic_terms.push(archive_url.to_owned());
        }
        generic_terms.push(source_coordinate.canonical());
        if let Some(digest) = row.pointer("/archive_digest/value").and_then(Value::as_str) {
            generic_terms.push(digest.to_owned());
        }
        let forge_document = ForgeSearchDocument::from_pinned_facts(
            source_coordinate,
            coordinate.clone(),
            DiscoveryFacet::Unknown,
            DiscoveryFacet::Unknown,
            DiscoveryFacet::Unknown,
            DiscoveryFacet::Unknown,
            DiscoveryFacet::Unknown,
            generic_terms,
        )?;
        let source = DiscoverySearchSource::Forge(forge_document.source.clone());
        return Ok(BenchmarkDocument {
            row,
            source: Some(source),
            registry_source: None,
            coordinate: Some(coordinate),
            forge_document: Some(forge_document),
            skip_reason: None,
        });
    }
    let ecosystem_name = string_at(&row, &["ecosystem"])?;
    let Some(ecosystem) = ecosystem(ecosystem_name) else {
        return Ok(BenchmarkDocument {
            row,
            source: None,
            registry_source: None,
            coordinate: None,
            forge_document: None,
            skip_reason: Some("ecosystem_not_supported_by_registry_discovery_ranker"),
        });
    };
    let coordinate =
        ProductPackageCoordinate::parse(purl_for_row(&row, ecosystem_name)?).map_err(|error| {
            format!(
                "parse package coordinate for {}: {error:?}",
                row["document_id"]
            )
        })?;
    let endpoint = endpoint(ecosystem)?;
    let source = registry::discovery_source_identity(&endpoint);
    Ok(BenchmarkDocument {
        row,
        source: Some(DiscoverySearchSource::Registry(source)),
        registry_source: Some(source),
        coordinate: Some(coordinate),
        forge_document: None,
        skip_reason: None,
    })
}

fn ecosystem(name: &str) -> Option<RegistryEcosystem> {
    match name {
        "cargo" => Some(RegistryEcosystem::Cargo),
        "npm" => Some(RegistryEcosystem::Npm),
        "pypi" => Some(RegistryEcosystem::Pypi),
        "go" => Some(RegistryEcosystem::Golang),
        "maven" => Some(RegistryEcosystem::Maven),
        "nuget" => Some(RegistryEcosystem::Nuget),
        _ => None,
    }
}

fn search_ecosystem(name: &str) -> Option<RegistryEcosystem> {
    if name == "forge" {
        Some(RegistryEcosystem::Cpp)
    } else {
        ecosystem(name)
    }
}

fn endpoint(ecosystem: RegistryEcosystem) -> Result<RegistryEndpoint, String> {
    let (kind, url) = match ecosystem {
        RegistryEcosystem::Cargo => (ecosystem, CARGO_SPARSE_INDEX),
        RegistryEcosystem::Npm => (ecosystem, NPM_REGISTRY),
        RegistryEcosystem::Pypi => (ecosystem, PYPI_SIMPLE_API),
        RegistryEcosystem::Golang => (ecosystem, GO_MODULE_PROXY),
        RegistryEcosystem::Maven => (ecosystem, MAVEN_CENTRAL),
        RegistryEcosystem::Nuget => (ecosystem, NUGET_V3),
        RegistryEcosystem::Cpp => (ecosystem, CONAN_CENTER),
    };
    RegistryEndpoint::new(kind, url).map_err(|error| format!("admit benchmark endpoint: {error}"))
}

fn purl_for_row(row: &Value, ecosystem_name: &str) -> Result<String, String> {
    let name = string_at(row, &["name"])?;
    let version = string_at(row, &["version"])?;
    if version.is_empty() {
        return Err(format!(
            "versionless benchmark row is unsupported: {}",
            row["document_id"]
        ));
    }
    let purl = match ecosystem_name {
        "cargo" => format!("pkg:cargo/{name}@{version}"),
        "npm" => format!("pkg:npm/{name}@{version}"),
        "pypi" => format!("pkg:pypi/{name}@{version}"),
        "go" => format!("pkg:golang/{name}@{version}"),
        "maven" => {
            let coordinate = string_at(row, &["coordinate"])?;
            let body = coordinate
                .strip_prefix("maven:")
                .ok_or_else(|| format!("invalid Maven coordinate: {coordinate}"))?;
            let (group, artifact_version) = body
                .split_once(':')
                .ok_or_else(|| format!("invalid Maven coordinate: {coordinate}"))?;
            let (artifact, pinned_version) = artifact_version
                .rsplit_once('@')
                .ok_or_else(|| format!("invalid Maven coordinate: {coordinate}"))?;
            if pinned_version != version {
                return Err(format!("Maven version mismatch: {coordinate}"));
            }
            format!("pkg:maven/{group}/{artifact}@{version}")
        }
        "nuget" => format!("pkg:nuget/{name}@{version}"),
        _ => return Err(format!("unsupported package ecosystem: {ecosystem_name}")),
    };
    Ok(purl)
}

fn fact_from_row(
    row: &Value,
    source: DiscoverySourceIdentity,
    coordinate: ProductPackageCoordinate,
) -> Result<DiscoveryFact, String> {
    let (source_event, source_event_time) = parse_source_event(row, source)?;
    fact_from_row_with_event(row, source, coordinate, source_event, source_event_time)
}

fn fact_from_row_with_event(
    row: &Value,
    source: DiscoverySourceIdentity,
    coordinate: ProductPackageCoordinate,
    source_event: DiscoverySourceEvent,
    source_event_time: Option<String>,
) -> Result<DiscoveryFact, String> {
    let fields = row
        .get("fields")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("document {} has no fields object", row["document_id"]))?;
    let aliases = string_list_facet(fields.get("aliases"), "aliases")?;
    let description = string_facet(fields.get("description"), "description")?;
    let keywords = string_list_facet(fields.get("keywords"), "keywords")?;
    let license = string_facet(fields.get("license"), "license")?;
    let published_at = string_facet(fields.get("published_at"), "published_at")?;
    let deprecation = string_facet(
        fields
            .get("deprecation")
            .or_else(|| fields.get("deprecated")),
        "deprecation",
    )?;
    let yanked = boolean_facet(fields.get("yanked"), "yanked")?;
    let downloads = u64_facet(fields.get("downloads"), "downloads")?;
    let advisories = advisory_facet(fields.get("advisories"))?;
    let cargo_sparse = cargo_sparse_facet(fields.get("cargo_sparse"))?;
    let standing = match &yanked {
        DiscoveryFacet::Known(true) => DiscoveryStanding::Yanked,
        // A pinned source archive proves an observed released artifact. The
        // independent yanked facet remains Unknown unless the frozen registry
        // snapshot explicitly supplied its bit.
        DiscoveryFacet::Known(false) | DiscoveryFacet::Absent | DiscoveryFacet::Unknown => {
            DiscoveryStanding::Published
        }
    };
    let metadata = DiscoveryMetadata {
        aliases,
        description,
        keywords,
        license,
        published_at,
        deprecation,
        yanked,
        advisories,
        downloads,
        cargo_sparse,
    };
    let proof = proof_from_row(row)?;
    Ok(DiscoveryFact {
        source,
        coordinate,
        standing,
        observed_at: DiscoveryObservedAt::from_unix_millis(SNAPSHOT_OBSERVED_AT_MS),
        source_event,
        source_event_time,
        proof,
        metadata,
    })
}

fn sync_document_metadata_fields(
    row: &mut Value,
    metadata: &DiscoveryMetadata,
) -> Result<(), String> {
    let fields = row
        .get_mut("fields")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "indexed document fields must be an object".to_owned())?;
    sync_metadata_facet(fields, "aliases", &metadata.aliases)?;
    sync_metadata_facet(fields, "description", &metadata.description)?;
    sync_metadata_facet(fields, "keywords", &metadata.keywords)?;
    sync_metadata_facet(fields, "license", &metadata.license)?;
    sync_metadata_facet(fields, "published_at", &metadata.published_at)?;
    sync_metadata_facet(fields, "deprecation", &metadata.deprecation)?;
    sync_metadata_facet(fields, "yanked", &metadata.yanked)?;
    sync_metadata_facet(fields, "advisories", &metadata.advisories)?;
    sync_metadata_facet(fields, "downloads", &metadata.downloads)?;
    sync_metadata_facet(fields, "cargo_sparse", &metadata.cargo_sparse)?;
    Ok(())
}

fn sync_metadata_facet<T: serde::Serialize>(
    fields: &mut serde_json::Map<String, Value>,
    name: &str,
    facet: &DiscoveryFacet<T>,
) -> Result<(), String> {
    let provenance_field = fields
        .get(name)
        .or_else(|| {
            (name == "deprecation")
                .then(|| fields.get("deprecated"))
                .flatten()
        })
        .cloned();
    let source_snapshot_id = provenance_field
        .as_ref()
        .and_then(|value| value.get("source_snapshot_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let source_pointer = provenance_field
        .as_ref()
        .and_then(|value| value.get("source_pointer"))
        .cloned()
        .unwrap_or(Value::Null);
    let (status, values) = match facet {
        DiscoveryFacet::Known(value) => {
            let encoded = serde_json::to_value(value)
                .map_err(|error| format!("encode {name} metadata facet: {error}"))?;
            let values = match encoded {
                Value::Array(values) => values,
                value => vec![value],
            };
            ("known", values)
        }
        DiscoveryFacet::Absent => ("absent", Vec::new()),
        DiscoveryFacet::Unknown => ("unknown", Vec::new()),
    };
    fields.insert(
        name.to_owned(),
        json!({
            "status": status,
            "values": values,
            "source_snapshot_id": source_snapshot_id,
            "source_pointer": source_pointer,
        }),
    );
    Ok(())
}

fn parse_source_event(
    row: &Value,
    source: DiscoverySourceIdentity,
) -> Result<(DiscoverySourceEvent, Option<String>), String> {
    let Some(event) = row.get("source_event") else {
        if row
            .get("source_event_time")
            .is_some_and(|value| !value.is_null())
        {
            return Err("source_event_time requires a source_event".to_owned());
        }
        return Ok((DiscoverySourceEvent::Snapshot, None));
    };
    let kind = match event.get("kind").and_then(Value::as_str) {
        Some(kind) => kind,
        None if event.get("state").and_then(Value::as_str) == Some("Snapshot") => "snapshot",
        None => return Err("source_event must be an object with a recognized kind".to_owned()),
    };
    let source_event_time = optional_source_event_time(row)?;
    match kind {
        "snapshot" => {
            if source_event_time.is_some() {
                return Err("snapshot source_event cannot carry source_event_time".to_owned());
            }
            Ok((DiscoverySourceEvent::Snapshot, None))
        }
        "unordered" => {
            if matches!(
                source.ecosystem(),
                RegistryEcosystem::Npm | RegistryEcosystem::Nuget
            ) {
                return Err(
                    "NPM and NuGet source events must carry typed ordering evidence".to_owned(),
                );
            }
            if source_event_time.is_some() {
                return Err("unordered source_event cannot carry source_event_time".to_owned());
            }
            Ok((DiscoverySourceEvent::Unordered, None))
        }
        "npm_change" => {
            if source.ecosystem() != RegistryEcosystem::Npm {
                return Err("npm_change source_event requires an NPM source".to_owned());
            }
            let sequence = event
                .get("sequence")
                .and_then(Value::as_u64)
                .filter(|sequence| *sequence > 0)
                .ok_or_else(|| "npm_change source_event requires a positive sequence".to_owned())?;
            let revision = match event.get("revision") {
                None | Some(Value::Null) => None,
                Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
                Some(_) => {
                    return Err("npm_change revision must be a nonempty string or null".to_owned());
                }
            };
            let change_proof = event
                .get("change_proof")
                .and_then(Value::as_str)
                .and_then(decode_hex)
                .ok_or_else(|| {
                    "npm_change change_proof must be 64 hexadecimal characters".to_owned()
                })?;
            let raw_time = source_event_time.as_deref().ok_or_else(|| {
                "npm_change source_event requires exact source_event_time".to_owned()
            })?;
            if raw_time.len() != 20
                || !raw_time.bytes().all(|byte| byte.is_ascii_digit())
                || raw_time.parse::<u64>().ok() != Some(sequence)
            {
                return Err(
                    "npm_change source_event_time must be the exact 20-digit sequence spelling"
                        .to_owned(),
                );
            }
            Ok((
                DiscoverySourceEvent::NpmChange {
                    sequence,
                    revision,
                    change_proof,
                },
                source_event_time,
            ))
        }
        "nuget_catalog" => {
            if source.ecosystem() != RegistryEcosystem::Nuget {
                return Err("nuget_catalog source_event requires a NuGet source".to_owned());
            }
            let commit_id = event
                .get("commit_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    "nuget_catalog source_event requires a nonempty commit_id".to_owned()
                })?
                .to_owned();
            let raw_time = source_event_time.as_deref().ok_or_else(|| {
                "nuget_catalog source_event requires exact source_event_time".to_owned()
            })?;
            let timestamp = DiscoveryTimestamp::parse_nuget_catalog_timestamp(raw_time)
                .map_err(|error| format!("invalid NuGet source_event_time: {error:?}"))?;
            Ok((
                DiscoverySourceEvent::NugetCatalog {
                    timestamp,
                    commit_id,
                },
                source_event_time,
            ))
        }
        _ => Err(format!("unsupported source_event kind: {kind}")),
    }
}

fn optional_source_event_time(row: &Value) -> Result<Option<String>, String> {
    match row.get("source_event_time") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => Err("source_event_time must be a nonempty string or null".to_owned()),
    }
}

fn update_source_event(
    update: &Value,
    source: DiscoverySourceIdentity,
) -> Result<(DiscoverySourceEvent, Option<String>), String> {
    if update.get("source_event").is_none()
        && update.get("source_event_time").is_none_or(Value::is_null)
    {
        return Ok((DiscoverySourceEvent::Snapshot, None));
    }
    let (event, event_time) = parse_source_event(update, source)?;
    match source.ecosystem() {
        RegistryEcosystem::Npm
            if !matches!(
                &event,
                DiscoverySourceEvent::Snapshot | DiscoverySourceEvent::NpmChange { .. }
            ) =>
        {
            return Err("NPM updates require a Snapshot or npm_change source_event".to_owned());
        }
        RegistryEcosystem::Nuget
            if !matches!(
                &event,
                DiscoverySourceEvent::Snapshot | DiscoverySourceEvent::NugetCatalog { .. }
            ) =>
        {
            return Err(
                "NuGet updates require a Snapshot or nuget_catalog source_event".to_owned(),
            );
        }
        RegistryEcosystem::Npm | RegistryEcosystem::Nuget => {}
        _ if !matches!(&event, DiscoverySourceEvent::Snapshot) => {
            return Err(
                "unsequenced benchmark updates must remain Snapshot observations".to_owned(),
            );
        }
        _ => {}
    }
    Ok((event, event_time))
}

fn validate_benchmark_transition(
    previous: &DiscoveryFact,
    incoming: &DiscoveryFact,
) -> Result<(), String> {
    let same_content = || {
        previous.source == incoming.source
            && previous.coordinate == incoming.coordinate
            && previous.standing == incoming.standing
            && previous.source_event == incoming.source_event
            && previous.source_event_time == incoming.source_event_time
            && previous.proof == incoming.proof
            && previous.metadata == incoming.metadata
    };
    match (&previous.source_event, &incoming.source_event) {
        (DiscoverySourceEvent::Snapshot, DiscoverySourceEvent::NpmChange { .. })
        | (DiscoverySourceEvent::Snapshot, DiscoverySourceEvent::NugetCatalog { .. }) => Ok(()),
        (DiscoverySourceEvent::NpmChange { .. }, DiscoverySourceEvent::Snapshot)
        | (DiscoverySourceEvent::NugetCatalog { .. }, DiscoverySourceEvent::Snapshot) => {
            Err("typed source events cannot transition back to a snapshot".to_owned())
        }
        (
            DiscoverySourceEvent::NugetCatalog {
                timestamp: old_timestamp,
                commit_id: old_commit,
            },
            DiscoverySourceEvent::NugetCatalog {
                timestamp: new_timestamp,
                commit_id: new_commit,
            },
        ) => {
            if old_commit == new_commit {
                if old_timestamp == new_timestamp && same_content() {
                    return Ok(());
                }
                return Err("same NuGet catalog event cannot change release content".to_owned());
            }
            match new_timestamp.cmp(old_timestamp) {
                std::cmp::Ordering::Greater => Ok(()),
                std::cmp::Ordering::Less => Err("NuGet catalog event is stale".to_owned()),
                std::cmp::Ordering::Equal => {
                    Err("different NuGet catalog events cannot share a timestamp".to_owned())
                }
            }
        }
        (
            DiscoverySourceEvent::NpmChange {
                sequence: old_sequence,
                ..
            },
            DiscoverySourceEvent::NpmChange {
                sequence: new_sequence,
                ..
            },
        ) => match new_sequence.cmp(old_sequence) {
            std::cmp::Ordering::Greater => Ok(()),
            std::cmp::Ordering::Less => Err("NPM change sequence is stale".to_owned()),
            std::cmp::Ordering::Equal if same_content() => Ok(()),
            std::cmp::Ordering::Equal => {
                Err("same NPM change event cannot change release content".to_owned())
            }
        },
        (
            DiscoverySourceEvent::Unordered | DiscoverySourceEvent::Snapshot,
            DiscoverySourceEvent::Unordered | DiscoverySourceEvent::Snapshot,
        ) => Ok(()),
        _ if same_content() => Ok(()),
        _ => Err("benchmark source event transition conflicts with retained provenance".to_owned()),
    }
}

enum FacetContents<'a> {
    Unknown,
    Absent,
    Known(Vec<&'a Value>),
}

fn facet_contents<'a>(
    facet: Option<&'a Value>,
    field_name: &str,
    allow_object_value: bool,
) -> Result<FacetContents<'a>, String> {
    let Some(facet) = facet else {
        return Ok(FacetContents::Unknown);
    };
    if facet.is_null() {
        return Ok(FacetContents::Absent);
    }
    if let Some(values) = facet.as_array() {
        return Ok(FacetContents::Known(values.iter().collect()));
    }
    if let Some(object) = facet.as_object() {
        let status_form = object.contains_key("status");
        let tagged_form = object.contains_key("state");
        let unexpected_member = if status_form {
            object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "status" | "values" | "source_snapshot_id" | "source_pointer"
                )
            })
        } else if tagged_form {
            object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "state" | "value" | "source_snapshot_id" | "source_pointer"
                )
            })
        } else {
            false
        };
        if unexpected_member {
            return Err(format!("{field_name} facet has an unexpected member"));
        }
        if status_form && tagged_form {
            return Err(format!(
                "{field_name} facet cannot mix status and state forms"
            ));
        }
        let state = if let Some(status) = object.get("status") {
            status
                .as_str()
                .ok_or_else(|| format!("{field_name} facet status must be a string"))?
        } else if let Some(state) = object.get("state") {
            match state.as_str() {
                Some("Known" | "known") => "known",
                Some("Absent" | "absent") => "absent",
                Some("Unknown" | "unknown") => "unknown",
                Some(_) => return Err(format!("invalid {field_name} facet state")),
                None => return Err(format!("{field_name} facet state must be a string")),
            }
        } else if allow_object_value {
            return Ok(FacetContents::Known(vec![facet]));
        } else {
            return Err(format!("{field_name} facet object has no status or state"));
        };
        match state {
            "known" => {
                if object.contains_key("status") {
                    let values = object
                        .get("values")
                        .and_then(Value::as_array)
                        .ok_or_else(|| format!("known {field_name} facet has no values array"))?;
                    Ok(FacetContents::Known(values.iter().collect()))
                } else {
                    let value = object
                        .get("value")
                        .ok_or_else(|| format!("known {field_name} facet has no value"))?;
                    if value.is_null() {
                        return Err(format!("known {field_name} facet value cannot be null"));
                    }
                    if let Some(values) = value.as_array() {
                        Ok(FacetContents::Known(values.iter().collect()))
                    } else {
                        Ok(FacetContents::Known(vec![value]))
                    }
                }
            }
            "absent" | "unknown" => {
                if let Some(values) = object.get("values") {
                    if !values.as_array().is_some_and(Vec::is_empty) {
                        return Err(format!("{field_name} {state} facet must not carry values"));
                    }
                }
                if let Some(value) = object.get("value")
                    && !value.is_null()
                {
                    return Err(format!("{field_name} {state} facet must not carry a value"));
                }
                Ok(if state == "absent" {
                    FacetContents::Absent
                } else {
                    FacetContents::Unknown
                })
            }
            status => Err(format!("invalid {field_name} facet status: {status}")),
        }
    } else {
        Ok(FacetContents::Known(vec![facet]))
    }
}

fn string_list_facet(
    facet: Option<&Value>,
    field_name: &str,
) -> Result<DiscoveryFacet<Vec<String>>, String> {
    if facet.is_some_and(|value| !value.is_null() && !value.is_array() && !value.is_object()) {
        return Err(format!(
            "{field_name} facet must be an array or state object"
        ));
    }
    if let Some(object) = facet.and_then(Value::as_object)
        && object.contains_key("state")
        && matches!(
            object.get("state").and_then(Value::as_str),
            Some("Known" | "known")
        )
        && !object.get("value").is_some_and(Value::is_array)
    {
        return Err(format!("known {field_name} state value must be an array"));
    }
    match facet_contents(facet, field_name, false)? {
        FacetContents::Unknown => Ok(DiscoveryFacet::Unknown),
        FacetContents::Absent => Ok(DiscoveryFacet::Absent),
        FacetContents::Known(values) => Ok(DiscoveryFacet::Known(
            values
                .into_iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("{field_name} values must be strings"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        )),
    }
}

fn string_facet(facet: Option<&Value>, field_name: &str) -> Result<DiscoveryFacet<String>, String> {
    if facet.is_some_and(Value::is_array) {
        return Err(format!("{field_name} facet must contain a scalar value"));
    }
    match facet_contents(facet, field_name, false)? {
        FacetContents::Unknown => Ok(DiscoveryFacet::Unknown),
        FacetContents::Absent => Ok(DiscoveryFacet::Absent),
        FacetContents::Known(values) => {
            let [value] = values.as_slice() else {
                return Err(format!(
                    "known {field_name} facet must contain exactly one value"
                ));
            };
            value
                .as_str()
                .map(|value| DiscoveryFacet::Known(value.to_owned()))
                .ok_or_else(|| format!("known {field_name} facet value must be a string"))
        }
    }
}

fn boolean_facet(facet: Option<&Value>, field_name: &str) -> Result<DiscoveryFacet<bool>, String> {
    if facet.is_some_and(Value::is_array) {
        return Err(format!("{field_name} facet must contain a scalar value"));
    }
    match facet_contents(facet, field_name, false)? {
        FacetContents::Unknown => Ok(DiscoveryFacet::Unknown),
        FacetContents::Absent => Ok(DiscoveryFacet::Absent),
        FacetContents::Known(values) => {
            let [value] = values.as_slice() else {
                return Err(format!(
                    "known {field_name} facet must contain exactly one value"
                ));
            };
            value
                .as_bool()
                .map(DiscoveryFacet::Known)
                .ok_or_else(|| format!("known {field_name} facet value must be a boolean"))
        }
    }
}

fn u64_facet(facet: Option<&Value>, field_name: &str) -> Result<DiscoveryFacet<u64>, String> {
    if facet.is_some_and(Value::is_array) {
        return Err(format!("{field_name} facet must contain a scalar value"));
    }
    match facet_contents(facet, field_name, false)? {
        FacetContents::Unknown => Ok(DiscoveryFacet::Unknown),
        FacetContents::Absent => Ok(DiscoveryFacet::Absent),
        FacetContents::Known(values) => {
            let [value] = values.as_slice() else {
                return Err(format!(
                    "known {field_name} facet must contain exactly one value"
                ));
            };
            value.as_u64().map(DiscoveryFacet::Known).ok_or_else(|| {
                format!("known {field_name} facet value must be a nonnegative integer")
            })
        }
    }
}

fn cargo_sparse_facet(
    facet: Option<&Value>,
) -> Result<DiscoveryFacet<CratesSparseMetadata>, String> {
    match facet_contents(facet, "cargo_sparse", true)? {
        FacetContents::Unknown => Ok(DiscoveryFacet::Unknown),
        FacetContents::Absent => Ok(DiscoveryFacet::Absent),
        FacetContents::Known(values) => {
            let [value] = values.as_slice() else {
                return Err("known cargo_sparse facet must contain exactly one value".to_owned());
            };
            serde_json::from_value((*value).clone())
                .map(DiscoveryFacet::Known)
                .map_err(|error| format!("decode cargo_sparse facet: {error}"))
        }
    }
}

fn advisory_facet(facet: Option<&Value>) -> Result<DiscoveryFacet<Vec<DiscoveryAdvisory>>, String> {
    match facet_contents(facet, "advisories", false)? {
        FacetContents::Unknown => Ok(DiscoveryFacet::Unknown),
        FacetContents::Absent => Ok(DiscoveryFacet::Absent),
        FacetContents::Known(values) => {
            let mut advisories = Vec::with_capacity(values.len());
            for value in values {
                if let Some(id) = value.as_str() {
                    advisories.push(DiscoveryAdvisory {
                        id: id.to_owned(),
                        aliases: DiscoveryFacet::Unknown,
                        summary: DiscoveryFacet::Unknown,
                        severity: DiscoveryFacet::Unknown,
                        fixed_in: DiscoveryFacet::Unknown,
                    });
                    continue;
                }
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "advisory row must have a string id".to_owned())?;
                let aliases = string_list_facet(value.get("aliases"), "advisory aliases")?;
                advisories.push(DiscoveryAdvisory {
                    id: id.to_owned(),
                    aliases,
                    summary: direct_string_facet(value.get("summary"), "advisory summary")?,
                    severity: direct_string_facet(value.get("severity"), "advisory severity")?,
                    fixed_in: direct_string_list_facet(value.get("fixed_in"), "advisory fixed_in")?,
                });
            }
            Ok(DiscoveryFacet::Known(advisories))
        }
    }
}

fn direct_string_facet(
    value: Option<&Value>,
    field_name: &str,
) -> Result<DiscoveryFacet<String>, String> {
    string_facet(value, field_name)
}

fn direct_string_list_facet(
    value: Option<&Value>,
    field_name: &str,
) -> Result<DiscoveryFacet<Vec<String>>, String> {
    string_list_facet(value, field_name)
}

fn proof_from_row(row: &Value) -> Result<[u8; 32], String> {
    if let Some(line_hash) = row
        .get("registry_index_line_sha256")
        .and_then(Value::as_str)
    {
        let hex = line_hash.strip_prefix("sha256:").unwrap_or(line_hash);
        if let Some(proof) = (hex.len() == 64).then(|| decode_hex(hex)).flatten() {
            return Ok(proof);
        }
        return Err("registry index line hash must be 64 hexadecimal characters".to_owned());
    }
    // Nix pins provide an archive digest but this adapter does not fetch or
    // verify archive bytes. Use the canonical frozen observation row as the
    // local proof value instead of misrepresenting its declared archive hash.
    let bytes = serde_json::to_vec(row)
        .map_err(|error| format!("encode frozen observation for proof: {error}"))?;
    Ok(*backend_engine::blake3::hash(&bytes).as_bytes())
}

fn decode_hex(value: &str) -> Option<[u8; 32]> {
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        output[index] = (high << 4) | low;
    }
    Some(output)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn snapshot_cursor(
    source: DiscoverySourceIdentity,
    batch_sequence: usize,
) -> Result<DiscoveryCursor, String> {
    let mut token = String::from("search-quality-snapshot-");
    for byte in source.id() {
        use std::fmt::Write as _;
        write!(token, "{byte:02x}").map_err(|error| format!("format cursor: {error}"))?;
    }
    use std::fmt::Write as _;
    write!(token, "-{batch_sequence:016}")
        .map_err(|error| format!("format snapshot batch cursor: {error}"))?;
    DiscoveryCursor::new(token.into_bytes())
        .map_err(|error| format!("create snapshot cursor: {error}"))
}

fn benchmark_update_sequence(cursor: &DiscoveryCursor) -> u64 {
    std::str::from_utf8(cursor.as_bytes())
        .ok()
        .and_then(|value| value.strip_prefix("z-benchmark-update-"))
        .and_then(|sequence| sequence.parse::<u64>().ok())
        .unwrap_or(0)
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Result<&'a str, String> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .ok_or_else(|| format!("missing JSON field: {}", path.join(".")))?;
    }
    current
        .as_str()
        .ok_or_else(|| format!("JSON field is not a string: {}", path.join(".")))
}

fn facet_is_yanked(facet: &Value) -> bool {
    matches!(
        boolean_facet(Some(facet), "yanked"),
        Ok(DiscoveryFacet::Known(true))
    )
}

fn standing_name(standing: SearchStandingEvidence) -> &'static str {
    match standing {
        SearchStandingEvidence::Available => "available",
        SearchStandingEvidence::RecipeAvailable => "recipe_available",
        SearchStandingEvidence::Yanked => "yanked",
        SearchStandingEvidence::Withdrawn => "withdrawn",
        SearchStandingEvidence::Absent => "absent",
        SearchStandingEvidence::Unknown => "unknown",
    }
}

fn tantivy_projection_bytes(search: &DiscoverySearchIndex) -> Result<u64, String> {
    let release_bytes = text_index_bytes(&search.inner)?;
    let lineage_bytes = text_index_bytes(&search.lineages.inner)?;
    Ok(release_bytes.saturating_add(lineage_bytes))
}

fn text_index_bytes<K>(index: &super::TextSearchIndex<K>) -> Result<u64, String> {
    let directory = index._index.directory();
    let mut total = 0_u64;
    for path in directory.list_managed_files() {
        // `meta.json` is listed as managed but is intentionally written with
        // `atomic_write`, without the footer used by `open_read` for segment
        // files. Read the raw managed bytes so all files are counted and the
        // metadata file does not fail footer validation.
        let bytes = directory
            .atomic_read(&path)
            .map_err(|error| format!("read Tantivy managed file {}: {error}", path.display()))?;
        let bytes = u64::try_from(bytes.len())
            .map_err(|_| "Tantivy index byte count overflow".to_owned())?;
        total = total.saturating_add(bytes);
    }
    Ok(total)
}

fn directory_bytes(root: &Path) -> Result<u64, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0_u64;
    while let Some(path) = pending.pop() {
        let entries =
            fs::read_dir(&path).map_err(|error| format!("read index directory: {error}"))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("read index entry: {error}"))?;
            let kind = entry
                .file_type()
                .map_err(|error| format!("stat index entry: {error}"))?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total = total.saturating_add(
                    entry
                        .metadata()
                        .map_err(|error| format!("stat index file: {error}"))?
                        .len(),
                );
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod benchmark_contract_tests {
    use super::*;

    fn registry_source(ecosystem: RegistryEcosystem) -> DiscoverySourceIdentity {
        let endpoint_value = endpoint(ecosystem).expect("known registry endpoint");
        registry::discovery_source_identity(&endpoint_value)
    }

    fn coordinate(ecosystem: &str, name: &str) -> ProductPackageCoordinate {
        ProductPackageCoordinate::parse(format!("pkg:{ecosystem}/{name}@1.0.0"))
            .expect("valid package coordinate")
    }

    fn facet_row(fields: Value) -> Value {
        json!({
            "document_id": "fixture:package@1.0.0",
            "fields": fields,
        })
    }

    #[test]
    fn aliases_keep_unknown_absent_and_known_empty_distinct() {
        assert_eq!(
            string_list_facet(None, "aliases").expect("missing aliases"),
            DiscoveryFacet::Unknown
        );
        assert_eq!(
            string_list_facet(Some(&Value::Null), "aliases").expect("null aliases"),
            DiscoveryFacet::Absent
        );
        assert_eq!(
            string_list_facet(Some(&json!({"status": "known", "values": []})), "aliases")
                .expect("known empty aliases"),
            DiscoveryFacet::Known(Vec::new())
        );
        assert_eq!(
            string_list_facet(Some(&json!({"state": "Unknown"})), "aliases")
                .expect("tagged unknown aliases"),
            DiscoveryFacet::Unknown
        );
    }

    #[test]
    fn malformed_present_facets_fail_instead_of_becoming_unknown() {
        assert!(string_list_facet(Some(&json!({"status": true})), "aliases").is_err());
        assert!(string_list_facet(Some(&json!("alias")), "aliases").is_err());
        assert!(
            string_list_facet(
                Some(&json!({"status": "known", "values": ["valid", 7]})),
                "aliases"
            )
            .is_err()
        );
        assert!(
            string_facet(
                Some(&json!({"status": "known", "values": []})),
                "description"
            )
            .is_err()
        );
        assert!(string_facet(Some(&json!(["one"])), "description").is_err());
        assert!(
            boolean_facet(
                Some(&json!({"status": "known", "values": ["false"]})),
                "yanked"
            )
            .is_err()
        );
        assert!(u64_facet(Some(&json!(-1)), "downloads").is_err());
        assert_eq!(
            u64_facet(Some(&json!({"state": "Unknown"})), "downloads")
                .expect("unavailable download count remains unknown"),
            DiscoveryFacet::Unknown
        );
        assert!(
            advisory_facet(Some(&json!({
                "status": "known",
                "values": [{"id": "OSV-1", "aliases": ["CVE-1", 7]}]
            })))
            .is_err()
        );
    }

    #[test]
    fn metadata_projection_preserves_registry_facet_states_and_values() {
        let row = facet_row(json!({
            "aliases": {"state": "Known", "value": []},
            "description": {"state": "Absent"},
            "keywords": {"state": "Unknown"},
            "license": {"state": "Known", "value": "MIT"},
            "published_at": {"state": "Known", "value": "2026-10-01T12:00:00Z"},
            "deprecated": {"state": "Known", "value": "use replacement"},
            "yanked": {"state": "Known", "value": true},
            "downloads": {"state": "Known", "value": 42},
            "advisories": {
                "status": "known",
                "values": [{
                    "id": "OSV-1",
                    "aliases": null,
                    "summary": null,
                    "severity": "HIGH",
                    "fixed_in": {"status": "known", "values": []}
                }]
            }
        }));
        let fact = fact_from_row(
            &row,
            registry_source(RegistryEcosystem::Cargo),
            coordinate("cargo", "sample"),
        )
        .expect("valid metadata row");
        assert_eq!(fact.source_event, DiscoverySourceEvent::Snapshot);
        assert_eq!(fact.metadata.aliases, DiscoveryFacet::Known(Vec::new()));
        assert_eq!(fact.metadata.description, DiscoveryFacet::Absent);
        assert_eq!(fact.metadata.keywords, DiscoveryFacet::Unknown);
        assert_eq!(
            fact.metadata.license,
            DiscoveryFacet::Known("MIT".to_owned())
        );
        assert_eq!(
            fact.metadata.deprecation,
            DiscoveryFacet::Known("use replacement".to_owned())
        );
        assert_eq!(fact.metadata.downloads, DiscoveryFacet::Known(42));
        assert_eq!(fact.standing, DiscoveryStanding::Yanked);
        let DiscoveryFacet::Known(advisories) = fact.metadata.advisories else {
            panic!("known advisories were lost");
        };
        assert_eq!(advisories[0].aliases, DiscoveryFacet::Absent);
        assert_eq!(advisories[0].summary, DiscoveryFacet::Absent);
        assert_eq!(advisories[0].fixed_in, DiscoveryFacet::Known(Vec::new()));
    }

    #[test]
    fn typed_events_keep_exact_ordering_evidence_and_reject_invented_times() {
        let npm = registry_source(RegistryEcosystem::Npm);
        let proof = "ab".repeat(32);
        let npm_row = json!({
            "source_event": {
                "kind": "npm_change",
                "sequence": 10,
                "revision": "1-abc",
                "change_proof": proof
            },
            "source_event_time": "00000000000000000010",
            "fields": {}
        });
        let (event, raw_time) = parse_source_event(&npm_row, npm).expect("typed NPM event");
        assert_eq!(raw_time.as_deref(), Some("00000000000000000010"));
        assert!(matches!(
            event,
            DiscoverySourceEvent::NpmChange { sequence: 10, .. }
        ));
        assert_eq!(
            update_source_event(&json!({"fields": {}}), npm)
                .expect("missing upstream event remains a snapshot")
                .0,
            DiscoverySourceEvent::Snapshot
        );
        assert_eq!(
            parse_source_event(
                &json!({"source_event": {"state": "Snapshot", "reason": "unsequenced source"}}),
                npm
            )
            .expect("explicit upstream snapshot")
            .0,
            DiscoverySourceEvent::Snapshot
        );
        let mut wrong_time = npm_row.clone();
        wrong_time["source_event_time"] = json!("00000000000000000011");
        assert!(parse_source_event(&wrong_time, npm).is_err());

        let nuget = registry_source(RegistryEcosystem::Nuget);
        let nuget_row = json!({
            "source_event": {"kind": "nuget_catalog", "commit_id": "catalog-opaque-1"},
            "source_event_time": "2026-10-01T12:30:00.100Z",
            "fields": {}
        });
        let (event, raw_time) = parse_source_event(&nuget_row, nuget).expect("typed NuGet event");
        assert_eq!(raw_time.as_deref(), Some("2026-10-01T12:30:00.100Z"));
        assert!(matches!(event, DiscoverySourceEvent::NugetCatalog { .. }));
    }

    #[test]
    fn same_typed_event_cannot_relabel_changed_release_metadata() {
        let source = registry_source(RegistryEcosystem::Npm);
        let proof = "cd".repeat(32);
        let base = json!({
            "document_id": "fixture:npm/sample@1.0.0",
            "source_event": {
                "kind": "npm_change",
                "sequence": 10,
                "revision": "1-abc",
                "change_proof": proof
            },
            "source_event_time": "00000000000000000010",
            "fields": {"aliases": {"status": "known", "values": ["old"]}}
        });
        let existing =
            fact_from_row(&base, source, coordinate("npm", "sample")).expect("baseline NPM fact");
        let mut changed = base.clone();
        changed["fields"]["aliases"] = json!({"status": "known", "values": ["new"]});
        let mut incoming =
            fact_from_row(&changed, source, coordinate("npm", "sample")).expect("same event fact");
        incoming.proof = existing.proof;
        assert!(validate_benchmark_transition(&existing, &incoming).is_err());

        changed["source_event"]["sequence"] = json!(11);
        changed["source_event"]["change_proof"] = json!("ef".repeat(32));
        changed["source_event_time"] = json!("00000000000000000011");
        let mut newer =
            fact_from_row(&changed, source, coordinate("npm", "sample")).expect("newer event fact");
        newer.proof = existing.proof;
        assert!(validate_benchmark_transition(&existing, &newer).is_ok());
    }

    #[test]
    fn snapshot_metadata_updates_do_not_invent_typed_events() {
        let source = registry_source(RegistryEcosystem::Npm);
        let base = json!({
            "document_id": "fixture:npm/sample@1.0.0",
            "fields": {"aliases": {"status": "known", "values": ["old"]}}
        });
        let existing =
            fact_from_row(&base, source, coordinate("npm", "sample")).expect("snapshot baseline");
        let mut changed = base.clone();
        changed["fields"]["aliases"] = json!({"status": "absent", "values": []});
        let mut incoming =
            fact_from_row(&changed, source, coordinate("npm", "sample")).expect("snapshot update");
        incoming.proof = existing.proof;
        assert_eq!(incoming.source_event, DiscoverySourceEvent::Snapshot);
        assert!(validate_benchmark_transition(&existing, &incoming).is_ok());
    }

    #[test]
    fn replay_sync_keeps_facet_state_and_snapshot_provenance() {
        let mut row = facet_row(json!({
            "aliases": {
                "status": "known",
                "values": ["old"],
                "source_snapshot_id": "fixture:source-v1"
            }
        }));
        let metadata = DiscoveryMetadata {
            aliases: DiscoveryFacet::Known(Vec::new()),
            description: DiscoveryFacet::Absent,
            ..DiscoveryMetadata::default()
        };
        sync_document_metadata_fields(&mut row, &metadata).expect("sync metadata into row");
        let fields = row
            .get("fields")
            .and_then(Value::as_object)
            .expect("fields object");
        assert_eq!(
            string_list_facet(fields.get("aliases"), "aliases").expect("aliases facet"),
            DiscoveryFacet::Known(Vec::new())
        );
        assert_eq!(
            fields["aliases"]["source_snapshot_id"],
            json!("fixture:source-v1")
        );
        assert_eq!(
            string_facet(fields.get("description"), "description").expect("description facet"),
            DiscoveryFacet::Absent
        );
    }
}
