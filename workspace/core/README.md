# nudox-core

Shared types, traits, and error types for the nudox-occurrences workspace. This crate contains no business logic — it defines the contract that every other crate implements or consumes. It has no dependencies on other nudox-* crates.

## Key types

**`BlobInfo`** — the central artifact. One record per symbol occurrence, containing the occurrence id, symbol name, origin, optional resolved global id, source chunk, embedding records, and chunk metadata. This is the system of record; search and vector indexes hold only pointers to it.

**`SymbolOrigin`** — either `Repo { repo_id }` for a symbol found in an indexed repository, or `ExternalLib { lib: LibRef }` for a dependency. The orchestrator uses this to decide whether resolution is immediate or deferred.

**`EmbeddingRecord`** — a single embedding vector with its model provenance: `model_type: ModelType`, `model: String`, `purpose: EmbeddingPurpose`, and `vector: Vec<f32>`.

**`ModelType`** — discriminates the embedding backend: `Placeholder`, `Mock`, `InProcess`, `Openai`, `SelfHosted`, `Other(String)`.

**`ChunkMetadata`** — context about where a chunk was extracted: repo id, file path, byte span, parse timestamp, language, optional language version, and `blob_schema_version`.

**`ResolutionOutcome`** — returned by the orchestrator after ingest. Either `Resolved { global_id, blob_ref }` or `Deferred { lib, blob_ref }`.

**`ResolveLibReport`** — summary statistics from a `resolve_lib` pass: `blobs_seen`, `blobs_resolved`, `blobs_skipped`.

**`LibRef`** — identifies an external library by `name` and `version`.

**ID newtypes** — `OccurrenceId`, `GlobalSymbolId`, `RepoId`, `BlobRef`. All are UUID- or string-backed newtypes with `Serialize`/`Deserialize`.

## Key traits

| Trait | Purpose |
|---|---|
| `Embedder` | Computes embedding vectors for source chunks |
| `BlobStore` | Persists and retrieves `BlobInfo` records |
| `GlobalSymbolStore` | Maps library symbols to `GlobalSymbolId` values |
| `FutureParseQueue` | Holds deferred `BlobRef`s awaiting library parse |
| `SearchIndex` | Full-text search over `BlobInfo` fields |
| `VectorIndex` | Vector similarity index over `EmbeddingRecord` vectors |

All traits use `async_trait`. All trait methods return `nudox_core::Result<T>`.

## Schema versioning

`BLOB_SCHEMA_VERSION: u32 = 2` is the current version of the `BlobInfo` serialization format. Every `BlobInfo` carries this value in `metadata.blob_schema_version`. The blob store validates the version on both `put` and `get`, rejecting mismatches. Bump this constant whenever the `BlobInfo` field layout changes.

Current history:
- v1: initial schema
- v2: `EmbeddingRecord` gained `model_type: ModelType`
