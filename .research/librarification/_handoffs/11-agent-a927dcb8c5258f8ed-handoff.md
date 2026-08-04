# Handoff: Research sqlite for remote INDEX
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a927dcb8c5258f8ed.jsonl`
- Agent id: `agent-a927dcb8c5258f8ed`
- Deliverable: `/Users/philocalyst/Projects/Backend/.research/librarification/11-sqlite-index.md`
- Existing report: False (0 lines)
- Tools used: 27 ({'WebFetch': 6, 'ToolSearch': 1, 'Agent': 4, 'WebSearch': 16})
- Files/greps touched: 0
- Web queries: 22

## Web queries performed
- Litestream v0.5 2026 SQLite replication kubernetes sidecar status
- Turso libsql 2026 company status Rust rewrite licensing
- LiteFS fly.io 2026 maintenance status active
- SQLite WAL write throughput rows per second benchmark batching 2025 2026
- sqlx 0.8 SQLite pool options writer single connection reader multiple async rust
- SQLite version 3.46 3.47 3.48 2025 2026 new features JSONB
- libsql embedded replica Rust crate 2026 consistency offline behavior
- SQLite transactional outbox pattern polling rust without postgres
- rqlite 2026 version write throughput kubernetes
- sea-query 0.32 SQLite backend dialect differences postgres 2025
- sqlx sqlite "query!" macro compile time check works 2025
- SQLite page_size mmap_size cache_size tuning read heavy multi-GB production 2025
- rusqlite update_hook reactive desktop app GUI Rust 2025
- FETCH: https://litestream.io/guides/kubernetes/
- FETCH: https://emschwartz.me/psa-your-sqlite-connection-pool-might-be-ruining-your-write-performance/
- FETCH: https://turso.tech/blog/upcoming-changes-to-the-turso-platform-and-roadmap
- FETCH: https://phiresky.github.io/blog/2020/sqlite-performance-tuning/
- FETCH: https://medium.com/charisol-pulse/sqlite-doesnt-care-about-your-scaling-assumptions-how-i-served-250gb-at-40ms-ab1e264e66b0
- cr-sqlite CRDT extension SQLite 2026 production maturity
- SQLite "data_version" pragma change detection polling alternative NOTIFY
- FETCH: https://docs.rs/rusqlite/latest/rusqlite/hooks/index.html
- SQLite ATTACH database cross-db transaction limitations write isolation 2025

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll launch multiple parallel research agents to cover all the topics simultaneously, then synthesize into the full report.

### fragment

All four research agents are running in parallel. Let me also do some targeted searches while waiting.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive external research for a major architecture plan. Today is 2026-07-16 — verify versions/status with WebSearch/WebFetch; cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence platform migrating OFF Postgres entirely. Today Postgres holds: a global package index (package identity + ResolutionState), a poison-pill-safe job queue with exponential backoff, a transactional outbox fanning writes to derived stores (Qdrant/TerminusDB/Tantivy), per-session exploration graphs, and watermarks for index pollers. THE PLAN: the job queue and load-balancing move OUT to Kubernetes primitives (separate research covers that); everything else moves to SQLITE ON THE REMOTE ("the INDEX": sqlite + S3 blobs, k8s-orchestrated) and sqlite locally in a desktop app ("the REGISTRY"). Rust stack: sqlx 0.8 (postgres features today; libsqlite3-sys already bundled), sea-query 0.32 + sea-query-binder.

YOUR MISSION — exhaustive research:
1. SQLite as a SERVER-SIDE store in 2026: the credible deployment patterns — (a) plain sqlite + WAL on a PV with single-writer service, (b) Litestream (v0.5+? verify state; continuous S3 replication, restore-on-boot), (c) LiteFS (fly.io — maintenance status 2026? still alive?), (d) libsql/Turso (the open-source libsql server + embedded replicas — verify 2026 company/product state: Turso rewrite in Rust? licensing), (e) cr-sqlite / other CRDT extensions, (f) rqlite/dqlite (Raft sqlite). For each: write scalability ceiling, read replica story, failure/restore path, k8s fit (StatefulSet + PVC patterns), operational maturity. RECOMMEND one primary + one fallback for the INDEX.
2. Concurrency reality check: sqlite single-writer + WAL — what write throughput is realistic (rows/s for small rows, with batching), WAL checkpoint tuning, busy_timeout, connection pooling from an async Rust service (sqlx sqlite pool semantics — writer pool of 1 + reader pool of N pattern), BEGIN IMMEDIATE discipline. Where does our INDEX workload sit vs the ceiling? (Workload: package/version/symbol metadata writes on each ingest — bursty; reads: high, cacheable.)
3. Migration mapping from today's Postgres usage — for each feature, the sqlite replacement: LISTEN/NOTIFY (→ polling? data_version pragma? update_hook via libsqlite3-sys? outbox table + poller?), FOR UPDATE SKIP LOCKED job claiming (→ gone to k8s, but pattern if needed: UPDATE ... RETURNING with immediate tx), advisory locks, jsonb (sqlite JSONB in 3.45+ — verify), uuid, arrays (→ JSON or junction tables), partial indexes, upserts (ON CONFLICT — supported), triggers, FTS (we use tantivy, but note FTS5 exists). sqlx/sea-query dialect portability: what breaks moving sqlx postgres → sqlite (macros checked queries? migrations? types), sea-query's sqlite backend fidelity.
4. Schema/topology for the INDEX: one sqlite db vs several (packages.db, sessions.db, traces.db — attach semantics, per-db writer isolation benefit); page_size/mmap/cache tuning for read-heavy service; VACUUM/ANALYZE ops; backup via Litestream vs sqlite .backup to S3; size limits in practice (multi-GB fine?).
5. The transactional outbox WITHOUT postgres: same-transaction outbox row + poller in sqlite (works fine single-node — confirm patterns), vs k8s-native alternative; exactly-once-ish delivery to derived stores; idempotency keys.
6. Local REGISTRY sqlite (desktop): same engine locally — schema sharing between INDEX and REGISTRY (one schema crate, two deployments), rusqlite vs sqlx-sqlite for a GUI app (async needs, update_hook for reactive UI!), litestream-style backup locally (no), and syncing REMOTE→LOCAL: does libsql embedded-replica sync fit the "remote serves while syncing → local takes over" pattern, or is app-level blob+row sync cleaner? (A parallel agent covers sync deeply; you cover the sqlite-engine angle: what libsql embedded replicas actually guarantee in 2026.)

SYNTHESIS: recommended stack (exact crates + versions: sqlx-sqlite vs rusqlite vs libsql crate), deployment shape for the INDEX (StatefulSet + Litestream sidecar?), the schema-portability strategy (sea-query everywhere? plain SQL with a thin dialect layer?), migration sequencing from postgres, and a table of every current postgres feature → its replacement.

DELIVERABLE:
- Write the FULL report (dense markdown, 500-1000+ lines, URLs inline) to /Users/philocalyst/Projects/Backend/.research/librarification/11-sqlite-index.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) recommended stack bullets, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)

### WebSearch: LiteFS fly.io 2026 maintenance status active
```
Web search results for query: "LiteFS fly.io 2026 maintenance status active"

