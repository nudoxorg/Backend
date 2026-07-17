# Registry + Server + Heart Audit (Librarification)

**Date:** 2026-07-16  
**Scope (live code only):** `workspace/registry`, `workspace/server`, `workspace/heart`  
**Out of scope:** root-level `compiler/` (legacy), building/compiling, pre-existing build failures  
**Purpose:** Feed the INDEX/REGISTRY split, Postgres→SQLite (remote) / Postgres→k8s-queue drop, and `server`→`client` library restructure  

---

## 0. Architecture snapshot (current)

```
                    ┌─────────────────────────────────────────────┐
                    │  workspace/server (axum binary + lib)       │
                    │  Server<M: EmbeddingModel>                  │
                    │  Role: Gateway | Forge | All                │
                    └───────────────┬─────────────────────────────┘
                                    │ federation of SourceStores
          ┌─────────────────────────┼─────────────────────────────┐
          ▼                         ▼                             ▼
   GlobalStore (pg)          Queue (pg SKIP LOCKED)        Outbox (pg)
   packages/parse_status/    jobs                          outbox +
   symbols                                                 sink_watermarks
          │                         │                             │
          │              Indexer pipeline (Forge)                 │ consumers
          │              acquire→extract→compile→emit             │ (Gateway)
          ▼                         ▼                             ▼
   Store (object_store)      BlobManifest + CAS sections    Text (tantivy)
   cas/{blake3} + ptr/{uuid}                                Vector (qdrant)
                                                            Graph (terminus)
   heart vocabulary: PackageId/SymbolId/ContentHash/Connect/Cold/Live/
                     ResolutionState/Failure/DerivedStore/Federation
```

**Key crates:**

| Crate | Path | Role |
|---|---|---|
| `heart` | `workspace/heart` | Shared vocabulary: identity, typestate, failures, cache/CAS primitives, federation, telemetry (feature-gated) |
| `registry` | `workspace/registry` | Service logic: postgres spine, CAS store, queue, outbox, ingest, search ranking, folded-in runtime (text/vector/graph/session) |
| `server` | `workspace/server` | Axum HTTP surface, config, coordination (indexing/search/health), poll loops, compiler HTTP client |

**Notable absences vs prompt assumptions:**

1. **No `ForgeRuntime<Cold/Ready>`** — removed in Buck2→Cargo migration; replaced by plain `CompilerClient` (`workspace/server/lib.rs:120–122`, `compiler_client.rs`).
2. **No separate `Cas` trait inside `registry`** — object store is `registry::store::Store`; the generic `Cas`/`MemoryCas`/`DiskCas`/`Tiered` lives in **`heart::cache`** (`workspace/heart/cache/mod.rs:26–48`).
3. **No `sqlx::migrate!` migration files** — schema is pure `sea_query` DDL applied on boot (`workspace/registry/schema/mod.rs:8–11`).
4. **`libsqlite3-sys` already depends in registry** (`workspace/registry/Cargo.toml:38`) but is unused for the live spine (still postgres-only via sqlx/sea-query binders).
5. **No LinkML → schema.json pipeline visible** in these crates for Terminus; document model is comment-documented and written ad-hoc (`workspace/registry/runtime/graph/mod.rs:323–331`).

---

## 1. POSTGRES INVENTORY (critical)

### 1.1 How schema is applied (no migration crate)

- Single source of truth: `workspace/registry/schema/mod.rs`.
- `create_all()` / `create_indexes()` / `schema_ddl()` render Postgres DDL via `PostgresQueryBuilder`.
- Applied by `GlobalStore::connect` on every boot with `IF NOT EXISTS` (`schema/mod.rs:8–11`, `index/mod.rs` connect path).
- Discriminants are **text + CHECK**, never native PG ENUMs (`schema/mod.rs:14–21`) — deliberate for evolvability.
- Sessions table is **separate**: created by `PgSessionStore::migrate()` ad-hoc SQL (`runtime/session.rs`), not in `schema/mod.rs`.

**Portability note:** Absence of hand-written migrations is *good* for a clean SQLite re-target: re-render `sea_query` with `SqliteQueryBuilder` (or dual backends). The hard part is PG-only query features, not drift of numbered migrations.

### 1.2 Tables

#### T1 — `packages` (`schema/mod.rs:42–71`, create `260–302`)

| Column | Type | Purpose |
|---|---|---|
| `id` | `uuid` PK | Deterministic `PackageId` (UUIDv5) |
| `language` | `text` CHECK | Ecosystem token from `Language::VARIANTS` |
| `origin_token` | `text` | Registry origin stable token |
| `name_canonical` / `name_original` | `text` | Identity vs display name |
| `version_canonical` | `text` | Normalized version |
| `visibility` | `text` CHECK | `Visibility` |
| `owner_tenant` | `uuid` | Tenant id |
| `owner_kind` | `text` CHECK | `OwnerKind` |
| `toolchain` | `jsonb` | Serialized `Toolchain` |
| `created_at` / `updated_at` | `timestamptz` | Row times; `updated_at` drives package-search sync cursor |

**Stores:** Canonical package identity + ownership + toolchain provenance.  
**PG features:** `uuid`, `jsonb`, `timestamptz`, CHECK, UNIQUE secondary index on coordinates.  
**Portability:** **(a) trivially portable to SQLite** — map uuid→BLOB/TEXT, jsonb→TEXT/JSON1, timestamptz→TEXT/INTEGER. Coordinates UNIQUE index is plain SQL.

#### T2 — `parse_status` (`schema/mod.rs:73–100`, create `305–344`)

| Column | Type | Purpose |
|---|---|---|
| `package_id` | `uuid` PK + FK CASCADE | One lifecycle row per package |
| `state` | `text` CHECK | `unindexed`/`progressing`/`stored`/`failed`/`deadlettered` |
| `phase` | `text` NULL CHECK | `Phase` when Progressing |
| `content_hash` | `bytea` NULL | 32-byte BLAKE3 when Stored |
| `attempts` | `int` | Failure attempt counter |
| `failure` | `jsonb` NULL | Serialized `Failure` |
| `facets` | `jsonb` NULL | `SearchFacets` (keywords + quality_ppm) |
| `needed` | `bool` | Dependency-ordered scheduling flag |
| `updated_at` | `timestamptz` | Last transition |

**Stores:** The `ResolutionState` machine — orchestration SoT for "where is this package".  
**PG features:** FK CASCADE, jsonb, bytea, CHECK.  
**Portability:** **(a) trivially portable** for columns; transactional co-commit with outbox is **(b)** if SQLite WAL single-writer is the only concurrency model (fine for REGISTRY local; for INDEX multi-writer needs redesign or single writer).

#### T3 — `jobs` (`schema/mod.rs:102–121`, create `347–376`)

| Column | Type | Purpose |
|---|---|---|
| `id` | `bigserial` PK | Job row identity (`JobId`) |
| `package_id` | `uuid` UNIQUE + FK | One live job per package |
| `state` | `text` CHECK | Mirrored lifecycle discriminant |
| `attempts` | `int` | Attempt count (incremented on dequeue) |
| `enqueued_at` | `timestamptz` | FIFO secondary order |
| `lease_until` | `timestamptz` NULL | Lease; NULL/past = runnable |
| `priority` | `int` | Higher first |

**Stores:** Durable work queue with leases + priority.  
**PG features:** `bigserial`, **`FOR UPDATE SKIP LOCKED`** (dequeue), concurrent multi-worker.  
**Portability:**  
- INDEX fleet: **(c) obviated by moving queueing to k8s** — Jobs become k8s Job/CronJob/queue CRDs; `jobs` table deleted.  
- REGISTRY local: **(b) portable with redesign** — single-writer SQLite can use simple lease columns without SKIP LOCKED; multi-process local workers need file locks or "only GUI process runs queue".

#### T4 — `outbox` (`schema/mod.rs:123–138`, create `379–406`)

| Column | Type | Purpose |
|---|---|---|
| `seq` | `bigserial` PK | Monotonic watermark cursor |
| `package_id` | `uuid` FK | Package that changed |
| `generation` | `bytea` | 32-byte content hash of generation |
| `sink_kind` | `text` CHECK | `vector`/`graph`/`text` (`DerivedStore`) |
| `created_at` | `timestamptz` | Intent time |

