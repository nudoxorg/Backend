# nudox-blobstore

Implementations of the `BlobStore` trait from `nudox-core`. The blob store is the system of record: every `BlobInfo` is written here first, and all other indexes (search, vector) hold only references to blob store entries.

## Implementations

**`ObjectStoreBlobStore`** — the production backend, backed by the `object_store` crate. Currently only the local filesystem is wired up. An S3-backed constructor is intended to land later behind a feature flag.

Constructor:

```rust
let store = ObjectStoreBlobStore::local(PathBuf::from("target/nudox-blobs"))?;
```

This creates the directory if it does not already exist.

**`InMemoryBlobStore`** — an in-memory store for use in tests. Thread-safe via `Arc<Mutex<HashMap>>`.

```rust
let store = InMemoryBlobStore::new();
```

## Blob path convention

Every blob is stored as JSON at `blobs/{uuid}.json` relative to the store root. The UUID is freshly generated at `put` time; callers receive a `BlobRef` containing this UUID and use it for all subsequent `get` and `update_resolution` calls.

## Schema version validation

`put` and `get` both check `info.metadata.blob_schema_version` against `BLOB_SCHEMA_VERSION`. Any mismatch is returned as a `BlobStore` error. This prevents silently reading stale blobs after a schema bump.

## BlobStore trait operations

- `put(&BlobInfo) -> BlobRef` — serialize and store; fails on schema version mismatch
- `get(&BlobRef) -> BlobInfo` — retrieve and deserialize; fails on schema version mismatch or missing blob
- `update_resolution(&BlobRef, GlobalSymbolId)` — read, set `resolved_global_id`, write back

All methods are instrumented with `tracing` spans carrying `occurrence_id`, `blob_ref`, and `global_id` fields.

## Note on S3

The S3 backend is deferred. When it lands it will be a second constructor on `ObjectStoreBlobStore` (or a sibling type) behind a feature flag, using the same `object_store` API surface.
