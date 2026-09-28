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
    CARGO_SPARSE_INDEX, CONAN_CENTER, DiscoveryAdvisory, DiscoveryBatch, DiscoveryCompleteness,
    DiscoveryCursor, DiscoveryFacet, DiscoveryFact, DiscoveryMetadata, DiscoveryObservedAt,
    DiscoveryPackageRetraction, DiscoverySourceIdentity, DiscoveryStanding, GO_MODULE_PROXY,
    MAVEN_CENTRAL, MAX_DISCOVERY_PAGE_ITEMS, NPM_REGISTRY, NUGET_V3, PYPI_SIMPLE_API,
    RegistryEcosystem, RegistryEndpoint,
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
            for (batch_index, chunk) in facts.chunks(MAX_DISCOVERY_PAGE_ITEMS).enumerate() {
                let next_cursor = snapshot_cursor(source, batch_index.saturating_add(1))?;
                store
                    .commit(DiscoveryBatch {
                        source,
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
    let search = DiscoverySearchIndex::open_with_forge(&store, &forge_documents)
        .map_err(|error| format!("rebuild production Tantivy projection: {error}"))?;
    Ok(BenchmarkIndex {
        root: index_root.to_path_buf(),
        store,
        search,
        documents,
        canonical_document_ids,
        update_sequence: 0,
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
    /// ids that matched inside that lineage.
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
        Ok(response)
    }

    /// Applies a package-level NPM deletion event through the authoritative
    /// discovery journal, then keeps the production search projection in sync.
    /// This hook exists only under the benchmark feature for retraction tests.
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
        self.update_sequence = self.update_sequence.saturating_add(1);
        let observed_at = self
            .store
            .latest_observed_at(source)
            .unwrap_or(SNAPSHOT_OBSERVED_AT_MS)
            .saturating_add(1);
        let next_cursor = DiscoveryCursor::new(
            format!("z-benchmark-update-{:016}", self.update_sequence).into_bytes(),
        )
        .map_err(|error| format!("create retraction cursor: {error}"))?;
        let previous_cursor = self.store.cursor(source);
        self.store
            .commit(DiscoveryBatch {
                source,
                previous_cursor,
                next_cursor: next_cursor.clone(),
                source_high_watermark: next_cursor,
                caught_up: false,
                observed_at: DiscoveryObservedAt::from_unix_millis(observed_at),
                completeness: DiscoveryCompleteness::Windowed,
                facts: Vec::new(),
                package_retractions: vec![DiscoveryPackageRetraction {
                    package_name: package_name.clone(),
                    source_event_time: format!("benchmark-retraction-{:016}", self.update_sequence),
                    proof: [0xA7; 32],
                }],
            })
            .map_err(|error| format!("commit NPM package retraction: {error:?}"))?;
        self.search
            .sync(&self.store)
            .map_err(|error| format!("synchronize production Tantivy postings: {error}"))?;
        Ok(json!({
            "op": "retract-package",
            "package_name": package_name,
            "updated_releases": release_count,
        }))
    }

    /// Applies one or more document-field mutations through the discovery
    /// journal and synchronizes the production incremental Tantivy projection.
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
        let mut updated = 0_usize;
        for update in updates {
            let document_id = string_at(update, &["document_id"])?.to_owned();
            let mut document = self
                .documents
                .get(&document_id)
                .cloned()
                .ok_or_else(|| format!("unknown update document: {document_id}"))?;
            let source = document
                .registry_source
                .ok_or_else(|| format!("document is not searchable: {document_id}"))?;
            let coordinate = document
                .coordinate
                .clone()
                .ok_or_else(|| format!("document has no registry coordinate: {document_id}"))?;
            let field_updates = update
                .get("fields")
                .and_then(Value::as_object)
                .ok_or_else(|| "update fields must be a JSON object".to_owned())?;
            let fields = document
                .row
                .get_mut("fields")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| "indexed document fields must be an object".to_owned())?;
            for (field, value) in field_updates {
                fields.insert(field.clone(), value.clone());
            }
            let existing = self
                .store
                .fact(source, coordinate.as_str())
                .cloned()
                .ok_or_else(|| {
                    format!("document is absent from the discovery journal: {document_id}")
                })?;
            let observed_at = existing.observed_at.as_unix_millis().saturating_add(1);
            let mut incoming = fact_from_row(&document.row, source, coordinate)?;
            incoming.proof = existing.proof;
            incoming.source_event_time = existing.source_event_time;
            incoming.observed_at = DiscoveryObservedAt::from_unix_millis(observed_at);
            let previous_cursor = self.store.cursor(source);
            self.update_sequence = self.update_sequence.saturating_add(1);
            let next_cursor = DiscoveryCursor::new(
                format!("z-benchmark-update-{:016}", self.update_sequence).into_bytes(),
            )
            .map_err(|error| format!("create update cursor: {error}"))?;
            self.store
                .commit(DiscoveryBatch {
                    source,
                    previous_cursor,
                    next_cursor: next_cursor.clone(),
                    source_high_watermark: next_cursor,
                    caught_up: false,
                    observed_at: DiscoveryObservedAt::from_unix_millis(observed_at),
                    completeness: DiscoveryCompleteness::Windowed,
                    facts: vec![incoming],
                    package_retractions: Vec::new(),
                })
                .map_err(|error| format!("commit document update: {error:?}"))?;
            self.search
                .sync(&self.store)
                .map_err(|error| format!("synchronize production Tantivy postings: {error}"))?;
            self.documents.insert(document_id, document);
            updated = updated.saturating_add(1);
        }
        Ok(json!({"op": "update", "updated": updated}))
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
    let source = DiscoverySourceIdentity::from_endpoint(&endpoint);
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
    let fields = row
        .get("fields")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("document {} has no fields object", row["document_id"]))?;
    let aliases = string_list_facet(fields.get("aliases"), "aliases")?;
    let description = string_facet(fields.get("description"), "description")?;
    let keywords = string_list_facet(fields.get("keywords"), "keywords")?;
    let yanked = boolean_facet(fields.get("yanked"), "yanked")?;
    let advisories = advisory_facet(fields.get("advisories"))?;
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
        yanked,
        advisories,
        ..DiscoveryMetadata::default()
    };
    let proof = proof_from_row(row)?;
    Ok(DiscoveryFact {
        source,
        coordinate,
        standing,
        observed_at: DiscoveryObservedAt::from_unix_millis(SNAPSHOT_OBSERVED_AT_MS),
        source_event_time: None,
        proof,
        metadata,
    })
}