**UNIQUE** `(package_id, generation, sink_kind)` for idempotent re-emit (`schema/mod.rs:499–508`).

**Stores:** Transactional fan-out intents to derived read-plane stores.  
**PG features:** `bigserial`, UNIQUE multi-col, FK CASCADE.  
**Portability:** **(b) portable with redesign** for REGISTRY (sqlite autoincrement + same unique key). For INDEX: still needed as "generation published" log unless replaced by object-store events / k8s orchestration events. Pattern is essential; postgres is not.

#### T5 — `sink_watermarks` (`schema/mod.rs:140–151`, create `409–428`)

| Column | Type | Purpose |
|---|---|---|
| `sink_kind` | `text` PK CHECK | Consumer identity |
| `last_seq` | `bigint` | Last consumed `outbox.seq` |
| `updated_at` | `timestamptz` | Last advance |

**PG-specific watermark advance:** `GREATEST(sink_watermarks.last_seq, $2)` in ON CONFLICT (`queries.rs` outbox module).  
**Portability:** **(a)** with `MAX()` rewrite for SQLite. Per-replica text index also keeps a **file sidecar** watermark (`runtime/text/poll.rs` `watermark.json`) — already half-portable.

#### T6 — `symbols` (`schema/mod.rs:153–168`, create `431–453`)

| Column | Type | Purpose |
|---|---|---|
| `id` | `uuid` PK | Deterministic `SymbolId` |
| `package_id` | `uuid` FK | Owning package |
| `fq_name` | `text` | Fully-qualified name |
| `kind` | `text` CHECK | `SymbolKind` |
| `generation` | `bytea` | Generation hash this projection reflects |

**Stores:** Serving projection used by text poller + outbox materializers (avoids reading IR blobs for search upsert).  
**Portability:** **(a) trivially portable**. Likely remains in both INDEX (sqlite) and REGISTRY (sqlite).

#### T7 — `sessions` (NOT in schema/mod.rs — `runtime/session.rs`)

```
sessions(
  id uuid PRIMARY KEY,
  graph jsonb NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now()
)
```

Created by `PgSessionStore::migrate()` with `CREATE TABLE IF NOT EXISTS`.  
**Stores:** Per-user exploration graph (join-semilattice of nodes+edges).  
**PG features:** row `FOR UPDATE` on merge, jsonb.  
**Portability:** **(a/b)** — Memory + Directory backends already exist (`MemorySessionStore`); PG backend is for multi-replica gateway only. Local REGISTRY uses Memory/Directory; INDEX may keep sqlite JSON blob or drop multi-replica sessions.

### 1.3 Indexes (`schema/mod.rs:461–525`)

| Index | Columns | Purpose | Portability |
|---|---|---|---|
| `idx_packages_coords` | UNIQUE `(language, origin_token, name_canonical, version_canonical)` | Coordinate identity | (a) |
| `idx_parse_status_state` | `state` | Scheduling scans | (a) |
| `idx_parse_status_hash` | `content_hash` | Freshness lookups | (a) |
| `idx_jobs_runnable` | `(priority, enqueued_at)` | Dequeue hot path; partial-predicate comment notes sea-query 0.32 lacks portable partial-index builder (`488–491`) | (c) with k8s; else (b) |
| `idx_outbox_dedupe` | UNIQUE `(package_id, generation, sink_kind)` | Idempotent fan-out | (a) |
| `idx_outbox_sink_seq` | `(sink_kind, seq)` | Watermark reads | (a) |
| `idx_symbols_package` | `package_id` | Symbol listing | (a) |

### 1.4 Discriminant domains (from `strum::VariantNames`)

Defined in `schema/mod.rs` ~207–244:

| Domain | Source |
|---|---|
| `STATE_VALUES` | hard-coded lifecycle tokens matching `ResolutionState` |
| `PHASE_VALUES` | `heart::Phase::VARIANTS` |
| `SINK_KIND_VALUES` | `heart::DerivedStore::VARIANTS` |
| `SYMBOL_KIND_VALUES` | `heart::SymbolKind::VARIANTS` |
| `LANGUAGE_VALUES` | `heart::Language::VARIANTS` |
| `OWNER_KIND_VALUES` | `heart::OwnerKind::VARIANTS` |
| `VISIBILITY_VALUES` | `heart::Visibility::VARIANTS` |

SQLite CHECK constraints support the same pattern.

### 1.5 Every query builder (`schema/queries.rs`)

All statements render with `sea_query::PostgresQueryBuilder` (`queries.rs:35–38`). Grouped by store:

#### index::*

| Function | Semantics | PG-isms | Portability |
|---|---|---|---|
| `upsert_package` | INSERT packages ON CONFLICT(id) DO UPDATE … RETURNING id | ON CONFLICT, RETURNING | (a) SQLite supports both |
| `set_state` | UPSERT parse_status full lifecycle tuple | ON CONFLICT | (a) |
| `set_facets` | UPDATE facets jsonb | jsonb bind | (a) JSON1/text |
| `get_state` | SELECT state/phase/hash/needed/failure | — | (a) |
| `get_generation` | SELECT content_hash WHERE state='stored' | — | (a) |
| `get_package` | packages ⟕ parse_status facets | LEFT JOIN | (a) |
| `select_progressing` | package_ids where state=progressing | — | (a) |
| `upsert_symbol` | UPSERT symbols | ON CONFLICT | (a) |
| `symbols_for` | symbols ⋈ packages language | INNER JOIN | (a) |

#### queue::*

| Function | Semantics | PG-isms | Portability |
|---|---|---|---|
| `enqueue` | INSERT jobs ON CONFLICT(package_id) DO NOTHING RETURNING id | ON CONFLICT DO NOTHING | (a) local; (c) INDEX→k8s |
| **`dequeue_batch`** | UPDATE … WHERE id IN (SELECT … **FOR UPDATE SKIP LOCKED**) | **SKIP LOCKED**, concurrent claim | **(c) k8s** / **(b) single-writer sqlite** |
| `complete` | DELETE job if lease held | lease guard + RETURNING | (a/b) |
| `fail_retry` | UPDATE state=failed, lease_until=backoff | — | (a/b) |
| `fail_deadletter` | DELETE job under lease | — | (a/b) |
| `get_attempts` / `get_job_id_for_package` / `get_package_for_job` | lookups | — | (a) |
| `renew_lease` | heartbeat UPDATE lease_until | — | (a/b) |
| `reclaim_expired_leases` | SET lease_until=NULL WHERE expired | — | (a) |

**Dequeue implementation evidence** (`queries.rs:400–433`): `LockType::Update` + `LockBehavior::SkipLocked` — **the single hardest Postgres dependency for multi-worker queueing**.

#### outbox::*

| Function | Semantics | PG-isms | Portability |
|---|---|---|---|
| `append_one` / `append_all` | INSERT intents ON CONFLICT DO NOTHING | multi-row insert | (a) |
| `read_since` | seq > watermark ORDER BY seq LIMIT | — | (a) |
| `head` | MAX(seq) | — | (a) |
| `advance_watermark` | UPSERT with **GREATEST** | GREATEST | (a) rewrite MAX |
| **`try_advisory_lock`** | `SELECT pg_try_advisory_lock($class,$objid)` | **advisory locks** | **(b/c)** — INDEX: k8s leader election / lease; REGISTRY: unnecessary (single process) |
| **`advisory_unlock`** | `pg_advisory_unlock` | same | same |
| `read_watermark` | SELECT last_seq | — | (a) |
| `min_consumed_watermark` | CASE WHEN all sinks present THEN min ELSE 0 | CASE/aggregate | (a) |
| `delete_consumed_below` | DELETE seq <= floor | — | (a) |

Advisory lock class: `ADVISORY_LOCK_CLASS_OUTBOX_SINK = 0x0B0B_0001` (queries outbox section); used by `Outbox::try_lock_sink` (`coordination.rs:224–253`).

#### search::*

| Function | Semantics | Portability |
|---|---|---|
| `changed_since` | packages changed after `updated_at` cursor, joined parse_status | (a) — drives package Tantivy sync |

#### persist::*

