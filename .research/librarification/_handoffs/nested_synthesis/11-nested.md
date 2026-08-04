# Nested synthesis for task 11

# Nested agent agent-a676a95c53436d123
Description: Research SQLite server-side deployment patterns 2026
Size: 81125

I'll run the deep-research workflow now. Let me decompose the question into 5 search angles and run them in parallel.

---

All 5 search agents are running in parallel. I'll wait for all results before synthesizing.


# Nested agent agent-acb42c0898579a7a9
Description: Research SQLite concurrency, async Rust sqlx patterns
Size: 211760

I'll run several searches in parallel to gather comprehensive information on all these topics.


# Nested agent agent-a277e35eb4a1875e2
Description: Research Postgres to SQLite migration mapping for Rust/sqlx
Size: 80065

I'll run the deep-research workflow now. Let me decompose this into 5 search angles and run them in parallel.

---

5 parallel search agents are running. Waiting for all results to come in before synthesizing.


# Nested agent agent-a264f5415893645e8
Description: Research SQLite schema topology, tuning, outbox pattern, and local REGISTRY sync
Size: 187231

Now I'll run a targeted adversarial verification pass and fill in the remaining gaps: FOR UPDATE SKIP LOCKED in SQLite (it doesn't exist), the VACUUM semantics, and the rusqlite update_hook specifically.

---

Now I have enough to do the adversarial verification and synthesize. Let me do one final targeted fetch on the outbox SQLite workaround (no SKIP LOCKED) and the Turso crate clarification.

---

I now have comprehensive, adversarially-verified data across all four topics. Let me synthesize the full report.

---

# SQLite Architecture Deep Research Report (2026)

---

## TOPIC 1 — Schema/Topology for Read-Heavy Services

### One DB vs Multiple (ATTACH)

SQLite's writer isolation is **per-file**: a single `BEGIN IMMEDIATE` or `BEGIN EXCLUSIVE` transaction locks the entire database file. Using `ATTACH DATABASE` gives you separate writer lanes — a write to `db_a` does not block readers or writers on `db_b`. This is the primary motivation for splitting by domain (e.g., separate files for blobs, indexes, event log).

Key constraint: `mmap_size` is applied **per attached file**. If you attach 5 databases each with `mmap_size = 30000000000`, the process can map up to 150 GB of virtual address space — acceptable on 64-bit but requires auditing on constrained systems.

Cross-file JOINs over ATTACH work but cannot span transactions across files atomically (no cross-file SAVEPOINT). For an INDEX server with a hot read path and a separate write-append log, two files with ATTACH is a clean pattern.

### page_size

- Default: 4096 bytes.
- For blob-heavy or analytical read patterns (large sequential scans): `PRAGMA page_size = 32768` or `65536`. One benchmark showed 65536-byte pages achieving 117 GB/s read throughput on NVMe vs significantly lower with 4096-byte pages.
- Must be set **before** any data is written; changing it on an existing DB requires `VACUUM`.
- Smaller page sizes remain better for highly selective point-lookup OLTP workloads.

**Recommendation for read-heavy INDEX server:** `page_size = 16384` or `32768`. Apply at DB creation time via `PRAGMA page_size = 32768; VACUUM;` on a fresh database.

### mmap_size