fn string_list_facet(
    facet: Option<&Value>,
    field_name: &str,
) -> Result<DiscoveryFacet<Vec<String>>, String> {
    let Some(facet) = facet else {
        return Ok(DiscoveryFacet::Unknown);
    };
    match facet
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "known" => {
            let values = facet
                .get("values")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("known {field_name} facet has no values array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("{field_name} values must be strings"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(DiscoveryFacet::Known(values))
        }
        "absent" => Ok(DiscoveryFacet::Absent),
        "unknown" => Ok(DiscoveryFacet::Unknown),
        status => Err(format!("invalid {field_name} facet status: {status}")),
    }
}

fn string_facet(facet: Option<&Value>, field_name: &str) -> Result<DiscoveryFacet<String>, String> {
    let Some(facet) = facet else {
        return Ok(DiscoveryFacet::Unknown);
    };
    match facet
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "known" => {
            let value = facet
                .get("values")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(DiscoveryFacet::Known(value.to_owned()))
        }
        "absent" => Ok(DiscoveryFacet::Absent),
        "unknown" => Ok(DiscoveryFacet::Unknown),
        status => Err(format!("invalid {field_name} facet status: {status}")),
    }
}

fn boolean_facet(facet: Option<&Value>, field_name: &str) -> Result<DiscoveryFacet<bool>, String> {
    let Some(facet) = facet else {
        return Ok(DiscoveryFacet::Unknown);
    };
    match facet
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "known" => {
            let value = facet
                .get("values")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("known {field_name} facet must contain a boolean"))?;
            Ok(DiscoveryFacet::Known(value))
        }
        "absent" => Ok(DiscoveryFacet::Absent),
        "unknown" => Ok(DiscoveryFacet::Unknown),
        status => Err(format!("invalid {field_name} facet status: {status}")),
    }
}

fn advisory_facet(facet: Option<&Value>) -> Result<DiscoveryFacet<Vec<DiscoveryAdvisory>>, String> {
    let Some(facet) = facet else {
        return Ok(DiscoveryFacet::Unknown);
    };
    match facet
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "known" => {
            let values = facet
                .get("values")
                .and_then(Value::as_array)
                .ok_or_else(|| "known advisories facet has no values array".to_owned())?;
            let mut advisories = Vec::with_capacity(values.len());
            for value in values {
                if let Some(id) = value.as_str() {
                    advisories.push(DiscoveryAdvisory {
                        id: id.to_owned(),
                        aliases: Vec::new(),
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
                let aliases = value
                    .get("aliases")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                advisories.push(DiscoveryAdvisory {
                    id: id.to_owned(),
                    aliases,
                    summary: direct_string_facet(value.get("summary")),
                    severity: direct_string_facet(value.get("severity")),
                    fixed_in: direct_string_list_facet(value.get("fixed_in")),
                });
            }
            Ok(DiscoveryFacet::Known(advisories))
        }
        "absent" => Ok(DiscoveryFacet::Absent),
        "unknown" => Ok(DiscoveryFacet::Unknown),
        status => Err(format!("invalid advisories facet status: {status}")),
    }
}

fn direct_string_facet(value: Option<&Value>) -> DiscoveryFacet<String> {
    value
        .and_then(Value::as_str)
        .map(|text| DiscoveryFacet::Known(text.to_owned()))
        .unwrap_or(DiscoveryFacet::Unknown)
}

fn direct_string_list_facet(value: Option<&Value>) -> DiscoveryFacet<Vec<String>> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            DiscoveryFacet::Known(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            )
        })
        .unwrap_or(DiscoveryFacet::Unknown)
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
    facet.get("status").and_then(Value::as_str) == Some("known")
        && facet
            .get("values")
            .and_then(Value::as_array)
            .and_then(|values| values.first())
            .and_then(Value::as_bool)
            == Some(true)
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
        let slice = directory
            .open_read(&path)
            .map_err(|error| format!("read Tantivy managed file {}: {error}", path.display()))?;
        let bytes = u64::try_from(slice.len())
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