Links: [{"title":"LiteFS - SQLite Replication File System | EveryDev.ai","url":"https://www.everydev.ai/tools/litefs"},{"title":"Fly.io Status","url":"https://status.flyio.net/"},{"title":"fly litefs cloud status · Fly Docs","url":"https://fly.io/docs/flyctl/litefs-cloud-status/"},{"title":"What is the status of LiteFS ? - Fly.io","url":"https://community.fly.io/t/what-is-the-status-of-litefs/23883"},{"title":"Sunsetting LiteFS Cloud - login-only - Fly.io","url":"https://community.fly.io/t/sunsetting-litefs-cloud/20829"},{"title":"LiteFS - Distributed SQLite · Fly Docs","url":"https://fly.io/docs/litefs/"},{"title":"LiteFS FAQ · Fly Docs","url":"https://fly.io/docs/litefs/faq/"},{"title":"fly litefs cloud · Fly Docs","url":"https://fly.io/docs/flyctl/litefs-cloud/"},{"title":"Fly.io Status - Incident History","url":"https://status.flyio.net/history"},{"title":"My site is in a really bad state - litefs - Fly.io","url":"https://community.fly.io/t/my-site-is-in-a-really-bad-state/16097"}]

Based on the search results, here's what I found about LiteFS and its maintenance status:

## Project Maintenance Status

The LiteFS repository was last pushed to in May 2026, indicating active maintenance. Additionally, the latest release is v0.5.14, published on April 22, 2025.

## Project Status

LiteFS is stable and running in production environments, though the project is still pre-1.0 so APIs may change and features could be removed. The project is written in Go, licensed under Apache 2.0, and is actively maintained while currently in a beta state.

## Important Note

It's worth noting that LiteFS Cloud, the managed service, is no longer available as of October 15, 2024. However, LiteFS itself is an open-source project that runs directly in your own instances, and continues to be actively maintained.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebSearch: Litestream v0.5 2026 SQLite replication kubernetes sidecar status
```
Web search results for query: "Litestream v0.5 2026 SQLite replication kubernetes sidecar status"

Links: [{"title":"Running in a Kubernetes cluster - Litestream","url":"https://litestream.io/guides/kubernetes/"},{"title":"GitHub - benbjohnson/litestream: Streaming replication for SQLite. · GitHub","url":"https://github.com/benbjohnson/litestream"},{"title":"Sqlite in Kubernetes Using Litestream | breadNET","url":"https://breadnet.co.uk/sqlite-in-kubernetes-using-litestream/"},{"title":"SQLite, Kubernetes & Litestream - monotux.tech","url":"https://www.monotux.tech/posts/2025/05/sqlite-kubernetes-sts/"},{"title":"GitHub - benbjohnson/litestream at v0.5.0","url":"https://github.com/benbjohnson/litestream/tree/v0.5.0"},{"title":"Litestream v0.5.0 is Here · The Fly Blog","url":"https://fly.io/blog/litestream-v050-is-here/"},{"title":"Litestream - Streaming SQLite Replication","url":"https://litestream.io/"},{"title":"Database corrupted when restoring from backup · Issue #1164 · benbjohnson/litestream","url":"https://github.com/benbjohnson/litestream/issues/1164"},{"title":"Getting Started - Litestream","url":"https://litestream.io/getting-started/"},{"title":"GitHub - mattn/litestream-sidecar-example · GitHub","url":"https://github.com/mattn/litestream-sidecar-example"}]

Based on the search results, here's information about Litestream v0.5 and SQLite replication in Kubernetes with a sidecar pattern:

## Litestream v0.5 Overview

Litestream v0.5 is faster and now supports efficient point-in-time recovery (PITR). CGO is now gone and the project has settled on modernc.org/sqlite instead of mattn/go-sqlite3. Additionally, a replica type for NATS JetStream was added, allowing users that already have JetStream running to get Litestream going without adding an object storage dependency.

## Kubernetes Sidecar Deployment

Litestream runs as a sidecar to your application within Kubernetes using StatefulSets. Litestream runs as a sidecar to the application and monitors the shared data directory.

The typical deployment pattern involves:
- An init container that runs a restore check automatically before the application starts
- Litestream runs as a sidecar process in the background, intercepting WAL checkpoints and streaming them to object storage in real time

## Current Status (2026)

Recent documentation from February 26, 2026 shows active usage of Litestream v0.5.9 in Kubernetes deployments. The tool continues to be a viable option for running SQLite-backed applica
```

### WebSearch: Turso libsql 2026 company status Rust rewrite licensing
```
Web search results for query: "Turso libsql 2026 company status Rust rewrite licensing"

Links: [{"title":"libSQL - Turso","url":"https://docs.turso.tech/libsql"},{"title":"GitHub - tursodatabase/turso: Turso is an in-process SQL database, compatible with SQLite. · GitHub","url":"https://github.com/tursodatabase/turso"},{"title":"Pekka Enberg on X: \"What are Turso, Turso Cloud, and libSQL? I get this question a lot, and AI agents seem confused about it, too. tl;dr: Turso is a new SQLite-compatible database, rewritten from scratch in Rust, currently in beta. libSQL is our open-source fork of SQLite, which powers Turso Cloud\" / X","url":"https://x.com/penberg/status/2032373944007688226"},{"title":"SQLite Rewritten in Rust with MVCC, Async I/O & Vector ...","url":"https://www.explainx.ai/blog/turso-database-sqlite-rust-rewrite-guide-2026"},{"title":"Turso: A Rust Rewrite of SQLite. Setup Guide and Whether It's Worth Your Time - DEV Community","url":"https://dev.to/arshtechpro/turso-a-rust-rewrite-of-sqlite-setup-guide-and-whether-its-worth-your-time-16lk"},{"title":"GitHub - tursodatabase/libsql: libSQL is a fork of SQLite that is both Open Source, and Open Contributions. · GitHub","url":"https://github.com/tursodatabase/libsql"},{"title":"Turso - Databases Everywhere","url":"https://turso.tech/"},{"title":"Meet Turso, a Rust-Based Evolution of SQLite","url":"https://blog.openreplay.com/turso-rust-sqlite-evolution/"},{"title":"Upcoming changes to the Turso Platform and Roadmap","url":"https://turso.tech/blog/upcoming-changes-to-the-turso-platform-and-roadmap"},{"title":"Turso · Database of Databases","url":"https://new.dbdb.io/db/turso"}]

Based on the web search results, here's a comprehensive overview of Turso's current status in 2026:

## Company Status & Strategic Direction

Turso is in beta with early customers working toward production deployments. It runs in production today at multiple organizations, though the maintainers say plainly that libSQL is production ready while the Rust engine is not, even though it is evolving quickly.

Project Limbo was officially renamed "Turso" and released in alpha form as a standalone SQLite-compatible database system, effectively becoming the successor to libSQL as the company's primary database engineering effort.

## Rust Rewrite Status

Rewriting SQLite in Rust started as an unassuming experiment, and due to its incredible success, replaces libSQL as their intended direction. Both run in production today: libSQL
```

