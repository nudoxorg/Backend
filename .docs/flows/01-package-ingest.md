# Flow: package ingest

**Entry point:** `workspace/driver/http/handlers/indexing.rs:30` — `add_package`
**Terminates at:** the derived stores — Qdrant (vector sink) and the
replica-local package tantivy index (text sink)
**Crosses:** HTTP → the DoltLite catalog → an upstream registry over the network
→ a SmolvmCage microVM (a process and VM boundary) → the object store → the
outbox → Qdrant/tantivy

## Why this flow matters

This is the system's spine. Everything a query can possibly answer got there
through this path. It is also the only path in the repo that spans every store:
the catalog holds the lifecycle state and the job queue, the object store holds
the content-addressed blob, and the outbox is the transactional handoff that
keeps the derived stores from silently diverging from the blob. If ingest
breaks, nothing downstream has anything to serve; if ingest is *subtly* wrong —
a phase that advances before its work commits — the derived stores drift and no
query surface can tell you.

## The path

The request half is short: it validates, records, and enqueues. Everything
after step 5 happens on a background worker.

1. **`workspace/driver/http/handlers/indexing.rs:30` — `add_package`**
   Deserializes `Json<AddPackageDto>`, mints a `WriteCap` via
   `authorize_write(&principal, "packages.ensure_initialized")` (allow-all
   today, see [api-contract.md](../api-contract.md#authentication)).
   → `AddPackageDto`

2. **`workspace/driver/http/dto.rs:39` — `AddPackageDto::into_coordinates`**
   Validates the package name against its ecosystem's grammar
   (`PackageName::new`), parses the version for that ecosystem, and resolves
   the origin — either the ecosystem default (`dto.rs:55`) or a named custom
   registry from the process-wide `CUSTOM_REGISTRIES` table (`dto.rs:87`).
   A bad name, version, or unknown origin is a typed 400 here, before any
   store is touched. → `PackageCoordinates`

3. **`workspace/driver/coordination/initialization.rs:94` — `Server::ensure_initialized`**
   Derives the deterministic `PackageId` from the coordinates
   (`coordinates.id()`), reads the current `ResolutionState` from
   `global_store`, and runs the decision table. → `InitializationDecision`

4. **`workspace/driver/coordination/initialization.rs:40` — `initialization_decision`**
   The pure policy, testable without a database:

   | Current state | Freshness | Decision |
   | --- | --- | --- |
   | absent, or `Unindexed` | — | `Enqueue` |
   | `Progressing(_)` | — | `AlreadyInFlight` |
   | `Stored { hash }` | `Stale` | `Enqueue` |
   | `Stored { hash }` | `Fresh` or unknown | `Serve` |
   | `Failed(_)` | — | `Enqueue` |
   | `DeadLettered(_)` | — | `Hold` — a human owns it; never auto-requeue |

   The `POST /packages` path always passes `freshness = None`, so a stored
   package is treated as fresh (`initialization.rs:110`). Recomputing freshness
   means re-acquiring the source, which is `POST /packages/:id/sync`'s job.

5. **`workspace/driver/coordination/initialization.rs:120-122`** — on `Enqueue`
   only: upsert a provisional `GlobalPackage` (identity + `Unindexed { needed:
   true }` + a *provisional* toolchain that is provenance, never identity), then
   `queue.enqueue(package)`. Both writes are idempotent — an identity upsert and
   a unique-live-job-per-package enqueue — so the pair converges even if the
   process dies between them. The handler answers `Initialized { package,
   state, enqueued }` and the request is over.

--- background from here ---

6. **`workspace/driver/poll.rs:46` — `queue_worker`**
   Spawned by `serve()` (`workspace/driver/lib.rs:527`) only when
   `role.runs_forge()`. A supervisor: restarts `run_worker_until` with a 5 s
   backoff on error (`poll.rs:28`), and returns cleanly — without restarting —
   when the drain token fires.

7. **`workspace/driver/coordination/indexing.rs:482` — `Indexer::run_worker_until`**
   Per tick, for each federated source in precedence order:
   `reclaim_expired_leases()` (logs a warning when it reclaims anything), then
   `dequeue_batch(max_inflight, lease)`, then drives the batch with
   `for_each_concurrent(max_inflight, …)`. Sleeps `limits.poll_interval` (2 s)
   between ticks. → `Vec<LeasedJob>`

8. **`workspace/driver/coordination/indexing.rs:525` — `drive_job`**
   Races three things with `tokio::select! { biased; … }`: the job under a
   `job_deadline` timeout (600 s), and a lease heartbeat loop. Either the job
   finishing or the heartbeat *returning* ends the select — the heartbeat only
   returns on `QueueError::LeaseLost` (`indexing.rs:602`), which means another
   worker took the job. Success settles the queue entry with
   `ResolutionState::Stored { hash }`; failure goes to `handle_job_failure`.

9. **`workspace/driver/coordination/indexing.rs:90` — `run_indexing_job_on`**
   The four phases, in order, each `advance()`-ing the durable state first.

10. **`workspace/driver/coordination/indexing.rs:118` — `execute_acquire_phase`**
    `advance(Progressing(Acquiring))`, then read the coordinates back out of
    `global_store`. → `PackageCoordinates`

11. **`workspace/driver/coordination/indexing.rs:133` — `execute_extract_phase`**
    `fetch_archive` (`indexing.rs:347`) downloads the source archive from the
    resolved upstream URL (`archive_url` at `:368`, with a PyPI-specific sdist
    resolution at `:431`), then `advance(Progressing(Extracting))`, then
    `ingest_archive(package, toolchain, cursor, archive_format,
    ExtractionLimits::DEFAULT, EntryAllowlist::SAFE)` — the untrusted-archive
    sanitizer. → `BlobBuilder` holding the staged source files

12. **`workspace/driver/coordination/indexing.rs:179` — `execute_compile_phase`**
    `advance(Progressing(Compiling))`, then:
    - materialize the staged sources onto a `TempDir` with `src/` and
      `scratch/` subtrees (`materialize_sources`, `indexing.rs:858`). Both die
      with the scope; nothing survives the job on the host. Path components
      that are absolute or contain `..` are skipped defensively.
    - resolve the toolchain image digest: a real `ToolchainImageStore` lookup
      when one was attached via `with_toolchain_images` (`indexing.rs:75`),
      otherwise a deterministic per-language placeholder
      (`toolchain_image_digest_placeholder`, `indexing.rs:847`). **The
      placeholder is the default** — `Indexer::new` sets `toolchain_images:
      None` (`indexing.rs:65`), and nothing in `driver` calls
      `with_toolchain_images`.
    - run the cage on `spawn_blocking` (`indexing.rs:221`).
    → `Vec<u8>` of IR bytes, then `Vec<String>` of symbol identifiers

13. **`workspace/driver/coordination/indexing.rs:892` — `run_producer_in_cage`** (synchronous)
    `RootfsStore::from_env()` (reads `NUDOX_GUEST_ROOTFS`) → `SmolvmRuntime` →
    `prepare_golden(&image)` (idempotent) → `fork_golden(&golden)` for a warm
    clone. `VmError::Unsupported` is the **one** tolerated failure: it logs and
    cold-boots a fresh cage instead (`indexing.rs:927-934`). Any other error
    fails the job.

    The capability budget (`indexing.rs:943-949`): `FsGrant::scratch(scratch)
    .ro(source)`, `NetGrant::Off`, `Env::empty()`, and the language's
    `ProducerProfile` limits. There are **no host-side toolchain binds** — the
    toolchain lives inside the golden guest image.

    The command is `producer_command(language)` (`indexing.rs:719`), a fixed
    per-language contract: a guest binary path plus argv. Every producer takes
    `--source /mnt/ro0 --emit ndirf1` (`GUEST_SOURCE_MOUNT`, `indexing.rs:679`)
    and streams NdIrF1 frames on stdout.

    | Language | Guest entrypoint | Extra argv |
    | --- | --- | --- |
    | Rust | `/opt/nudox/rust/bin/nudox-rust-producer` | `--edition 2021` |
    | Java | `/opt/nudox/java/bin/nudox-java-producer` | `--target-jdk 21` |
    | Go | `/opt/nudox/go/bin/nudox-go-producer` | `--module-root /mnt/ro0` |
    | C# | `/opt/nudox/dotnet/bin/nudox-csharp-producer` | `--target-tfm net9.0` |
    | Nix | `/opt/nudox/nix/bin/nudox-nix-producer` | `--flake` |
    | TypeScript | `/opt/nudox/ts/bin/nudox-ts-producer` | — |
    | Python | `/opt/nudox/python/bin/nudox-python-producer` | — |
    | C/C++ | `/opt/nudox/cpp/bin/nudox-cpp-producer` | — |

    On the warm path the clone is `kill`ed immediately after `exec`
    (`indexing.rs:969`) — one VM per job. A non-zero exit becomes
    `InternalError::ProducerFailed` carrying the first 2000 chars of stderr.

    **These guest binaries are not built from `crates/nudox-producer-*`.** They
    are provisioned inside the golden OCI images, whose content is a deployment
    concern (`indexing.rs:685-687`). The server plane does not link the producer
    crates at all — see [flows/02](02-producer-to-ir.md).

14. **`workspace/driver/coordination/indexing.rs:1047` — `ingest_ir_bytes`**
    Decodes the producer's postcard-framed stream (`[u32 LE length][postcard
    bytes]`) with `ir_vcs::protocol::StreamReceiver`. A `Hello` frame carries
    the job key and producer id; a missing or malformed one means the producer
    wrote nothing usable. `Symbols` frames become the IR blob section
    (`builder.set_ir`) and the returned identifier list; `Bodies` frames are
    lowered into the blob's `ReferenceSet` (`builder.set_references`,
    `build_reference_set_from_bodies` at `:1197`), keeping only oracle-resolved
    references at `Confidence >= Index`. `Occurrences` frames are opaque and
    are not decoded here.

    An `Abort` frame, or any framing/version decode error, is **non-fatal**:
    the sections are attached with whatever arrived and an empty identifier
    list is returned (`indexing.rs:1041-1046`, `attach_empty_ir_sections` at
    `:1292`). A package can therefore reach `Stored` with an empty IR section.

15. **`workspace/driver/coordination/indexing.rs:252` — `execute_emit_phase`**
    `advance(Progressing(Emitting))`, then `builder.finalize()` → `(manifest,
    sections)`; snapshot hash = `ContentHash::of_bytes(manifest.identity_bytes())`.
    Fetches listing signals (`:1312`) and download counts (`:1396`) — both
    **non-fatal**, logged and skipped on failure. Re-runs squat detection after
    downloads land so dead-stub detection sees volume.

16. **`workspace/index/blob/emit.rs:44` — `registry::blob::emit::emit`**
    (a) every section lands in `cas/` idempotently (`put_section` returns
    written-vs-deduped); (b) the manifest becomes its own `cas/` object and the
    package pointer is repointed at it; (c) `outbox.append(package, generation,
    &SinkKind::iter().collect())` — one intent per derived sink, in the same
    catalog transaction family as the pointer move. → `Emitted { written,
    deduped }`

17. **`indexing.rs:306` — `outbox.record_stored(global_store, package, snapshot, facets)`**
    Records the `Stored { hash }` transition and the extracted facets.

18. **`workspace/driver/poll.rs:73` — `outbox_consumer`**
    One task per `SinkKind` variant, spawned from `serve()`
    (`workspace/driver/lib.rs:537`) when `role.runs_gateway()`. Per tick, per
    source: take the sink lock, `consume_once`, release.

19. **`workspace/driver/poll.rs:120` — `consume_once`**
    `read_watermark(sink)` → `read_since(sink, watermark, 64)` → for each
    entry: `materialize` **then** `advance_watermark`. The order is the whole
    safety argument: a crash between the two re-delivers the intent rather than
    dropping it, and every sink write is idempotent.

20. **`workspace/driver/poll.rs:167` — `materialize`**
    Dispatches on `entry.op` (`Upsert` / `Delete`) and then on `entry.kind`.
    Terminates in `materialize_vector` (`poll.rs:240`) for the vector sink, or
    in nothing at all for the other two — see
    [What is a stub](#what-is-a-stub).

## The phase state machine

```
                   POST /packages
                        │
                        ▼
              ┌──────────────────────┐
              │ Unindexed { needed } │◀── provisional record, upsert-idempotent
              └──────────┬───────────┘
                         │ queue.enqueue
                         ▼
              ┌──────────────────────┐
   ┌─────────▶│ Progressing(Acquiring)│  read coordinates from catalog
   │          └──────────┬───────────┘
   │                     ▼
   │          ┌──────────────────────┐
   │          │ Progressing(Extracting)│ download + sanitize archive
   │          └──────────┬───────────┘
   │                     ▼
   │          ┌──────────────────────┐
   │          │ Progressing(Compiling)│ SmolvmCage → NdIrF1 → BlobBuilder
   │          └──────────┬───────────┘
   │                     ▼
   │          ┌──────────────────────┐
   │          │ Progressing(Emitting) │ finalize → cas/ → outbox
   │          └──────────┬───────────┘
   │                     ▼
   │          ┌──────────────────────┐
   │          │  Stored { hash }     │──────┐
   │          └──────────────────────┘      │
   │                                        │  content hash differs on
   │          ┌──────────────────────┐      │  POST /packages/:id/sync
   └──────────│  Failed(Failure)     │◀─────┘
   retry      └──────────┬───────────┘
   (≤ 5)                 │ attempts exhausted
                         ▼
              ┌──────────────────────┐
              │ DeadLettered(Failure)│  terminal: a human owns it
              └──────────────────────┘
```

The outbox fan-out, after `Stored`:

```
     emit::emit
         │
         └─▶ outbox.append(package, generation, [Text, Vector, Graph])
                    │
      ┌─────────────┼──────────────┐
      ▼             ▼              ▼
  SinkKind::Text  SinkKind::Vector  SinkKind::Graph
      │             │              │
      │             │              └─▶ no-op (poll.rs:199)
      │             │
      │             └─▶ global_store.symbols_for(package)
      │                    → embed each symbol (cache-first)
      │                    → qdrant upsert by PointId::from_symbol
      │
      └─▶ nothing in the consumer (poll.rs:190); the watermark advances only.
          The replica-local package tantivy index is fed independently by
          package_index_poller (poll.rs:395), which reads the SAME Text-sink
          outbox rows under its OWN watermark.
```

## Two enums are both called `SinkKind`

This will bite anyone reading `poll.rs`. There are two distinct types:

| Type | Variants | Role |
| --- | --- | --- |
| `index::coordination::SinkKind` — a **re-export of `heart::DerivedStore`** (`workspace/index/coordination/outbox.rs:78`; `heart::DerivedStore` at `workspace/heart/sink.rs:37`) | `Vector`, `Graph`, `Text` | the serving-side vocabulary. This is what `driver` matches on. |
| `index::enums::SinkKind` (`workspace/index/enums.rs:180`) | `Text`, `Vector`, `UsageIndex` | the catalog `TEXT` column codec for `outbox.sink_kind` / `sink_watermarks.sink_kind` |

They are bridged by `catalog_sink` / `serving_sink`
(`workspace/index/coordination/outbox.rs:84-97`), and the mapping is
`Graph ↔ UsageIndex`. So `Graph` is a real, persisted sink whose stored token is
`"usage_index"` — not a phantom variant.

## State transitions and idempotency

Every step on this path is idempotent, and the pipeline's whole crash-safety
argument rests on that.

| Step | Idempotent? | Why it matters |
| --- | --- | --- |
| `global_store.upsert(provisional)` | yes — identity upsert | a crash after upsert, before enqueue, leaves a re-enqueueable `Unindexed` record |
| `queue.enqueue(package)` | yes — one live job per package | a duplicate `POST /packages` cannot double-queue |
| `advance(phase)` | yes — a state write | a re-run replays phases from `Acquiring` |
| `fetch_archive` | yes — a GET | re-downloaded on retry |
| `ingest_archive` | yes — pure over the bytes | |
| `run_producer_in_cage` | yes — fresh VM each time | a forge node that cannot run the cage **fails loudly** rather than emitting an IR-less blob (`error.rs:290-297`) |
| `store.put_section` / `put_manifest` | yes — content-addressed | `Emitted.deduped` counts the no-ops |
| `outbox.append` | yes — idempotent intent | |
| `materialize` (vector) | yes — `PointId::from_symbol` upsert replaces in place | re-delivery cannot duplicate points |
| `advance_watermark` | monotone | advances **after** materialization, so a crash re-delivers |

A crash mid-phase leaves a `Progressing(Phase)` record and an expired lease.
`reclaim_expired_leases` (`indexing.rs:498`) returns the job to the runnable set
and the whole pipeline replays from `Acquiring`.

## What is a stub

| Hop | Behaviour | Evidence |
| --- | --- | --- |
| `SinkKind::Text` upsert in `outbox_consumer` | nothing. The consumer advances the watermark only; the package tantivy index is fed by `package_index_poller` off the same rows under a different watermark. | `workspace/driver/poll.rs:190` |
| `SinkKind::Graph` upsert | no-op. The graph plane moved off the removed Terminus store onto the IR reverse-position index, which is not materialized in-process. | `workspace/driver/poll.rs:194-199` |
| `SinkKind::Graph` delete | no-op — nothing to tombstone yet. | `workspace/driver/poll.rs:216-218` |
| Blob / CAS garbage collection | `cas_gc` (`poll.rs:356`) reclaims **only** consumed outbox rows below the minimum watermark. Blob GC is a documented TODO pending a safe live-reference oracle. | `workspace/driver/lib.rs:543-545`, `poll.rs:317-321` |
| P3 listing persistence | `run_indexing_job_on` deliberately does **not** call `set_listing` — the acquire path fetches archives directly and never runs `registry::resolve::resolve`, so there is no observed listing snapshot. Calling `set_listing(_, None)` would clear state the catalog-follower path may have written. | `workspace/driver/coordination/indexing.rs:97-104` |
| Toolchain images | no caller wires a real `ToolchainImageStore`; every language uses the deterministic placeholder digest. | `indexing.rs:65`; no call site for `with_toolchain_images` in `workspace/driver/` |
| The `ingest` binary | `workspace/index/ingest/main.rs` prints a message describing the intended composition and exits. It wires no engine, no transport, and no watermark store. It is one of only two binaries in the workspace. | `workspace/index/ingest/main.rs:17-30` |

## The sink lock is in-process only

`poll.rs:79-83` says the sink is claimed via "a postgres advisory lock so
exactly one replica drains it." **That comment is wrong.**
`Outbox::try_lock_sink` (`workspace/index/coordination/outbox.rs:277`) is
`Arc::clone(&self.sink_guards[slot]).try_lock_owned()` — a
`tokio::sync::Mutex` held in the `Outbox` struct. It excludes concurrent tasks
inside **one process**. Two `driver` processes pointed at the same catalog will
both drain the same sink.

There is also no Postgres anywhere in this repo (see
[architecture.md](../architecture.md#data-stores)).

Because every sink write is idempotent and the watermark advance is monotone,
concurrent drain is wasteful rather than corrupting — but it is not the
exclusion the comment claims. Recorded as
[OQ-12](../open-questions.md#oq-12--is-single-replica-outbox-drain-a-requirement).

## Failure modes

| What fails | What the caller sees | Where it is logged | Retried? |
| --- | --- | --- | --- |
| Bad ecosystem / name / version | `400` from `into_coordinates` | `error.rs:378` (`warn`) | no — client error |
| Unknown custom `origin` | `400 UnknownCustomRegistry` | same | no |
| `POST /packages` body > `min(max_request_bytes, 64 KiB)` | `413` from axum's body limit | — | no |
| Request outlives `limits.upload_timeout` (2 s) | `504`, plain text | — | no |
| Catalog unreachable at `ensure_initialized` | `503` (`RegistryError` → `Registry` → 503) | `error.rs:376` (`error`) | yes — `is_retryable` |
| Upstream archive 404 | job fails, `FailureKind::SourceUnavailable`, state → `Failed` | `indexing.rs:565` (`warn`) | yes, ≤ 5 attempts |
| Archive over `ExtractionLimits::DEFAULT` | `FailureKind::Malformed`, state → `Failed` | same | yes, ≤ 5 (and will fail identically) |
| `RootfsStore::from_env()` fails (no `NUDOX_GUEST_ROOTFS`) | `InternalError::CageCompile`, state → `Failed` | same | yes — and will fail identically on every retry until the forge node is provisioned |
| Golden fork unsupported on this host | **not a failure** — cold-boots a fresh cage | `indexing.rs:928` (`info`) | n/a |
| Producer exits non-zero in the cage | `InternalError::ProducerFailed` with 2000 chars of stderr, state → `Failed` | `indexing.rs:565` | yes, ≤ 5 |
| Producer emits a malformed / aborted IR stream | **job succeeds** with an empty IR section | `indexing.rs:1058` (`warn`) | n/a — this is the silent-degradation case |
| Job exceeds `job_deadline` (600 s) | `FailureKind::Timeout`, state → `Failed` | `indexing.rs:565` | yes, ≤ 5 |
| Lease lost to another worker | heartbeat returns → select resolves → `IndexingDeadlineExceeded` | `indexing.rs:603` | the other worker owns it |
| Listing-signal or download-count fetch fails | **non-fatal** — facets keep `None`, the job succeeds | `indexing.rs:1312`/`:1396` (`debug`) | n/a |
| Attempts exhausted | `DeadLettered`; never auto-requeued | `handle_job_failure` | **no** |
| Qdrant down during vector fan-out | the intent is not consumed, the watermark does not advance | `poll.rs:103` (`warn`) | yes, next tick |
| Embeddings endpoint down | same | same | yes |

## Where to start reading

1. `workspace/driver/coordination/indexing.rs` — the whole pipeline, in order,
   1637 lines. Start at `run_indexing_job_on` (line 90) and read the four
   `execute_*_phase` functions.
2. `workspace/driver/poll.rs` — the background loops. `outbox_consumer` (73)
   and `materialize` (167) are the fan-out half.
3. `workspace/index/coordination/outbox.rs` — the transactional outbox, the two
   `SinkKind`s, and `try_lock_sink`.
4. `workspace/index/blob/emit.rs` — 30 lines, the commit point.