Source: [phiresky's blog](https://phiresky.github.io/blog/2020/sqlite-performance-tuning/), [oldmoe's blog](https://oldmoe.blog/2024/02/03/turn-on-mmap-support-for-your-sqlite-connections/)

- `PRAGMA mmap_size = 30000000000;` (30 GB) is the canonical large-system setting.
- mmap lets the OS page cache manage SQLite pages, eliminating `read(2)` syscalls and enabling page sharing across processes (critical if multiple workers read the same file).
- **Multi-process benchmark**: mmap used 301 MB RSS vs 1,607 MB for cache-only, with **36% better throughput** under concurrent read+write load.
- Single-process, single-connection: cache and mmap are roughly equivalent. mmap wins at scale.
- Practical max: 2 GB on 64-bit before hitting `SQLITE_MAX_MMAP_SIZE` compile default; override at build time or use `SQLITE_CONFIG_MMAP_SIZE`.

**Recommendation:** `PRAGMA mmap_size = 8589934592;` (8 GB) for a server with a multi-GB index database. Pair with WAL mode.

### cache_size

- Default: `-2000` (2 MB). Counterintuitively, with mmap enabled, keep `cache_size` **small** (default or slightly above) — the page cache and mmap overlap and large cache_size wastes private RSS per-connection without benefit when pages are already OS-cached via mmap.
- If using mmap: `PRAGMA cache_size = -8000;` (8 MB) as a modest intra-transaction buffer.
- If NOT using mmap (single-connection embedded): `PRAGMA cache_size = -131072;` (128 MB) is reasonable for a DB that fits in RAM.

### WAL + Synchronous Mode (prerequisite for all of the above)

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;   -- safe with WAL; only checkpoint needs fsync
PRAGMA temp_store = MEMORY;
PRAGMA busy_timeout = 5000;    -- ms; prevents "database is locked" under contention
```

### VACUUM and ANALYZE in Production

- **VACUUM**: Rebuilds the DB file, reclaims free pages, defragments. Requires disk space equal to DB size. Blocks writers during execution. For large DBs (>1 GB), use `VACUUM INTO 'backup.db'` to produce a compacted copy without disturbing the live file, then atomically swap.
- **INCREMENTAL VACUUM**: `PRAGMA auto_vacuum = INCREMENTAL; PRAGMA incremental_vacuum(100);` — reclaims N pages at a time, usable in production without blocking. Best for write-heavy workloads with row churn.
- **ANALYZE**: Updates query planner statistics. Run after bulk loads or schema changes. `PRAGMA optimize;` (SQLite 3.18+) automatically decides which tables need ANALYZE and runs them — safe to call at DB open and periodically (e.g., hourly on busy DBs).

**Production schedule recommendation:**
- `PRAGMA optimize;` — at connection open + every 2 hours.
- `PRAGMA incremental_vacuum(500);` — nightly during low-traffic window.
- Full `VACUUM INTO` — monthly or after large bulk deletes, swapping the file during a maintenance window.

### Multi-GB SQLite: Size Limits and Performance

SQLite's theoretical max is 281 TB (2^48 pages × 65536 bytes/page). Practical limits in 2026 are hardware/OS constrained. Reported production deployments:

- Sub-millisecond reads on NVMe with WAL + mmap for databases up to ~50 GB.
- Write throughput: 10,000–50,000 writes/second on modern NVMe.
- Migration trigger: multiple app servers needed, writes consistently >5K/s, or analytics degrading user-facing reads.

Source: [SQLite in Production - daily.dev](https://daily.dev/blog/sqlite-production-guide-when-how-to-use-beyond-prototyping/), [SQLite in 2026 - Java Code Geeks](https://www.javacodegeeks.com/2026/05/sqlite-in-2026-why-serious-apps-are-choosing-it-over-postgres.html)

### Litestream vs .backup API to S3

Source: [Backup strategies - oldmoe's blog](https://oldmoe.blog/2024/04/30/backup-strategies-for-sqlite-in-production/), [Litestream on Medium](https://medium.com/@cosmicray001/going-production-ready-with-sqlite-how-litestream-makes-it-possible-74f894fc96f0)

| | **Litestream** | **.backup API** |
|---|---|---|
| Mechanism | Streams WAL frames to S3 continuously (~1s lag) | Point-in-time page-by-page copy |
| RPO | ~1 second | Interval between runs (e.g., hourly) |
| Restore time | Download base + incremental WAL frames | Download single file |
| CPU overhead | Minimal (<1ms latency per txn) | Burst during backup window |
| Cost (1 GB DB) | ~$0.50/month S3 | Depends on frequency |
| WAL2 compatibility | **Incompatible** | Compatible |
| Complexity | External sidecar binary | Built-in via rusqlite `backup` API |

**Recommendation:** Litestream for production INDEX server needing low RPO. Use `.backup API` (via `rusqlite::backup::Backup`) for the REGISTRY local desktop — simpler, no external dependency, RPO = backup interval. For point-in-time recovery on multi-GB: Litestream is the only viable option without custom WAL shipping.

---

## TOPIC 2 — Transactional Outbox Pattern with SQLite

### Core Pattern

The same-transaction outbox row + poller pattern works with SQLite, but SQLite lacks two PostgreSQL features that outbox implementations typically rely on:

1. **`SELECT FOR UPDATE SKIP LOCKED`** — does not exist in SQLite. SQLite has no row-level locking. A `BEGIN IMMEDIATE` transaction locks the entire file.
2. **`LISTEN/NOTIFY`** — does not exist. Polling is the only mechanism.

**SQLite-compatible outbox schema:**

```sql
CREATE TABLE outbox (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type  TEXT NOT NULL,
    payload     BLOB NOT NULL,
    created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
    published_at INTEGER
);
CREATE INDEX idx_outbox_unpublished ON outbox(id) WHERE published_at IS NULL;
```

### Single-Writer Poller in Rust

Since SQLite has no row-level locking, the outbox poller must use application-level exclusivity:

```rust
// In a single dedicated tokio::task or OS thread
// Use spawn_blocking since rusqlite is sync
let rows = conn.query("SELECT id, payload FROM outbox WHERE published_at IS NULL ORDER BY id LIMIT 100", [])?;
// BEGIN IMMEDIATE implicitly obtained by the write below
for row in rows {
    publish_to_broker(&row).await?;
    conn.execute("UPDATE outbox SET published_at = unixepoch() WHERE id = ?", [row.id])?;
}
```

**Key design constraints:**
- Run exactly **one poller task** per SQLite file. Multiple pollers fight over `BEGIN IMMEDIATE` and produce `SQLITE_BUSY` errors.
- Use `busy_timeout = 5000` to absorb transient lock contention from the main write path.
- The poller reads then marks: if it crashes between publish and mark, the event re-publishes on restart → **at-least-once delivery**, not exactly-once.

### Exactly-Once at Consumer (Idempotency Keys)

True exactly-once requires consumer-side idempotency. Pattern:

```sql
CREATE TABLE processed_events (
    event_id TEXT PRIMARY KEY,
    processed_at INTEGER NOT NULL DEFAULT (unixepoch())
);
```

Consumer executes in one transaction:
```sql
BEGIN;
INSERT OR IGNORE INTO processed_events(event_id) VALUES (?);
-- if INSERT affected 0 rows: skip (duplicate); else: apply business logic
COMMIT;
```

Periodic cleanup: `DELETE FROM processed_events WHERE processed_at < unixepoch() - 86400;`

### K8s-Native Alternatives

If running in k8s and want to avoid a background poller process:

- **CronJob**: A k8s CronJob running a separate Rust binary that drains the outbox on a schedule. Simpler ops than an in-process goroutine/task.
- **Sidecar**: A Litestream-adjacent sidecar that reads WAL frames and publishes events (Litestream can be extended with custom destinations).
- **Debezium SQLite connector**: Exists as a community connector; reads WAL pages and publishes to Kafka. More complex but decouples producer from consumer.

Source: [Transactional outbox - james-carr.org](https://james-carr.org/posts/2026-01-15-transactional-outbox-pattern/), [event-driven.io](https://event-driven.io/en/outbox_inbox_patterns_and_delivery_guarantees_explained/)

---

## TOPIC 3 — libsql Embedded Replicas for Local Desktop Sync

### 2026 Crate Landscape

As of July 2026, there are **two distinct Rust crates** from Turso:

| Crate | Engine | Status | Use case |
|---|---|---|---|
| `libsql` (v0.9.30, July 2026) | C fork of SQLite | Production | Remote access, existing embedded replica deployments |
| `turso` | Pure Rust rewrite (MVCC) | Beta | New local-first / sync projects |

**Turso's own guidance**: "For new projects, use `turso` for local/embedded use or sync. Use `libsql` with the `remote` feature for over-the-wire access only."

The `turso` crate uses a new CDC-based sync protocol (`push()`/`pull()` separate operations) that is **8.9x–312x faster** than the old libsql embedded replica sync for read-your-writes scenarios.

### libsql Embedded Replica — Current API

```rust
// libsql 0.9.30
let db = Builder::new_remote_replica("local.db", url, token)
    .sync_interval(Duration::from_secs(300))  // auto-sync every 5min
    .read_your_writes(true)                    // default; ensures strong local consistency after writes
    .build()
    .await?;
let conn = db.connect()?;
db.sync().await?;  // explicit sync
```

### Consistency Model

- **Writes**: Always routed to remote primary. After a successful write, the local replica is updated before returning (when `read_your_writes = true`).
- **Reads**: Served locally from the replica file. May be stale by up to the sync interval for data written elsewhere.
- **Offline reads**: Work from local replica file — fully offline capable for reads.
- **Offline writes**: The **old** libsql embedded replica does NOT support offline writes — writes require the remote primary to be reachable.
- **New Turso offline sync** (public beta, July 2025+): Supports offline writes that sync later. "Local database operations can proceed normally, with automatic sync occurring once connectivity is restored." **Caveat: No durability guarantees during beta period — data loss is possible.**

### "Remote serves while syncing → local takes over" Pattern

This pattern is **not natively supported** by libsql's embedded replica. The embedded replica always reads locally; the remote is only involved for writes and sync. There is no automatic fallback mechanism that switches from remote to local — local is always the read source.

For the Turso offline sync beta, the local DB accepts reads and writes offline, with sync queued for when connectivity returns. This is closer to "local-first" but without the "remote serves first" semantics — it's local-always.

### rusqlite vs sqlx-sqlite for Desktop

Source: [Rust ORMs in 2026 - Medium](https://aarambhdevhub.medium.com/rust-orms-in-2026-diesel-vs-sqlx-vs-seaorm-vs-rusqlite-which-one-should-you-actually-use-706d0fe912f3)

| | **rusqlite** | **sqlx (sqlite feature)** |
|---|---|---|
| Async | No (sync only) | Yes (tokio/async-std) |
| Desktop suitability | Excellent | Good with `spawn_blocking` overhead |
| `update_hook` / `preupdate_hook` | Yes, via feature flags | Partial (preupdate-hook feature flag) |
| Bundled SQLite | Yes (`bundled` feature) | Yes |
| Compile-time query checks | No | Yes (`.sqlx` files) |
| Migration support | Manual or via refinery crate | Built-in (`sqlx::migrate!()`) |
| GUI framework fit | Direct (no async overhead) | Requires async runtime context |

**Recommendation for GUI/desktop (egui, iced, native):** `rusqlite` with `bundled` feature. The synchronous nature matches GUI event loop patterns. For Tauri specifically, `tauri-plugin-rusqlite` exists with hooks support.

### update_hook for Reactive UI

`rusqlite`'s `update_hook` fires on INSERT/UPDATE/DELETE within the same connection:

```rust
conn.update_hook(Some(|action: Action, db: &str, table: &str, rowid: i64| {
    // Send to a crossbeam channel or trigger a UI repaint signal
    tx.send(UIEvent::DbChanged { table: table.to_string(), rowid }).ok();
}));
```

**Limitation**: The hook fires on the connection that performed the write. If you have a background writer and a foreground reader on separate connections, you need a different notification mechanism (e.g., a shared `Arc<Notify>` that the writer signals after each commit, or polling).

`preupdate_hook` (requires `hooks` + `preupdate_hook` feature in rusqlite) gives access to old/new row values before the change commits — useful for change-tracking without a separate audit table.

### Async for Desktop SQLite

| Pattern | Pros | Cons |
|---|---|---|
| `rusqlite` + `spawn_blocking` | Simple, no connection pool needed | Each call crosses thread boundary |
| `sqlx` sqlite pool | Async-native, compile-time checks | Pool overhead; update_hook not readily available |
| `rusqlite` on dedicated thread + channel | Low latency, hook-friendly | More boilerplate |

**Recommendation:** For a desktop app with a Tokio runtime (e.g., Tauri), use a **dedicated background thread** running a rusqlite connection, with a channel (crossbeam or tokio::mpsc) for query dispatch. This gives you hook access on the writer thread and avoids `spawn_blocking` overhead for frequent operations.

---

## TOPIC 4 — Schema Sharing Strategy

### One Schema Crate for Two Deployments

The recommended structure:

```
crates/
  db-schema/         # shared: table definitions via sea-query, migration files
  index-server/      # uses db-schema
  registry-local/    # uses db-schema
```

### sea-query for Dialect-Independent Schema

sea-query 1.0.0 (released May 30, 2026) is production-stable with full SQLite, PostgreSQL, and MySQL support behind a common API.

```rust
use sea_query::{Table, ColumnDef, SqliteQueryBuilder};

let stmt = Table::create()
    .table(Symbol::Table)
    .if_not_exists()
    .col(ColumnDef::new(Symbol::Id).integer().not_null().auto_increment().primary_key())
    .col(ColumnDef::new(Symbol::Name).text().not_null())
    .build(SqliteQueryBuilder);
```

**Limitations**: SQLite doesn't support `TRUNCATE`, `ALTER COLUMN` (type changes), or adding foreign key constraints to existing tables. Design schema for forward-only migrations (add columns only, never drop or retype).

### Migration Strategy with sqlx

`sqlx::migrate!()` for SQLite is production-proven and the canonical choice for sqlx-based projects:

```rust
sqlx::migrate!("./migrations").run(&pool).await?;
```

For the schema-sharing pattern:
- Migrations live in `crates/db-schema/migrations/`.
- Both INDEX server and REGISTRY reference this path at compile time via `sqlx::migrate!("../db-schema/migrations")`.
- sea-query generates `CREATE TABLE` DDL for review/testing; actual migrations are `.sql` files committed to the migrations folder.
- If using rusqlite (desktop) instead of sqlx: use the `refinery` crate, which supports the same `migrations/` folder convention.

**Practical divergence**: If INDEX server ever needs PostgreSQL-specific features (e.g., `JSONB`, full-text with `tsvector`), maintain a `migrations/sqlite/` and `migrations/postgres/` split inside the shared crate, with a feature flag selecting the right folder at compile time.

Source: [sea-query GitHub](https://github.com/SeaQL/sea-query), [sqlx GitHub](https://github.com/transact-rs/sqlx)

---

## Consolidated Recommendations

### Pragma Baseline (production read-heavy server)
```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA page_size = 32768;        -- set at DB creation only
PRAGMA mmap_size = 8589934592;   -- 8 GB
PRAGMA cache_size = -8000;       -- 8 MB (modest; mmap handles the rest)
PRAGMA temp_store = MEMORY;
PRAGMA busy_timeout = 5000;
PRAGMA optimize;                 -- run at open + periodically
```

### Outbox Pattern (SQLite Rust)
- Single dedicated writer task/thread; `BEGIN IMMEDIATE` wraps both business insert + outbox row insert.
- Poller: one task, poll every 1s, mark `published_at` after successful publish.
- Consumer-side idempotency table for effective exactly-once.
- No `SKIP LOCKED` — app-level single-poller exclusivity suffices.

### libsql / Turso for Desktop Sync
- Avoid `libsql` embedded replica for new desktop projects; use `turso` crate (beta) for offline-first sync.
- For offline reads only (no writes offline): `libsql` embedded replica is production-ready today.
- `rusqlite` + dedicated thread + channel remains the lowest-friction choice for purely local desktop apps without cloud sync.

### Schema Sharing
- `sea-query` 1.0.0 for schema definition; `.sql` migration files for sqlx.
- `sqlx::migrate!()` for server (async); `refinery` for desktop (rusqlite/sync).
- Single `migrations/` folder works for SQLite-only deployments; split by dialect if diverging.

---

**Key sources:**
- [phiresky — SQLite performance tuning](https://phiresky.github.io/blog/2020/sqlite-performance-tuning/)
- [oldmoe — mmap support](https://oldmoe.blog/2024/02/03/turn-on-mmap-support-for-your-sqlite-connections/)
- [oldmoe — backup strategies](https://oldmoe.blog/2024/04/30/backup-strategies-for-sqlite-in-production/)
- [Turso SDK Rust reference](https://docs.turso.tech/sdk/rust/reference)
- [Turso offline sync public beta](https://turso.tech/blog/turso-offline-sync-public-beta)
- [Turso sync benchmark (CDC vs embedded replica)](https://turso.tech/blog/sync-benchmark)
- [libsql crate docs.rs](https://docs.rs/libsql)
- [Transactional outbox pattern — james-carr.org](https://james-carr.org/posts/2026-01-15-transactional-outbox-pattern/)
- [Rust ORMs in 2026 — Medium](https://aarambhdevhub.medium.com/rust-orms-in-2026-diesel-vs-sqlx-vs-seaorm-vs-rusqlite-which-one-should-you-actually-use-706d0fe912f3)
- [sea-query GitHub](https://github.com/SeaQL/sea-query)
- [Litestream on Medium](https://medium.com/@cosmicray001/going-production-ready-with-sqlite-how-litestream-makes-it-possible-74f894fc96f0)
- [SQLite in Production — daily.dev](https://daily.dev/blog/sqlite-production-guide-when-how-to-use-beyond-prototyping/)
- [SQLite locking — sqldocs.org](https://sqldocs.org/sqlite-locks/)
- [aide-de-camp-sqlite](https://docs.rs/aide-de-camp-sqlite)
- [tauri-plugin-rusqlite](https://docs.rs/tauri-plugin-rusqlite)


