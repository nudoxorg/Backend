# nudox-occurrences

Symbol occurrence indexing for user repositories. A companion subsystem to the
Nudox library-API extraction pipeline. Where the compiler extracts *what* a
library exports, this subsystem indexes *how* symbols from that library are used
inside user repos.

The original standalone workspace (`occurences/`) was migrated here so that
`nudox-pipeline` could depend on the Backend IR's tree-sitter parser without
cross-workspace path hacks. The `occurences/` directory is kept for historical
reference; `Backend/crates/` is canonical.

---

## Architecture

```
User repo analysis
       │
       ▼  POST /api/occurrences
 PipelineInput
       │
       ▼
 nudox-pipeline ─── tree-sitter parse ──► snippet + sexp + references
       │ (parse_and_extract via ir::syntax)
       ▼
 BlobInfo (always produced, embeddings always generated)
       │
       ▼
 nudox-orchestrator
       │
       ├── blob_store.put() ───────────────────────────► BlobStore  (always first)
       │
       ▼
 [symbol_origin == ExternalLib?]
       │
  YES  │  NO (Repo-local)
       │  └──► new GlobalSymbolId (UUID v4)
       │        global_store.associate()
       │        blob_store.update_resolution()
       │        search.index() + vector.upsert()
       │        return Resolved
       ▼
 global_store.lookup(lib, symbol_name)  ◄── SQLite (nudox-store)
       │
  Some │  None
       │  └──► queue.enqueue(lib, blob_ref) ──────────► deferred_queue (SQLite)
       │        return Deferred
       ▼
 blob_store.update_resolution()
 global_store.associate()
 search.index() + vector.upsert()
 return Resolved

[Later: library parsed by compiler]
       │
       ▼
 nudox-store.register_library(lang, lib, version, entry_uris)
       │
       ▼
 orchestrator.resolve_lib(lib)
       │
       └──► queue.drain_for_lib()
             for each blob: get → update_resolution → index → upsert
```

**Invariant: BlobInfo is the system of record.** The search index, vector index,
and SQLite associations are all derivable from blob storage alone.
`Orchestrator::rebuild_indexes()` re-indexes all resolved blobs from the blob
store when needed (schema migration, index corruption, fresh deployment).

---

## Workspace layout

| Crate | Role |
|---|---|
| `nudox-core` | Shared types, traits, error type — no logic |
| `nudox-embed` | `Embedder` implementations: Placeholder, Mock, InProcess stub, RemoteEmbedder (HTTP) |
| `nudox-blobstore` | `BlobStore` implementations: `ObjectStoreBlobStore` (local FS via `object_store`), `InMemoryBlobStore` |
| `nudox-search` | `SearchIndex` via tantivy, `VectorIndex` via Qdrant, in-memory stubs, `SearchQuery` + `VectorQuery` trait impls |
| `nudox-pipeline` | Converts `PipelineInput` → `BlobInfo` with tree-sitter snippet extraction and embeddings |
| `nudox-orchestrator` | Coordination layer: ingest routing, deferred queue management, `resolve_lib`, `rebuild_indexes` |
| `nudox-store` | SQLite-backed `GlobalSymbolStore` + `FutureParseQueue`. Canonical link between TerminusDB and occurrences |
| `nudox-indexer` | Demo binary wiring all crates end-to-end |

**Dependency rule:** only `nudox-orchestrator` depends on more than one
implementation crate. No cycles. `nudox-core` depends on nothing in this
workspace.

---

## Building

All commands must run inside the nix flake environment — `Backend/.cargo/config.toml`
requires `clang` as the linker, which is not in the regular system PATH:

```bash
# Enter the environment (or use direnv)
nix develop /home/leaf/Dev/nudox/Backend

# Then run any cargo command normally:
cargo check --workspace
cargo test --workspace
cargo build --release -p nudox           # compiler binary
cargo run -p nudox-indexer               # demo
NUDOX_QDRANT_URL=http://localhost:6334 cargo run -p nudox-indexer
```