| Function | Semantics | Portability |
|---|---|---|
| `reset_transient_parse_status` | Progressing → Unindexed | (a) |
| `clear_all_leases` | Null all leases on restart | (a) — or delete with k8s queue |

### 1.6 Runtime SQL outside queries.rs

| Site | SQL | Purpose | Portability |
|---|---|---|---|
| `runtime/text/poll.rs` `PENDING_SQL` | outbox WHERE sink_kind='text' AND seq > $1 | Symbol-text poller (parallel path to server outbox consumer) | (a) |
| `runtime/text/poll.rs` `SYMBOLS_SQL` | symbols JOIN packages WHERE package_id = **ANY($1)** | **PG array `ANY`** | **(b)** — rewrite as `IN (...)` for SQLite |
| `runtime/session.rs` PgSessionStore | CREATE/INSERT/SELECT FOR UPDATE/UPDATE sessions | Multi-replica sessions | (b) or drop for local |

### 1.7 Transactional semantics inventory

| Pattern | Where | Meaning for librarification |
|---|---|---|
| Schema DDL txn on connect | `GlobalStore::connect` | INDEX sqlite: same idea with `IF NOT EXISTS` |
| Upsert package + set_state atomic | `GlobalStore::upsert` | Keep |
| **record_stored: set_state + set_facets + append_all one txn** | `coordination.rs:147–180` | **Keep pattern** on both INDEX and REGISTRY; engine can be sqlite |
| dequeue lease claim one statement | `queue` dequeue_batch | Replace with k8s claim or local single-writer |
| Session merge FOR UPDATE | `PgSessionStore::merge_into` | Only multi-replica |
| Restart reconcile: reset Progressing + clear leases | `persist.rs:34–82` | REGISTRY keep; INDEX if queue stays DB-backed |
| GC delete_consumed_below | `coordination.rs:270–288` | Keep for outbox log growth |

### 1.8 Postgres-specific feature scorecard

| Feature | Used? | Sites | INDEX plan | REGISTRY plan |
|---|---|---|---|---|
| LISTEN/NOTIFY | **No** | — | N/A | N/A |
| FOR UPDATE SKIP LOCKED | **Yes** | queue dequeue | **Delete** (k8s) | Single-writer / no concurrent dequeue |
| Advisory locks | **Yes** | outbox sink drain | k8s lease / singleton consumer | Unneeded |
| jsonb | **Yes** | toolchain, failure, facets, session graph | sqlite JSON | same |
| bytea | **Yes** | content_hash, generation | BLOB | BLOB |
| uuid type | **Yes** | all ids | TEXT/BLOB | same |
| arrays / ANY() | **Yes** | text poller package_id = ANY | rewrite IN | same |
| bigserial | **Yes** | jobs.id, outbox.seq | AUTOINCREMENT | same |
| GREATEST | **Yes** | watermark advance | MAX rewrite | same |
| Triggers | **No** | — | — | — |
| Native ENUM | **No** | text CHECK | keep CHECK | keep |
| Partial indexes | Intent only | idx_jobs_runnable comment | N/A if no jobs | optional |

### 1.9 Portability classification summary (prompt §1)

| Subsystem | Class | Rationale |
|---|---|---|
| packages + parse_status + symbols | **(a)** trivial sqlite | Standard CRUD + upserts |
| facets/failure jsonb | **(a)** | JSON text |
| queue SKIP LOCKED multi-worker | **(c)** INDEX via k8s; **(b)** REGISTRY single-writer | Core design change for remote |
| outbox + watermarks | **(b)** portable with redesign | Keep pattern; drop advisory locks |
| sessions PG | **(b)** or **delete for local** | Memory/Directory already exist |
| package search changed_since | **(a)** | Timestamp cursor |
| text poller ANY() | **(b)** | Rewrite |

---

## 2. CAS / Object store

### 2.1 Two CAS layers (do not confuse)

| Layer | Location | API | Backing |
|---|---|---|---|
| **Service CAS (live path)** | `registry::store::Store` | `put_section` / `get_section` / `put_manifest` / `list_cas` | `object_store` (file/S3/…) |
| **Generic CAS trait (shared)** | `heart::cache::{Cas, MemoryCas, DiskCas, Tiered, NoL3}` | `get` / `put` / `put_keyed` first-write-wins | moka L1 + disk L2 + optional L3 |

The prompt's `MemoryCas/DiskCas/RegistryCas/Tiered` maps to **heart**, not registry. There is no type named `RegistryCas`. Registry's production path is `Store` over `object_store`.

### 2.2 Key layout (`store.rs:1–20`, `97–116`)

```
cas/{blake3-hex-64}   — immutable content-addressed bytes
ptr/{package-uuid}    — 32 raw bytes = current manifest ContentHash
cas/.reachability-probe — connect sentinel (NotFound = healthy)
```

- Pointer leaf is **PackageId UUID**, never raw name segments — safe for scoped npm names (`store.rs:103–116`).
- Identical content across packages dedupes automatically (same cas key).

### 2.3 Operations

| Method | Lines | Behavior |
|---|---|---|
| `put_section` | `122–136` | HEAD first; if missing PUT; **returns bool** written vs already present (**first-write-wins**); `verify_integrity` before write |
| `get_section` | `140–146` | GET + BLAKE3 verify |
| `get_section_range` | `151–161` | Ranged read **without** whole-hash verify (documented trade-off) |
| `put_manifest` | `166–190` | validate → postcard encode → put as cas section → atomic pointer replace |
| `get_manifest` | `194–210` | read ptr → get section → decode + validate |
| `exists` | `214–220` | HEAD ptr |
| `list_cas` | `240–264` | list cas/ prefix; skip non-64-hex (sentinel) |
| Connect probe | `56–78` | HEAD sentinel |

### 2.4 Blob manifest layer (`blob/`)

**`BlobManifest`** (`blob/mod.rs:44–66` approx):

- `package: PackageId`
- `files: NonEmpty<FileEntry>` — path + hash + size (bytes in cas/)
- `ir_ref: ContentHash`
- `references_ref: ContentHash`
- `toolchain: Toolchain`

**Two distinct hashes** (must not unify — module comment):

1. **Generation stamp** `identity_bytes` — length-prefixed sorted (path, file-hash) + ir_ref + references_ref + postcard(toolchain). Stable cache freshness across schema field adds.
2. **CAS key** `manifest_cas_key` — `ContentHash::of_bytes(postcard(manifest))`. Address in cas/ and value of ptr/.

**`BlobBuilder`** (`blob/creation.rs`): streaming assembly; never holds more than one file; `finalize()` returns `(BlobManifest, Vec<PendingSection>)` with no I/O.

**`emit`** (`blob/emit.rs`): put sections → put_manifest → outbox append all SinkKinds. First-write-wins via `put_section` bool.

### 2.5 GC status

- **Outbox GC exists:** `Outbox::gc_consumed` + `poll::cas_gc` hourly (`server/poll.rs` GC_INTERVAL 3600s).
- **Blob GC does NOT exist:** `list_cas` is audit-only; poll comment documents unsolved race between mark-and-sweep and in-flight `put_manifest`.
- **heart::cache EvictableCas** can invalidate L1/L2 only; not object store.

### 2.6 INDEX(S3) / REGISTRY(disk) readiness

| Concern | Status | Gap |
|---|---|---|
| Pluggable backend via object_store URL | Ready | `file://` works today; S3 schemes need object_store features enabled (server note: vendored build may lack cloud features — `lib.rs:438–441`) |
| Content addressing + integrity | Ready | BLAKE3 everywhere |
| Pointer model for package→manifest | Ready | Directly maps INDEX and REGISTRY |
| heart DiskCas for local trusted | Ready | Not wired into registry Store path yet |
| Unified Cas trait over registry Store | Missing | Registry Store ≠ heart::Cas; dual APIs |
| GC | Missing | Must design before unbounded S3 growth |
| IR + tree-sitter trees + source | Partial | IR + references + source files in cas/; **resolved tree-sitter trees as first-class cas objects not clearly separated** beyond IR/refs |

