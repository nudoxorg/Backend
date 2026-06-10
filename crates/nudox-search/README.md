# nudox-search

Search and vector indexing backends for nudox-occurrences. Implements the `SearchIndex` and `VectorIndex` traits from `nudox-core`, plus in-memory stubs for tests.

## TantivySearchIndex

Full-text and exact-match search over `BlobInfo` fields. Uses an `MmapDirectory` for durable on-disk storage.

Constructor:

```rust
let index = TantivySearchIndex::open_or_create(Path::new("target/nudox-tantivy"))?;
```

Creates the directory if absent; reopens an existing index if present. The v1 schema has seven fields:

| Field | Type | Indexed as |
|---|---|---|
| `occurrence_id` | string | exact (STRING) |
| `global_id` | string | exact (STRING) |
| `symbol_name` | text | full-text (TEXT) |
| `lib_name` | text | full-text (TEXT) |
| `lib_version` | string | stored only |
| `repo_id` | string | exact (STRING) |
| `blob_ref` | string | stored only |

All fields are STORED so documents can be retrieved after a search. Commits happen on every `index()` call; batched commits and background merge policy are deferred optimizations.

## QdrantVectorIndex

Vector similarity search via Qdrant gRPC (default port 6334). Each `EmbeddingRecord` is upserted as one Qdrant point.

Constructor:

```rust
let index = QdrantVectorIndex::connect("http://localhost:6334", "nudox-embeddings".into()).await?;
index.ensure_collection(256).await?;
```

`ensure_collection` creates the collection with cosine distance if it does not exist; it is idempotent.

Point IDs are UUID v5 values derived deterministically from `(blob_ref, model_name, purpose)`. This means re-upserting the same `(blob_ref, model, purpose)` tuple overwrites the existing point rather than creating a duplicate. Each point payload carries: `blob_ref`, `global_id`, `model_type`, `model_name`, `purpose`.

## In-memory stubs

`InMemorySearchIndex` and `InMemoryVectorIndex` are always available (not gated behind a feature flag). They are intended for unit tests and the orchestrator smoke test.

`InMemorySearchIndex` exposes an `entries()` method that returns all indexed entries for test assertions.

`InMemoryVectorIndex` exposes a `get(&BlobRef)` method that returns the `GlobalSymbolId` associated with a blob for test assertions.

## Qdrant integration tests

Integration tests are gated behind the `qdrant-integration` feature and require a Qdrant instance at `localhost:6334`:

```bash
# start Qdrant
sudo docker run -d --name nudox-qdrant -p 6333:6333 -p 6334:6334 \
  -v $HOME/.qdrant-nudox:/qdrant/storage qdrant/qdrant:latest

# run integration tests
cargo test -p nudox-search --features qdrant-integration
```

Each test creates a uniquely-named collection (timestamped) and deletes it on teardown to avoid cross-test interference.