Tests require no external infrastructure except the Qdrant integration tests:

```bash
cargo test --workspace
cargo test --workspace --features nudox-search/qdrant-integration  # requires Qdrant on localhost:6334
```

---

## Environment variables

Copy `Backend/.env.example` to `Backend/.env` and fill in values.

| Variable | Default | Used by |
|---|---|---|
| `NUDOX_DATA_DIR` | `.nudox-data` | compiler server, storage root |
| `NUDOX_BIND_ADDR` | `0.0.0.0:3000` | compiler server |
| `NUDOX_BLOB_STORE_ROOT` | `target/nudox-blobs` | indexer demo |
| `NUDOX_TANTIVY_DIR` | `target/nudox-tantivy` | indexer demo |
| `NUDOX_QDRANT_ENDPOINT` | — | compiler server |
| `NUDOX_QDRANT_URL` | — | indexer demo |
| `NUDOX_QDRANT_COLLECTION` | `nudox-embeddings` | indexer demo |
| `NUDOX_QDRANT_DIM` | `256` | indexer demo |
| `NUDOX_EMBED_DIM` | `256` | indexer demo |
| `NUDOX_EMBEDDING_MODEL` | `text-embedding-3-small` | compiler server |
| `OPENAI_API_KEY` | — | `RemoteEmbedder` when pointed at OpenAI |
| `RUST_LOG` | `info,nudox=debug` | tracing filter |

The compiler server also reads `NUDOX_TERMINUS_*` vars for the TerminusDB
connection; the SQLite occurrence store is automatically enabled when those are
set (see **nudox-store** below).

---

## nudox-core

The contract crate. Zero logic, only types and traits.

### ID types

All ID types implement `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`,
`Serialize`, `Deserialize`, and **`Display`** (display uses the inner value
directly, e.g. the UUID string).

| Type | Wraps | Meaning |
|---|---|---|
| `OccurrenceId` | `Uuid` | One occurrence of one symbol at one location |
| `GlobalSymbolId` | `Uuid` | Version-agnostic symbol identity across all libraries |
| `RepoId` | `String` | A repository being indexed |
| `BlobRef` | `String` | Opaque pointer into blob storage |

### Core types

**`BlobInfo`** — the central artifact. Every other artifact in the system
(search index entry, vector point, SQLite row) is a pointer or derived value.
If you can't rebuild it from a `BlobInfo`, the design is wrong.

**`SymbolOrigin`** — `Repo { repo_id }` or `ExternalLib { lib: LibRef }`.
Drives orchestrator routing. Classification (pipeline) and resolution
(orchestrator) are distinct predicates.

**`SourceChunk`** — the extracted code snippet: `raw_code` (the snippet text,
not the full file), `treesitter_repr` (JSON payload from pipeline), and
`symbol_span` (byte offsets *within `raw_code`*, not the original file).

**`EmbeddingRecord`** — one vector with provenance: `model_type`, `model`,
`purpose`, `vector`. A `BlobInfo` typically carries two records per embedder
(Code + Docstring).

**`TreesitterRepr`** — opaque `Vec<u8>` serialized as JSON with shape:
```json
{ "sexp": "...", "snippet_span": [start, end], "references": [...] }
```
The `sexp` is for the extracted snippet only (not the full file). The
`snippet_span` is the byte range within the original `raw_code` input.
References are `{ name, kind, span }` triples.

### Traits

| Trait | Write side | Read side |
|---|---|---|
| `Embedder` | `embed(&SourceChunk, purpose)` | — |
| `BlobStore` | `put`, `update_resolution`, `list` | `get` |
| `GlobalSymbolStore` | `associate` | `lookup` |
| `FutureParseQueue` | `enqueue` | `drain_for_lib` |
| `SearchIndex` | `index` | — |
| `SearchQuery` | — | `search`, `find_by_global_id` |
| `VectorIndex` | `upsert` | — |
| `VectorQuery` | — | `search` |