**Verdict:** Layout is **already close** to the split. INDEX = `Store` over S3 (+ sqlite index). REGISTRY = `Store` over local filesystem or `Tiered<DiskCas>` + sqlite. Main work: wire heart::Cas into the emit path (or implement heart::Cas for object_store), enable S3 features, add GC.

---

## 3. Tantivy

There are **two** Tantivy indexes:

### 3.A Package search index (`registry/search/tantivy.rs` + `server/search/registry.rs`)

**Role:** Package discovery (`POST /packages/search`). Postgres is SoT; Tantivy is disposable replica.

**Schema fields** (`search/tantivy.rs` ~99–114):

| Field | Indexing | Role |
|---|---|---|
| `package_id` | STRING\|STORED | Upsert/delete key |
| `name` | TEXT | Search |
| `description` | TEXT | Declared but **never populated** in absorb |
| `keywords` | TEXT | From facets |
| `ecosystem` | STRING\|STORED | Filter |
| `record` | STORED only | Full JSON `GlobalPackage` for hydration (no PG round-trip) |

**Sync:** `sync_from` uses `queries::search::changed_since` with microsecond watermark in `sync_watermark.json`. Batch 1024. Orphan guard: if watermark > 0 but num_docs==0, reset to 0.

**Ranking fusion** (`search/ranking.rs`): five-stage pipeline ported from lib.rs monorepo:

1. `fuse_scores` — BM25 × quality kink (+ exact/contains bonuses)
2. Sort fused desc, name asc
3. `diversity_pass` (only if n ≥ 25)
4. `pull_up_representatives`
5. `downloads_bubble` (pairs)

**Gaps:**

- `downloads` hardcoded `0` in `search/mod.rs` collect path → bubble/pull-up download logic inert.
- Description field unused.
- Cursor snapshot mismatch is **advisory** (debug log, still serves).

**Facets column mirroring failure shape:** `parse_status.facets` jsonb mirrors nullable-jsonb pattern of `failure` (`schema/mod.rs:92–94`, `coordination.rs:140–146`).

### 3.B Symbol text index (`registry/runtime/text/`)

**Role:** Precise symbol search (`POST /search`).

**Schema** (`text/index.rs`): 11 fields — id, package, ecosystem, kind, name, fq_name, name_lower, fq_lower, name_tokens, fq_tokens.

**Identifier tokenizer** (`text/tokenizer.rs`):

- Registered name `"ident"`
- CamelCase / acronym / digit-letter splits
- CLR arity suffix strip `` `N ``
- Max token 64 chars; LowerCaser + RemoveLongFilter
- `WithFreqsAndPositions` for phrase queries

**Query** (`text/query.rs`): 5-tier Should disjunction:

1. Exact `name_lower`
2. Regex contains `name_lower`
3. Regex contains `fq_lower`
4. Subtoken Must conjunction `name_tokens`
5. Subtoken Must conjunction `fq_tokens`

Plus optional ecosystem/kind Must filters. Keyset pagination with **Enforced** snapshot policy (`snapshot_hash` of segment ids + doc counts).

**Indexing drive:**

1. **Server path:** `poll::outbox_consumer` SinkKind::Text → `upsert_batch` after symbols_for.
2. **Registry path:** `text/poll.rs` Poller with own `PENDING_SQL`/`SYMBOLS_SQL` and `watermark.json` — **duplicate materialization path** risk if both run; in production server uses outbox_consumer primarily while TextIndex open_or_create is local.

**Upsert strategy:** delete-by-id term then add; batch commit once. **No package-level wipe** on generation change beyond per-symbol upsert — stale symbols from previous generation if not re-upserted/deleted: **incremental story is package-intent-level, not symbol-diff-level**. Deletes: `remove` exists but generation-diff GC of removed symbols not evident in poll path.

**Writer:** Mutex IndexWriter, 50MB heap, ReloadPolicy::Manual.

---

## 4. Qdrant (vector)

### 4.1 Types (`runtime/vector/mod.rs`)

- `Semantic<M, S>` — branded by embedding model `M` and Cold/Live.
- Single global `CollectionName` (validated charset); scoping via payload filters, not multi-collection.
- `SymbolPoint { symbol, embedding, ecosystem, purpose }`
- Point id = symbol UUID string; payload `ecosystem` + `purpose` (`code`|`documentation`).

### 4.2 Connect checks

1. health_check
2. collection_info → assert dimension == `M::DIMENSIONS` or `DimensionMismatch`

### 4.3 Embedder origin

- **Not local model in-process.** `server/search/semantic/embedder.rs` `HttpEmbedder<M>` posts OpenAI-compatible JSON to configured URL (default ollama-style `http://127.0.0.1:11434/v1/embeddings` in config defaults).
- Models catalogued as ZSTs: `E5Small` (384, `intfloat/e5-small-v2`), `OpenAi3Small` (1536) — `runtime/vector/model/catalog.rs:11–25`.
- Purpose enum separates code vs documentation embeddings (`embedding.rs`).

### 4.4 EmbeddingCache

- Key: `{model: ModelId, text_hash: ContentHash}` — **cross-package dedupe** intentional.
- moka future cache; get-then-insert (not try_get_with) because errors non-Clone; concurrent double-embed possible but deterministic.

### 4.5 SemanticGate

- Non-Copy, non-Clone, must_use, consumed by value (`gate.rs:16–36`).
- Issue only from SearchPlanner (+ `extend_across_federation`) or `for_readiness`.
- Makes accidental expensive search unrepresentable.

### 4.6 VectorSink

- tower::Service batch upsert, MAX_BATCH 256, `wait(true)`.
- Search requires gate; cursor is **Advisory** (no cheap ANN snapshot hash).

### 4.7 Materialization

- `poll.rs materialize_vector`: per-symbol cache→embed→SymbolPoint→uploader.
- Relations/graph edges are **not** embedded here; vectors are symbol text.

---

## 5. TerminusDB client

### 5.1 Connection (`runtime/graph/mod.rs`)

- `Graph<S>` with org/db/credentials/endpoint.
- Connect: GET `/api/info` (auth), GET `/api/db/{org}/{db}` (existence; 404→SchemaMismatch).
- No schema push from this crate on connect — **assumes schema already present**.

### 5.2 Document model (comment-documented, not LinkML-generated here)

```
Symbol { id, package, ecosystem, plain, fully_qualified, kind }  @id = Symbol/{uuid}
Relation { from, kind, to }
```

RelationKind: Member, Reference, Occurrence, Implements, Extends, ReExport.

### 5.3 Writes

- `insert_symbols` POST `/api/document/{org}/{db}?graph_type=instance` with `full_replace=false`, upsert by `@id` (`graph/mod.rs:439–493`).
- **Idempotent re-delivery safe.**
- Materialize path today inserts **symbols only** from poll (`poll.rs`); Relation document publication not fully wired in the outbox materializer path audited — graph **reads** assume Relation edges exist (occurrences/references). **Lineage/edge emission gap** relative to full model.

### 5.4 Reads (WOQL JSON-LD mini-DSL)

- Builders: variable, string, triple, is_a, and, select (`woql` submodule).
- `get_occurrences` / `get_references` / `are_related` / `outgoing_edges`.
- Expansion BFS (`expansion.rs`) with depth/breadth bounds; score `1/depth`.

### 5.5 Resolution / structure

- `resolution.rs`: pure FQ-path + Jaccard rename detection between symbol sets (Stable/Moved/Removed/Added) — useful for incremental symbol identity.
- `structure.rs`: assemble parent edges into StructureNode tree.

### 5.6 Schema management

- **No LinkML → schema.json pipeline in registry/server/heart.** If it exists, it's outside these crates (compiler/graph comments mention WOQL elsewhere). Terminus schema is operational precondition, not bootstrapped by `Connect`.

### 5.7 Librarification note

- Plan: Terminus only for hottest packages (leaky-bucket). Today **all** Stored packages fan out Graph outbox intents equally — **no admission control** at outbox append time. Gate exists only for **query** semantic path, not graph materialization.

---

## 6. Ingest (archive sanitization)

### 6.1 Limits (`ingest/mod.rs`)

`ExtractionLimits::DEFAULT`: 512 MiB total, 64 MiB/file, 50k files, depth 32.

### 6.2 Pipeline

