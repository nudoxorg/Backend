# nudox-indexer

Binary entry point and end-to-end demo for nudox-occurrences. It wires every crate in the workspace together and runs three symbols through the full pipeline to demonstrate ingest, deferred resolution, and back-fill.

## Running the demo

```bash
cargo run -p nudox-indexer
```

Output is written to `target/demo-output/<timestamp>/` by default. Pass a custom directory as the first argument:

```bash
cargo run -p nudox-indexer -- ./my-demo-output
```

## What the demo does

1. Creates a `Pipeline` with a `PlaceholderEmbedder` (dim=8).
2. Creates an `ObjectStoreBlobStore` rooted at the output directory.
3. Creates in-memory stubs for global store, deferred queue, search index, and vector index.
4. Pre-populates `serde::Serialize` in the global store.
5. Processes three `PipelineInput`s through the pipeline:
   - `local_helper` — repo-local symbol in `demo-repo`
   - `Serialize` — external lib symbol from `serde 1.0` (resolvable immediately)
   - `spawn` — external lib symbol from `tokio 1.0` (deferred on first ingest)
6. Ingests all three through the orchestrator, printing each outcome.
7. Simulates `tokio` being parsed by inserting `spawn` into the global store.
8. Calls `resolve_lib(&tokio_lib)` and prints the `ResolveLibReport`.
9. Prints the search index entries and vector index state.
10. Lists all blob JSON files written to disk with their symbol name, resolved global id, and embedding count.

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `NUDOX_BLOB_STORE_ROOT` | `target/nudox-blobs` | Root directory for the local-filesystem blob store |
| `NUDOX_TANTIVY_DIR` | `target/nudox-tantivy` | Directory for the tantivy full-text index |
| `NUDOX_QDRANT_URL` | (none) | Qdrant gRPC URL; if unset uses `InMemoryVectorIndex` |
| `NUDOX_QDRANT_COLLECTION` | `nudox-embeddings` | Qdrant collection name |
| `NUDOX_QDRANT_DIM` | `256` | Vector dimension for Qdrant collection creation |
| `NUDOX_EMBED_DIM` | `256` | Dimension passed to the placeholder embedder |

## Backend selection

| Backend | Controlled by | Default |
|---|---|---|
| Blob store | `NUDOX_BLOB_STORE_ROOT` | `ObjectStoreBlobStore` at `target/nudox-blobs` |
| Search index | `NUDOX_TANTIVY_DIR` | `InMemorySearchIndex` (current demo hardcodes in-memory) |
| Vector index | `NUDOX_QDRANT_URL` | `InMemoryVectorIndex` if unset |
| Embedder dimension | `NUDOX_EMBED_DIM` | `256` |

The current demo binary hardcodes the in-memory search and vector indexes. Setting `NUDOX_QDRANT_URL` is wired up for future expansion; for now it documents intent.

## Inspecting output

After the demo runs, blob JSON files are in `<output-dir>/blobs/`. Each file is a serialized `BlobInfo`:

```bash
# list blobs
ls target/demo-output/*/blobs/

# inspect a blob
cat target/demo-output/<timestamp>/blobs/<uuid>.json | python3 -m json.tool
```

Key fields to look at: `symbol_name`, `symbol_origin`, `resolved_global_id`, `embeddings`, `metadata.blob_schema_version`.