### WebSearch: SQLite WAL write throughput rows per second benchmark batching 2025 2026
```
Web search results for query: "SQLite WAL write throughput rows per second benchmark batching 2025 2026"

Links: [{"title":"SQLite vs MySQL 2026: 4.9x Read Gap [Tested] - Tech Insider","url":"https://tech-insider.org/sqlite-vs-mysql-2026/"},{"title":"SQLite in Production - A Real-World Benchmark","url":"https://shivekkhurana.com/blog/sqlite-in-production/"},{"title":"Beyond the Single-Writer Limitation with Turso's Concurrent Writes","url":"https://turso.tech/blog/beyond-the-single-writer-limitation-with-tursos-concurrent-writes"},{"title":"Django SQLite Benchmark | Anže's Blog","url":"https://blog.pecar.me/django-sqlite-benchmark/"},{"title":"SQLite Optimizations For Ultra High-Performance","url":"https://powersync.com/blog/sqlite-optimizations-for-ultra-high-performance"},{"title":"SQLite performance tuning - Scaling SQLite databases to many concurrent readers and multiple gigabytes while maintaining 100k SELECTs per second - phiresky's blog","url":"https://phiresky.github.io/blog/2020/sqlite-performance-tuning/"},{"title":"Two-Way Confidential VMs (2cVM): Collaborative Confidential Computing for Mutually Distrustful Parties","url":"https://arxiv.org/pdf/2606.10615"},{"title":"Basic Performance Measurements of the Intel Optane DC Persistent Memory   Module","url":"https://arxiv.org/pdf/1903.05714"},{"title":"Design and Reliability of a User Space Write-Ahead Log in Rust","url":"https://arxiv.org/pdf/2507.13062"}]

Based on the search results, here's what I found regarding SQLite WAL write throughput with batching:

## Key Findings:

**Write Throughput with Batching:**
SQLite can easily write 500k rows per second with proper batching. This is a commonly cited benchmark figure for SQLite's write performance when optimized.

**Batching Strategy:**
For high throughput of writes, wrap multiple writes in the same transaction. A recent post claiming SQLite reaches 100k TPS using batched BEGIN IMMEDIATE transactions highlights that transaction batching is a key optimization technique.

**WAL Mode Benefits:**
SQLite's WAL mode, introduced in version 3.7.0 and refined through 3.46, partially mitigates the single-writer limitation. WAL mode allows readers to proceed concurrently with a single writer without blocking.

**Recent Context (2025-2026):**
SQLite 3.46, released in late 2025, weighs approximately 250 KB as a compiled library. The search results suggest that with proper tuning for modern hardware (SSDs), SQLite performance has improved significantly compare
```

### WebSearch: sqlx 0.8 SQLite pool options writer single connection reader multiple async rust
```
Web search results for query: "sqlx 0.8 SQLite pool options writer single connection reader multiple async rust"

Links: [{"title":"Pool in sqlx - Rust","url":"https://docs.rs/sqlx/latest/sqlx/struct.Pool.html"},{"title":"PSA: Your SQLite Connection Pool Might Be Ruining Your Write Performance – Evan Schwartz","url":"https://emschwartz.me/psa-your-sqlite-connection-pool-might-be-ruining-your-write-performance/"},{"title":"GitHub - transact-rs/sqlx: 🧰 The Rust SQL Toolkit. An async, pure Rust SQL crate featuring compile-time checked queries without a DSL. Supports PostgreSQL, MySQL, and SQLite.","url":"https://github.com/launchbadge/sqlx"},{"title":"rocket_db_pools - Rust","url":"https://api.rocket.rs/master/rocket_db_pools/"},{"title":"sqlx::Pool - Rust","url":"https://docs.rs/sqlx/0.5.0/sqlx/struct.Pool.html"},{"title":"sqlx - crates.io: Rust Package Registry","url":"https://crates.io/crates/sqlx"},{"title":"How to Handle Database Connection Pooling in Rust with SQLx and Deadpool","url":"https://oneuptime.com/blog/post/2026-01-07-rust-database-connection-pooling/view"},{"title":"PoolOptions in sqlx::pool - Rust","url":"https://docs.rs/sqlx/latest/sqlx/pool/struct.PoolOptions.html"},{"title":"Efficient Database Connection Management with sqlx and bb8/deadpool in Rust | Leapcell","url":"https://leapcell.io/blog/efficient-database-connection-management-with-sqlx-and-bb8-deadpool-in-rust"}]

Based on the search results, here's what I found relevant to your query about SQLx 0.8 SQLite pool options for handling writer and reader connections:

## Key Findings

**Pool Configuration:**
You can configure the connection limit and other parameters using PoolOptions. Create a pool with Pool::connect or Pool::connect_with and then call Pool::acquire to get a connection from the pool; when the connection is dropped it will return to the pool so it can be reused.

**SQLite-Specific Architecture Issue:**
For SQLite with async Rust applications, there's an important consideration: SQLite is single-writer, so any amount of contention at the SQLite level will severely hurt write performance. The fix is to use a single writer connection with writes queued at the application level, and a separate connection pool for concurrent reads.

**Performance Benefits:**
In benchmarks, the separate pools approach was ~20x faster than using a single pool with multiple connections. With this setup, write transactions serialize within the application. Tasks will queue waiting for the single
```

### WebSearch: SQLite version 3.46 3.47 3.48 2025 2026 new features JSONB
```
Web search results for query: "SQLite version 3.46 3.47 3.48 2025 2026 new features JSONB"

Links: [{"title":"Release History Of SQLite","url":"https://sqlite.org/changes.html"},{"title":"Recent SQLite News","url":"https://sqlite.org/news.html"},{"title":"SQLite Release 3.51.0 On 2025-11-04","url":"https://sqlite.org/releaselog/3_51_0.html"},{"title":"SQLite Release 3.51.2 On 2026-01-09","url":"https://sqlite.org/releaselog/3_51_2.html"},{"title":"SQLite User Forum: JSONB has landed","url":"https://sqlite.org/forum/forumpost/fa6f64e3dc1a5d97"},{"title":"SQLite Release 3.51.3 On 2026-03-13","url":"https://sqlite.org/releaselog/3_51_3.html"},{"title":"JSON Functions And Operators","url":"https://sqlite.org/json1.html"},{"title":"An overview of how JSON and new JSONB can be used with SQLite - Fedora Discussion","url":"https://discussion.fedoraproject.org/t/an-overview-of-how-json-and-new-jsonb-can-be-used-with-sqlite/103662"},{"title":"SQLite 3.51: New JSON Functions and Performance Boost","url":"https://www.linkedin.com/posts/arpitbhayani_sqlite-351-is-out-and-here-are-two-things-activity-7392178743345283072-dfaF"},{"title":"Native JSONB support for SQLite · SeaQL/sea-orm · Discussion #2426","url":"https://github.com/SeaQL/sea-orm/discussions/2426"}]

Based on the search results, here's what I found about SQLite versions 3.46-3.48 and JSONB features in 2025-2026:

## Version 3.46.0

Version 3.46.0 is an enhancement release with improvements to the query planner, especially a new faster implementation for VALUES clauses with many terms. The json_pretty() function was added with SQLite version 3.46.0 (2024-05-23).

## Version 3.47.0

Version 3.47.0 is an enhancement release with many feature improvements and performance enhancements, and notably the sqlite3_rsync command-line utility is now available for doing bandwidth-efficient live backups over SSH. Negative array index support for JSON operators was added in SQLite versions 3.47.0 (2024-10-21) and later.

## Version 3.48.0

The search results mention version 3.48.0 but provide limited specific details about its features.

## Version 3.51.0 (2025-2026)

For the most recent JSONB improvements, two new JSON functions, jsonb_each() and jsonb_tree() work the same as the existing json_each() and json_tree() functions except that they return JSONB for the "value" column when the "type" is 'array' or 'object'. The carray and percentile extensions are now built into the amalgamation, though they are disabled by def
```