`SearchQuery` and `VectorQuery` are **deliberately separate traits** from
`SearchIndex` / `VectorIndex`. This lets you hold write-only handles in the
orchestrator and read-only handles in a query server, with no shared mutable
state.

**`BLOB_SCHEMA_VERSION: u32 = 2`** — bump when `BlobInfo` field layout changes.
The `ObjectStoreBlobStore` validates this on every `put` and `get`.

---

## nudox-pipeline

Converts `PipelineInput` → `BlobInfo`.

### PipelineInput

```rust
pub struct PipelineInput {
    pub raw_code:      String,       // source surrounding the symbol
    pub symbol_name:   String,       // as it appeared at the use site
    pub symbol_span:   ByteSpan,     // byte offset of symbol within raw_code
    pub symbol_origin: SymbolOrigin,
    pub metadata:      ChunkMetadata,
    pub docstring:     Option<String>,
}
```

There is no `treesitter_repr` field on `PipelineInput`. The pipeline always
re-parses `raw_code` internally to implement the snippet-boundary policy. Any
pre-existing tree is ignored.

### Snippet-boundary policy (`treesitter.rs`)

`parse_and_extract(raw_code, lang, symbol_span, max_context_lines)`:

1. Parses `raw_code` with the Rust grammar via `arborium-tree-sitter`.
2. Walks up from the symbol's leaf node via `Node::parent()` until a
   `function_item` or `closure_expression` is found.
3. If the enclosing function is ≤ `max_context_lines`, the snippet is that
   function. Otherwise falls back to a centered line window.
4. If no enclosing function exists, uses `raw_code` directly (or the centered
   window if it exceeds the limit).
5. Parses the snippet separately, walks references via `ir::syntax::walk_references`
   using `classify_rust`, and stores the snippet-scoped sexp + references as JSON
   in `TreesitterRepr`.
6. Adjusts `symbol_span` to be relative to the extracted snippet before storing
   in `SourceChunk`.

The `classify_rust` closure mirrors the categorizer in `ir/syntax/tests/common.rs`.
`NudoxPath::Local(PathBuf::from(name))` is used as a placeholder for reference
targets; full resolution happens later in the orchestrator via `GlobalSymbolStore`.

### Pipeline struct

Takes `Vec<Box<dyn Embedder>>` and runs every embedder against every chunk.
Each embedder produces one `EmbeddingRecord` for `Code` purpose, plus one for
`Docstring` if `embed_docstrings` is true and `PipelineInput.docstring` is set.

`max_context_lines` defaults to 80. This is the fallback window size when the
enclosing function is too large.

---

## nudox-embed

Four `Embedder` implementations:

| Type | `ModelType` | Notes |
|---|---|---|
| `MockEmbedder::new(dim)` | `Mock` | Returns `vec![0.1; dim]`. Tests only. |
| `PlaceholderEmbedder::new(model_id, dim)` | `Placeholder` | Deterministic LCG vectors from a hash of the input. Stable and content-derived. |
| `InProcessEmbedder::load(path)` | `InProcess` | Derives model id from path filename. Real inference deferred. |
| `RemoteEmbedder::builder(url, model).api_key(k).build()` | `SelfHosted` / `Openai` | Calls OpenAI-compatible `/v1/embeddings`. Real HTTP via `reqwest`. |

### RemoteEmbedder

```rust
let embedder = RemoteEmbedder::builder(
    Url::parse("https://api.openai.com/v1/embeddings").unwrap(),
    "text-embedding-3-small",
)
.api_key(std::env::var("OPENAI_API_KEY").unwrap())
.model_type(ModelType::Openai)  // default is SelfHosted
.build();
```

For a local Ollama server using the OpenAI-compat endpoint:
```rust
RemoteEmbedder::builder(
    Url::parse("http://localhost:11434/v1/embeddings").unwrap(),
    "nomic-embed-text",
)
.build()
```

---

## nudox-blobstore

### ObjectStoreBlobStore

