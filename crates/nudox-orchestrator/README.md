# nudox-orchestrator

Coordination layer for nudox-occurrences. Wires together the blob store, global symbol store, deferred queue, search index, and vector index, and implements the two core flows: ingest and resolve-lib.

## Orchestrator

`Orchestrator` takes five `Arc<dyn Trait>` arguments, one for each backend:

```rust
use std::sync::Arc;
use nudox_orchestrator::Orchestrator;

let orchestrator = Orchestrator::new(
    Arc::new(global_store),   // Arc<dyn GlobalSymbolStore>
    Arc::new(blob_store),     // Arc<dyn BlobStore>
    Arc::new(queue),          // Arc<dyn FutureParseQueue>
    Arc::new(search),         // Arc<dyn SearchIndex>
    Arc::new(vector),         // Arc<dyn VectorIndex>
);
```

## ingest

```rust
let outcome: ResolutionOutcome = orchestrator.ingest(blob_info).await?;
```

Ingest always stores the blob first, then routes based on `symbol_origin`:

- **`Repo` origin** — assigns a fresh `GlobalSymbolId` (UUID v4), updates the blob's `resolved_global_id`, indexes in search and vector, returns `Resolved`.
- **`ExternalLib` origin, `global_store` returns `Some`** — uses the existing `GlobalSymbolId`, updates the blob, indexes in search and vector, returns `Resolved`.
- **`ExternalLib` origin, `global_store` returns `None`** — enqueues `(lib, blob_ref)` in the deferred queue, returns `Deferred`.

## resolve_lib

```rust
let report: ResolveLibReport = orchestrator.resolve_lib(&lib_ref).await?;
```

Drains the deferred queue for the given library, then for each queued `BlobRef`:

1. Retrieves the `BlobInfo` from the blob store.
2. Looks up the symbol in `global_store`. If found, calls `update_resolution`, then indexes in search and vector. If not found, increments `blobs_skipped`.

Returns a `ResolveLibReport` with `blobs_seen`, `blobs_resolved`, and `blobs_skipped` counts.

## Data flow summary

```
ingest(BlobInfo)
  blob_store.put()                         -- always first
  if Repo:
    fresh global_id
    blob_store.update_resolution()
    search.index() + vector.upsert()
    return Resolved
  if ExternalLib:
    global_store.lookup()
    if Some(global_id):
      blob_store.update_resolution()
      search.index() + vector.upsert()
      return Resolved
    if None:
      queue.enqueue()
      return Deferred

resolve_lib(lib)
  queue.drain_for_lib()
  for each blob_ref:
    blob_store.get()
    global_store.lookup()
    if Some(global_id):
      blob_store.update_resolution()
      search.index() + vector.upsert()
      blobs_resolved++
    else:
      blobs_skipped++
  return ResolveLibReport
```

## In-memory stubs

`InMemoryGlobalSymbolStore` and `InMemoryFutureParseQueue` are available under `features = ["test-stubs"]` (or when compiled with `cfg(test)`). They are used in the orchestrator's own smoke test and in `nudox-indexer`.

```toml
nudox-orchestrator = { path = "../nudox-orchestrator", features = ["test-stubs"] }
```

`InMemoryGlobalSymbolStore` exposes an `insert(&LibRef, symbol_name, GlobalSymbolId)` method for pre-populating mappings in tests.

`InMemoryFutureParseQueue` exposes a `peek_for_lib(&LibRef)` method for inspecting queue contents without draining.

## Tracing

Every public async method emits an `info`-level `tracing` span. The `ingest` span carries `symbol`. The `resolve_lib` span carries `lib`. Internal helpers carry `blob_ref`, `global_id`, and `occurrence_id` fields.