### WebSearch: libsql embedded replica Rust crate 2026 consistency offline behavior
```
Web search results for query: "libsql embedded replica Rust crate 2026 consistency offline behavior"

Links: [{"title":"libsql - Rust","url":"https://docs.rs/libsql"},{"title":"libSQL — db interface for Rust","url":"https://lib.rs/crates/libsql"},{"title":"Reference - Turso","url":"https://docs.turso.tech/sdk/rust/reference"},{"title":"Database in libsql - Rust","url":"https://docs.rs/libsql/latest/libsql/struct.Database.html"},{"title":"libSQL - Rust Utilities","url":"https://rustutils.com/tools/libsql/"},{"title":"Introducing Embedded Replicas: Deploy Turso anywhere | by Glauber Costa | Turso blog | Medium","url":"https://medium.com/chiselstrike/introducing-embedded-replicas-deploy-turso-anywhere-2085aa0dc242"},{"title":"Microsecond-level SQL query latency with libSQL local replicas | by Pekka Enberg | Turso blog | Medium","url":"https://medium.com/chiselstrike/microsecond-level-sql-query-latency-with-libsql-local-replicas-5e4ae19b628b"},{"title":"libsql-sys — db interface for Rust // Lib.rs","url":"https://lib.rs/crates/libsql-sys"},{"title":"Builder in libsql - Rust","url":"https://docs.rs/libsql/latest/libsql/struct.Builder.html"},{"title":"GitHub - tursodatabase/embedded-replica-examples: Examples of using Turso/libSQL embedded replica sync capability. · GitHub","url":"https://github.com/tursodatabase/embedded-replica-examples"}]

I've found comprehensive information about the libsql embedded replica Rust crate. Here's what the search results reveal:

## Overview

libSQL is an embeddable SQL database engine based on SQLite, and the Rust API is a batteries-included wrapper around the SQLite C API to support transparent replication while retaining compatibility with the SQLite ecosystem, such as the SQL dialect and extensions.

## Embedded Replica Features

With libSQL Embedded Replicas, reads are local and writes are sent to the cloud primary, then reflected back to the replica. Embedded replicas allow you to run a local in-process copy of a remote libSQL database that stays in sync automatically; local reads are instant, writes are forwarded.

## Consistency and Offline Behavior

The Database type can be configured to open a local database file with the ability to sync from a remote database in consistent mode, which means that when a write happens it will not complete until that write is visible in the local db.

Optional strong consistency is available on a request-basis: if a particular request needs to be fully consistent, you can transparently g
```

### WebSearch: rqlite 2026 version write throughput kubernetes
```
Web search results for query: "rqlite 2026 version write throughput kubernetes"

Links: [{"title":"FAQ | rqlite","url":"https://rqlite.io/docs/faq/"},{"title":"Performance | rqlite","url":"https://rqlite.io/docs/guides/performance/"},{"title":"GitHub - rqlite/kubernetes-configuration: Configuration for running rqlite on Kubernetes","url":"https://github.com/rqlite/kubernetes-configuration"},{"title":"Features and Use Cases | rqlite","url":"https://rqlite.io/docs/features/"},{"title":"Kubernetes | rqlite","url":"https://rqlite.io/docs/guides/kubernetes/"},{"title":"rqlite/rqlite - Docker Image","url":"https://hub.docker.com/r/rqlite/rqlite"},{"title":"rqlite 7.5.0: Trading durability for write performance – Philip O'Toole","url":"https://philipotoole.com/rqlite-trading-durability-for-performance/"},{"title":"GitHub - rqlite/rqlite: The lightweight, fault-tolerant database built on SQLite. Designed to keep your data highly available with minimal effort. · GitHub","url":"https://github.com/rqlite/rqlite"},{"title":"rqlite - the lightweight, fault-tolerant database built on SQLite","url":"https://rqlite.io/"},{"title":"Write throughput when globally distributed · Issue #1020 · rqlite/rqlite","url":"https://github.com/rqlite/rqlite/issues/1020"}]

Based on the search results, here's what I found about rqlite, write throughput, and Kubernetes:

## Write Throughput

Depending on your machine (particularly its disk IO performance) and network, write throughput could be anything from 10 requests per second to hundreds of request per second. Disk performance is the single biggest determinant of rqlite performance on a low-latency network.

Every change to the system must go through the Raft subsystem, and the Raft subsystem calls fsync() after every write to its log. Raft does this to ensure that the change is safely persisted in permanent storage before writing those changes to the SQLite database.

## Performance Optimization

By using the bulk API, transactions, or both, write throughput will increase significantly, often by 2 orders of magnitude. Additionally, rqlite offers a Queued Writes mode for better write throughput when ultimate durability is not critical.

## Kubernetes Support

There are fully-featured Helm Charts available for rqlite on GitHub and Artifact Hub. You can also deploy your rqlite cluster on Kubernetes step-by-step, running rqlite as a Kubernetes StatefulSet.

## Design Philosophy

rqlite does not provide any scaling for writes, since all w
```

### WebSearch: SQLite transactional outbox pattern polling rust without postgres
```
Web search results for query: "SQLite transactional outbox pattern polling rust without postgres"

Links: [{"title":"russellromney/honker","url":"https://simonwillison.net/2026/Apr/24/honker/"},{"title":"Honker | Honker","url":"https://honker.dev/"},{"title":"Implementing the Outbox Pattern with SQLite and using Brighter | by Rafael Andrade | Medium","url":"https://medium.com/@actor-swe/implementing-the-outbox-pattern-with-sqlite-and-using-brighter-7da81c628c2b"},{"title":"GitHub - Vancoola/oxide-outbox: A high-performance, transactional Outbox pattern implementation for Rust applications","url":"https://github.com/Vancoola/oxide-outbox"},{"title":"GitHub - russellromney/honker: SQLite extension + bindings for Postgres NOTIFY/LISTEN semantics with durable queues, streams, pub/sub, and scheduler · GitHub","url":"https://github.com/russellromney/honker"},{"title":"Honker Review: The SQLite Extension That Yells Across Processes So You Don't Have To — The Agent Post","url":"https://theagentpost.co/posts/review-honker"},{"title":"Implementing the Outbox Pattern with SQLite and using Brighter - DEV Community","url":"https://dev.to/actor-dev/implementing-the-outbox-pattern-with-sqlite-and-using-brighter-15ha"},{"title":"honker/README.md at main · russellromney/honker","url":"https://github.com/russellromney/honker/blob/main/README.md"},{"title":"Revisiting the Outbox Pattern","url":"https://www.decodable.co/blog/revisiting-the-outbox-pattern"}]

Based on the web search results, I found several relevant resources for implementing the transactional outbox pattern with SQLite and polling in Rust:

## Key Solutions for SQLite Transactional Outbox Pattern

### 1. **Honker** (Primary Solution)
Honker implements the transactional outbox pattern, which ensures items are only queued if a transaction successfully commits. Honker provides bindings for Python, Node, Rust, Go, Ruby, Bun, Elixir, C++, .NET / C#, Java, and Kotlin.

The implementation uses a clever polling approach: A background thread fans the tick out to every subscriber, which runs SELECT ... WHERE id > last_seen and yields new rows. One poller thread per database regardless of subscriber count.

For Rust specifically, calling queue.enqueue(payload, tx=tx) inside your business transaction means the job row is ACID with the INSERT INTO orders that preceded it.

### 2. **Oxide Outbox** (Rust-Specific)
Oxide Outbox is a high-performance, flexible implementation of the Transactional Outbox pattern for Rust that e
```