Local-filesystem backend using `object_store::LocalFileSystem`. Blobs stored at
`{root}/blobs/{uuid}.json`. JSON serialization, schema version validated on both
`put` and `get`.

`update_resolution` is a read-modify-write: reads the blob, sets
`resolved_global_id`, writes it back. There is no locking — concurrent
`update_resolution` calls on the same blob can race. Acceptable in v1 since
resolution is idempotent (always the same `GlobalSymbolId` for a given symbol).

`list()` uses `object_store::ObjectStore::list_with_delimiter` on the `blobs/`
prefix. Returns `Vec<BlobRef>` with order unspecified.

`ObjectStoreBlobStore` implements `Clone` (wraps `Arc<dyn ObjectStore>`).

### InMemoryBlobStore

`HashMap<BlobRef, BlobInfo>` behind a `Mutex`. Used in tests and the orchestrator
demo. Implements `list()` by returning the map's keys.

---

## nudox-search

### TantivySearchIndex

v1 tantivy schema:

| Field | Options | Notes |
|---|---|---|
| `occurrence_id` | `STRING \| STORED` | UUID string |
| `global_id` | `STRING \| STORED` | UUID string, empty when deferred |
| `symbol_name` | `TEXT \| STORED` | tokenized |
| `lib_name` | `TEXT \| STORED` | tokenized |
| `lib_version` | `STORED` | not indexed |
| `repo_id` | `STRING \| STORED` | exact match |
| `blob_ref` | `STORED` | not indexed; looked up by occurrence_id |

**`index()` has upsert semantics.** It calls `writer.delete_term(occurrence_id)` before
adding the new document. This prevents duplicate entries when a deferred blob is
re-indexed after `resolve_lib` back-fills its `global_id`.

Commits on every `index()` call. Performance tuning (batched commits, background
merge policy) is a future concern.

Implements both `SearchIndex` (write) and `SearchQuery` (read):
- `search(query, limit)` — QueryParser over `symbol_name`, `lib_name`, `repo_id`
- `find_by_global_id(gid, limit)` — TermQuery on `global_id` field

### QdrantVectorIndex

One Qdrant point per `EmbeddingRecord`. Point ID is a UUID v5 derived from
`(blob_ref, model_name, purpose)` — re-upserting is idempotent (overwrites in
place, no duplicates).

Payload schema per point:
```
blob_ref, global_id, model_type, model_name, purpose
```

Implements `VectorQuery::search(vector, limit)` via `SearchPointsBuilder`.
Payload values parsed from Qdrant's protobuf `Value` type.

### In-memory stubs

`InMemorySearchIndex` — `HashMap<OccurrenceId, SearchEntry>`. Upsert semantics
(re-indexing updates in place). Implements `SearchQuery` with substring matching.

`InMemoryVectorIndex` — `HashMap<blob_ref_str, (GlobalSymbolId, Vec<EmbeddingRecord>)>`.
Implements `VectorQuery` with cosine similarity computed in-process.

---

## nudox-store

The canonical link between TerminusDB document identities and the occurrence
system. **This is the only correct `GlobalSymbolStore` and `FutureParseQueue`
for production.** The in-memory stubs in `nudox-orchestrator` are for tests only.

### GlobalSymbolId derivation

IDs are deterministic UUID v5 values:

```
GlobalSymbolId = UUID_v5(NUDOX_SYMBOL_NS, "{terminus_instance}\0{entry_uri}")
```

where:
- `NUDOX_SYMBOL_NS` = `6e756478-2073-796d-626f-6c2d6e730001` (fixed constant in the crate)
- `terminus_instance` = `"{org}/{db}"` from `TerminusConfig`
- `entry_uri` = TerminusDB document `@id`, e.g. `"Entry/rust/serde/Serialize"`

This is **stable and computable offline**. Any caller who knows both inputs can
derive the UUID without a database round-trip. The `symbol_id(instance, uri)`
function is public for this purpose.

```rust
use nudox_store::symbol_id;
let gid = symbol_id("nudox_org/nudox_lib", "Entry/rust/serde/Serialize");
// Same call will always produce the same UUID.
```

