# SQLite for the Remote INDEX (replacing Postgres)

**Research date:** 2026-07-16  
**Scope:** Deploy SQLite as the remote INDEX (package identity, ResolutionState, transactional outbox, watermarks, session graphs) after the job queue moves to Kubernetes; compare vs local REGISTRY; recommend crates, topology, migration path.  
**Code anchors:** `workspace/registry/` — especially `schema/mod.rs`, `schema/queries.rs`, `index/mod.rs`, `queue/mod.rs`, `coordination.rs`, `Cargo.toml`.  
**Related research:** orchestration (job queue → k8s), client-sync (14), storage (13).

---

## Executive summary (preview)

**Primary recommendation:** run the INDEX as **one writer process** on a **Kubernetes StatefulSet (`replicas: 1`)** with **SQLite in WAL mode on a PVC**, continuous backup via a **Litestream v0.5.x sidecar** streaming to **S3**, and **restore-on-boot** via Litestream init container. Do **not** put multi-writer Raft (rqlite/dqlite) or Turso Cloud under the INDEX unless HA RTO requirements force it later.

**Fallback:** **rqlite v10.x** (Raft HA, Kubernetes StatefulSet) if multi-node automatic failover is mandatory before ops can accept single-writer + Litestream restore RTO. Accept lower write throughput and HTTP SQL surface.

**Reject for INDEX primary:** LiteFS (limited maintenance; Fly-centric; Cloud sunset; lease footguns), Turso Cloud / libSQL server as sole SoT (product pivot; closed-source multitenant server; edge-replica simplification), cr-sqlite multi-master (not a global package SoT), multi-connection write pools without app-level single-writer.

**Rust stack:** keep **sqlx 0.8** with `sqlite` (bundled) + dual pools (writer `max_connections(1)`, reader pool N); keep **sea-query** (0.32 today; evaluate 1.x) with `SqliteQueryBuilder` + `sea-query-binder` `sqlx-sqlite`; desktop REGISTRY: **rusqlite** (`bundled` + hooks) on a dedicated thread. Schema as shared DDL/migrations; optional shared sea-query Idens.

**Workload fit:** INDEX writes are bursty package/version/symbol metadata + outbox appends — well below SQLite batched WAL ceilings (tens–hundreds of k rows/s with batching). Reads dominate and are cacheable. Queue/`SKIP LOCKED` leaves the critical path (k8s). Outbox + watermark pollers map cleanly to single-node SQLite.

---

## 0. What Postgres owns today (code map)

| Concern | Module | Postgres mechanism | Post-migration home |
|---|---|---|---|
| Package identity | `registry/index/mod.rs`, `schema` `packages` | `uuid` PK, upsert `ON CONFLICT` | INDEX sqlite `packages` |
| ResolutionState machine | `parse_status` | text CHECK + jsonb `failure`/`facets` | INDEX sqlite |
| Symbols projection | `symbols` | uuid + bytea generation | INDEX sqlite |
| Job queue | `registry/queue/mod.rs` | `FOR UPDATE SKIP LOCKED` + leases | **Out → k8s** (separate research) |
| Transactional outbox | `registry/coordination.rs` | same-txn append; bigserial `seq` | INDEX sqlite outbox |
| Sink watermarks | `sink_watermarks` | per-sink cursor | INDEX sqlite |
| Outbox exclusive drain | `pg_try_advisory_lock` | session advisory lock | Single poller process / lease row / k8s leader |
| JSON payloads | `Toolchain`, `Failure`, `SearchFacets` | `jsonb` | sqlite JSON/JSONB TEXT or BLOB |
| DDL boot | `schema_ddl` via sea-query Postgres | IF NOT EXISTS create | sqlite dialect + migrations |
| Query build | `schema/queries.rs` | `PostgresQueryBuilder` + sqlx-postgres | `SqliteQueryBuilder` + sqlx-sqlite |
| Blobs | S3 / object_store | already not Postgres | unchanged |
| FTS | Tantivy | already not Postgres | unchanged |
| Vectors | Qdrant | already not Postgres | unchanged |

**Cargo today** (`workspace/registry/Cargo.toml`):

```toml
sqlx = { version = "0.8", features = [
  "runtime-tokio-rustls", "macros", "postgres",
  "uuid", "json", "chrono", "migrate",
] }
libsqlite3-sys = { version = "0.30", features = ["bundled"] }  # already present, unused for INDEX
sea-query = { version = "0.32" }
sea-query-binder = { version = "0.7", features = ["sqlx-postgres", ...] }
```

Design intent in `schema/mod.rs` already favors portability: **no Postgres ENUMs** (text + CHECK), **no LISTEN/NOTIFY dependency in schema**, outbox is classic append + watermark poll (not NOTIFY-driven). The hard Postgres-specific pieces are: `FOR UPDATE SKIP LOCKED`, `pg_try_advisory_lock`, `uuid`/`jsonb`/`timestamptz` column types as rendered by sea-query Postgres, and `RETURNING` patterns that sqlx/Pg support cleanly (SQLite has RETURNING since 3.35).

---

## 1. SQLite as a server-side store in 2026

### 1.1 Pattern matrix

| Pattern | Write scale | Read replicas | Failure/restore | k8s fit | Maturity 2026 | INDEX verdict |
|---|---|---|---|---|---|---|
| **(a) Plain SQLite + WAL on PVC, single-writer service** | 1 writer; high batched TPS | Same-node multi-reader; no live remote replicas | PVC durability; app-level backup | StatefulSet `replicas:1` + RWO PVC | Excellent (SQLite itself) | **Base of primary** |
| **(b) Litestream v0.5.x → S3** | Same as (a) | No live read replica; restore-on-boot / offline copies | Continuous WAL ship; RPO ~1s; PITR in v0.5 | Official sidecar + init restore guide | **Active; primary backup story** | **Primary add-on** |
| **(c) LiteFS (FUSE FS + lease)** | 1 primary writer; replicas get LTX stream | Yes (read replicas) | Lease + LTX; Cloud gone | Fly-native; awkward pure k8s | Stable but **limited maintenance**; pre-1.0; no support commitment | **Not primary** |
| **(d) libSQL / Turso** | Depends (cloud primary; embedded replica local reads) | Embedded replicas / (legacy edge) | Cloud managed; product in flux | Optional managed; self-host libSQL | libSQL production-capable; **Rust Turso engine beta**; cloud server closed | **Sync tool / not INDEX SoT** |
| **(e) cr-sqlite / CRDT** | Multi-writer merge | Peer merge | CRDT reconcile | Poor for central SoT | Experimental / niche | **Reject for INDEX** |
| **(f) rqlite / dqlite (Raft)** | Leader-only writes; Raft tax | Read-only followers | Raft HA | StatefulSet + Helm | rqlite **v10.x (Jul 2026)** mature | **Fallback HA** |