### WebSearch: sqlx sqlite "query!" macro compile time check works 2025
```
Web search results for query: "sqlx sqlite "query!" macro compile time check works 2025"

Links: [{"title":"GitHub - transact-rs/sqlx: 🧰 The Rust SQL Toolkit. An async, pure Rust SQL crate featuring compile-time checked queries without a DSL. Supports PostgreSQL, MySQL, and SQLite.","url":"https://github.com/launchbadge/sqlx"},{"title":"query in sqlx - Rust","url":"https://docs.rs/sqlx/latest/sqlx/macro.query.html"},{"title":"sqlx - crates.io: Rust Package Registry","url":"https://crates.io/crates/sqlx"},{"title":"sqlx-macros — db interface for Rust // Lib.rs","url":"https://lib.rs/crates/sqlx-macros"},{"title":"How to Write Compile-Time Checked Queries with sqlx — Rust FAQ","url":"https://www.rustfaq.org/en/how-to-write-compile-time-checked-queries-with-sqlx/"},{"title":"SQLx — db interface for Rust // Lib.rs","url":"https://lib.rs/crates/sqlx"},{"title":"Crate Organization | launchbadge/sqlx | DeepWiki","url":"https://deepwiki.com/launchbadge/sqlx/3.1-compile-time-query-checking"},{"title":"sqlx 0.9.0 - Docs.rs","url":"https://docs.rs/crate/sqlx/latest"},{"title":"How to Use sqlx for Compile-Time Checked SQL Queries — Rust FAQ","url":"https://www.rustfaq.org/en/how-to-use-sqlx-for-compile-time-checked-sql-queries/"}]

Based on the search results, I can provide you with information about SQLx's SQLite "query!" macro compile-time checking:

## SQLx SQLite query! Macro Compile-Time Checking

SQLx is an async, pure Rust SQL crate featuring compile-time checked queries without a DSL. The "macros" feature enables support for the query*! macros, which allows compile-time checked queries.

### How It Works with SQLite

For SQLite, the macro can step through the output of EXPLAIN to identify columns that may or may not be NULL. SQLx macros like query! validate SQL syntax and types at compile time, preventing runtime database errors.

### Key Capabilities

The compiler checks your SQL syntax, validates column names, and ensures the Rust types match the database types. For SQLite specifically, INTEGER maps to i64.

### Offline Mode for Build Environments

The macros can be configured to not require a live database connection for compilation by running cargo install sqlx-cli, setting DATABASE_URL and running cargo sqlx prepare in your project, and checking the generated .sqlx directory into version control.

The compile-time checking functionality is well-established and continues to work in current versions (as evidenced by recent documentation from 2025-2026).


R
```

### WebSearch: sea-query 0.32 SQLite backend dialect differences postgres 2025
```
Web search results for query: "sea-query 0.32 SQLite backend dialect differences postgres 2025"

Links: [{"title":"Releases · SeaQL/sea-query","url":"https://github.com/SeaQL/sea-query/releases"},{"title":"What's new in SeaQuery 0.32.x | SeaQL","url":"https://www.sea-ql.org/blog/2024-12-03-whats-new-in-seaquery-0.32.x/"},{"title":"GitHub - SeaQL/sea-query: 🔱 A dynamic SQL query builder for MySQL, Postgres and SQLite","url":"https://github.com/seaql/sea-query"},{"title":"Dialect differences between SQLite and Postgres in Sequelize - DEV Community","url":"https://dev.to/grouparoo/dialect-differences-between-sqlite-and-postgres-in-sequelize-1f83"},{"title":"Dialect differences between SQLite and Postgres in Sequelize_node.js_Postgredaxiang-PostgreSQL","url":"https://devpress.csdn.net/postgresql/62f4d9a9c6770329307fa58a.html"},{"title":"sea-query/CHANGELOG.md at master · SeaQL/sea-query","url":"https://github.com/SeaQL/sea-query/blob/master/CHANGELOG.md"},{"title":"What's new in SeaQuery 0.28.0 | SeaQL","url":"https://www.sea-ql.org/blog/2022-12-30-whats-new-in-seaquery-0.28.0/"},{"title":"sea-query","url":"https://lib.rs/crates/sea-query"},{"title":"SQL Dialect differences in Sequelize: Grouparoo Blog","url":"https://www.grouparoo.com/blog/sql-dialect-differences"},{"title":"Runnable SQLite Docs: SQLite vs Postgres | Coddy","url":"https://coddy.tech/docs/sqlite/sqlite-vs-postgres"}]

Based on the search results, I found relevant information about SeaQuery 0.32 and the SQLite/Postgres dialect differences:

## SeaQuery 0.32 SQLite and Postgres Backend Differences

SeaQuery 0.32 added database-specific expression traits: PgExpr for Postgres and SqliteExpr for SQLite, implemented for FunctionCall, ColumnRef, Keyword, LikeExpr, and Value.

Postgres-specific and SQLite-specific expressions are organized into corresponding traits, and you need to import the trait into scope before constructing expressions with those backend-specific methods.

### Key Dialect Differences Supported:

1. **Partial Index Support**: SeaQuery supports partial indexes for SQLite using `CREATE UNIQUE INDEX IF NOT EXISTS` with WHERE clauses, while PostgreSQL has its own partial index syntax.

2. **Database-Specific Operators**: Postgres supports operators like word similarity (`<%`), while SQLite supports the MATCH operator for pattern matching.

3. **Vector Type Support**: SeaQuery 0.32 added Vector type, ColumnType::Vector, and Postgres-specific binary operators like EuclideanDistance, Neg
```