1. Stream compressed archive with `.take(ceiling+1)` — compressed-size bound using uncompressed ceiling.
2. `spawn_blocking` → decompress (gz/zstd/raw tar).
3. Walk entries → `sanitize_entry`.

### 6.3 Security choke points (`ingest/extract.rs`)

| Control | Behavior |
|---|---|
| `EntryAllowlist::SAFE` | regular + directories only; **symlinks/hardlinks/devices/FIFO denied** |
| `jail_path` | reject absolute, `\`, NUL, Windows drives, `..` escape, empty, depth > limit |
| Non-UTF8 paths | rejected as UnsafeArchive |
| Declared size pre-check | header size vs max_file_bytes before read |
| Budget.charge | all-or-nothing total/count/per-file |

### 6.4 REGISTRY reuse assessment

| Piece | Untrusted remote INDEX | Trusted local REGISTRY |
|---|---|---|
| jail_path + allowlist | **Required** | Still useful (path normalization) but can relax symlink policy for monorepos if careful |
| Byte budgets | Required | Configurable higher / optional for trusted |
| Streaming BlobBuilder | Reuse | Reuse for local project walk (replace tar with filesystem walk) |
| Compressed-size take | Tar path only | N/A for directory ingest |

**Recommendation:** Split `ingest` into `sanitize` (reusable) + `archive_source` (INDEX) + `directory_source` (REGISTRY). Keep Budget as shared.

---

## 7. Server

### 7.1 Route table (`http/router.rs`)

#### Read plane (body ≤ 2 MiB)

| Method | Path | Handler | Cap |
|---|---|---|---|
| POST | `/search` | search::search | ReadCap `search.symbols` |
| POST | `/search/semantic` | search::search_semantic | forces semantic |
| POST | `/packages/search` | search::search_packages | ReadCap `search.packages` |
| POST | `/expand` | search::expand | ReadCap `search.expand` |
| GET | `/symbols/:id` | search::get_symbol | ReadCap `search.resolve_symbol` |
| GET | `/sessions/:id` | search::get_session | Principal |

#### Write plane mutations (body ≤ 64 KiB, upload timeout)

| Method | Path | Handler | Cap |
|---|---|---|---|
| POST | `/packages` | indexing::add_package | WriteCap ensure_initialized |
| GET | `/packages/:id` | indexing::get_package | none |
| POST | `/packages/:id/sync` | indexing::sync_package | WriteCap sync |

#### Ops (unbounded)

| GET `/healthz` | livez always 200 |
| GET `/readyz` | probes all backends |
| GET `/metrics` | Prometheus |

#### Admin

| POST `/admin/packages/:id/verify` | AdminCap |
| POST `/admin/packages/:id/rebuild` | AdminCap |

Middleware: RED metrics route_layer, OTel TraceLayer, per-plane body limits, timeout on write/admin.

### 7.2 SearchPlanner (`search/planner.rs`)

- `Plan::Precise | Semantic(SemanticGate)`
- Literal → always Precise
- Abstract → quota admit (64 / 60s default) or degrade to Precise
- `authorize_similar` dead surface (no route)
- `extend_across_federation` mints per-source gates

### 7.3 No ForgeRuntime

`CompilerClient` HTTP postcard to `{base}/compile` (`compiler_client.rs`). Cold/Live remains on stores only.

### 7.4 Background loops (`poll.rs`, started `lib.rs:386–400`)

| Loop | Role gate | Duty |
|---|---|---|
| `queue_worker` | Forge\|All | Indexer drain with drain token |
| `outbox_consumer` × SinkKind | Gateway\|All | advisory lock + materialize + watermark |
| `package_index_poller` | Gateway\|All | Package Tantivy sync |
| `text_index_poller` | Gateway\|All | **Metrics only** lag gauges |
| `cas_gc` | Gateway\|All | outbox GC + cas count gauge (no blob delete) |

### 7.5 Config (`config.rs`)

Layered figment: defaults ← `NUDOX_CONFIG`/nudox.toml ← `NUDOX_*` env (`__` nesting).

Key knobs: serving_address, role, deployment, compiler_endpoint, metadata_data_dir, limits.*, definitive/overlays endpoints (postgres, terminus, qdrant, embeddings, object_store).

Production boot guard rejects default postgres URL and terminus password `root`.

### 7.6 Coordination

- **initialization:** decision table Unindexed/Progressing/Stored/Failed/DeadLettered × freshness → Enqueue/Serve/Hold (`coordination/initialization.rs`).
- **indexing pipeline:** Acquire → Extract (ingest) → Compile (HTTP) → Emit (CAS+outbox+record_stored). Heartbeat lease every lease/3. FailureKind classification.
- **search:** precise multi-source tantivy merge; semantic embed once + fan-out; expand 1-deep BFS breadth 16.
- **health:** definitive Down vs overlay Degraded.

### 7.7 Future placement triage

| Component | Future home |
|---|---|
| http/dto, search/query, QueryError | **shared client library** |
| SearchPlanner + SemanticGate policy | client (local) + INDEX gateway |
| Indexer pipeline, poll loops, save/* | **INDEX service** (+ forge split) |
| authz caps | INDEX edge / deleted for local |
| CompilerClient | both (remote daemon vs embed) |
| Server::assemble federation | INDEX; client uses typestate Connect |
| Role Gateway/Forge | INDEX k8s deployment model; local always "All" simplified |

---

## 8. heart inventory

### 8.1 Identity

| Type | Definition | Derivation |
|---|---|---|
| `Id<T>` | `identity/id.rs:10` Uuid + PhantomData | `from_uuid`, `new_random` v4, `from_name` v5, `cast` |
| `PackageId` | `Id<Package>` | `Coordinates::id` ← `Id::from_name(namespace::PACKAGE, identity_bytes)` |
| `SymbolId` | `Id<Symbol>` | `EntryUri::symbol_id(instance_token)` ← instance ‖ NUL ‖ canonical URI |
| `EntryUri` | package + path segments | `canonical()` uses `/` join |
| `ContentHash` | `[u8;32]` BLAKE3 | `of_bytes`, builder, hex |
| `JobKey` | newtype ContentHash | length-prefixed producer‖toolchain‖source‖dep_lock |
| Namespaces | `namespace.rs` | PACKAGE, SYMBOL uuids; SourceId separate |

### 8.2 Connect typestate (`connection.rs`)

```
Cold → Connect::connect → Live
```

Precedent for client remote-vs-local typestates. Used by GlobalStore, Queue, Outbox, Store, Graph, Semantic.

### 8.3 Failure taxonomy (`error/failure.rs`)

- `Phase`: Acquiring, Extracting, Compiling, Emitting
- `Failure`: attempts, phase, message, cause, at
- `FailureKind`: Transient, SourceUnavailable, Malformed, Timeout, Unsafe, Internal — **retriable = Transient|Timeout**
- `ResolutionState`: Unindexed{needed}, Progressing(Phase), Stored{hash}, Failed(Failure), DeadLettered(Failure)

### 8.4 DerivedStore (`sink.rs`)

`Vector | Graph | Text` — wire tokens lowercase; CHECK domain source.

### 8.5 Federation (`access/federation.rs`)

- One definitive base + ordered overlays
- `in_precedence`: overlays then base
- `query` first Some wins with Sourced tag

### 8.6 Cache module (`cache/`)

- Stampede: SingleFlight, StampedeCache (moka+XFetch+SWR), jitter
- Cas trait first-write-wins put_keyed
- MemoryCas, DiskCas, Tiered, NoL3, EvictableCas
- Keys: ContentHash / JobKey

### 8.7 Other vocabulary

- `Cursor` Enforced vs Advisory snapshot policies
- `Score` / `Scored` / `Page`
- `BackendKind`: Postgres, Qdrant, Terminus, ObjectStore, Tantivy
- `ConnectFailure`: Unreachable, Auth, SchemaMismatch, DimensionMismatch, Timeout, Other
- `OwnerKind` / `Visibility` / `Language` / `Toolchain`
- Telemetry feature-gated for server only

---

## 9. INDEX / REGISTRY module mapping table

Legend: **Home** ∈ {INDEX, REGISTRY, SHARED client lib, ORCH (k8s orchestration server), DELETE}. **Cost** L/M/H. **Blockers** called out.

### 9.1 registry modules

| Module | Path | Current role | Future home | Cost | Blockers / notes |
|---|---|---|---|---|---|
| `lib.rs` Package/GlobalPackage | registry/lib.rs | Core types | SHARED | L | Keep facets optional |
| `schema/*` | schema/ | PG DDL+queries | INDEX sqlite port + REGISTRY sqlite subset | H | Dual query builder; drop SKIP LOCKED/advisory |
| `index/*` | index/ | GlobalStore | INDEX + REGISTRY (same trait) | M | Connect over sqlx sqlite |
| `queue/*` | queue/ | Job queue | ORCH (INDEX) / REGISTRY simplified | H | k8s job claim API redesign |
| `coordination.rs` Outbox | coordination.rs | Fan-out | INDEX + REGISTRY | M | Replace advisory locks |
| `persist.rs` | persist.rs | Crash reconcile | REGISTRY + INDEX if DB queue | L | With k8s queue: mostly DELETE |
| `store.rs` | store.rs | object_store CAS | INDEX S3 / REGISTRY disk | L | Enable S3 features; GC |
| `blob/*` | blob/ | Manifest/builder/emit | SHARED | L | Already pure |
| `ingest/*` | ingest/ | Untrusted archives | INDEX full; REGISTRY sanitize subset | M | Add directory walker |
| `metadata/*` | metadata/ | Facets/heuristics | SHARED | L | downloads still missing |
| `search/*` | search/ | Package tantivy+rank | INDEX gateway + REGISTRY local | M | Watermark without PG seq optional |
| `runtime/text/*` | runtime/text/ | Symbol tantivy | INDEX replica + REGISTRY embedded | M | Dual poll vs outbox consumer unify |
| `runtime/vector/*` | runtime/vector/ | Qdrant client+gate | INDEX remote; REGISTRY embedded/alt | H | Local vector store choice TBD |
| `runtime/graph/*` | runtime/graph/ | Terminus client | INDEX hot tier only | H | Cold path = IR blob graph ops |
| `runtime/session.rs` | session | Exploration state | REGISTRY Memory/Dir; INDEX optional | L | Drop PgSessionStore for local |
| `runtime/pagination.rs` | pagination | Keyset helper | SHARED | L | — |
| `protocol.rs` | protocol | Compile wire | SHARED (compiler client) | L | postcard stable |
| `resolve.rs` | resolve | Version selection | INDEX (fetch registries) | M | Local may use lockfiles only |
| `health/*` | health | Aggregate probe | both | L | BackendKind may drop Postgres |
| `identity.rs` | identity | Re-exports | SHARED via heart | L | — |
| `error.rs` | error | Error taxonomy | SHARED split | M | SQLSTATE codes PG-specific |
| `package` | package | Coordinates wrapper | SHARED | L | — |

### 9.2 server modules

| Module | Path | Future home | Cost | Notes |
|---|---|---|---|---|
| `http/router` + handlers | http/ | INDEX HTTP; client has no axum routes (or thin local) | M | DTO→client |
| `http/dto` | http/dto.rs | SHARED client | L | — |
| `search/planner` | search/planner.rs | SHARED policy | L | Gate issuance |
| `search/query` | search/query.rs | SHARED | L | — |
| `search/registry|symbols|semantic` | search/ | INDEX + REGISTRY adapters | M | — |
| `coordination/indexing` | coordination/indexing.rs | INDEX forge + REGISTRY local pipeline | H | Trusted path skips download/sanitize |
| `coordination/*` rest | coordination/ | INDEX primarily | M | — |
| `poll.rs` | poll.rs | INDEX roles; REGISTRY in-process simplified | H | k8s replaces queue_worker |
| `config.rs` | config.rs | Split INDEX service config vs client config | M | NUDOX_* stays remote |
| `compiler_client.rs` | compiler_client.rs | SHARED | L | Local may call embed lib |
| `authz.rs` | authz.rs | INDEX edge | L | Local: open/trusted |
| `save/*` | save/ | INDEX admin | L | — |
| `main.rs` / binary | main.rs | INDEX binary only | L | client is library |
| `lib.rs` Server | lib.rs | INDEX assembly; client new typestate facade | H | Biggest API redesign |

### 9.3 heart modules

| Module | Future home | Cost | Notes |
|---|---|---|---|
| identity/*, content, package | SHARED foundation | L | Stable |
| connection Cold/Live | SHARED — **expand** for Remote/Local client | M | Precedent intact |
| error/failure ResolutionState | SHARED | L | May add Local phases |
| sink DerivedStore | SHARED | L | Maybe add tiers |
| access/federation | INDEX multi-source; REGISTRY single | L | — |
| cache Cas/DiskCas/Tiered | REGISTRY primary; INDEX L3 S3 adapter | M | Wire to emit path |
| cursor/score/search Page | SHARED | L | — |
| telemetry | INDEX server | L | feature stays |
| tenant | INDEX multi-tenant | L | Local single-tenant |

### 9.4 Target architecture mapping (prompt goals)

| Planned piece | Built from |
|---|---|
| INDEX service (sqlite + S3, k8s) | server binary − local-only paths + schema sqlite + Store S3 + queue→ORCH |
| REGISTRY local (sqlite + disk + tantivy + vectors) | registry subset + heart::DiskCas + embedded indexes + directory ingest |
| client library | server search/query/dto + Connect typestate Remote\|Local |
| orchestration server | new; absorbs queue SKIP LOCKED + Role::Forge scheduling |
| Terminus hot-only | gate materialize_graph behind admission; cold uses IR blobs + resolution.rs |
| Incremental symbol-level | needs new change detection; today package-generation outbox only |

---

## 10. Cross-cutting risks & gaps

1. **Dual text materializers** (outbox_consumer vs text::Poller) — pick one.
2. **No blob GC** — INDEX S3 will grow unbounded.
3. **Graph relations under-published** relative to read model.
4. **No Terminus admission** despite hot-tier plan.
5. **Symbol delete/reindex** on generation change incomplete (upsert only).
6. **downloads=0** ranking dead code paths.
7. **description** tantivy field unused.
8. **sqlx postgres-only** + sea-query-binder sqlx-postgres — dual backend is a real port, not a flip.
9. **libsqlite3-sys** present but inert — either use or drop.
10. **ForgeRuntime removal** means typestate precedent for compiler is only on stores; client Remote/Local must be designed fresh using heart::Connect.
11. **Session PG** couples gateway stickiness solution to postgres — migrate with index.
12. **Production object_store cloud features** may be disabled in current build.

---

## 11. Concrete recommendations (actionable)

1. **Define a `MetaStore` trait** over packages/parse_status/symbols/outbox with sqlx sqlite + postgres impls; move SKIP LOCKED behind `QueueBackend` trait with `PgSkipLocked` and `SqliteSingleWriter` and `K8sJobs`.
2. **Delete jobs table from INDEX** once ORCH exists; keep `parse_status` as lifecycle SoT in sqlite.
3. **Keep transactional outbox** on sqlite for both planes — it's the right pattern for derived indexes without 2PC.
4. **Unify CAS**: implement `heart::cache::Cas` for `object_store` adapter; REGISTRY uses `Tiered<DiskCas, ObjectStoreCas>` or Disk-only; INDEX uses S3 Cas + ptr layout unchanged.
5. **Split ingest** trusted/untrusted; directory walker for REGISTRY reusing jail_path optionally.
6. **Introduce GraphAdmission** (leaky bucket) at `record_stored` / materialize_graph — not only query gate.
7. **Symbol generation GC**: on package materialize, delete symbols in index/qdrant/tantivy not in new generation set (use resolution::diff).
8. **Promote heart::Connect** to client facade: `Client<Cold>` → connect remote or open local → `Client<Live>`.
9. **Move DTOs + Query + Planner** into a thin `nudox-client` crate early (low risk).
10. **Blob GC design spike** before S3 production (generation refs from sqlite + manifest walk).
11. **Enable object_store S3 features** in workspace deps when INDEX lands.
12. **Collapse text poller** into single outbox consumer path to avoid dual writers.

---

## 12. Executive summary

The live spine is already a clean **write-plane (registry) + HTTP mesh (server) + vocabulary (heart)** split. Postgres is the orchestration SoT for six core tables (`packages`, `parse_status`, `jobs`, `outbox`, `sink_watermarks`, `symbols`) plus an out-of-band `sessions` table. Schema is **sea_query-generated with no migrate files** — excellent for a SQLite re-target of the index data, terrible if you hoped SKIP LOCKED and advisory locks would come along for free. Those two features are the only true multi-worker postgres dependencies: the job queue and multi-replica outbox drain. Both map cleanly onto the librarification plan (**k8s for remote queue**, **single-writer sqlite + no advisory locks locally**).

Content addressing is mature: `cas/{blake3}` + `ptr/{package-uuid}`, first-write-wins section puts, dual-hash manifests (generation vs CAS key), BLAKE3 integrity on full reads. This is already the INDEX(S3)/REGISTRY(disk) shape; `heart::cache::{DiskCas,Tiered}` exists but is **not** yet the path `registry::store::Store` uses. Blob GC is explicitly unsolved.

Search is three-plane: package Tantivy (postgres `updated_at` watermark + ranking fusion), symbol Tantivy (ident tokenizer + 5-tier query), Qdrant (HTTP embeddings, SemanticGate quota, model-branded types). Terminus is a WOQL/document client with symbol upsert idempotency; **no LinkML pipeline in-tree**, no hot-tier admission, and relation writes look thinner than the read model.

Ingest is a strong security boundary (path jail, allowlist, budgets) and should be reused for local REGISTRY with a directory source. Server exposes 12 routes, Role-split pollers, and has **already removed ForgeRuntime** in favor of `CompilerClient`. heart's Cold/Live Connect, deterministic ids, ResolutionState, and Federation are the right substrate for the future client library.

**Bottom line:** Postgres removal is feasible without rewriting business logic if (1) queue moves to k8s on INDEX, (2) sea_query gains a sqlite backend for the remaining tables, (3) CAS stays as-is with disk/S3 backends, (4) Terminus becomes optional/hot-gated, (5) server thins into client+INDEX. Highest cost items: queue redesign, dual-SQL port, local vector/graph offline modes, and symbol-level incrementalism (today is package-generation granularity only).

---

## Appendix A — File index (primary)

```
workspace/registry/
  lib.rs, store.rs, coordination.rs, persist.rs, protocol.rs, resolve.rs, error.rs, identity.rs
  schema/{mod,queries,codec}.rs
  index/mod.rs, queue/mod.rs, blob/{mod,creation,emit}.rs
  ingest/{mod,extract}.rs, metadata/{mod,hash,heuristics,rich}.rs
  search/{mod,tantivy,ranking,multi_parent}.rs
  runtime/{mod,error,pagination,session}.rs
  runtime/text/{mod,index,poll,query,tokenizer}.rs
  runtime/vector/{mod,cache,embedding,gate,similarity,model/*}.rs
  runtime/graph/{mod,expansion,resolution,structure}.rs
  health/mod.rs

workspace/server/
  lib.rs, main.rs, config.rs, poll.rs, authz.rs, compiler_client.rs, error.rs
  http/{mod,router,dto}.rs, http/handlers/{mod,search,indexing,health,admin}.rs
  coordination/{mod,indexing,initialization,search,health}.rs
  search/{mod,planner,query,registry,symbols,semantic/*}.rs
  save/{mod,blobs}.rs

workspace/heart/
  lib.rs, connection.rs, content.rs, sink.rs, cursor.rs, score.rs, search.rs, ...
  identity/{id,package,symbol,namespace,mod}.rs
  error/{mod,failure,connect}.rs
  access/{federation,source,mod}.rs
  cache/{mod,disk,memory,tiered,stampede,single_flight,jitter,error}.rs
  package/coordinates.rs, telemetry/* (feature)
```

## Appendix B — Prompt checklist coverage

| # | Topic | Section |
|---|---|---|
| 1 | Postgres inventory | §1 |
| 2 | CAS/object store | §2 |
| 3 | Tantivy | §3 |
| 4 | Qdrant | §4 |
| 5 | Terminus | §5 |
| 6 | Ingest | §6 |
| 7 | Server routes/planner/poll/config | §7 |
| 8 | heart vocabulary | §8 |
| 9 | INDEX/REGISTRY mapping table | §9 |
| — | Executive summary | §12 |


---

## Appendix C — Dense file:line evidence ledger

### C.1 Postgres spine

| Claim | Evidence |
|---|---|
| No sqlx migrate; sea_query DDL on boot | `workspace/registry/schema/mod.rs:8–11` |
| Text CHECK not native ENUM | `schema/mod.rs:14–21` |
| packages PK uuid | `schema/mod.rs:264` |
| parse_status FK CASCADE | `schema/mod.rs:336–342` |
| jobs bigserial + unique package | `schema/mod.rs:351–352` |
| outbox bigserial seq | `schema/mod.rs:383` |
| outbox dedupe unique index | `schema/mod.rs:499–508` |
| idx_jobs_runnable partial predicate note | `schema/mod.rs:488–491` |
| PostgresQueryBuilder only | `schema/queries.rs:35–38` |
| dequeue SKIP LOCKED | `schema/queries.rs:414–417`; `queue/mod.rs:358–359` |
| lease guard complete/fail | `schema/queries.rs:439–442` |
| advisory lock class | `schema/queries.rs` outbox try_advisory_lock (~675–705); `coordination.rs:224–253` |
| record_stored single txn 3 steps | `coordination.rs:147–180` |
| gc_consumed min watermark floor | `coordination.rs:255–288` |
| SinkKind = DerivedStore alias | `coordination.rs:55–58` |
| reconcile Progressing on start | `persist.rs:34–82` |
| GlobalStore schema apply on connect | `index/mod.rs` Connect impl (~83–118) |
| sessions table migrate separate | `runtime/session.rs` PgSessionStore (~279–287 migrate) |
| session merge FOR UPDATE | nested audit / session.rs merge_into |
| SQLSTATE retryable 40001/40P01/57P03/53300 | `error.rs` ~620–621 |
| libsqlite3-sys present unused for spine | `registry/Cargo.toml:38` |
| sqlx features postgres only | `registry/Cargo.toml:28–36` |
| sea-query-binder sqlx-postgres | `registry/Cargo.toml:41–47` |
| text poller ANY($1) PG array | `runtime/text/poll.rs:54–56` |
| text poller PENDING_SQL outbox | `runtime/text/poll.rs:48–49` |
| watermark.json sidecar | `runtime/text/poll.rs:44–45` |

### C.2 CAS / blob

| Claim | Evidence |
|---|---|
| cas/ + ptr/ layout | `store.rs:4–16`, `97–116` |
| first-write-wins put_section bool | `store.rs:118–136` |
| verify before write | `store.rs:126` |
| put_manifest atomic pointer | `store.rs:182–189` |
| list_cas no delete | `store.rs:223–239` |
| heart Cas trait FWW | `heart/cache/mod.rs:83–108` |
| DiskCas/MemoryCas/Tiered | `heart/cache/mod.rs:26–48`, `70–73` |
| EvictableCas not on object store | `heart/cache/mod.rs:111–120` |
| two-hash manifest design | `blob/mod.rs` identity_bytes vs manifest_cas_key docs |
| emit: sections→manifest→outbox | `blob/emit.rs:41–65` |
| ExtractionLimits DEFAULT | `ingest/mod.rs:41–42` |
| EntryAllowlist SAFE only regular+dir | `ingest/extract.rs` EntryAllowlist |
| jail_path rejects .. absolute \\ NUL | `ingest/extract.rs` jail_path ~124–159 |

### C.3 Search / derived stores

| Claim | Evidence |
|---|---|
| package tantivy schema 6 fields | `search/tantivy.rs` schema fn |
| description never written | absorb path omits description |
| downloads hardcoded 0 | `search/mod.rs` collect_ranked_hits |
| ranking 5 stages | `search/ranking.rs` rank_pipeline |
| diversity min set 25 | `search/ranking.rs` RankingConfig default |
| symbol TextSchema 11 fields | `runtime/text/index.rs` TextSchema |
| ident tokenizer | `runtime/text/tokenizer.rs` IDENT_TOKENIZER |
| Enforced cursor for text | `runtime/text/query.rs` / text/mod.rs TextCursorKey |
| Advisory cursor for vector | `runtime/vector/mod.rs` SemanticCursorKey docs |
| SemanticGate non-Copy must_use | `runtime/vector/gate.rs:30–36` |
| E5 384 / OpenAI 1536 | `runtime/vector/model/catalog.rs:11–25` |
| HttpEmbedder OpenAI wire | `server/search/semantic/embedder.rs:17–45` |
| EmbeddingCache key model+text_hash | `runtime/vector/cache.rs` EmbeddingKey |
| insert_symbols full_replace=false | `runtime/graph/mod.rs:439–476` |
| WOQL mini DSL | `runtime/graph/mod.rs:334–377` |
| document model Symbol/Relation | `runtime/graph/mod.rs:323–331` |
| no LinkML in these crates | grep empty for LinkML under workspace/registry,server,heart |

### C.4 Server surface

| Claim | Evidence |
|---|---|
| read plane routes | `http/router.rs:169–177` |
| write mutations | `http/router.rs:187–190` |
| ops healthz/readyz/metrics | `http/router.rs:197–200` |
| admin verify/rebuild | `http/router.rs:210–211` |
| READ_PLANE 2MiB | `http/router.rs:31` |
| WRITE_PLANE 64KiB | `http/router.rs:36` |
| SearchPlanner 64/60s | `search/planner.rs:20–21,39–59` |
| authorize_similar dead | `search/planner.rs:65–69` (no route wire) |
| ForgeRuntime removed | `lib.rs:120–122` |
| SourceStores fields | `lib.rs:72–90` |
| connect_source concurrent | `lib.rs:239–306` |
| Role forge/gateway | `config.rs:102–124`; `lib.rs:386–400` |
| queue_worker drain | `poll.rs:42–65` |
| outbox_consumer advisory | `poll.rs:70–111` |
| cas_gc interval 1h | `poll.rs:29–32,313+` |
| Indexer phases A/E/C/E | `coordination/indexing.rs:86–127` acquire/extract |
| ingest TarGz SAFE DEFAULT | `indexing.rs:117–124` |
| config figment layers | `config.rs` resolve ~281+ |
| INDEXING_RETRY 5 attempts | `lib.rs:61–65` |
| EMBEDDING_CACHE 65536 | `lib.rs:68` |

### C.5 heart vocabulary

| Claim | Evidence |
|---|---|
| Id\<T\> | `identity/id.rs:10–29` |
| PackageId | `identity/package.rs:15–18` |
| Coordinates::id | `package/coordinates.rs:21–39` |
| EntryUri::symbol_id | `identity/symbol.rs:42–47` |
| ContentHash blake3 | `content.rs:10–20` |
| Cold/Live/Connect | `connection.rs:9–25` |
| Phase/Failure/FailureKind/ResolutionState | `error/failure.rs:27–99` |
| FailureKind retriable | `error/failure.rs:80–83` |
| DerivedStore | `sink.rs:37–41` |
| Federation | `access/federation.rs:45–96` |
| BackendKind | `error/mod.rs` |
| heart re-exports | `heart/lib.rs:31–51` |
| telemetry feature | `heart/Cargo.toml` + `lib.rs:28–29` |

### C.6 Portability decision matrix (condensed)

```
KEEP AS-IS (logic): blob, metadata, ranking, tokenizer, identity, Connect, ingest jail
PORT SQL (a): packages, parse_status, symbols, watermarks, outbox CRUD, changed_since
REDESIGN (b): outbox multi-replica (drop advisory), text ANY→IN, session store, dual sea-query
OBVIATE (c): jobs SKIP LOCKED queue → k8s ORCH on INDEX; multi-worker leases
OPTIONAL/HOT: Terminus Graph materialize; full Relation graph
DELETE/DEAD: authorize_similar route gap; downloads=0 path; description field; ForgeRuntime
```

### C.7 Suggested cut lines for librarification PRs (order)

1. Extract `nudox-client` types: Query/DTO/Planner/SemanticGate issuance policy (no infra).
2. Introduce `MetaStore` trait; implement existing PG behind it (no behavior change).
3. Add sqlite MetaStore for packages/parse_status/symbols/outbox; integration tests.
4. Wire `heart::cache::DiskCas` behind a Cas-shaped adapter used by local emit path.
5. Directory ingest source for trusted projects (reuse Budget/jail optionally).
6. ORCH prototype: enqueue = create k8s Job; drop jobs table on INDEX path.
7. GraphAdmission leaky bucket before Terminus write.
8. Symbol generation GC using `runtime/graph/resolution::diff`.
9. Thin server binary to INDEX-only; GUI depends on client lib.
10. Blob GC design + implementation once reference oracle exists.

### C.8 Open questions for architecture plan

1. Local REGISTRY vector store: still Qdrant embedded, or candle/ort in-process, or skip semantic locally?
2. Does INDEX sqlite live one-db-per-shard or single global sqlite (likely not) / litestream / turso?
3. Who owns orchestration server — new crate or server Role::Forge only talking to k8s API?
4. Federation overlays on INDEX: still N full stacks, or one stack + overlay CAS prefixes?
5. Tree-sitter resolved trees storage: separate cas objects or only IR+references today sufficient?
6. Should `parse_status` remain in REGISTRY when all work is local sync, or collapse to manifest-only?
7. Session graphs: stay multi-replica on INDEX or move to client-local only?
8. Producer version / JobKey interaction with registry generation stamp — single freshness model?

---

## Appendix D — Module-level LOC orientation (approx line counts)

| File | ~LOC | Density of PG/port risk |
|---|---|---|
| `schema/queries.rs` | 919 | **Critical** |
| `schema/mod.rs` | 571 | **Critical** |
| `queue/mod.rs` | 716 | **Critical** |
| `server/coordination/indexing.rs` | 756 | High (pipeline) |
| `registry/error.rs` | large | Medium (SQLSTATE) |
| `search/ranking.rs` | large | Low (pure) |
| `runtime/graph/mod.rs` | large | Medium (HTTP) |
| `runtime/vector/mod.rs` | 495 | Medium |
| `runtime/session.rs` | 397 | Medium (PG sessions) |
| `coordination.rs` | 357 | **Critical** |
| `store.rs` | 291 | Low-Med (S3 features) |
| `server/lib.rs` | 491 | High (assembly) |
| `server/poll.rs` | large | High |
| `server/config.rs` | large | Medium |
| `heart/cache/*` | split | Low (ready for REGISTRY) |

---

## Appendix E — End-to-end package lifecycle (evidence chain)

```
POST /packages
  http/handlers/indexing.rs → Server::ensure_initialized
    coordination/initialization.rs decision table
    GlobalStore::upsert (packages + parse_status Unindexed)
    Queue::enqueue (jobs ON CONFLICT DO NOTHING)

queue_worker (Role Forge|All)
  Indexer::run_worker_until
    Queue::dequeue_batch  ← SKIP LOCKED
    drive_job + lease heartbeat
      execute_acquire_phase   → Phase::Acquiring
      execute_extract_phase   → fetch archive + ingest_archive (SAFE, DEFAULT limits)
      execute_compile_phase   → CompilerClient postcard /compile
      execute_emit_phase      → BlobBuilder.finalize
                                blob::emit::emit → Store.put_section* + put_manifest
                                Outbox.record_stored  ← ONE TXN: Stored + facets + outbox rows

outbox_consumer (Role Gateway|All) per SinkKind
  try_lock_sink (advisory)
  read_watermark → read_since → materialize → advance_watermark
    Text   → TextIndex.upsert_batch
    Vector → embed + qdrant upsert
    Graph  → insert_symbols

package_index_poller
  PackageSearchIndex.synchronize ← changed_since(updated_at)

GET /readyz aggregates BackendKind probes (Postgres, ObjectStore required for Health::Down)
```

This chain is the cut-surface map: everything above `dequeue_batch` becomes ORCH+worker on INDEX; everything from emit down becomes dual (INDEX materializers vs REGISTRY local materializers); `ensure_initialized` becomes client API for both remote enqueue and local sync.