### SQLite schema

Three tables (created inline by `NudoxStore::ensure_schema` — no migration file):

**`global_symbols`** — primary registry. Unique on `(terminus_instance, entry_uri)`.
Indexed by `(lib_name, lib_version, symbol_name)` for O(log n) `lookup()`.

**`occurrence_associations`** — append-only log of `(global_symbol_id, occurrence_id)`
pairs. Written by `GlobalSymbolStore::associate()`. References `global_symbols(id)`
with `ON DELETE CASCADE`.

**`deferred_queue`** — `(blob_ref, lib_name, lib_version)` triples. Primary key
prevents duplicates. Indexed by `(lib_name, lib_version)` for drain. `drain_for_lib`
selects and deletes in one transaction (atomic drain).

### Opening the store

```rust
let store = NudoxStore::open(
    Path::new("/var/nudox/nudox-links.db"),
    "nudox_org/nudox_lib",   // terminus_instance = "{org}/{db}"
).await?;

// Pass the same Arc for both orchestrator arguments:
let orchestrator = Orchestrator::new(
    Arc::clone(&store) as Arc<dyn GlobalSymbolStore>,
    blob_store,
    Arc::clone(&store) as Arc<dyn FutureParseQueue>,
    search,
    vector,
);
```

### Registering a parsed library

Called by the compiler after every successful `run_rust_pipeline` /
`run_typescript_pipeline`. The DocStore keys starting with `"Entry/"` are the
entry URIs:

```rust
// In ingest.rs, finalize_pipeline():
let entry_uris: Vec<&str> = doc_store.docs.keys()
    .filter(|k| k.starts_with("Entry/"))
    .map(|k| k.as_str())
    .collect();
store.register_library(language, lib_name, &version.to_string(), entry_uris).await?;
```

`symbol_name` is extracted as the last `/`-delimited component of the entry URI,
e.g. `"Serialize"` from `"Entry/rust/serde/ser/Serialize"`. This matches what
occurrence callers provide in `LibRef` lookups.

### Introspection

```rust
store.symbol_count("serde", "1.0.0").await?     // how many symbols registered
store.deferred_count("tokio", "1.0.0").await?   // how many blobs deferred
store.resolve_symbol("serde", "1.0.0", "Serialize").await?
// → Some((GlobalSymbolId, "Entry/rust/serde/Serialize"))
```

---

## nudox-orchestrator

Wires the five backends into two flows.

### ingest

Always stores to the blob store first, then routes by `symbol_origin`:

- **`Repo`** — fresh UUID v4 `GlobalSymbolId`, immediate `associate` +
  `update_resolution` + `index` + `upsert`, returns `Resolved`.
- **`ExternalLib`, `lookup` returns `Some`** — use existing id, same
  resolution steps, returns `Resolved`.
- **`ExternalLib`, `lookup` returns `None`** — enqueue and return `Deferred`.
  No indexing until `resolve_lib` runs.

`associate()` is called on every resolution so `GlobalSymbolStore` tracks the
`(GlobalSymbolId → OccurrenceId)` mapping.

### resolve_lib

Drains the deferred queue for a library atomically, then for each `BlobRef`
attempts resolution. Calls `associate` + `update_resolution` + `index` + `upsert`
on success.

### rebuild_indexes

```rust
let report = orchestrator.rebuild_indexes().await?;
```

Walks `blob_store.list()` and re-indexes every blob with a resolved
`GlobalSymbolId`. Deferred blobs are skipped. Use this after a schema change or
index corruption.

### In-memory stubs

Available under `features = ["test-stubs"]`:

```toml
nudox-orchestrator = { path = "../nudox-orchestrator", features = ["test-stubs"] }
```

`InMemoryGlobalSymbolStore::insert(&LibRef, symbol_name, GlobalSymbolId)` —
pre-populates a mapping.