### WebSearch: SQLite page_size mmap_size cache_size tuning read heavy multi-GB production 2025
```
Web search results for query: "SQLite page_size mmap_size cache_size tuning read heavy multi-GB production 2025"

Links: [{"title":"High-Performance SQLite Reads in a Go Server - DEV Community","url":"https://dev.to/lovestaco/high-performance-sqlite-reads-in-a-go-server-4on3"},{"title":"SQLite performance tuning - Scaling SQLite databases to many concurrent readers and multiple gigabytes while maintaining 100k SELECTs per second - phiresky's blog","url":"https://phiresky.github.io/blog/2020/sqlite-performance-tuning/"},{"title":"SQLite Doesn’t Care About Your Scaling Assumptions: How I Served 250GB at 40ms. | by Anyim Ossi | Charisol Pulse | Medium","url":"https://medium.com/charisol-pulse/sqlite-doesnt-care-about-your-scaling-assumptions-how-i-served-250gb-at-40ms-ab1e264e66b0"},{"title":"SQLite in Production - A Real-World Benchmark","url":"https://shivekkhurana.com/blog/sqlite-in-production/"},{"title":"SQLite Memory Management Internals Explained","url":"https://www.sqliteforum.com/p/sqlite-memory-management-internals"},{"title":"SQLite 4.0 as a Production Database: 2025 Benchmarks and Pitfalls | Markaicode","url":"https://markaicode.com/sqlite-4-production-database-benchmarks-pitfalls/"},{"title":"How to Set Up SQLite for Production Use","url":"https://oneuptime.com/blog/post/2026-02-02-sqlite-production-setup/view"},{"title":"Tuning SQLite: When to Use and When to Avoid SQLITE_CONFIG_MMAP_SIZE","url":"https://runebook.dev/en/docs/sqlite/c3ref/c_config_covering_index_scan/sqliteconfigmmapsize"},{"title":"The SQLite Renaissance: Why the World's Most Deployed Database Is Taking Over Production in 2026 - Pockit Blog","url":"https://pockit.tools/blog/sqlite-renaissance-turso-d1-libsql-production-guide/"},{"title":"SQLite User Forum: memory vs mmap","url":"https://sqlite.org/forum/forumpost/322e041633"}]

I found comprehensive information about SQLite tuning for read-heavy, multi-GB production workloads. Here's what the search results reveal:

## Key Tuning Parameters for Read-Heavy Multi-GB Production Workloads

**Memory-Mapped I/O (mmap_size):**
Set mmap_size larger than your database file. Memory-mapped I/O creates a direct mapping where the application accesses database pages through pointer dereferences, and the kernel handles page-in transparently through the MMU's demand-paging mechanism. For production environments with multi-GB databases, common configurations use PRAGMA mmap_size = 30000000000 (30GB).

**Page Cache (cache_size):**
The Page Cache 
```

### WebSearch: rusqlite update_hook reactive desktop app GUI Rust 2025
```
Web search results for query: "rusqlite update_hook reactive desktop app GUI Rust 2025"

Links: [{"title":"rusqlite::hooks - Rust","url":"https://docs.rs/rusqlite/latest/rusqlite/hooks/index.html"},{"title":"Data change notifications · Issue #784 · rusqlite/rusqlite","url":"https://github.com/rusqlite/rusqlite/issues/784"},{"title":"🦀 Building Cross-Platform Desktop Apps in Rust with Dioxus 0.6","url":"https://chinmayvivek.medium.com/building-cross-platform-desktop-apps-in-rust-with-dioxus-0-6-46b4259d219e"},{"title":"Are we GUI yet?","url":"https://areweguiyet.com/"},{"title":"GUI — list of Rust libraries/crates // Lib.rs","url":"https://lib.rs/gui"},{"title":"rusqlite - Rust","url":"https://docs.rs/rusqlite/"},{"title":"GitHub - rusqlite/rusqlite: Ergonomic bindings to SQLite for Rust · GitHub","url":"https://github.com/rusqlite/rusqlite"},{"title":"Tauri - Todoアプリ(rusqlite版) - GitHub 出張所","url":"https://nkon.github.io/Tauri-Todo-Rusqlite/"},{"title":"Crate rusqlite - Rust","url":"https://prisma.github.io/prisma-engines/doc/rusqlite/index.html"}]

Based on the search results, here's what I found about combining rusqlite update_hook with reactive desktop app GUI development in Rust:

## Key Findings

Rusqlite provides commit, data change and rollback notification callbacks through its hooks module. However, the search results indicate that the update hook function from the SQLite C API wasn't initially exposed in rusqlite, though this may have been addressed in more recent versions.

## Reactive Desktop GUI Frameworks

For building reactive desktop applications in Rust, there are several modern options:

1. **Dioxus**: With frameworks like Dioxus, you can build beautiful, reactive desktop apps in Rust, leveraging a React-like component model with native performance.

2. **Other Frameworks**: Azul GUI is a free, functional, reactive GUI framework for rapid development of desktop applications written in Rust.

## Integration Pattern

A practical example from the results shows using rusqlite with Dioxus by importing Connection and params from rusqlite for database operations while building the UI with Dioxus components.

For the most current implementation details specific to update_hook functionality in 2025, you may want to check the official rusqlite documentation at docs.rs/rusqlite or the GitHub repository for the latest API updates.


REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
```

### WebFetch: https://litestream.io/guides/kubernetes/
```
# Litestream Kubernetes Sidecar Deployment Pattern

## Architecture Overview

The deployment uses Kubernetes StatefulSets with a single replica constraint, as Litestream "only runs on a single node at a time." Two containers work together: an init container handles database restoration, while a sidecar manages continuous replication.

## Init Container Pattern

The initialization phase runs before application startup:

```yaml
litestream restore -if-db-not-exists -if-replica-exists /var/lib/myapp/db
```

This command executes conditionally—skipping restoration if the database already exists or if no replicas are available. The init container mounts both the persistent volume and ConfigMap, receiving AWS credentials via environment variables from Kubernetes secrets.

## Sidecar Container Specification

The Litestream sidecar runs the `replicate` command continuously alongside your application. It shares the same persistent volume mount and ConfigMap configuration, exposing port 9090 for Prometheus metrics. The container specification mirrors the init container's environment setup but maintains a long-running process.

## Restore-on-Boot Procedure

Recovery occurs automatically during pod initialization. When a pod restarts after infrastructure failure, the init container checks for existing replicas in S3 and restores the database before the application container starts. Adding `-integrity-check full` to the restore command provides verification logs.

## StatefulSet Configuration Details

The setup requires:
- `spec.replicas: 1` for single-node constraint
- `volumeClaimTemplates` with persistent storage (recommended 100Mi+)
- ConfigMap volume mounting the `litestream.yml` file
- Service name configuration for StatefulSet pod naming

## S3 Replication Setup

Configuration specifies the database path and S3 destination:

```yaml
dbs:
  - path: /var/lib/myapp/db
    replica:
      url: s3://YOURBUCKET/db
```

Credentials inject via Kubernetes secrets as `LITESTREAM_ACCESS_KEY_ID` and `LITESTREAM_SECRET_ACCESS_KEY` environment variables.

## WAL Checkpointing Interaction

The guide references "WAL" in configuration contexts but doesn't detail checkpoint mechanics. The sidecar continuously monitors the shared data directory, implying real-time WAL frame replication to S3 as the application writes to SQLite.
```

### WebFetch: https://emschwartz.me/psa-your-sqlite-connection-pool-might-be-ruining-your-write-performance/
```
# SQLite Connection Pool Setup: Single Writer + Multiple Readers Pattern

## Core Architecture

The recommended approach mirrors SQLite's concurrency model by implementing separate pools: "one writer connection with writes queued at the application level, and a separate connection pool for concurrent reads."

## Configuration Code Example

```rust
let write_options = SqliteConnectOptions::new()
    .filename("my.db")
    .journal_mode(SqliteJournalMode::Wal);

let read_options = SqliteConnectOptions::new()
    .filename("my.db")
    .journal_mode(SqliteJournalMode::Wal)
    .read_only(true);

// Writer: single connection
let writer = SqlitePoolOptions::new()
    .max_connections(1)
    .connect_with(write_options)
    .await?;

// Readers: concurrent connections
let reader = SqlitePoolOptions::new()
    .max_connections(num_cpus::get() as u32)
    .connect_with(read_options)
    .await?;