### 1.2 (a) Plain SQLite + WAL, single-writer service

**How it works:** One process opens the DB file on a local filesystem (PVC). WAL mode allows concurrent readers while one writer holds the write lock. Kubernetes enforces single primary via `replicas: 1` StatefulSet (or lease election if you later scale out carefully — not recommended initially).

**Write ceiling:** Single writer serializes transactions. With `BEGIN IMMEDIATE` + multi-row batches, modern NVMe routinely supports **10k–100k+ small-row inserts/s** (and commonly cited ~**500k rows/s** under ideal batching). Per-transaction fsync (depending on `synchronous`) dominates latency more than row count.

**Read story:** Many connections/readers in WAL. App-level caches (moka already in tree) further reduce pressure.

**Failure path:** PVC survives pod restart. Node/disk loss loses local state unless continuous backup (→ Litestream).

**k8s:** Canonical: StatefulSet + `volumeClaimTemplates` RWO + anti-affinity not required for n=1.

**Ops maturity:** SQLite itself is the most mature piece in this matrix. The *service* maturity is about your process discipline (single writer, busy_timeout, checkpointing).

### 1.3 (b) Litestream v0.5.x — **recommended continuous backup**

**Status (verified 2026-07-16):**

- Litestream v0.5.0 announced **2025-10-02** ([Fly blog](https://fly.io/blog/litestream-v050-is-here/)); series actively maintained as **v0.5.x** ([install docs](https://litestream.io/install/), [migration guide](https://litestream.io/docs/migration/) references builds such as **v0.5.14**).
- v0.5 changes: pure-Go sqlite driver (`modernc.org/sqlite`, no CGO), page-based replication with efficient **point-in-time recovery**, NATS JetStream replica type, MCP support in later 0.5.x.
- Early 0.5.0 had restore bugs; community advised waiting for **0.5.2+** ([mtlynch notes](https://mtlynch.io/notes/hold-off-on-litestream-0.5.0/)). Pin a patched 0.5.x, not bare 0.5.0.
- Official Kubernetes guide: [https://litestream.io/guides/kubernetes/](https://litestream.io/guides/kubernetes/)

**Architecture (official):**

1. StatefulSet `replicas: 1`
2. Shared PVC mounted at data dir for app + Litestream
3. **Init container:** `litestream restore -if-db-not-exists -if-replica-exists /path/db` (image `litestream/litestream:0.5`)
4. **Sidecar:** `litestream replicate` continuous ship to `s3://bucket/db`
5. Metrics on `:9090` via `addr` in config
6. Secrets for `LITESTREAM_ACCESS_KEY_ID` / `SECRET`

**Write scalability:** Unchanged from single SQLite writer; Litestream overhead is small (WAL frame shipping).

**Read replicas:** Litestream does **not** provide live multi-primary or hot read replicas in the Postgres sense. You get durable off-site copies and restore. (Live read scaling is local multi-reader or separate app caches.)

**Failure/restore:**

| Scenario | Behavior |
|---|---|
| Pod restart, PVC intact | App reopens local DB; Litestream resumes |
| PVC lost | Init restore from S3 before app starts |
| Bad deploy / logical corruption | Use v0.5 PITR to restore to timestamp |
| S3 lag | RPO typically ~1 second of WAL shipping under normal config |

**k8s fit:** Excellent — documented pattern; fits nudox INDEX exactly.

**Caveats:**

- Only one live writer node (Litestream docs: single node at a time; multi-live-replica still "upcoming").
- Do not enable WAL2 if incompatible with Litestream (community reports WAL2 incompatibility — stick to classic WAL).
- Restore latency scales with DB size + number of LTX/segments; multi-GB is fine but budget boot time.

### 1.4 (c) LiteFS — not recommended as INDEX primary

**Status:**

- Open-source LiteFS still documented by Fly ([docs](https://fly.io/docs/litefs/)): "stable and running in production", **pre-1.0**, APIs may change.
- **LiteFS Cloud sunset** Oct 15, 2024 ([community](https://community.fly.io/t/sunsetting-litefs-cloud/20829)).
- Fly staff (Feb 2025): not discontinued but **limited updates / refocused effort** ([thread](https://community.fly.io/t/what-is-the-status-of-litefs/23883)).
- Docs banner (2026): *"We are not able to provide support or guidance for this product. Use with caution."*
- Operational risk: combining with Fly autostop/autostart can cause lease races and data loss (explicit Fly warning).

**Why skip for nudox INDEX:** You need a k8s-first, low-ops story with clear S3 backup. LiteFS's FUSE + Consulease world is Fly-shaped, under-supported, and adds complexity without solving a problem Litestream + single-writer already solves for this workload.

### 1.5 (d) libSQL / Turso

**Naming (2026):**

| Name | What it is | Status |
|---|---|---|
| **libSQL** | Open-source **fork of SQLite (C)** with extensions (replication, remote, etc.) | Production-grade engine lineage |
| **Turso (engine)** | **Rust rewrite** of SQLite (formerly "Limbo"), MIT client-side | **Beta / evolving**; MVCC concurrent writes story |
| **Turso Cloud** | Managed service | Product simplified: edge replicas discontinued for *new* users; multitenant server **closed source**; client remains open |

Sources: [Turso roadmap post](https://turso.tech/blog/upcoming-changes-to-the-turso-platform-and-roadmap) (Jan 2025), [sync benchmark / migrate off libsql sync](https://turso.tech/blog/sync-benchmark) (Apr 2026), [libSQL GitHub](https://github.com/tursodatabase/libsql), [Turso engine](https://github.com/tursodatabase/turso).

**Embedded replicas (libsql crate):**

- Local file + remote primary URL/token.
- Reads local; writes go to primary; optional `read_your_writes` / `sync()`.
- Classic embedded replica: **offline reads OK; offline writes require remote** (local-first offline writes are the newer Turso sync path / beta territory).
- **Does not** natively implement "remote serves HTTP until local catches up then flip" — local is always the read surface once replica is open.

**INDEX verdict:** Do **not** make Turso Cloud the source of truth for the global package index. Self-hosting libSQL *server* adds another distributed system. Prefer vanilla SQLite + Litestream for INDEX durability. Consider libsql/turso **only** for desktop REGISTRY sync experiments (see §6), with app-level sync as the conservative default.

### 1.6 (e) cr-sqlite / CRDT extensions

[vlcn-io/cr-sqlite](https://github.com/vlcn-io/cr-sqlite): multi-master merge via CRDTs. Useful for collaborative local-first docs — **wrong model** for a single authoritative package ResolutionState machine. Conflict-free merge of `parse_status` is a product bug, not a feature. **Reject for INDEX.**

### 1.7 (f) rqlite / dqlite — HA fallback

**rqlite** ([rqlite.io](https://rqlite.io/), latest **v10.2.7** as of Jul 6, 2026 on GitHub): Raft consensus, SQLite under the hood, HTTP API, Kubernetes guides/Helm, read-only nodes for read scale.

**Performance reality** ([performance guide](https://rqlite.io/docs/guides/performance/), updated 2026):

- Writes go through Raft + fsync; **10–hundreds req/s** typical single statements; bulk API/transactions can gain ~100×.
- Explicit design: **HA and fault tolerance, not write scaling**. Writes slower than standalone SQLite.
- v9.2+ improved multi-GB restart (resume vs full rebuild).

**dqlite:** Canonical's Raft SQLite (C-Go), used in LXD — less Rust-friendly ecosystem for this stack.

**When to choose rqlite:** Multi-AZ automatic failover with RTO of seconds and ops cannot tolerate Litestream restore + single pod. Cost: write path, operational cluster, SQL over HTTP (not native sqlx file), weaker fit with existing sea-query/sqlx transaction model.

### 1.8 Recommendation: primary + fallback

| Role | Choice | Why |
|---|---|---|
| **PRIMARY** | SQLite WAL file on PVC + **Litestream 0.5.x+** sidecar/init + S3 | Matches workload (single logical writer), official k8s pattern, low RPO, keeps sqlx/sea-query file semantics, no Raft tax |
| **FALLBACK** | **rqlite v10.x** StatefulSet | If automatic multi-node HA becomes a hard requirement |
| Explicit non-choices | LiteFS, Turso Cloud as SoT, cr-sqlite multi-master | Maintenance/product/model mismatch |

---

## 2. Concurrency reality check

### 2.1 SQLite locking model (WAL)

- **One writer** at a time per database file.
- **Many readers** concurrent with a writer in WAL mode.
- Writers queue; `SQLITE_BUSY` if timeout exceeded.
- `BEGIN DEFERRED` upgrades to write lock late → more `BUSY` under contention.
- **`BEGIN IMMEDIATE`** takes RESERVED lock up front — **required discipline** for write transactions in a multi-connection service.

Official: [File Locking And Concurrency](https://sqlite.org/lockingv3.html), [WAL](https://sqlite.org/wal.html).

### 2.2 Realistic write throughput

| Pattern | Order-of-magnitude |
|---|---|
| One row per transaction, `synchronous=FULL` | Hundreds–low thousands TPS (fsync bound) |
| Batched multi-row txn, `synchronous=NORMAL` + WAL | **10k–100k+ rows/s** common |
| Highly tuned batch insert benchmarks | Often cited **~100k–500k rows/s** |

Sources/context: [phiresky performance tuning](https://phiresky.github.io/blog/2020/sqlite-performance-tuning/), production write anecdotes, [Evan Schwartz dual-pool benchmarks](https://emschwartz.me/psa-your-sqlite-connection-pool-might-be-ruining-your-write-performance/) (pool shape matters more than raw SQLite).

**INDEX workload placement:**

- Writes: package upsert, `parse_status` transition, symbol rows, outbox rows — **bursty on ingest**, small rows.
- Even aggressive multi-package ingest (e.g. 1k packages × 100 symbols = 100k rows) fits in seconds under batching.
- Steady-state is **read-heavy** (resolution lookups, watermark polls, API).
- **Conclusion:** well under ceiling if you batch symbol inserts and use single writer connection.

### 2.3 WAL checkpoint tuning

| Pragma / op | Role |
|---|---|
| `journal_mode=WAL` | Concurrent readers |
| `wal_autocheckpoint` (default 1000 pages) | Passive PASSIVE checkpoint threshold |
| `wal_checkpoint(PASSIVE)` | Non-blocking; may not finish if readers hold snapshots |
| `wal_checkpoint(RESTART/TRUNCATE)` | More aggressive; can stall writers briefly |
| Litestream | Interacts with WAL shipping — avoid fighting it with exotic checkpoint storms |

Practice: leave autocheckpoint default unless WAL file grows unbounded under long readers; monitor WAL size; avoid long-lived read transactions.

### 2.4 `busy_timeout`

```sql
PRAGMA busy_timeout = 5000;  -- ms
```

Without this, concurrent writers/readers under load fail fast with "database is locked". Set on **every** connection (sqlx `after_connect` / connect options).

### 2.5 sqlx 0.8 pool semantics — critical pattern

**Anti-pattern:** one big `SqlitePool` with many connections for mixed read/write → writers contend at SQLite **and** thrash connection acquisition. Evan Schwartz measured ~**20×** throughput difference:

| Config | Throughput (illustrative) |
|---|---|
| Single pool, 50 conns | ~2.6k rows/s, catastrophic P99 |
| Single writer conn | ~60k rows/s |

Source: [https://emschwartz.me/psa-your-sqlite-connection-pool-might-be-ruining-your-write-performance/](https://emschwartz.me/psa-your-sqlite-connection-pool-might-be-ruining-your-write-performance/)

**Recommended dual-pool (INDEX server):**

```rust
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;

let write_opts = SqliteConnectOptions::from_str("sqlite:///var/lib/nudox/index.db")?
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .busy_timeout(std::time::Duration::from_secs(5))
    .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

let read_opts = write_opts.clone().read_only(true);

// Single writer — serializes all writes at the pool
let writer = SqlitePoolOptions::new()
    .max_connections(1)
    .connect_with(write_opts)
    .await?;

// Many readers
let reader = SqlitePoolOptions::new()
    .max_connections(num_cpus::get().max(4) as u32)
    .connect_with(read_opts)
    .await?;
```

**Optional:** channel-based write queue (all write SQL goes through one tokio task) for explicit batching of outbox + state transitions.

**sqlx features to enable:**

```toml
sqlx = { version = "0.8", default-features = false, features = [
  "runtime-tokio-rustls",
  "macros",
  "sqlite",          # bundled libsqlite3
  "uuid", "json", "chrono", "migrate",
] }
# drop "postgres" when cutover completes
```

`query!` / `query_as!` **work for SQLite** with compile-time checking (live DB or `cargo sqlx prepare` offline cache). Types differ from Postgres (no native uuid — often TEXT/BLOB; see §3).

### 2.6 BEGIN IMMEDIATE discipline

For every write transaction that might contend:

```sql
BEGIN IMMEDIATE;
-- business writes + outbox insert
COMMIT;
```

In sqlx, use explicit transaction options / raw `BEGIN IMMEDIATE` rather than deferred default when doing multi-statement writes.

---

## 3. Migration mapping: Postgres features → SQLite

### 3.1 Feature replacement table

| Postgres feature (today) | Used for | SQLite replacement | Notes |
|---|---|---|---|
| `LISTEN/NOTIFY` | (not primary path; pollers use watermarks) | Outbox watermark poll; optional `PRAGMA data_version`; hooks in-process | Polling already matches architecture |
| `FOR UPDATE SKIP LOCKED` | Job dequeue (`queries::queue::dequeue_batch`) | **Removed** (queue → k8s). If needed: single-writer `UPDATE...RETURNING` claim | No row-level locks in SQLite |
| `pg_try_advisory_lock` | Exclusive outbox drain per sink | One drain task per process; or k8s lease; or `locks` table with single-row claim | Session locks don't exist |
| `jsonb` | `toolchain`, `failure`, `facets` | `TEXT` JSON or SQLite **JSONB BLOB** (3.45+) via `jsonb()` | **Not** wire-compatible with PG jsonb |
| `uuid` type | PackageId, SymbolId, tenants | `BLOB(16)` or `TEXT` RFC4122 | Prefer BLOB for compactness; keep codec |
| `bytea` | content_hash, generation | `BLOB` | Direct map |
| `timestamptz` | created_at, lease_until, etc. | `TEXT` ISO-8601 or `INTEGER` unix ns/ms | Recommend **INTEGER unix ms** for INDEX simplicity + sortability |
| Arrays | (minimal — not heavily used) | JSON arrays or junction tables | Prefer junction if queried |
| Partial indexes | e.g. `idx_jobs_runnable` | **Supported** `CREATE INDEX ... WHERE` | sea-query supports |
| `ON CONFLICT` upsert | packages, parse_status, jobs, outbox | **Supported** | sea-query OK |
| `RETURNING` | enqueue, dequeue, complete | **Supported** (3.35+) | Keep patterns |
| Triggers | (none required today) | Supported | Optional |
| FTS | Tantivy externally | FTS5 available but **don't replace Tantivy** | Note only |
| CHECK constraints | state enums as text | Supported | Keep design from `schema/mod.rs` |
| FK + ON DELETE CASCADE | parse_status → packages | Supported (must `PRAGMA foreign_keys=ON`) | Enable every connection |
| `bigserial` | jobs.id, outbox.seq | `INTEGER PRIMARY KEY AUTOINCREMENT` | Monotonic seq preserved |
| Concurrent multi-worker lease queue | queue module | k8s jobs / work queue | Out of sqlite scope |

### 3.2 LISTEN/NOTIFY alternatives (detail)

Nudox already uses **watermark polling** on outbox (`read_since`, `advance_watermark`) — the right pattern for SQLite.

Additional tools if needed:

1. **`PRAGMA data_version`** — integer changes when *another connection* commits. Cheap change detector for "should I poll?". Does **not** change for commits on the same connection. ([SQLite forum discussions](https://sqlite.org/forum/info/e4dd574a6a5d0d29))
2. **`update_hook` / `commit_hook`** — process-local only (same connection / C API). Great for GUI; useless across pods.
3. **Honker** ([github.com/russellromney/honker](https://github.com/russellromney/honker)) — extension emulating NOTIFY-like fanout; optional, not required.
4. **Outbox table + 100–1000 ms poll** — sufficient for Qdrant/Tantivy/Terminus materializers.

### 3.3 SKIP LOCKED if ever needed on SQLite

SQLite has **no** `SKIP LOCKED`. Patterns:

```sql
-- Single writer process only:
BEGIN IMMEDIATE;
UPDATE jobs
SET lease_until = ?, attempts = attempts + 1
WHERE rowid IN (
  SELECT rowid FROM jobs
  WHERE lease_until IS NULL OR lease_until < ?
  ORDER BY priority DESC, enqueued_at ASC
  LIMIT ?
);
-- then SELECT the claimed rows
COMMIT;
```

With a **single writer pool**, two app workers cannot truly interleave claims inside one DB file without cooperating through that writer. For multi-worker job execution, **k8s is the right layer** (mission plan already moves queue out).

### 3.4 JSON / JSONB in SQLite

From [sqlite.org/json1.html](https://sqlite.org/json1.html) (updated 2026-06-24):

- JSON functions built-in since 3.38.
- **JSONB** binary storage since **3.45.0 (2024-01-15)** — internal parse tree as BLOB; faster than re-parsing text; **not** PostgreSQL jsonb format.
- `jsonb_*` function family; `jsonb_each` / `jsonb_tree` since **3.51.0 (2025-11-04)**.
- Operators `->` and `->>` available.

**Recommendation for INDEX:** store `failure` / `facets` / `toolchain` as **TEXT JSON** initially (simplest sqlx `Json<T>` interop) OR as JSONB BLOBs if profiling shows parse cost. Do not expect PG jsonb GIN-style indexing — use generated columns / expression indexes if needed.

### 3.5 sqlx / sea-query portability

**What breaks moving Postgres → SQLite:**

| Area | Breakage | Fix |
|---|---|---|
| `sea_query::PostgresQueryBuilder` | Wrong dialect (`$1` vs `?`, types) | Parameterize builder: `SqliteQueryBuilder` |
| `sea-query-binder` feature `sqlx-postgres` | Wrong bind | Enable `sqlx-sqlite` |
| `ColumnDef::uuid()` / `json_binary()` / `timestamp_with_time_zone()` | PG-oriented DDL | Map to `blob`/`text`/`integer` explicitly for sqlite |
| `sqlx::PgPool` / `Postgres` | Types | `SqlitePool` |
| `query!` offline cache | PG descriptors | Re-`sqlx prepare` against sqlite |
| `LockBehavior::SkipLocked` in sea-query | Emits PG/MySQL SQL | Delete with queue move |
| `Expr::current_timestamp()` | OK on both but type differs | Prefer app-supplied unix ms |
| Array / some PG functions | N/A or different | Avoid |
| Migrations | Today: runtime sea-query DDL, not `sqlx::migrate!` | Introduce versioned SQL migrations for sqlite (or dual-render DDL) |

**sea-query status:** Project on **0.32** (`Cargo.toml`). crates.io shows sea-query activity through **2026-05**. 0.32 already has `SqliteQueryBuilder` and `SqliteExpr` dialect traits ([SeaQL 0.32 notes](https://www.sea-ql.org/blog/2024-12-03-whats-new-in-seaquery-0.32.x/)). Upgrading to 1.x is optional; not required for sqlite cutover.

**Portability strategy (recommended):**

1. Keep **sea-query Idens** (`Packages`, `ParseStatus`, ...) as shared schema AST.
2. Introduce `type Db = SqliteQueryBuilder` (or generic `Q: QueryBuilder`).
3. Hand-write **sqlite-specific type mapping** in `create_*` functions (blob/text/integer).
4. Prefer **versioned `.sql` migrations** executed by `sqlx::migrate!` for deploy safety; use sea-query for dynamic DML.
5. Avoid `query!` until schema stable; runtime `query_as` is fine during port.

### 3.6 Concrete column type map for INDEX schema

| Column (today PG) | SQLite type | Codec notes |
|---|---|---|
| `packages.id uuid` | `BLOB NOT NULL` (16) or `TEXT` | Keep `uuid` crate; encode/decode in `schema/codec.rs` |
| `packages.toolchain jsonb` | `TEXT NOT NULL` | `serde_json` |
| `parse_status.failure/facets jsonb` | `TEXT` NULL | same |
| `parse_status.content_hash bytea` | `BLOB` | 32 bytes |
| `outbox.seq bigserial` | `INTEGER PRIMARY KEY AUTOINCREMENT` | watermark cursor |
| `outbox.generation bytea` | `BLOB NOT NULL` | |
| `*.timestamptz` | `INTEGER NOT NULL` (unix ms) | app sets `Utc::now()` |
| CHECK text enums | unchanged | portable |

**Foreign keys:** `PRAGMA foreign_keys = ON` on every connection (sqlx does not always enable by default historically — set explicitly).

---

## 4. Schema / topology for the INDEX

### 4.1 One DB vs several

| Topology | Pros | Cons | Recommendation |
|---|---|---|---|
| **Single `index.db`** | Simple backup (one Litestream target); atomic txn across packages/state/outbox | One write lock domain | **Start here** |
| Split `packages.db` + `sessions.db` + `traces.db` | Separate writer lanes; isolate session churn | Cross-db atomicity needs multi-file commit (supported but slower); more ops | Split **sessions** later if write contention appears |
| ATTACH at runtime | JOIN across files | mmap budget per file; complexity | Optional |

**Correctness note (re-verified):** SQLite **does** support multi-database transactions via ATTACH with a multi-journal commit protocol when more than one DB is written ([ATTACH](https://sqlite.org/lang_attach.html), [isolation](https://sqlite.org/isolation.html)). Atomicity holds; performance is worse than single-file commits. Prefer single file for core INDEX (`packages` + `parse_status` + `symbols` + `outbox` + `sink_watermarks`).

**Sessions:** per-session exploration graphs can later live in `sessions.db` (or even ephemeral) so interactive write load never blocks ingest/outbox.

### 4.2 Suggested INDEX tables (post-queue-extraction)

Keep from today:

- `packages`
- `parse_status`
- `symbols`
- `outbox`
- `sink_watermarks`

Drop or stop using on INDEX:

- `jobs` (→ k8s orchestration)

Optional:

- `schema_migrations` (sqlx)
- `processed_events` / consumer idempotency if sinks need it inside sqlite (usually sink-side)
- `sync_meta` for client pull cursors (if INDEX serves sync protocol)

### 4.3 Pragma baseline (read-heavy multi-GB)

```sql
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;     -- safe with WAL for most deployments
PRAGMA temp_store = MEMORY;
PRAGMA busy_timeout = 5000;
PRAGMA mmap_size = 8589934592;   -- 8 GiB; raise toward DB size if RAM allows
PRAGMA cache_size = -8000;       -- 8 MiB page cache; OS cache/mmap does heavy lifting
PRAGMA wal_autocheckpoint = 1000;
-- page_size only at creation:
-- PRAGMA page_size = 8192;      -- or 16384; then VACUUM on empty DB
PRAGMA optimize;                 -- on open + periodically
```

Sources: [phiresky](https://phiresky.github.io/blog/2020/sqlite-performance-tuning/), production multi-GB reports ([Charisol 250GB read path](https://medium.com/charisol-pulse/sqlite-doesnt-care-about-your-scaling-assumptions-how-i-served-250gb-at-40ms-ab1e264e66b0) — specialized read-only extremes).

**page_size:** default 4096 fine for OLTP point lookups. 8192–16384 reasonable for INDEX. Set **before** first data; changing requires rebuild/VACUUM.

### 4.4 VACUUM / ANALYZE ops

| Op | When | Notes |
|---|---|---|
| `PRAGMA optimize` | Connect + hourly | Safe auto-ANALYZE subset |
| `ANALYZE` | After bulk ingest waves | Planner stats |
| `incremental_vacuum` | If auto_vacuum=INCREMENTAL | Rarely needed if little delete churn |
| `VACUUM INTO 'path'` | Maintenance window | Compact copy without long exclusive on live file; swap carefully |
| Full `VACUUM` | Rare | Blocks writers; needs free space ≈ DB size |

### 4.5 Backup: Litestream vs `.backup`

| | Litestream | `sqlite3_backup` / `.backup` |
|---|---|---|
| RPO | ~seconds continuous | Batch interval |
| PITR | v0.5 yes | Point copies only |
| INDEX | **Yes — primary** | Secondary/export |
| Desktop REGISTRY | Overkill | **Yes** — periodic local/export |

### 4.6 Size limits

- Theoretical: very large (281 TiB class limits).
- Practical multi-GB–tens of GB on NVMe with mmap: widely reported workable.
- 250GB class needs careful I/O tuning and is **read-path** territory — INDEX metadata should stay far smaller (hashes in S3, vectors in Qdrant, text in Tantivy).
- **Plan:** keep INDEX relational metadata lean; blobs stay in object_store.

---

## 5. Transactional outbox without Postgres

### 5.1 Pattern (works on single-node SQLite)

Same atomicity guarantee as today: **business state + outbox rows in one transaction**.

```sql
BEGIN IMMEDIATE;
UPDATE parse_status SET state = 'stored', ... WHERE package_id = ?;
INSERT INTO outbox(package_id, generation, sink_kind)
  VALUES (?, ?, 'tantivy')
  ON CONFLICT DO NOTHING;
-- ... other sinks ...
COMMIT;
```

This matches `Outbox::record_stored` intent in `coordination.rs`.

### 5.2 Poller

```sql
SELECT seq, package_id, generation, sink_kind
FROM outbox
WHERE sink_kind = ? AND seq > ?
ORDER BY seq
LIMIT 100;
-- materialize to sink, then:
UPDATE sink_watermarks SET last_seq = ?, updated_at = ? WHERE sink_kind = ?;
```

**Exactly-once-ish:**

- Delivery to sinks is **at-least-once** (crash between materialize and watermark advance → redelivery).
- Sinks must be **idempotent** on `(package_id, generation)` or event id (already natural for content-addressed generations).

### 5.3 No SKIP LOCKED; no advisory locks

Exclusive drain today uses `pg_try_advisory_lock` (`queries::outbox::try_advisory_lock`). Replacements:

1. **Single INDEX writer process** with one poller task per sink (simplest).
2. **Kubernetes leader election** / lease for the drain Deployment if app is scaled (but DB is single-writer anyway — scale drains only if multiple processes).
3. **Lease row table** if multiple processes ever share one DB (discouraged).

### 5.4 Schema sketch

```sql
CREATE TABLE outbox (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  package_id BLOB NOT NULL,
  generation BLOB NOT NULL,
  sink_kind TEXT NOT NULL CHECK (sink_kind IN (/* DerivedStore tokens */)),
  created_at INTEGER NOT NULL,
  UNIQUE (package_id, generation, sink_kind)
);
CREATE INDEX idx_outbox_sink_seq ON outbox (sink_kind, seq);

CREATE TABLE sink_watermarks (
  sink_kind TEXT PRIMARY KEY,
  last_seq INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);
```

Optional retention: `DELETE FROM outbox WHERE seq < min_watermark - N` periodically (today's reclaim path in coordination).

### 5.5 k8s-native alternatives

| Approach | Pros | Cons |
|---|---|---|
| In-process poller | Same as today; lowest latency | Tied to app process |
| Sidecar poller | Isolation | Must share DB carefully (prefer read + single writer API) |
| CronJob drain | Simple | Higher latency; still needs single-writer access |
| NATS/Kafka outbox forwarder | Scale consumers | Extra infra — usually unnecessary |

**Recommendation:** keep **in-process outbox pollers** in the INDEX service; they already match derived-store architecture (Tantivy/Qdrant/Terminus).

---

## 6. Local REGISTRY sqlite (desktop) vs remote INDEX

### 6.1 Same engine, two deployments

| | INDEX (remote) | REGISTRY (desktop) |
|---|---|---|
| Engine | SQLite WAL file | SQLite WAL file |
| Process | sqlx async service | rusqlite dedicated thread |
| Backup | Litestream → S3 | Optional local `.backup` / user export |
| Multi-reader | sqlx reader pool | UI + worker connections |
| Authority | Global SoT | Local cache / user's packages / offline |
| Sync | Serves pulls | Consumes remote |

### 6.2 Schema sharing

Recommended crate layout:

```
crates/
  index-schema/          # Idens + migration SQL (sqlite)
  registry/              # server INDEX uses sqlx + index-schema
  gui-or-desktop/        # rusqlite + same migrations via refinery or include_str!
```

**Rules:**

- One **logical** schema version sequence.
- Desktop may be a **subset** of tables (no need for full outbox drain machinery).
- Avoid Postgres-only types in shared migrations (sqlite-first DDL).

### 6.3 rusqlite vs sqlx-sqlite for GUI

| | rusqlite | sqlx-sqlite |
|---|---|---|
| API | Sync | Async |
| GUI fit | Excellent | Need runtime + spawn_blocking |
| `update_hook` / `commit_hook` | **Yes** ([hooks module](https://docs.rs/rusqlite/latest/rusqlite/hooks/index.html)) | Limited (preupdate feature exists; less ergonomic for UI) |
| Compile-time SQL check | No | Yes |
| Migrations | refinery / manual | sqlx migrate |

**Recommendation:** **rusqlite + dedicated OS thread + mpsc command channel** for desktop REGISTRY. Fire `update_hook` → UI event bus for reactive views.

```rust
conn.update_hook(Some(move |action, _db, table, rowid| {
    let _ = tx.send(DbEvent { table: table.to_string(), rowid, action });
}));
```

**Caveat:** hooks fire on the connection that writes. Centralize writes on the hooked connection.

### 6.4 Sync: libsql embedded replica vs app-level

| Approach | Guarantees (2026) | Fit for "remote serves → local takes over" |
|---|---|---|
| **libsql embedded replica** | Local reads; writes to remote primary; sync interval; offline reads; classic path **not** offline-write | Local always reads replica file — not "remote first then flip" |
| **Turso engine sync (beta)** | push/pull CDC; offline-first claims; faster than old sync per Turso blog | Better local-first, but beta durability caveats |
| **App-level sync** (snapshot + row cursors + S3 blobs) | Explicit cursors, versioning, conflict policy you own | **Recommended default** |

**Engine-angle conclusion:** libsql embedded replicas are a convenient **read cache of a Turso/libsql primary**, not a drop-in for "INDEX is vanilla SQLite file + Litestream". For vanilla INDEX, implement **protocol-level sync** (watermarks, package generations, blob fetch) — aligns with content-addressed `generation` already in schema. Defer engine-level replica coupling until/unless INDEX itself runs libsql.

---

## 7. Recommended stack (exact)

### 7.1 INDEX server crates

| Crate | Version | Role |
|---|---|---|
| `sqlx` | **0.8.x** features: `runtime-tokio-rustls`, `sqlite`, `macros`, `migrate`, `uuid`, `json`, `chrono` | Async DB access |
| `sea-query` | **0.32** (current) or evaluate 1.x | Dynamic SQL / DDL AST |
| `sea-query-binder` | **0.7** with `sqlx-sqlite` | Bind values to sqlx |
| `uuid` | 1.x (already) | IDs |
| `chrono` or `time` | existing | timestamps as i64 ms |
| `libsqlite3-sys` | already 0.30 bundled | Can rely on sqlx's bundled instead; avoid dual linkage issues |

**Do not add for INDEX primary path:** `libsql`, `turso`, `rusqlite` (server), rqlite client — unless fallback HA path.

### 7.2 Desktop REGISTRY crates

| Crate | Role |
|---|---|
| `rusqlite` with `bundled`, `hooks`, `backup` | Local DB + reactive hooks |
| `sea-query` + `sea-query-rusqlite` (0.8.x ecosystem) | Optional shared query build |
| `refinery` or shared SQL files | Migrations |

### 7.3 Ops binaries

| Component | Version pin guidance |
|---|---|
| Litestream | **≥ 0.5.2**, prefer latest 0.5.x (e.g. 0.5.14+); image `litestream/litestream:0.5` |
| SQLite amalgamation | Whatever sqlx 0.8 bundles (3.4x+); ensure ≥ 3.45 if using JSONB |

### 7.4 Deployment shape (INDEX)

```
StatefulSet index-0 (replicas: 1)
├── PVC data (RWO, sized for metadata growth + WAL headroom)
├── initContainer: litestream restore -if-db-not-exists -if-replica-exists
├── container: nudox-index (sqlx writer+reader pools)
└── container: litestream replicate → s3://nudox-index/{env}/
```

- Health: SQL `SELECT 1` + Litestream metrics scrape.
- Disruption: `PodDisruptionBudget` minAvailable 0 with care — single replica; prefer controlled failovers.
- Node drain: ensure PVC relocatable or restore path tested.
- Network: INDEX service ClusterIP; no multi-pod shared volume.

### 7.5 Schema portability strategy

1. **SQLite-first DDL** as source of truth going forward.
2. sea-query Idens shared; `build(SqliteQueryBuilder)` for DML.
3. `sqlx::migrate!("./migrations")` with ordered SQL files.
4. Delete PG-only paths (`SkipLocked`, advisory lock SQL, `PostgresQueryBuilder`) after dual-running period ends.
5. Codec layer absorbs uuid/json representation differences (`schema/codec.rs`).

### 7.6 Migration sequencing (Postgres → SQLite INDEX)

| Phase | Work | Exit criteria |
|---|---|---|
| **0** | Move job queue to k8s (parallel research) | No runtime dependency on `jobs` SKIP LOCKED |
| **1** | Introduce sqlite dual-pool module; shadow-write schema empty | Boots alongside PG |
| **2** | Port sea-query DML to SqliteQueryBuilder; codec types | Unit tests on sqlite |
| **3** | Dual-write packages/parse_status/symbols/outbox PG+sqlite | Row counts match |
| **4** | Point outbox pollers at sqlite; PG outbox cold | Sinks healthy |
| **5** | Read path cutover (API/index lookups → sqlite) | Latency SLOs hold |
| **6** | Stop PG writes; Litestream restore drill | RPO/RTO documented |
| **7** | Decommission PG for INDEX | Cost reclaimed |

**Data copy tools:** one-shot ETL job (sqlx PG read → sqlite write) per table; verify `outbox.seq` continuity or rebuild watermarks carefully (prefer drain PG outbox first).

---

## 8. INDEX vs REGISTRY comparison matrix

| Dimension | INDEX (remote) | REGISTRY (local) |
|---|---|---|
| SoT | Yes (global) | No (local + synced subset) |
| HA | Litestream restore; single writer | Device disk |
| Write rate | Ingest bursts | User actions |
| Readers | API + pollers | UI |
| Backup | Continuous S3 | Optional |
| Hook reactivity | Not needed | Essential |
| Access library | sqlx | rusqlite |
| AuthN/Z | Service mesh / API | Local OS user |
| Schema | Full | Subset + local-only tables |

---

## 9. Worked examples

### 9.1 Dual pool + IMMEDIATE outbox append (sketch)

```rust
async fn record_stored(
    writer: &sqlx::SqlitePool,
    package: &[u8; 16],
    generation: &[u8; 32],
    sinks: &[&str],
) -> sqlx::Result<()> {
    let mut tx = writer.begin().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *tx).await.ok(); // if not already immediate
    sqlx::query(
        "UPDATE parse_status SET state = 'stored', content_hash = ?1, updated_at = ?2 WHERE package_id = ?3"
    )
    .bind(generation.as_slice())
    .bind(chrono::Utc::now().timestamp_millis())
    .bind(package.as_slice())
    .execute(&mut *tx)
    .await?;

    for sink in sinks {
        sqlx::query(
            "INSERT INTO outbox (package_id, generation, sink_kind, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT DO NOTHING"
        )
        .bind(package.as_slice())
        .bind(generation.as_slice())
        .bind(*sink)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
```

### 9.2 Litestream config sketch

```yaml
# litestream.yml
addr: ":9090"
dbs:
  - path: /var/lib/nudox/index.db
    replica:
      url: s3://nudox-index-prod/index.db
```

### 9.3 Reader query

```rust
let state: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(
    "SELECT state, content_hash FROM parse_status WHERE package_id = ?"
)
.bind(package_uuid.as_bytes().as_slice())
.fetch_optional(&reader)
.await?;
```

---

## 10. Risks, open questions, non-goals

### 10.1 Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Single writer pod as availability bottleneck | Medium | Litestream fast restore; PDB discipline; multi-AZ PVC/storage class |
| Litestream 0.5.x restore bugs | Medium | Pin ≥0.5.2; quarterly restore game days |
| Dual linkage of libsqlite3 (sqlx + libsqlite3-sys + rusqlite) | Medium | One bundled provider per binary |
| sqlx sqlite type mismatches for uuid | Low | Explicit blob codec |
| Accidental multi-replica StatefulSet scale | High | Set replicas=1; admission policy; document |
| Long reader transactions block checkpoints | Medium | Short read txns; monitor WAL size |
| Assuming Turso offline sync durability | High if used | Don't for INDEX; treat beta carefully on desktop |
| Outbox redelivery storms after crash | Low | Idempotent sinks on generation |

### 10.2 Open questions

1. **RTO target** for INDEX after AZ loss — minutes (Litestream) vs seconds (rqlite)?
2. **Sessions DB split** timing — only after measuring write contention?
3. **UUID storage** TEXT vs BLOB — pick one in codec ADR.
4. **Whether desktop sync** uses custom protocol only vs experimental Turso — prefer custom until INDEX is stable on vanilla sqlite.
5. **sea-query 1.x upgrade** bundled with cutover or separate?
6. **Page size** 4k vs 8k/16k — microbench on symbol bulk insert + point lookup mix.

### 10.3 Non-goals

- Replacing Tantivy/Qdrant/Terminus with sqlite FTS/vectors.
- Multi-master CRDT package state.
- Keeping PG `jobs` on sqlite long-term.
- LiteFS on Fly as production INDEX.

### 10.4 Relationship to Doltgres (edge-tech 01)

**Addendum 2026-07-16** (cross-link only; does not change the primary SQLite+Litestream recommendation above).

| Store | Role |
|---|---|
| **SQLite INDEX (this plan)** | **Operational source of truth** — package identity rows, `parse_status`, transactional outbox, watermarks, hot serving `symbols`, single-writer WAL + Litestream |
| **Doltgres catalog (edge-tech 01 §9R)** | **Optional** versioned **package metadata** (crates.io-page class: description, owners, versions, downloads, license, links, readme, keywords, features, …) and **symbol catalog** rows with commit/`dolt_diff`/`AS OF` history |
| **Terminus / cold IR** | Graph edges and multi-hop — not Doltgres, not sqlite FTS |

- Do **not** put jobs/outbox/parse_status SoT on Doltgres.
- Do **not** block this SQLite migration on Doltgres 1.0 or catalog spikes.
- If A′/B′ catalog is adopted: **async dual-write / outbox sink** into Doltgres beside INDEX; desktop still **sqlite REGISTRY** snapshots (no Doltgres embed).
- Full treesitter trees remain re-parse/CAS (plan 13); catalog holds moniker/kind/path/sig_hash rows only.

Authoritative Doltgres write-up: `docs/research/edge-tech/01-doltgres.md` (twin: `01-doltgres/PLAN.md`), §9R.

---

## 11. Actionable recommendations (priority order)

1. **Adopt PRIMARY stack:** StatefulSet n=1 + SQLite WAL PVC + Litestream ≥0.5.2 → S3 + dual sqlx pools.
2. **Finish queue extraction to k8s** before/with sqlite cutover so SKIP LOCKED dies cleanly.
3. **Port `schema/` to SqliteQueryBuilder** with integer timestamps + blob UUIDs; add `sqlx::migrate!`.
4. **Keep transactional outbox** in sqlite; single poller; idempotent sinks.
5. **Desktop:** rusqlite thread + hooks; shared migration SQL subset; app-level sync not engined replica.
6. **Document restore drill** (delete PVC → init restore → service healthy) as release gate.
7. **Hold rqlite** as documented fallback ADR, not dual-running complexity.
8. **Reject LiteFS/Turso Cloud/cr-sqlite** for INDEX SoT in architecture decision record.

---

## 12. Executive summary

Nudox can replace Postgres for the remote INDEX with **vanilla SQLite** without giving up durability or the transactional outbox model that the registry already implements. The credible 2026 server-side pattern is **not** a distributed SQLite product: it is a **single-writer SQLite file on a Kubernetes PVC**, continuous **Litestream v0.5.x** replication to **S3**, and **restore-on-boot** via an init container. That pattern is officially documented, operationally simple, and preserves file-backed `sqlx` transactions. LiteFS is stable-ish but under-supported and Fly-centric; Turso/libSQL are better treated as sync/edge experiments (and a product in active rewrite) than as the global package SoT; rqlite remains a solid **HA fallback** if multi-node automatic failover becomes mandatory, at the cost of Raft write overhead and a less natural Rust SQL integration.

Concurrency is manageable for this workload. INDEX writes (package identity, ResolutionState, symbols, outbox) are bursty and small; SQLite's ceiling with WAL + batched `BEGIN IMMEDIATE` sits far above expected ingest. The load-bearing discipline is **architectural**: one writer connection (`sqlx` pool `max_connections(1)`), a separate read pool, `busy_timeout`, and no multi-pod writers on one file. Naive multi-connection write pools are a known footgun (~order-of-magnitude self-inflicted slowdowns).

Postgres-specific features map cleanly once the job queue leaves for Kubernetes. The codebase already avoided PG ENUMs (text + CHECK) and already uses watermark-polled outbox rather than LISTEN/NOTIFY. Remaining translations are mechanical: `jsonb` → JSON text or SQLite JSONB BLOBs (3.45+; not PG-compatible); `uuid`/`bytea` → BLOB; `timestamptz` → integer unix ms; `pg_try_advisory_lock` → single drain task; `SKIP LOCKED` → deleted with the queue. `sea-query` 0.32 already speaks SQLite; switch `PostgresQueryBuilder` → `SqliteQueryBuilder` and binder features accordingly.

Topology should start as **one `index.db`** containing packages, parse_status, symbols, outbox, and watermarks so state transitions and outbox appends stay single-file atomic. Sessions can split later if interactive writes contend. Multi-GB metadata is fine on NVMe with mmap; keep blobs in S3 and secondary indexes in Tantivy/Qdrant so the relational file stays lean. The outbox pattern is first-class on single-node SQLite: same-transaction insert, poll by `seq`, at-least-once delivery, sink idempotency on content generation.

Local REGISTRY should share schema ideas and migrations but not the server access stack: **rusqlite + hooks** on a dedicated thread for reactive desktop UX; **app-level sync** against the INDEX API/blobs rather than coupling to libsql embedded replicas (which assume a libsql/Turso primary and do not model "remote HTTP first, then local takeover").

**Net:** ship INDEX as SQLite+Litestream+sqlx dual-pool; sequence cutover behind queue extraction; keep rqlite in the ADR back pocket; do not bet the package graph on LiteFS or Turso Cloud.

---

## Sources (inline recap)

- Litestream K8s: https://litestream.io/guides/kubernetes/
- Litestream v0.5: https://fly.io/blog/litestream-v050-is-here/
- Litestream install/migration: https://litestream.io/install/ , https://litestream.io/docs/migration/
- LiteFS status: https://fly.io/docs/litefs/ , https://community.fly.io/t/what-is-the-status-of-litefs/23883 , https://community.fly.io/t/sunsetting-litefs-cloud/20829
- Turso roadmap: https://turso.tech/blog/upcoming-changes-to-the-turso-platform-and-roadmap
- Turso sync vs libsql: https://turso.tech/blog/sync-benchmark
- sqlx pool PSA: https://emschwartz.me/psa-your-sqlite-connection-pool-might-be-ruining-your-write-performance/
- SQLite tuning: https://phiresky.github.io/blog/2020/sqlite-performance-tuning/
- SQLite JSON/JSONB: https://sqlite.org/json1.html
- SQLite WAL: https://sqlite.org/wal.html
- rqlite performance: https://rqlite.io/docs/guides/performance/
- rqlite HA philosophy: https://rqlite.io/docs/faq/
- Multi-GB SQLite reads: https://medium.com/charisol-pulse/sqlite-doesnt-care-about-your-scaling-assumptions-how-i-served-250gb-at-40ms-ab1e264e66b0
- rusqlite hooks: https://docs.rs/rusqlite/latest/rusqlite/hooks/index.html
- sea-query 0.32: https://www.sea-ql.org/blog/2024-12-03-whats-new-in-seaquery-0.32.x/
- Code: `workspace/registry/schema/mod.rs`, `schema/queries.rs`, `coordination.rs`, `queue/mod.rs`, `index/mod.rs`, `Cargo.toml`