`InMemoryGlobalSymbolStore::occurrences_for(GlobalSymbolId) -> Vec<OccurrenceId>` —
verifies that `associate()` was called.

`InMemoryFutureParseQueue::peek_for_lib(&LibRef)` — inspects queue without draining.

---

## Integration with the compiler

The compiler (`compiler/src/`) was extended to register symbols after every
library parse and to create the SQLite store at startup.

### Startup (`server.rs`)

```rust
let nudox_store = if let Some(ref terminus) = config.pipeline.terminus {
    let db_path = config.storage_root.join("nudox-links.db");
    let instance = format!("{}/{}", terminus.org, terminus.db);
    NudoxStore::open(&db_path, instance).await.ok()
} else {
    None
};

let registry = LocalRegistry::new(storage, interval, config.pipeline.clone())
    .with_nudox_store(store.unwrap()); // optional; sync works without it
```

The SQLite file lands at `{NUDOX_DATA_DIR}/nudox-links.db`.

### Symbol registration (`ingest.rs`)

`finalize_pipeline` calls `store.register_library(...)` after `emit_store`
produces the `DocStore`. This is the only change to the existing pipeline; all
other behavior (TerminusDB upload, Qdrant vector upload) is unchanged.

Registration is **non-fatal** — a failure logs a warning and the pipeline
continues. The occurrence system will leave blobs deferred until the next
successful ingestion of the library.

### Occurrence ingestion endpoint

Add to `compiler/src/api.rs`:

```
POST /api/occurrences
```

Body:
```json
{
  "raw_code": "fn local_helper() -> usize { 42 }",
  "symbol_name": "local_helper",
  "symbol_offset": 3,
  "file_path": "src/lib.rs",
  "repo_id": "my-repo",
  "lib_name": null,        // null → Repo-local
  "lib_version": null,
  "docstring": "Returns the answer."
}
```

This endpoint is not yet wired in `api.rs` — the routing and handler code is in
the integration instructions in the session log. The `Pipeline` and `Orchestrator`
instances are stored on `AppState`.

---

## Test inventory

| Crate | Tests | Notes |
|---|---|---|
| `nudox-core` | 11 | Serialization, ID types |
| `nudox-embed` | 8 | Vector properties, HTTP builder |
| `nudox-blobstore` | 35 | Round-trip, update_resolution, list, schema version rejection |
| `nudox-search` | 19 | Tantivy: 11 (including upsert/no-dup test, SearchQuery); Memory: 5 (incl. VectorQuery cosine sim) |
| `nudox-pipeline` | 9 | Embedding count, snippet extraction, sexp in payload |
| `nudox-orchestrator` | 2 | End-to-end ingest+resolve smoke test, rebuild_indexes |
| `nudox-store` | 13 + 2 doc-tests | UUID stability, register idempotency, lookup scoping, drain atomicity |

Total: **98 passing tests**, 0 warnings.

The 5 `nudox` (compiler) integration tests (`rust_registry_results_snapshot`,
`npm_registry_results_snapshot`, etc.) require live network access to crates.io
and npm and are expected to fail in offline CI.

---

## What is not yet implemented

| Item | Notes |
|---|---|
| `POST /api/occurrences` handler | Routing code written in session notes; needs wiring into `api.rs` and `AppState` |
| `InProcessEmbedder` real inference | Placeholder vectors only; swap in a candle/ort impl when ready |
| S3 blob store | `object_store` supports it; add an `s3` feature flag to `nudox-blobstore` |
| SQLite `GlobalSymbolStore` for Repo-local | Currently Repo-local symbols get ephemeral UUID v4 ids. A persistent SQLite table keyed by `(repo_id, symbol_path)` would make ids stable across restarts |
| Re-embedding on schema change | A `nudox-rebuilder` binary that walks `blob_store.list()` and re-runs the pipeline against existing `SourceChunk.raw_code` values |
| Language support beyond Rust | `nudox-core::Language` has only `Rust`; `arborium::get_language` supports more; `classify_rust` needs a parallel `classify_typescript` etc. |