```

## Performance Metrics

Under concurrent load testing, this dual-pool strategy vastly outperformed alternatives:

| Configuration | Total Time | Throughput | P50 Latency | P99 Latency |
|---|---|---|---|---|
| Single pool (50 connections) | 1.93s | 2,586 rows/sec | 474ms | 182s |
| Single writer connection | 83ms | 60,061 rows/sec | 43ms | 82ms |

This represents approximately **20x performance improvement**.

## Key Configuration Points

- **Writer pool**: `max_connections(1)` enforces serialization at application level
- **Reader pool**: `max_connections(num_cpus::get() as u32)` enables concurrent read access
- **Read-only mode**: Apply `.read_only(true)` to reader connections to prevent accidental writes
- **WAL mode**: Use `SqliteJournalMode::Wal` for both pools to enable concurrent read-write capability

An alternative implementation uses a `Mutex` wrapper achieving similar performance (~95ms), though separate pools make architectural intent clearer.
```

### WebFetch: https://phiresky.github.io/blog/2020/sqlite-performance-tuning/
```
# SQLite Performance Tuning for Read-Heavy Multi-GB Workloads

## Core PRAGMA Recommendations

**1. Journal Mode: WAL**
```sql
pragma journal_mode = WAL;
```
Rationale: Enables concurrent readers during write transactions by using a write-ahead log instead of direct database file modifications.

**2. Synchronous Commit**
```sql
pragma synchronous = normal;
```
Rationale: Reduces FSYNC overhead to WAL checkpoints only. The article notes this "is still completely corruption safe in WAL mode" while improving throughput.

**3. Memory-Based Temporary Storage**
```sql
pragma temp_store = memory;
```
Rationale: Stores transient indices in RAM rather than disk, avoiding I/O for intermediate query results.

**4. Memory Mapping**
```sql
pragma mmap_size = 30000000000;
```
Rationale: Leverages OS-managed virtual memory for database pages, reducing syscalls. The author emphasizes this approach doesn't consume physical memory proportional to the mapping size—the OS manages eviction dynamically.

**5. Page Size (Conditional)**
```sql
pragma page_size = 32768;
```
Rationale: Beneficial primarily when storing large binary objects; increases write overhead per operation since SQLite replaces whole pages.

## Performance Context
The article targets "100k SELECTs per second" on multi-gigabyte databases with many concurrent readers. However, no empirical performance numbers are provided—the recommendations are based on optimization principles rather than benchmarked results.

## Maintenance Operations
- Run `pragma optimize;` periodically to maintain query plans
- Execute `pragma wal_checkpoint(full)` to prevent unbounded WAL growth in write-heavy scenarios
```

### WebFetch: https://turso.tech/blog/upcoming-changes-to-the-turso-platform-and-roadmap
```
# Turso Platform Changes and Roadmap Summary

## Strategic Shift
Turso is refocusing resources on developing its Rust-based SQLite rewrite (formerly called "Limbo," now officially "Turso"). The company states: "Developers aren't just interested in using a better SQLite — they want to help build it."

## Immediate Changes for New Users

**Replication Model Simplification**
Edge replicas are being discontinued for new users. The company found that "70% of Turso users never create geographical replicas" and believes syncing SQLite files to APIs or devices is often more practical than edge replication.

**Database Capabilities Refocus**
Multi-DB schemas and database ATTACH capabilities are being removed for new users, though these will remain available to existing paid customers indefinitely.

## Near-Future Changes

**Infrastructure Consolidation**
Turso is migrating fully to AWS, discontinuing its Fly.io option. Free-tier users will be migrated automatically; paid users can choose to migrate but won't be required to.

**Project Naming**
The Limbo rewrite will be called "Turso" to reflect its significance, while distinguishing it from the cloud service.

## Licensing and Open Source Strategy

The new server implementation will be closed-source, but crucially: "everything that runs on the client will remain strictly open source." Users preferring open-source solutions can self-host using libSQL, as both implementations use compatible protocols.

## Customer Commitments

Existing paid customers retain all current features and production support indefinitely, regardless of platform changes.
```

### WebSearch: cr-sqlite CRDT extension SQLite 2026 production maturity
```
Web search results for query: "cr-sqlite CRDT extension SQLite 2026 production maturity"

Links: [{"title":"GitHub - nimmen/crdt-sqlite: Convergent, Replicated SQLite. Multi-writer and CRDT support for SQLite · GitHub","url":"https://github.com/nimmen/crdt-sqlite"},{"title":"GitHub - vlcn-io/cr-sqlite: Convergent, Replicated SQLite. Multi-writer and CRDT support for SQLite · GitHub","url":"https://github.com/vlcn-io/cr-sqlite"},{"title":"Trying out cr-sqlite on macOS | Simon Willison’s TILs","url":"https://til.simonwillison.net/sqlite/cr-sqlite-macos"},{"title":"Releases · vlcn-io/cr-sqlite","url":"https://github.com/vlcn-io/cr-sqlite/releases"},{"title":"GitHub - rodydavis/sqlite3_crdt: CRDT in SQLite (Pure C) · GitHub","url":"https://github.com/rodydavis/sqlite3_crdt"},{"title":"Intro – vlcn.io","url":"https://vlcn.io/docs/cr-sqlite/intro"},{"title":"https://github.com/vlcn-io/cr-sqlite | Ecosyste.ms: Awesome","url":"https://awesome.ecosyste.ms/projects/github.com/vlcn-io/cr-sqlite"},{"title":"GitHub - librocco/cr-sqlite · GitHub","url":"https://github.com/librocco/cr-sqlite"},{"title":"GitHub - awmuncy/sqlite-crdt: A simple library for creating CRDTs using SQLite, including persistent browser support · GitHub","url":"https://github.com/awmuncy/sqlite-crdt"}]

Based on the search results, here's what I found about cr-sqlite's production maturity status:

## CR-SQLite Overview

CR-SQLite is a run-time loadable extension for SQLite and libSQL that allows merging different SQLite databases together that have taken independent writes. In technical terms: cr-sqlite adds multi-master replication and partition tolerance to SQLite via conflict free replicated data types (CRDTs) and/or causally ordered event logs.

## Production Maturity Status

The search results indicate that cr-sqlite is still actively under development. The project maintains an active release cycle on GitHub with ongoing performance improvements and bug fixes. Recent efforts show a 10x improvement in perf when migrating crr tables, though there's a 15% reduction in perf when writing to crr tables, with a 5x reduction in CRDT metadata.

However, I should note that the search results don't contain specific information about cr-sqlite's production readiness status as of 2026, or detailed maturity metrics. The most recent information available discusses performance optimizations and ongoing development rather than an official production-ready designation. For the most current status regarding 202
```

### WebFetch: https://medium.com/charisol-pulse/sqlite-doesnt-care-about-your-scaling-assumptions-how-i-served-250gb-at-40ms-ab1e264e66b0
```
# SQLite Performance Optimization: Key Numbers & Configuration

## Performance Metrics

**Latency Improvement:** 180,000× reduction (initial >4 hours → final 40ms)

**Database Size:** 250GB uncompressed; reduced to ~155GB after defragmentation

**Hardware:** Single Linux server with 64GB RAM, NVMe storage

## Initial State ("250GB of Terrible")

- No indexes on 50+ million rows
- 4KB default page size
- 62% page utilization with 38% fragmentation
- ~4 million free-list pages
- No compression or full-text search
- Brute-force `LIKE '%pattern%'` queries requiring sequential scans

## Layer 1: Defragmentation Results

| Metric | Before | After |
|--------|--------|-------|
| File size | 250GB | ~155GB |
| Page utilization | 62% | ~98% |
| Free-list pages | ~4M | 0 |

**Action:** `VACUUM` operation with page size increased to 65KB (from 4KB default)

## Layer 2: Linux Kernel Tuning

- Read-ahead: reduced from 4MB to 128KB
- I/O scheduler: set to `none` (from mq-deadline/bfq)
- `vm.swappiness=10`
- Filesystem: XFS (over EXT4)

## Layer 3: PRAGMA Configuration

Key settings for read-only workload:

```
PRAGMA mmap_size = 274877906944  (~256GB memory-mapped I/O)
PRAGMA locking_mode = EXCLUSIVE
PRAGMA journal_mode = OFF
?immutable=1 (URI parameter)
```

**System Call Reduction:** 180 → 12 per query (15× improvement)

The article notes Part 2 covers compression, full-text search (FTS5), and sharding for the final optimization layers.
```

### WebSearch: SQLite "data_version" pragma change detection polling alternative NOTIFY
```
Web search results for query: "SQLite "data_version" pragma change detection polling alternative NOTIFY"

Links: [{"title":"SQLite User Forum: Most efficient way to tell if any database content has changed","url":"https://sqlite.org/forum/info/e4dd574a6a5d0d29"},{"title":"SQLite User Forum: Updating in-process connections on write","url":"https://sqlite.org/forum/info/bf7e0d8f1d3a50ad"},{"title":"Is it possible for SQLite to notify an application connected to it of database changes?","url":"https://sqlite-users.sqlite.narkive.com/bIy3dnQL/is-it-possible-for-sqlite-to-notify-an-application-connected-to-it-of-database-changes"},{"title":"SQLite User Forum: Global PRAGMA data_version","url":"https://sqlite.org/forum/info/e76cac71ac2db298"},{"title":"SQLite User Forum: Detecting database changes","url":"https://sqlite.org/forum/forumpost/2798df4be8"},{"title":"SQLite User Forum: Publish/subscribe queue","url":"https://sqlite.org/forum/info/304941521f0014c944e50b88fab6ca58a33d91384e2e58c95d1945c7609a635a"},{"title":"SQLite User Forum: Detecting Changes to Database tables? (for paged queries etc)","url":"https://sqlite.org/forum/info/1cb0160838780db1e216a145f285ad6dd36a0e0ede53e504f01baca963e1b99e"},{"title":"GitHub - russellromney/honker: SQLite extension + bindings for Postgres NOTIFY/LISTEN semantics with durable queues, streams, pub/sub, and scheduler · GitHub","url":"https://github.com/russellromney/honker"}]

Based on the search results, here's what I found about SQLite's `data_version` pragma and alternatives for change detection:

## PRAGMA data_version for Change Detection

The simplest approach is to hold open a second connection to the database and periodically execute PRAGMA data_version. If the value changes then a different connection has updated the data in the database.

However, there are important limitations to be aware of:

- The PRAGMA data_version value is unchanged for commits made on the same database connection
- An SQLite database connection can detect a change made to the database on another connection no better than a FILE* handle can detect an fwrite made to the same file on another FILE* handle - not at all.

## Alternatives to Polling

Several approaches can supplement or replace polling:

1. **Using PRAGMA user_version**: One workaround involves setting pragma user_version in the transaction and then polling the read-only connection until it sees the same user_version.

2. **Changelog Table Pattern**: A 'changelog' table can be us
```

### WebFetch: https://docs.rs/rusqlite/latest/rusqlite/hooks/index.html
```
# Rusqlite Hooks API Analysis

Based on the documentation provided, I cannot extract the specific details you've requested about `update_hook`, `commit_hook`, and `rollback_hook` implementations from this page content.

## What's Available

The page shows that rusqlite provides a `hooks` module with several structs and enums, including:

- **Structs**: `AuthContext` and `Wal`
- **Enums**: `Action`, `AuthAction`, `Authorization`, `CheckpointMode`, and `TransactionOperation`

The module description states it handles "Commit, Data Change and Rollback Notification Callbacks," but the actual method signatures, callback parameter details, and threading considerations are not included in this excerpt.

## What's Missing

To answer your specific questions about:
- Callback signatures and parameters for each hook
- Threading or async concerns
- The information each callback receives

You would need to view the actual API documentation for the `Connection` struct methods and the callback type definitions, which aren't fully detailed in this navigation/listing page.

**Recommendation**: Check the [rusqlite GitHub repository](https://github.com/rusqlite/rusqlite) or full docs.rs documentation for the complete `Connection` method signatures.
```

### WebSearch: SQLite ATTACH database cross-db transaction limitations write isolation 2025
```
Web search results for query: "SQLite ATTACH database cross-db transaction limitations write isolation 2025"

Links: [{"title":"Isolation In SQLite","url":"https://sqlite.org/isolation.html"},{"title":"ATTACH DATABASE","url":"https://sqlite.org/lang_attach.html"},{"title":"Beyond the SQLite single-writer limitation with concurrent writes | Hacker News","url":"https://news.ycombinator.com/item?id=45508462"},{"title":"Runnable SQLite Docs: ATTACH DATABASE | Coddy","url":"https://coddy.tech/docs/sqlite/attach-database"},{"title":"Does SQLite Support Transactions Across Multiple Databases? Ensuring Data Integrity in Separate DB Files — w3tutorials.net","url":"https://www.w3tutorials.net/blog/does-sqlite-support-transactions-across-multiple-databases/"},{"title":"SQLite Limitations and Internal Architecture - DEV Community","url":"https://dev.to/lovestaco/sqlite-limitations-and-internal-architecture-4o62"},{"title":"Transactions - Microsoft.Data.Sqlite | Microsoft Learn","url":"https://learn.microsoft.com/en-us/dotnet/standard/data/sqlite/transactions"},{"title":"File Locking And Concurrency In SQLite Version 3","url":"https://sqlite.org/lockingv3.html"},{"title":"SQLite concurrent writes and \"database is locked\" errors","url":"https://tenthousandmeters.com/blog/sqlite-concurrent-writes-and-database-is-locked-errors/"},{"title":"SQLite — SQLAlchemy 2.0 Documentation","url":"https://docs.sqlalchemy.org/en/20/dialects/sqlite.html"}]

I've completed the web search for your query about SQLite ATTACH database cross-database transaction limitations and write isolation. Here's what I found:

## Key Findings:

**Cross-Database Transactions:**
SQLite does support transactions across multiple databases via the ATTACH DATABASE command, with a single BEGIN/COMMIT cycle spanning the main database and all attached databases, ensuring atomicity for cross-database operations.

**Write Isolation and Locking:**
SQLite implements serializable transactions by actually serializing the writes, with only a single writer at a time to an SQLite database. While multiple database connections can be open and write to the database file simultaneously, they have to take turns, with SQLite using locks to serialize the writes automatically.

**Multi-Database Commit Protocol:**
When more than one attached database is being written to in a transaction, SQLite uses a more cautious commit protocol that requires a temporary journal, which is slower than single-file commits but still safe.

**Jou
```
