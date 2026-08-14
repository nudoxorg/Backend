# Handoff: Audit registry+server crates
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a1610edb95dba71f2.jsonl`
- Agent id: `agent-a1610edb95dba71f2`
- Deliverable: `/Users/philocalyst/Projects/Backend/docs/research/librarification/02-registry-server-audit.md`
- Existing report: False (0 lines)
- Tools used: 45 ({'Bash': 9, 'Read': 30, 'Agent': 6})
- Files/greps touched: 30
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)
- /Users/philocalyst/Projects/Backend/workspace/registry/Cargo.toml
- /Users/philocalyst/Projects/Backend/workspace/server/Cargo.toml
- /Users/philocalyst/Projects/Backend/workspace/heart/Cargo.toml
- /Users/philocalyst/Projects/Backend/workspace/registry/schema/queries.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/schema/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/coordination.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/index/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/queue/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/persist.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/connection.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/error/failure.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity/id.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity/symbol.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity/package.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/content.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/sink.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/access/federation.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/cache/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/cache/tiered.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/blob/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/store.rs
- /Users/philocalyst/Projects/Backend/workspace/server/http/router.rs
- /Users/philocalyst/Projects/Backend/workspace/server/config.rs
- /Users/philocalyst/Projects/Backend/workspace/server/poll.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/runtime/text/poll.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/runtime/vector/gate.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/runtime/graph/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/runtime/vector/embedding.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/runtime/text/index.rs
- /Users/philocalyst/Projects/Backend/workspace/registry/runtime/vector/mod.rs

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll conduct an exhaustive audit of workspace/registry, workspace/server, and workspace/heart. Let me start by exploring the directory structure broadly.

### fragment

Six agents are running in parallel covering all three crates. Let me also audit the Cargo.toml files and any migration files while waiting.

### fragment

I now have comprehensive information to write the full audit report. Let me check if any of the agents have completed and then write the report.

### fragment

Now I have enough information to write the comprehensive audit report. Let me create the output directory and write the full report:

## ORIGINAL PROMPT (complete this mission)

You are a research subagent producing an exhaustive codebase audit that feeds a major architecture plan.

CONTEXT — the nudox project (repo: /Users/philocalyst/Projects/Backend):
nudox is a multi-language code-intelligence / documentation platform. Current pipeline: package source is acquired, a per-language "producer" lowers it to a shared IR (workspace/ir), emitted as blobs into a content-addressed store, then fanned out to derived stores: Tantivy (text search), Qdrant (vectors), TerminusDB (graph), coordinated by Postgres (global index + job queue + transactional outbox) behind an axum server (workspace/server) with service logic in workspace/registry. A GPUI desktop app (workspace/gui) is the developer-facing product.

THE PLANNED RESTRUCTURE ("librarification"):
1. `compiler` becomes dual-form: stateless sandboxed server binary (untrusted packages, k8s fleet) + embeddable library (trusted local projects).
2. `server` becomes `client` — a library the GUI consumes, abstracting remote-vs-local compute behind traits/typestates (precedent: heart::Connect Cold/Live).
3. Remote side = INDEX (sqlite + S3, k8s-orchestrated); local side = REGISTRY (sqlite + disk blobs + embedded tantivy + embedded vector search). Both store IR + resolved tree-sitter trees + source efficiently.
4. Postgres dropped entirely: job queue → Kubernetes primitives via an orchestration server; global index → sqlite on the remote.
5. TerminusDB only for hottest packages (leaky-bucket admission); cold packages do graph ops over IR blobs directly.
6. Whole pipeline becomes deeply incremental (symbol-level change detection on committed changes).

YOUR MISSION — exhaustively audit workspace/registry, workspace/server, and workspace/heart. Read every module.

Answer with file:line evidence:
1. POSTGRES INVENTORY (the critical one): every table, migration file, and query in the codebase. For each: what it stores, which postgres-specific features it uses (LISTEN/NOTIFY, SKIP LOCKED, advisory locks, jsonb, arrays, uuid, triggers, transactional semantics), and whether it's (a) trivially portable to sqlite, (b) portable with redesign, (c) obviated by moving queueing to k8s. Cover: index/GlobalStore + ResolutionState, queue/ (poison-pill-safe job queue w/ backoff), coordination/ (transactional outbox fanning to Qdrant/Terminus/Tantivy), session store, tantivy watermark polling.
2. CAS/object store: registry's cas module (Cas trait: MemoryCas/DiskCas/RegistryCas/Tiered), object_store usage, blob naming, first-write-wins semantics, GC. How close is this already to the INDEX(S3)/REGISTRY(disk) split?
3. Tantivy: schema, tokenizers, what's indexed (symbols? packages?), the improvements that landed (symbol tokenizer, heuristics, ranking fusion, facets column mirroring `failure`), how indexing is driven (postgres watermarks), incremental story (deletes? re-index granularity).
4. Qdrant: collections, vectors, embedder (where do embeddings come from — local model? API?), EmbeddingCache, SemanticGate, payload schema.
5. TerminusDB client: how documents are published, schema management (LinkML → schema.json pipeline if visible), commit/branch usage, what lineage linking exists today.
6. Ingest: archive sanitization (streaming budget, path jail) — what's reusable for the local REGISTRY ingesting trusted projects?
7. Server: full route table (read plane + write/admin plane), SearchPlanner, forge.rs ForgeRuntime<Cold/Ready>, poll.rs background loops, config resolution (nudox.toml + NUDOX_* env), coordination handlers. Which parts belong in the future `client` library vs the INDEX service vs deleted?
8. heart: full inventory of identity types (PackageId, SymbolId, EntryUri, ContentHash), Connect/Cold/Live typestate machinery, Failure taxonomy, DerivedStore, Federation model, cache module (moka/blake3). These are the vocabulary the new trait layer builds on — document their exact shapes.
9. Assessment: for the INDEX/REGISTRY split, produce a table mapping every current registry/server module → its future home (INDEX service | REGISTRY local | shared client library | orchestration server | deleted), with the refactor cost and blockers for each.

DELIVERABLE:
- Write your FULL report as dense markdown to /Users/philocalyst/Projects/Backend/docs/research/librarification/02-registry-server-audit.md. Be exhaustive (600-1500+ lines). Cite file:line everywhere.
- Do NOT build anything; pre-existing build failures are out of scope. Root-level compiler/ is legacy; the live code is workspace/.
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) top concrete recommendations, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/Cargo.toml
```
1	[package]
2	name = "registry"
3	edition.workspace = true
4	version.workspace = true
5	license.workspace = true
6	
7	[lints]
8	workspace = true
9	
10	[lib]
11	path = "lib.rs"
12	
13	[dependencies]
14	# ── First-party workspace crates ──────────────────────────────────────────────
15	# `cas` and `version` were folded in: content-addressed storage now lives in
16	# `crate::cas` (backed by `object_store`), and version logic in `heart::version`.
17	heart = { path = "../heart" }
18	ir    = { path = "../ir" }
19	
20	# ── Async / concurrency ───────────────────────────────────────────────────────
21	tokio        = { version = "1.52",  features = ["full"] }
22	futures      = { version = "0.3",   features = ["std", "async-await", "alloc"] }
23	futures-util = { version = "0.3" }
24	tokio-stream = { version = "0.1" }
25	tokio-util   = { version = "0.6" }
26	
27	# ── SQL / postgres ────────────────────────────────────────────────────────────
28	sqlx = { version = "0.8", default-features = false, features = [
29	    "runtime-tokio-rustls",
30	    "macros",
31	    "postgres",
32	    "uuid",
33	    "json",
34	    "chrono",
35	    "migrate",
36	] }
37	libsqlite3-sys = { version = "0.30", features = ["bundled"] }
38	
39	# ── Query builder ─────────────────────────────────────────────────────────────
40	sea-query        = { version = "0.32" }
41	sea-query-binder = { version = "0.7", features = [
42	    "runtime-tokio-rustls",
43	    "sqlx-postgres",
44	    "with-chrono",
45	    "with-json",
46	    "with-uuid",
47	] }
48	
49	# ── Object / blob store ───────────────────────────────────────────────────────
50	object_store = { version = "0.11" }
51	
52	# ── Full-text search ──────────────────────────────────────────────────────────
53	tantivy = "0.22"
54	
55	# ── Read plane: vector store + caches (folded-in `runtime` module) ────────────
56	qdrant-client = "1.18"
57	moka     = { version = "0.12", features = ["future", "sync"] }
58	secrecy  = { version = "0.10", features = ["serde"] }
59
```

### Read: /Users/philocalyst/Projects/Backend/workspace/server/Cargo.toml
```
1	[package]
2	name = "server"
3	edition.workspace = true
4	version.workspace = true
5	license.workspace = true
6	
7	[lints]
8	workspace = true
9	
10	[lib]
11	name = "server"
12	path = "lib.rs"
13	
14	[[bin]]
15	name = "server"
16	path = "main.rs"
17	
18	[features]
19	default = []
20	
21	[dependencies]
22	# ── First-party workspace crates ──────────────────────────────────────────────
23	# The serving crates (runtime/protocol/cas) were consolidated into `registry`,
24	# which is now THE service. `telemetry`/`version` live in `heart` — the
25	# `telemetry` feature pulls in the OTLP/Pyroscope stack (server is its only
26	# consumer, so the heavy deps stay out of every other heart consumer).
27	heart    = { path = "../heart", features = ["telemetry"] }
28	ir       = { path = "../ir" }
29	registry = { path = "../registry" }
30	
31	# ── Async / concurrency ───────────────────────────────────────────────────────
32	tokio        = { version = "1.52",  features = ["full"] }
33	futures      = { version = "0.3",   features = ["std", "async-await", "alloc"] }
34	futures-util = { version = "0.3" }
35	async-stream = { version = "0.3" }
36	tokio-stream = { version = "0.1" }
37	tokio-util   = { version = "0.6" }
38	
39	# ── HTTP server (axum) ────────────────────────────────────────────────────────
40	axum       = { version = "0.7", features = ["macros"] }
41	tower      = { version = "0.4", features = ["util", "retry", "timeout"] }
42	tower-http = { version = "0.6", features = ["trace"] }
43	http       = "1"
44	http-body-util = { version = "0.1" }
45	hyper      = { version = "0.14" }
46	
47	# ── HTTP client (reqwest) ─────────────────────────────────────────────────────
48	reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
49	
50	# ── Object store ──────────────────────────────────────────────────────────────
51	object_store = { version = "0.11" }
52	
53	# ── Vector store ──────────────────────────────────────────────────────────────
54	qdra
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/Cargo.toml
```
1	[package]
2	name = "heart"
3	edition.workspace = true
4	version.workspace = true
5	license.workspace = true
6	
7	[lints]
8	workspace = true
9	
10	[lib]
11	path = "lib.rs"
12	
13	[features]
14	# The unified observability stack (heart::telemetry). Off by default so the
15	# heavy OpenTelemetry/Pyroscope deps are pulled in only by consumers that opt
16	# in (e.g. `server`); other consumers and the Buck build stay lean.
17	telemetry = [
18	    "dep:tracing-subscriber",
19	    "dep:metrics",
20	    "dep:metrics-exporter-prometheus",
21	    "dep:metrics-util",
22	    "dep:opentelemetry",
23	    "dep:opentelemetry_sdk",
24	    "dep:opentelemetry-otlp",
25	    "dep:opentelemetry-semantic-conventions",
26	    "dep:opentelemetry-appender-tracing",
27	    "dep:tracing-opentelemetry",
28	    "dep:pyroscope",
29	    "dep:pyroscope_pprofrs",
30	]
31	
32	[dependencies]
33	anyhow = "1.0"
34	
35	# `tracing` is a base dependency (the `cache` module logs unconditionally); the
36	# heavy subscriber/OTLP stack below stays behind the `telemetry` feature.
37	tracing = "0.1"
38	
39	# ── Telemetry stack (optional; enabled by the `telemetry` feature) ────────────
40	tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json", "registry"], optional = true }
41	metrics = { version = "0.24", optional = true }
42	metrics-exporter-prometheus = { version = "0.16", optional = true }
43	metrics-util = { version = "0.19", optional = true }
44	opentelemetry = { version = "0.28", features = ["metrics"], optional = true }
45	opentelemetry_sdk = { version = "0.28", features = ["rt-tokio", "logs", "metrics", "trace"], optional = true }
46	opentelemetry-otlp = { version = "0.28", features = ["http-proto", "reqwest-client", "logs", "metrics", "trace"], optional = true }
47	opentelemetry-semantic-conventions = { version = "0.28", optional = true }
48	opentelemetry-appender-tracing = { version = "0.28", optional = true }
49	tracing-opentelemetry = { version = "0.29", optional = true }
50
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/schema/queries.rs
```
1	//! Typed query builders — real `sea-query` statements bound onto sqlx-postgres.
2	//!
3	//! Every builder here returns a `(String, SqlxValues)` pair via
4	//! [`sea_query_binder::SqlxBinder`], ready to hand to
5	//! `sqlx::query_with(&sql, values)`. Statement *construction* is concrete and
6	//! total; the thin async execution/row-mapping glue lives in the store modules
7	//! (`index`, `queue`, `coordination`, `persist`) and calls into these.
8	//!
9	//! Grouped by the store that owns the table:
10	//! - [`index`] — `packages` + `parse_status` + `symbols`;
11	//! - [`queue`] — `jobs`, incl. the `FOR UPDATE SKIP LOCKED` dequeue;
12	//! - [`outbox`] — `outbox` + `sink_watermarks`;
13	//! - [`persist`] — the restart-reconciliation bulk reset.
14	
15	use sea_query::{Alias, Asterisk, Expr, Query, SelectStatement, SimpleExpr, UpdateStatement};
16	use sea_query_binder::{SqlxBinder, SqlxValues};
17	
18	use crate::{
19	    coordination::SinkKind,
20	    schema::{
21	        Jobs, Outbox, Packages, ParseStatus, SinkWatermarks, Symbols,
22	        codec::{self, StateColumns},
23	    },
24	};
25	
26	use crate::package::Coordinates as PackageCoordinates;
27	use heart::{
28	    ResolutionState, SymbolKind,
29	    content::ContentHash,
30	    ecosystem::Toolchain,
31	    identity::{PackageId, SymbolId},
32	};
33	use strum::IntoEnumIterator;
34	
35	/// The Postgres flavour every statement is rendered + bound against.
36	type Pg = sea_query::PostgresQueryBuilder;
37	
38	const PG: Pg = sea_query::PostgresQueryBuilder;
39	
40	// ═════════════════════════════════════════════════════════════════════════════
41	// index — packages + parse_status + symbols
42	// ═════════════════════════════════════════════════════════════════════════════
43	
44	/// Query builders over the `packages`, `parse_status`, and `symbols` tables.
45	pub mod index {
46	    use super::*;
47	
48	    /// `INSERT INTO packages (...) VALUES (...) ON CONFLICT (id) DO UPDATE ...
49	    /// RETURNING id` — the idempote
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/schema/mod.rs
```
1	//! The relational model — the single source of truth for the postgres data
2	//! layer.
3	//!
4	//! Every table is described here as a `sea_query::Iden` enum (the table name is
5	//! the first variant, the columns follow) plus a real
6	//! [`sea_query::TableCreateStatement`]. [`create_all`] and [`drop_all`] build the
7	//! whole schema; [`create_indexes`] emits the secondary indexes that the
8	//! scheduling / dequeue / watermark hot paths lean on. There is **no hand-written
9	//! SQL and no `sqlx::migrate!`**: [`schema_ddl`] renders these `sea_query`
10	//! statements to Postgres DDL and [`crate::index::GlobalStore::connect`] executes
11	//! them (all `IF NOT EXISTS`) on boot.
12	//!
13	//! ## Enum representation
14	//! Postgres native `ENUM` types are deliberately *not* used. Every discriminant
15	//! column (`parse_status.state`, `parse_status.phase`, `jobs.state`,
16	//! `outbox.sink_kind`, `packages.owner_kind`, `symbols.kind`, ...) is a plain
17	//! `text` column guarded by a `CHECK (col IN (...))` constraint. This keeps the
18	//! discriminant set evolvable with an ordinary `ALTER TABLE ... DROP/ADD
19	//! CONSTRAINT` (no `ALTER TYPE ... ADD VALUE` transaction hazards), keeps the
20	//! wire representation identical to the `strum`/serde string forms in
21	//! [`crate::schema::codec`], and lets sea-query bind them as plain strings.
22	//!
23	//! ## Byte / uuid / json columns
24	//! - `PackageId` / `SymbolId` / tenant ids  → `uuid`.
25	//! - `ContentHash` / `Generation`                 → `bytea` (exactly 32 bytes).
26	//! - `ResolutionState::Failed`/`DeadLettered` payload, `Toolchain` → `jsonb`.
27	
28	pub mod codec;
29	pub mod queries;
30	
31	use strum::VariantNames;
32	
33	use sea_query::{
34		ColumnDef, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, PostgresQueryBuilder,
35		Table, TableCreateStatement, TableDropStatement,
36	};
37	
38	// ─────────────────────────────────────────────────────────────────────────────
39	// Table + column identif
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/coordination.rs
```
1	//! The transactional outbox — how blob/manifest creation fans out to the
2	//! derived read-plane stores.
3	//!
4	//! When a new package generation is emitted, the registry records one
5	//! [`OutboxEntry`] per downstream sink in the *same* transaction that records
6	//! the manifest. Derived stores (qdrant vector index, terminus graph, tantivy
7	//! text index) each poll the outbox from their own watermark and materialize
8	//! what they missed. This is the standard transactional-outbox pattern: it makes
9	//! "the manifest exists but the vector index never heard about it" impossible
10	//! without a distributed transaction.
11	//!
12	//! Idempotency is enforced by a unique `(package, generation, kind)` dedupe key,
13	//! so re-emitting the same generation (e.g. after a retry) never double-fans.
14	
15	use chrono::{DateTime, Utc};
16	use heart::{
17		BackendKind, Cold, Connect, ConnectError, ConnectFailure, Live, PackageId, ResolutionState,
18		content::ContentHash,
19	};
20	use sqlx::{Row, postgres::PgRow};
21	
22	use crate::{
23		error::OutboxError,
24		index::GlobalStore,
25		schema::{codec, queries},
26	};
27	
28	/// A single fan-out intent: "package `X` reached generation `G`; sink `K` should
29	/// materialize it."
30	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
31	pub struct OutboxEntry {
32		/// The monotonic outbox sequence id — the watermark cursor pollers advance.
33		pub id: OutboxSeq,
34	
35		/// The package that changed.
36		pub package: PackageId,
37	
38		/// Which derived sink this intent is for.
39		pub kind: SinkKind,
40	
41		/// When the intent was recorded.
42		pub created_at: DateTime<Utc>,
43	}
44	
45	/// The monotonic sequence position of an outbox entry — pollers store the last
46	/// one they consumed as their watermark.
47	#[derive(
48		Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
49	)]
50	pub struct OutboxSeq(pub i64);
51	
52	/// Which derived read-plane
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/index/mod.rs
```
1	//! The global index — postgres, the orchestration source of truth.
2	//!
3	//! Every parsed package is recorded here with its deterministic [`PackageId`],
4	//! its current [`heart::ResolutionState`] (which phase it is in, what dependents
5	//! need it, whether it is stored), and the cross-store links the read plane
6	//! joins on. This is the relational spine; the object store holds the bytes, the
7	//! queue holds the work, and this holds the *truth* about what exists and where
8	//! it sits.
9	//!
10	//! Identity is never minted here — it is delegated to heart's deterministic
11	//! derivers ([`PackageCoordinates::id`], [`SymbolId::derive`]) so the same
12	//! identifier is recomputable offline against the same [`TerminusInstance`].
13	
14	use heart::{
15	    BackendKind, Cold, Connect, ConnectError, ConnectFailure, Live, Probeable, ResolutionState,
16	    content::ContentHash,
17	    identity::{EntryUri, PackageId, SymbolId},
18	    timed_probe,
19	};
20	use std::str::FromStr;
21	
22	use crate::package::Coordinates as PackageCoordinates;
23	use sqlx::{Row, postgres::PgRow};
24	
25	use sea_query_binder::SqlxValues;
26	
27	use crate::{
28	    GlobalPackage,
29	    error::IndexError,
30	    schema::{codec, queries},
31	};
32	
33	/// The `{organization}/{database}` TerminusDB instance every deterministic global
34	/// identifier is salted with, so a [`SymbolId`] is recomputable offline from the same instance.
35	///
36	/// The wrapped string is validated to the `organization/database` shape on construction.
37	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
38	pub struct TerminusInstance(String);
39	
40	impl TerminusInstance {
41	    /// Validate and wrap an `{organization}/{database}` instance token.
42	    /// Rejects anything that is not exactly two non-empty, slash-separated segments.
43	    pub fn new(token: impl Into<String>) -> Result<Self, IndexError> {
44	        let token = token.into();
45	
46	        match token.split_once
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/persist.rs
```
1	//! Durable persistence of the tracked-package registry across restarts.
2	//!
3	//! A process that dies mid-sync leaves packages stuck in
4	//! [`ResolutionState::Progressing`] with nobody working them — the state is
5	//! transient and meaningless across a restart. On load, this module resets every
6	//! such in-flight package back to a re-enqueueable state and hands them to the
7	//! queue, so a crash never strands work. Terminal states
8	//! ([`Stored`](ResolutionState::Stored), [`DeadLettered`](ResolutionState::DeadLettered))
9	//! are left untouched.
10	
11	use heart::{PackageId, ResolutionState};
12	
13	use crate::{error::RegistryError, index::GlobalStore, queue::Queue};
14	use crate::schema::{codec, queries};
15	
16	/// The outcome of a restart reconciliation: what was reset and re-enqueued.
17	#[derive(Debug, Clone, Default)]
18	pub struct Recovered {
19		/// Packages that were `Progressing` and got reset to re-enqueueable.
20		pub reset: Vec<PackageId>,
21	
22		/// Packages re-enqueued for a fresh attempt.
23		pub requeued: Vec<PackageId>,
24	}
25	
26	/// On startup, find every package stuck mid-sync, reset its transient state, and
27	/// re-enqueue it.
28	///
29	/// Reads the global index for all [`ResolutionState::Progressing`] packages,
30	/// transitions each back to `Unindexed { needed: false }` (a clean,
31	/// re-enqueueable state) in one transaction, and enqueues them. Idempotent: safe
32	/// to run on every boot.
33	#[tracing::instrument(skip_all)]
34	pub async fn reconcile_on_start(
35		index: &GlobalStore,
36		queue: &Queue,
37	) -> Result<Recovered, RegistryError> {
38		use sqlx::Row;
39	
40		use crate::error::IndexError;
41	
42		let pool = index.pool();
43	
44		// 1. Which packages were stranded mid-sync?
45		let (select_sql, select_vals) = queries::index::select_progressing();
46		let rows = sqlx::query_with(&select_sql, select_vals)
47			.fetch_all(pool)
48			.await
49			.map_err(IndexError::Database)?;
50		let reset: Vec<PackageId> = rows
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/queue/mod.rs
```
1	//! The durable job queue — the poison-pill-safe spine of the pipeline.
2	//!
3	//! A postgres-backed work queue using `SELECT ... FOR UPDATE SKIP LOCKED` so
4	//! many workers dequeue disjoint batches without blocking each other. Every job
5	//! carries its [`heart::ResolutionState`], its attempt count, and a lease; a
6	//! failed job is retried *only* if its [`heart::FailureKind`] is retriable *and*
7	//! it is under the attempt ceiling — otherwise it is dead-lettered. This is what
8	//! makes a package whose parse crashes or hangs a *bounded* problem rather than
9	//! a livelock.
10	
11	use std::{num::NonZeroU32, time::Duration};
12	
13	use chrono::{DateTime, Utc};
14	use heart::{
15		BackendKind, Cold, Connect, ConnectError, ConnectFailure, FailureKind, Live, PackageId,
16		ResolutionState,
17	};
18	use sqlx::{Row, postgres::PgRow};
19	
20	use crate::{
21		error::QueueError,
22		schema::{codec, queries},
23	};
24	
25	/// One unit of pipeline work: index this package.
26	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
27	pub struct Job {
28		/// The stable job id (row identity, distinct from the package it targets).
29		pub id: JobId,
30	
31		/// The package to index.
32		pub package: PackageId,
33	
34		/// The package's current lifecycle state (mirrors the global index; carried
35		/// on the job so a worker needn't re-read it to resume).
36		pub state: ResolutionState,
37	
38		/// How many times this job has been attempted.
39		pub attempts: u32,
40	
41		/// When the job was first enqueued.
42		pub enqueued_at: DateTime<Utc>,
43	
44		/// The lease deadline: while set and in the future, the job is owned by a
45		/// worker and invisible to `dequeue_batch`. `None` when idle.
46		pub lease_until: Option<DateTime<Utc>>,
47	}
48	
49	/// A compile-time witness that a job was leased by THIS worker **at dequeue
50	/// time**.
51	///
52	/// # What this witness proves — and what it does NOT
53	///
54	/// `dequeue_batch` is the only construc
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/connection.rs
```
1	//! Our typestate markers. We of course want to avoid connections or attempted
2	//! sends to a database that isn't actually alive.
3	//!
4	//! This is our attempt to mark this.
5	
6	use crate::error::ConnectError;
7	
8	/// A configured-but-unverified store handle.
9	pub struct Cold;
10	
11	/// A connected, ready store handle.
12	pub struct Live;
13	
14	/// A `Cold` store handle that can verify itself and transition to a `Live`
15	/// handle of the associated type.
16	#[diagnostic::on_unimplemented(
17		message = "`{Self}` cannot be connected",
18		note = "implement `Connect` so the server can bring this store up uniformly"
19	)]
20	pub trait Connect: Sized {
21		/// The `Live` handle produced on success (query methods live there).
22		type Live;
23	
24		/// Verify reachability/credentials/schema, then promote to `Live`.
25		async fn connect(self) -> Result<Self::Live, ConnectError>;
26	}
27	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/error/failure.rs
```
1	use chrono::{DateTime, Utc};
2	use serde::{Deserialize, Serialize};
3	
4	use crate::content::ContentHash;
5	
6	/// The distinct phases of indexing a package, in order.
7	///
8	/// The wire token for each variant is its lowercase name (`"acquiring"`, etc.),
9	/// matching the postgres `CHECK` domain.  `VariantNames::VARIANTS` is the
10	/// single source the schema CHECK constraint is derived from.
11	#[derive(
12	    Debug,
13	    Clone,
14	    Copy,
15	    PartialEq,
16	    Eq,
17	    Hash,
18	    Serialize,
19	    Deserialize,
20	    strum::Display,
21	    strum::EnumIter,
22	    strum::EnumString,
23	    strum::IntoStaticStr,
24	    strum::VariantNames,
25	)]
26	#[strum(serialize_all = "lowercase")]
27	pub enum Phase {
28	    /// Resolving the concrete version + downloading the source archive.
29	    Acquiring,
30	    /// Extracting + sanitizing the (untrusted) source archive.
31	    Extracting,
32	    /// The compiler is lowering source to IR.
33	    Compiling,
34	    /// Fanning the parsed result out to derived stores.
35	    Emitting,
36	}
37	
38	/// Structured cause captured for a failure without storing an untyped
39	/// `Box<dyn Error>` or bare ad-hoc String. The human message is always
40	/// derived from the cause at construction time.
41	#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
42	#[serde(tag = "type", content = "info")]
43	pub enum ErrorDetails {
44	    /// Fallback for cases where only a rendered message was captured.
45	    /// Prefer more specific variants when adding new failure paths.
46	    Message(String),
47	}
48	
49	/// A recorded failure, with enough context to decide retry vs dead-letter.
50	/// The serialized shape preserves the legacy "error" key for DB roundtrips.
51	#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
52	pub struct Failure {
53	    /// How many attempts have been made so far.
54	    pub attempts: u32,
55	    /// The phase the most recent attempt failed in.
56	    pub phase: Phase,
57	    /
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity/symbol.rs
```
1	//! Symbol identity: the in-package locator ([`EntryUri`]) and the deterministic,
2	//! per-instance [`SymbolId`] derived from it.
3	
4	use serde::{Deserialize, Serialize};
5	use smol_str::SmolStr;
6	
7	use super::{Id, PackageId, namespace};
8	use crate::symbol::Symbol;
9	
10	/// A globally-unique symbol identity — an [`Id`] branded with the serving
11	/// [`Symbol`] record, so it reads literally as `Id<Symbol>`. Deterministic per
12	/// graph instance (offline-recomputable) and salted with the TerminusDB instance
13	/// so two instances of the same corpus don't share ids. Derive one with
14	/// [`EntryUri::symbol_id`].
15	pub type SymbolId = Id<Symbol>;
16	
17	/// A symbol's path *within* a package — the ecosystem-relative locator that,
18	/// combined with the graph instance, yields a [`SymbolId`].
19	#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
20	pub struct EntryUri {
21		/// The package the symbol lives in.
22		pub package: PackageId,
23		/// The `::`/`.`-agnostic path segments to the symbol within the package.
24		pub path:    Box<[SmolStr]>,
25	}
26	
27	impl EntryUri {
28		/// The canonical string form of this URI: the package id followed by every
29		/// path segment, `/`-joined — a separator no ecosystem's symbol grammar uses,
30		/// so the form is unambiguous and injective.
31		pub fn canonical(&self) -> String {
32			self.path.iter().fold(self.package.to_string(), |mut uri, segment| {
33				uri.push('/');
34				uri.push_str(segment);
35				uri
36			})
37		}
38	
39		/// Derive the deterministic symbol id from the graph-instance token and this
40		/// URI — the one construction site. Salting with the instance keeps two
41		/// instances of the same corpus from colliding.
42		pub fn symbol_id(&self, instance_token: &str) -> SymbolId {
43			let mut bytes = instance_token.as_bytes().to_vec();
44			bytes.push(0);
45			bytes.extend_from_slice(self.canonical().as_bytes());
46			Id::from_name(&namespace::SYMBOL, &bytes)
47		}
48	}
49	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity/id.rs
```
1	//! The one identifier constructor for the whole system.
2	
3	use std::{fmt, marker::PhantomData};
4	
5	use serde::{Deserialize, Serialize};
6	use uuid::Uuid;
7	
8	/// Strongly-typed id: named by phantom `T` so ids for different entity kinds
9	/// are incompatible at the type level.
10	pub struct Id<T>(Uuid, PhantomData<fn() -> T>);
11	
12	impl<T> Id<T> {
13		/// Wrap a raw UUID as a tagged id — the inverse of [`Id::as_uuid`], used when
14		/// hydrating a row read back from storage.
15		pub const fn from_uuid(uuid: Uuid) -> Self { Self(uuid, PhantomData) }
16	
17		/// The raw UUID, for storage keys and wire encoding.
18		pub const fn as_uuid(&self) -> &Uuid { &self.0 }
19	
20		/// Mint a fresh random (v4) id. Use only for values with no natural,
21		/// reproducible key; anything content-derived should use [`Id::from_name`].
22		pub fn new_random() -> Self { Self(Uuid::new_v4(), PhantomData) }
23	
24		/// Derive a deterministic (v5) id from a namespace and name bytes — the single
25		/// primitive every `*Id` derivation (package coordinates, entry URIs, source
26		/// names) is built on, so "the same thing" always hashes to the same id.
27		pub fn from_name(namespace: &Uuid, name: &[u8]) -> Self {
28			Self(Uuid::new_v5(namespace, name), PhantomData)
29		}
30	
31		/// Re-tag this id as identifying a different type, preserving the UUID.
32		pub const fn cast<U>(self) -> Id<U> { Id(self.0, PhantomData) }
33	}
34	
35	impl<T> Clone for Id<T> {
36		fn clone(&self) -> Self { *self }
37	}
38	impl<T> Copy for Id<T> {}
39	impl<T> PartialEq for Id<T> {
40		fn eq(&self, other: &Self) -> bool { self.0 == other.0 }
41	}
42	impl<T> Eq for Id<T> {}
43	impl<T> PartialOrd for Id<T> {
44		fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
45	}
46	impl<T> Ord for Id<T> {
47		fn cmp(&self, other: &Self) -> std::cmp::Ordering { self.0.cmp(&other.0) }
48	}
49	impl<T> std::hash::Hash for Id<T> {
50		fn hash<H: std::hash::Hasher>(&self, state: &m
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity/package.rs
```
1	//! Package identity: the coordinates that name a package and the deterministic
2	//! [`PackageId`] derived from them.
3	
4	use std::borrow::Cow;
5	
6	use serde::{Deserialize, Serialize};
7	use smol_str::SmolStr;
8	use thiserror::Error;
9	
10	use super::Id;
11	use crate::ecosystem::Language;
12	
13	/// Identity tag for packages. Zero-variant: it exists only to brand [`Id`], never
14	/// to be constructed.
15	pub enum Package {}
16	
17	/// The stable, deterministic identity of a package across the whole system.
18	pub type PackageId = Id<Package>;
19	
20	/// Why a raw package name was rejected.
21	#[derive(Debug, Error, PartialEq, Eq)]
22	pub enum NameError {
23	    #[error("package name is empty")]
24	    NameEmpty,
25	    #[error("package name for {ecosystem} is too long (len={len}, max={max})")]
26	    NameTooLong { ecosystem: Language, len: usize, max: usize },
27	    #[error("package name for {ecosystem} contains invalid characters: {invalid_chars:?}")]
28	    NameHasInvalidChars { ecosystem: Language, invalid_chars: Vec<char> },
29	}
30	
31	/// Wrapper carrying raw input + source for Cargo/npm (both use semver but
32	/// kept distinct so ecosystem is traceable in the error chain).
33	#[derive(Debug, Error)]
34	#[error("invalid cargo version {raw:?}: {source}")]
35	pub struct CargoVersionError {
36	    raw: String,
37	    #[source]
38	    source: semver::Error,
39	}
40	
41	#[derive(Debug, Error)]
42	#[error("invalid npm version {raw:?}: {source}")]
43	pub struct NpmVersionError {
44	    raw: String,
45	    #[source]
46	    source: semver::Error,
47	}
48	
49	/// Wrapper for PEP 440.
50	#[derive(Debug, Error)]
51	#[error("invalid python version {raw:?}: {source}")]
52	pub struct PythonVersionError {
53	    raw: String,
54	    #[source]
55	    source: uv_pep440::VersionParseError,
56	}
57	
58	/// FlakeHub versions are Cargo-semver (`X.Y.Z+rev-{sha}`); kept distinct from
59	/// Cargo so the ecosystem stays traceable in the error chain.
60	#[derive(Debug, Error)]
6
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/content.rs
```
1	//! Content addressing, job keys, and freshness.
2	
3	use serde::{Deserialize, Serialize};
4	
5	/// The content hash which serves three roles:
6	/// 1. Ensuring that package freshness hasn't changed.
7	/// 2. Dedupe on the content
8	/// 3. type marking anything that depends on it
9	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
10	pub struct ContentHash([u8; 32]);
11	
12	impl ContentHash {
13		/// Wrap a raw 32-byte digest (e.g. read back from postgres).
14		pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }
15	
16		/// The raw digest bytes.
17		pub const fn as_bytes(&self) -> &[u8; 32] { &self.0 }
18	
19		/// Hash a contiguous byte buffer.
20		pub fn of_bytes(bytes: &[u8]) -> Self { Self(*blake3::hash(bytes).as_bytes()) }
21	
22		/// Lower-hex encoding for filesystem names and log lines.
23		pub fn hex(&self) -> String { data_encoding::HEXLOWER.encode(&self.0) }
24	
25		pub fn builder() -> ContentHasher { ContentHasher(blake3::Hasher::new()) }
26	}
27	
28	impl std::fmt::Display for ContentHash {
29		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
30			f.write_str(&self.hex())
31		}
32	}
33	
34	/// An incremental hasher for building a [`ContentHash`] from a stream of parts
35	/// without holding the whole package in memory.
36	pub struct ContentHasher(blake3::Hasher);
37	
38	impl ContentHasher {
39		/// Fold another chunk into the digest.
40		pub fn update(&mut self, bytes: &[u8]) -> &mut Self {
41			self.0.update(bytes);
42			self
43		}
44	
45		/// Finalize into a [`ContentHash`].
46		pub fn finalize(&self) -> ContentHash { ContentHash(*self.0.finalize().as_bytes()) }
47	}
48	
49	/// Domain-separated cache / producer job identity.
50	///
51	/// `JobKey = H(producer_version ‖ toolchain ‖ source ‖ dep_lock)` with each
52	/// component length-prefixed (little-endian `u64`), matching the historical
53	/// `CacheKey::derive` layout so keys stay stable across the CAS migration.
54	///
55	//
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/sink.rs
```
1	//! The remote-upload contract every derived store speaks.
2	//!
3	//! Exposes an extension trait, `SinkExt`, which adds retry-aware delivery
4	//! directly to any qualifying `tower::Service`.
5	
6	use std::future::{Ready, ready};
7	
8	use serde::{Deserialize, Serialize};
9	use tower::{
10	    retry::{Policy, Retry},
11	    Service, ServiceExt,
12	};
13	
14	use crate::error::Retryable;
15	
16	/// Which derived store a record fans out to.
17	///
18	/// The wire token for each variant is its lowercase name (`"vector"`, `"graph"`,
19	/// `"text"`), matching the postgres `CHECK` domain.  `VariantNames::VARIANTS`
20	/// is used to derive that domain from one source.
21	#[derive(
22	    Debug,
23	    Clone,
24	    Copy,
25	    PartialEq,
26	    Eq,
27	    Hash,
28	    Serialize,
29	    Deserialize,
30	    strum::Display,
31	    strum::EnumString,
32	    strum::EnumIter,
33	    strum::IntoStaticStr,
34	    strum::VariantNames,
35	)]
36	#[strum(serialize_all = "lowercase")]
37	pub enum DerivedStore {
38	    Vector,
39	    Graph,
40	    Text,
41	}
42	
43	#[derive(Debug, Clone)]
44	pub struct RetryTransient {
45	    remaining: usize,
46	}
47	
48	impl RetryTransient {
49	    pub const fn new(budget: usize) -> Self {
50	        Self { remaining: budget }
51	    }
52	}
53	
54	impl<Req, Res, E> Policy<Req, Res, E> for RetryTransient
55	where
56	    Req: Clone,
57	    E: Retryable,
58	{
59	    type Future = Ready<RetryTransient>;
60	
61	    fn retry(&self, _req: &Req, result: Result<&Res, &E>) -> Option<Ready<RetryTransient>> {
62	        match result {
63	            Err(e) if self.remaining > 0 && e.is_retryable() => {
64	                Some(ready(RetryTransient { remaining: self.remaining - 1 }))
65	            }
66	            _ => None,
67	        }
68	    }
69	
70	    fn clone_request(&self, req: &Req) -> Option<Req> {
71	        Some(req.clone())
72	    }
73	}
74	
75	/// Equips any compatible `tower::Service` with transient-retrying delivery.
76	pub trait SinkExt<Req>: S
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/access/federation.rs
```
1	//! Layered source federation.
2	//!
3	//! We have a single DEFINITIVE registry and any number of overlays that people can host themselves.
4	
5	use super::source::SourceId;
6	use serde::{Deserialize, Serialize};
7	
8	/// The role a source plays in a federation.
9	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display)]
10	pub enum SourceRole {
11	    Definitive,
12	    Overlay,
13	}
14	
15	/// A value tagged with the source it was resolved from.
16	#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
17	pub struct Sourced<T> {
18	    pub value: T,
19	    pub source: SourceId,
20	    pub role: SourceRole,
21	}
22	
23	impl<T> Sourced<T> {
24	    /// Map the value, preserving the source tag.
25	    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Sourced<U> {
26	        Sourced {
27	            value: f(self.value),
28	            source: self.source,
29	            role: self.role,
30	        }
31	    }
32	
33	    /// Converts a reference to a Sourced<T> into a Sourced<&T>.
34	    pub fn as_ref(&self) -> Sourced<&T> {
35	        Sourced {
36	            value: &self.value,
37	            source: self.source,
38	            role: self.role,
39	        }
40	    }
41	}
42	
43	/// A federation of registries: exactly one definitive base plus zero or more
44	/// precedence-ordered overlays.
45	pub struct Federation<S> {
46	    base: Sourced<S>,
47	    overlays: Vec<Sourced<S>>,
48	}
49	
50	impl<S> Federation<S> {
51	    /// Start a federation from its definitive base.
52	    pub fn new(base_id: SourceId, base: S) -> Self {
53	        Self {
54	            base: Sourced {
55	                value: base,
56	                source: base_id,
57	                role: SourceRole::Definitive,
58	            },
59	            overlays: Vec::new(),
60	        }
61	    }
62	
63	    /// Append an overlay at the lowest overlay precedence.
64	    pub fn with_overlay(mut self, id: SourceId, handle: S) -> Self {
65	        self.overlays.p
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/cache/mod.rs
```
1	//! Caching + content-addressed storage — the workspace's shared cache tier.
2	//!
3	//! This module unifies two things that used to be separate crates (`caching` and
4	//! `cas`): the stampede-resistant in-process cache primitives, and the tiered
5	//! content-addressed store built on top of them. They lived apart only for
6	//! historical reasons — `cas` depended on `caching` for its L1 tier, and both
7	//! depended on `heart` for [`ContentHash`](crate::ContentHash). Folding them in
8	//! here collapses that layering into one place.
9	//!
10	//! ## Stampede-resistant caching
11	//!
12	//! When a hot key expires (or a deploy cold-starts every replica at once), a
13	//! naive cache lets the whole herd recompute in lockstep and slam the backend.
14	//! That is a *cache stampede*. The primitives here package the three standard
15	//! defences:
16	//!
17	//! - [`single_flight`] — coalesce concurrent misses to one compute (the herd
18	//!   waits and shares the leader's result).
19	//! - [`stampede`] — [`StampedeCache`], a moka-backed cache that layers
20	//!   probabilistic early recomputation (XFetch) and stale-while-revalidate on
21	//!   top of coalescing, so a hot key is refreshed *before* it expires.
22	//! - [`jitter`] — de-synchronise the expiries of entries written together.
23	//!
24	//! ## Content-addressed storage
25	//!
26	//! One [`Cas`] trait, three tiers:
27	//! - **L1** — in-process [`StampedeCache`] (coalesced, stampede-resistant)
28	//! - **L2** — node-local plain-directory [`DiskCas`] (blake3-named files)
29	//! - **L3** — any [`Cas`] impl; typically a store over the live object store.
30	//!   When absent, the zero-sized [`NoL3`] sentinel fills the slot.
31	//!
32	//! Keys are [`ContentHash`](crate::ContentHash). Job-scoped composite keys are
33	//! [`JobKey`](crate::JobKey) (a newtype over the same digest).
34	//!
35	//! ### Erasure
36	//!
37	//! [`Cas`] uses `async fn` in trait (RPITIT) so it is not object-safe. The
38	//! design deliberately ke
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/cache/tiered.rs
```
1	//! L1 (StampedeCache) + L2 (DiskCas) + L3 (any `Cas`, `NoL3` when absent).
2	
3	use std::time::Duration;
4	
5	use bytes::Bytes;
6	use super::StampedeCache;
7	use crate::ContentHash;
8	
9	use super::disk::DiskCas;
10	use super::{Cas, CasError, EvictableCas};
11	
12	/// Assumed recompute cost when seeding L1 from a lower tier (XFetch bookkeeping).
13	const PROMOTE_COST: Duration = Duration::from_millis(1);
14	
15	/// Soft TTL for L1 entries. Cached producer outputs are content-addressed and
16	/// immutable under a fixed job key; a long TTL just bounds memory residency.
17	const L1_TTL: Duration = Duration::from_secs(24 * 60 * 60);
18	
19	/// Zero-sized sentinel meaning "no L3 configured".
20	///
21	/// Used as the default type parameter for [`Tiered`] so that bare `Tiered`
22	/// resolves to `Tiered<NoL3>` at existing call sites without any annotation.
23	/// This is the *only* way to express an absent L3 tier — there is no runtime
24	/// `Option` alongside it. All operations return [`CasError::Unsupported`], which
25	/// the tiered read/write paths skip via [`CasError::is_unsupported`].
26	///
27	/// `NoL3` holds no data; it is not [`EvictableCas`] because it has nothing to
28	/// evict (and, like any content-addressed L3, is treated as immutable).
29	#[derive(Debug, Default, Clone, Copy)]
30	pub struct NoL3;
31	
32	impl Cas for NoL3 {
33		async fn get(&self, _key: ContentHash) -> Result<Option<Bytes>, CasError> {
34			Err(CasError::Unsupported("L3 not configured (NoL3)"))
35		}
36	
37		async fn put(&self, _bytes: Bytes) -> Result<ContentHash, CasError> {
38			Err(CasError::Unsupported("L3 not configured (NoL3)"))
39		}
40	
41		async fn put_keyed(&self, _key: ContentHash, _bytes: Bytes) -> Result<bool, CasError> {
42			Err(CasError::Unsupported("L3 not configured (NoL3)"))
43		}
44	}
45	
46	/// Tiered CAS: in-process stampede cache, optional local disk, an L3 backend.
47	///
48	/// `L3` is any [`Cas`] implementation; the default is [`NoL3`], which the
49	/// comp
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/blob/mod.rs
```
1	//! Content-addressed package blobs.
2	//!
3	//! ## Why the old shape was unimplementable
4	//! The previous `Blob { concrete_syntax_tree: Tree, api_surface: Index,
5	//! source_text: tar::Archive<Cursor<Vec<u8>>> }` could not be built:
6	//! - a tree-sitter [`Tree`](arborium_tree_sitter::Tree) is non-`Send`,
7	//!   non-serializable C memory — it cannot cross a wire or land in a blob;
8	//! - one-tree-per-package is the wrong granularity (a package is many files);
9	//! - a `tar::Archive<_>` is a *reader*, not owned data — you cannot store it;
10	//! - holding the whole archive in a `Vec<u8>` is unbounded memory over
11	//!   untrusted input.
12	//!
13	//! ## The replacement: a manifest + content-addressed sections
14	//! A package snapshot is a [`BlobManifest`] — a small, serializable record —
15	//! plus a set of **individual content-addressed files**, each stored under its
16	//! own BLAKE3 digest in the object store's `cas/` space. This buys:
17	//! - **cross-version dedupe**: identical files across versions share one object;
18	//! - **ranged reads**: a caller can fetch one file without the whole package;
19	//! - **bounded memory**: files stream in and out; nothing is fully materialized;
20	//! - **integrity**: every read is verified against the hash it was keyed under.
21	//!
22	//! The CST is **not** stored — it is re-parsed on demand (tree-sitter is fast
23	//! and its output is non-portable). What we *do* persist from parsing is the set
24	//! of extracted [`ir::syntax::ResolvedReference`] spans, as plain serializable
25	//! data, so cross-references survive without the tree.
26	
27	use heart::{content::ContentHash, PackageId, Toolchain};
28	use ir::syntax::ResolvedReference;
29	use serde::{Deserialize, Serialize};
30	use smol_str::SmolStr;
31	
32	use crate::error::BlobError;
33	
34	pub mod creation;
35	pub mod emit;
36	
37	pub use creation::BlobBuilder;
38	
39	/// The serializable, content-addressed description of one package snapshot.
40	///
41	/// Sm
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/store.rs
```
1	//! The content-addressed object store — the registry's durable, immutable blob
2	//! layer over `object_store` (S3 / GCS / Azure / local filesystem).
3	//!
4	//! ## Layout
5	//! Two key spaces, both derived — never opaque refs:
6	//! - **`cas/{blake3-hex}`** — immutable, content-addressed file and section
7	//!   bytes. Keyed purely by hash, so identical content across packages and
8	//!   versions is stored exactly once (dedupe) and every read is integrity-
9	//!   checkable.
10	//! - **`ptr/{package-id}`** — a small mutable pointer from a package's
11	//!   deterministic [`PackageId`] (itself a UUIDv5 fingerprint of the validated
12	//!   coordinate tuple) to its current manifest's [`ContentHash`]. Keying on the
13	//!   id rather than raw name segments means a scoped name like `@types/node` or
14	//!   a version with slashes can never break the key layout or escape its
15	//!   prefix — the leaf is always a fixed-shape UUID.
16	//!
17	//! ## Idempotency
18	//! A `cas/` put of content whose hash already exists is a no-op — puts are
19	//! idempotent by construction, which is what makes blob emission safely
20	//! retryable.
21	
22	use std::sync::Arc;
23	
24	use futures::TryStreamExt as _;
25	use heart::{
26		BackendKind, Cold, Connect, ConnectError, ConnectFailure, Live, PackageId, Probeable,
27		content::ContentHash, timed_probe,
28	};
29	use crate::package::Coordinates as PackageCoordinates;
30	use object_store::{ObjectStore, path::Path};
31	
32	use crate::{
33		blob::{BlobManifest, creation::PendingSection},
34		error::{BlobError, StoreError, hash_hex, verify_integrity},
35	};
36	
37	/// The registry's content-addressed store over object storage.
38	///
39	/// `S` is the connection typestate: [`Cold`] until [`Connect::connect`] proves
40	/// the bucket is reachable, then [`Live`]. Read/write methods live only on the
41	/// `Live` form.
42	pub struct Store<S = Live> {
43		backend: Arc<dyn ObjectStore>,
44		_state: std::marker::PhantomData<S>,
45	}
46	
47	impl
```

### Read: /Users/philocalyst/Projects/Backend/workspace/server/http/router.rs
```
1	//! The route table.
2	//!
3	//! Splits a **read plane** (search, expand, sessions, health) from a
4	//! **write/admin plane** (add/get/sync packages) so the two can carry different
5	//! middleware (body limits, auth, rate limits) and a read never shares a code
6	//! path with ingest. Both planes share the `Arc<Server<M>>` application state.
7	
8	use std::sync::Arc;
9	use std::time::{Duration, Instant};
10	
11	use axum::{
12		Router,
13		error_handling::HandleErrorLayer,
14		extract::{DefaultBodyLimit, MatchedPath, Request},
15		http::StatusCode,
16		middleware::{self, Next},
17		response::Response,
18		routing::{get, post},
19	};
20	use tower_http::classify::ServerErrorsFailureClass;
21	use tower_http::trace::TraceLayer;
22	use tracing::Span;
23	
24	use registry::runtime::vector::EmbeddingModel;
25	use crate::config::Limits;
26	use crate::Server;
27	use crate::http::handlers::{admin, health, indexing, search};
28	
29	/// The largest read-plane request body: search/expand requests are JSON control
30	/// messages plus at most a pasted code snippet.
31	const READ_PLANE_BODY_CEILING: usize = 2 * 1024 * 1024;
32	
33	/// The largest write/admin-plane request body. The admin surface accepts only
34	/// small coordinate JSON, so its ceiling sits *below* the read plane's — the
35	/// configured `max_request_bytes` can tighten it further but never widen it.
36	const WRITE_PLANE_BODY_CEILING: usize = 64 * 1024;
37	
38	/// Build the full application router over a shared server handle.
39	///
40	/// Layers cross-cutting middleware via `tower`/axum: request tracing on both
41	/// planes (via [`tower_http::trace::TraceLayer`] — OBSERVABILITY-PLAN.md §6:
42	/// every route now gets one server span carrying `http.route`,
43	/// `http.request.method`, and `http.response.status_code` automatically,
44	/// replacing the previous hand-rolled `trace_request` middleware), per-plane
45	/// body limits (stricter on the write plane), and a request timeout on the
46	/// admin mutations. 
```

### Read: /Users/philocalyst/Projects/Backend/workspace/server/config.rs
```
1	//! Server configuration.
2	//!
3	//! Two fixes over the original design:
4	//! - endpoints are a **typed struct** ([`Endpoints`]), not a
5	//!   `HashMap<String, Url>` — a typo can no longer silently return `None`, and
6	//!   "not configured" is distinct from "misspelled";
7	//! - configuration is **layered** (defaults ← file ← environment) via `figment`
8	//!   rather than only constructible from `default()`, so prod/dev differ
9	//!   without a recompile.
10	
11	use std::net::SocketAddr;
12	use std::path::PathBuf;
13	
14	use secrecy::{ExposeSecret, SecretString};
15	use serde::{Deserialize, Serialize};
16	use smol_str::SmolStr;
17	use url::Url;
18	
19	/// The fully-resolved, typed configuration a [`crate::Server`] is built from.
20	///
21	/// The federation is configured as exactly one [`definitive`](Self::definitive)
22	/// source plus zero or more [`overlays`](Self::overlays), mirroring the runtime
23	/// [`heart::Federation`] topology (one base, ordered overlays).
24	#[derive(Debug, Clone, Serialize, Deserialize)]
25	pub struct ServerConfiguration {
26		/// The address the HTTP surface binds to.
27		pub serving_address: SocketAddr,
28	
29		/// The definitive (centrally-hosted) registry this server fronts.
30		pub definitive: SourceConfig,
31	
32		/// Self-hosted overlay registries, in precedence order (highest first). Each
33		/// extends and overrides the definitive base.
34		#[serde(default)]
35		pub overlays: Vec<SourceConfig>,
36	
37		/// Named custom registries clients may address packages against (the lookup
38		/// table behind an `origin` field on the add-package surface).
39		#[serde(default)]
40		pub custom_registries: Vec<CustomRegistry>,
41	
42		/// Operational limits (timeouts, body sizes, concurrency).
43		pub limits: Limits,
44	
45		/// This node's role in a horizontally-scaled deployment (`NUDOX_ROLE`).
46		/// Governs which background loops run; the HTTP surface (health/metrics)
47		/// is always served. Defaults to [`Role::All`] (single-no
```

### Read: /Users/philocalyst/Projects/Backend/workspace/server/poll.rs
```
1	//! The supervised background loops `serve()` runs alongside the HTTP surface:
2	//! the queue worker driving indexing jobs, one outbox consumer per derived
3	//! sink, and the replica-local index sync/watermark pollers.
4	//!
5	//! Every loop follows the same discipline: it never returns under normal
6	//! operation, it survives (logs + backs off) transient faults, and it is torn
7	//! down by task abort at its next await point when `serve()` winds down —
8	//! every unit of work it performs is idempotent, so an abort mid-tick is safe.
9	
10	use std::sync::Arc;
11	use std::time::Duration;
12	
13	use crate::registry::coordination::{OutboxEntry, SinkKind};
14	use registry::runtime::vector::{
15		EmbeddingCache, EmbeddingKey, EmbeddingModel, EmbeddingPurpose, SymbolPoint,
16	};
17	
18	use crate::coordination::indexing::Indexer;
19	use crate::error::ServerResult;
20	use crate::search::semantic::embedder::HttpEmbedder;
21	use crate::{Server, SourceStores};
22	
23	/// How long a crashed loop waits before its supervisor restarts it.
24	const SUPERVISOR_BACKOFF: Duration = Duration::from_secs(5);
25	
26	/// How many outbox intents a consumer drains per tick.
27	const OUTBOX_BATCH: usize = 64;
28	
29	/// How often the storage-reclamation duty runs. Deliberately coarse — GC is a
30	/// housekeeping sweep, not a hot path; running it hourly keeps its load off the
31	/// consumers while still bounding outbox growth.
32	const GC_INTERVAL: Duration = Duration::from_secs(3600);
33	
34	/// Supervise the indexing queue worker: restart it with backoff whenever the
35	/// underlying loop surfaces an error (a poisoned postgres connection, say).
36	///
37	/// `drain` is the graceful-shutdown signal: once fired the worker stops
38	/// dequeuing new jobs (in-flight jobs already finish inside `run_worker_until`),
39	/// `run_worker_until` returns `Ok(())`, and this supervisor exits cleanly rather
40	/// than restarting — so [`Server::serve`] can wait a bounded drain window for
41	/// in-
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/runtime/text/poll.rs
```
1	//! Keeping the text index fresh: tantivy polls postgres for newly-indexed work
2	//! and pulls it into the local index (rather than postgres pushing into
3	//! tantivy). This keeps the replica-local index a pure projection of the durable
4	//! source, catchable-up after a restart from a persisted watermark.
5	
6	use std::{
7		io::ErrorKind,
8		path::{Path, PathBuf},
9		time::Duration,
10	};
11	
12	use serde::{Deserialize, Serialize};
13	use sqlx::Row;
14	
15	use heart::Retryable;
16	
17	use crate::runtime::{error::{RowDecodeError, TextError}, text::index::TextIndex};
18	
19	/// A durable pointer into postgres marking how far the local index has been
20	/// caught up. Persisted alongside the tantivy directory so a restarted replica
21	/// resumes from where it left off rather than rebuilding from scratch.
22	///
23	/// Monotone: it only ever advances, so the poll loop is idempotent and
24	/// crash-safe (re-processing from a stale watermark just re-upserts, which is a
25	/// no-op by id).
26	#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
27	pub struct Watermark {
28		/// The last postgres change sequence pulled into the index.
29		pub sequence: u64,
30	}
31	
32	impl Watermark {
33		/// The bottom watermark: nothing consumed yet.
34		pub const BOTTOM: Watermark = Watermark { sequence: 0 };
35	}
36	
37	/// How many pending `outbox` intents one poll consumes at most.
38	const BATCH_LIMIT: i64 = 64;
39	
40	/// The backoff ceiling when transient poll errors stack up.
41	const MAX_BACKOFF: Duration = Duration::from_secs(60);
42	
43	/// The file the watermark persists to, next to the tantivy directory.
44	const WATERMARK_FILE: &str = "watermark.json";
45	
46	/// Pending change intents for the text sink, newest last:
47	/// `(outbox sequence, package id)`.
48	const PENDING_SQL: &str = "SELECT seq, package_id FROM outbox \
49		WHERE sink_kind = 'text' AND seq > $1 ORDER BY seq ASC LIMIT $2";
50	
51	/// The serving projection of every symbol in the giv
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/runtime/vector/gate.rs
```
1	//! The semantic-search gate — a real capability, not a formality.
2	//!
3	//! Qdrant-backed semantic search is heavy, so it is NEVER implicit: the default
4	//! surface is precise (tantivy) text search, and the semantic path runs only
5	//! when the caller presents a [`SemanticGate`]. The point of the gate is that
6	//! "ran an expensive semantic query by accident" is unrepresentable.
7	//!
8	//! ## Why a capability, not a free `engage()`
9	//! A gate you can mint for free is not a gate. User-facing search must go
10	//! through [`SemanticGate::issue`] via the server's query planner. The one
11	//! documented exception is [`SemanticGate::for_readiness`]: a fixed-reason
12	//! constructor for store liveness probes (zero-vector connectivity checks),
13	//! so audit tooling can distinguish readiness traffic from planner-issued
14	//! user queries.
15	//!
16	//! The token is:
17	//! - **non-`Copy` and non-`Clone`** — it cannot be duplicated;
18	//! - **`#[must_use]`** — obtaining one and dropping it is a warning;
19	//! - **consumed by value** in the search call — it cannot be stashed and reused
20	//!   for a second, unaudited query.
21	
22	/// An explicit, audited opt-in to (heavy) semantic search. Required by every
23	/// semantic query path.
24	///
25	/// Deliberately not `Copy`/`Clone`: one issuance authorizes one query.
26	///
27	/// **Issuance sites (closed set):**
28	/// 1. Server query planner — [`SemanticGate::issue`] with a planner reason
29	/// 2. Readiness probe — [`SemanticGate::for_readiness`] only
30	#[must_use = "a SemanticGate authorizes exactly one semantic query; dropping it wastes the authorization"]
31	#[derive(Debug)]
32	pub struct SemanticGate {
33		/// Why this gate was issued — carried for audit/tracing. Not part of any
34		/// equality/identity; purely explanatory.
35		reason: &'static str,
36	}
37	
38	impl SemanticGate {
39		/// Issue a gate authorizing one user-facing semantic query, recording *why*.
40		///
41		/// INVARIANT: call this o
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/runtime/graph/mod.rs
```
1	//! The graph runtime store (terminus) — the **source of truth** for how a
2	//! package's symbols are structured and related.
3	//!
4	//! Keyed on the durable [`SymbolId`] (not the old `Id<Symbol>`), so
5	//! results join cleanly with tantivy and qdrant. Every method that returns a set
6	//! streams it.
7	//!
8	//! ## Connection lifecycle
9	//! [`Graph<Cold>`] implements [`Connect`]: `connect()` verifies the endpoint,
10	//! credentials, and that the org/db exist, then promotes to [`Graph<Live>`]. The
11	//! verification is **one-time**. Thereafter the [`Live`] handle owns its own
12	//! reconnection: the pooled HTTP client re-establishes dropped connections
13	//! transparently, so a transient blip does not require re-running `connect()`.
14	
15	pub mod expansion;
16	pub mod resolution;
17	pub mod structure;
18	
19	use std::marker::PhantomData;
20	
21	use futures::Stream;
22	use secrecy::SecretString;
23	use serde::{Deserialize, Serialize};
24	use smol_str::SmolStr;
25	use url::Url;
26	
27	use heart::{
28		BackendKind, Cold, Connect, ConnectError, SymbolId, Live, Scored,
29		StoreError,
30	};
31	
32	use crate::runtime::error::GraphError;
33	
34	/// The kind of edge two symbols share. `are_related` returns the specific kind,
35	/// so callers can branch on *how* two symbols connect, not merely *whether*.
36	#[derive(
37		Debug,
38		Clone,
39		Copy,
40		PartialEq,
41		Eq,
42		PartialOrd,
43		Ord,
44		Hash,
45		Serialize,
46		Deserialize,
47		strum::Display,
48		strum::EnumString,
49	)]
50	pub enum RelationKind {
51		/// The target is a member of the source (a method of a type, a field of a
52		/// record, an item of a module).
53		Member,
54		/// The source references the target (a call, a use, a mention).
55		Reference,
56		/// The target occurs within the source's declaration/signature.
57		Occurrence,
58		/// The source implements the target (a type implements a trait/interface).
59		Implements,
60		/// The source extends/subclasses the target.
61		Extends,
62	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/runtime/vector/embedding.rs
```
1	//! Model-branded embedding vectors and the (fallible, batched) embedder that
2	//! produces them.
3	//!
4	//! The vector's *model* is lifted into the type via [`EmbeddingModel`], not just
5	//! its numeric dimension. So an `Embedding<OpenAi3Small>` and an
6	//! `Embedding<E5Small>` are different types even if their dimensions happened to
7	//! match — feeding one model's vectors to another model's store is a *compile*
8	//! error, not a silent semantic bug. The brand also supplies the dimension
9	//! (`M::DIMENSIONS`) and the stable [`ModelId`] (`M::id()`), so those never have
10	//! to be threaded or re-stated.
11	//!
12	//! The model brand/catalog/[`ModelId`] themselves live in [`super::model`]; this
13	//! file is only the embedding *value* type and the *embedder* interface.
14	//!
15	//! Storage is a validated `Arc<[f32]>`: length is checked once, at the
16	//! construction boundary, against `M::DIMENSIONS`. This drops the fixed-array
17	//! `serde_arrays` hack and removes any need for `generic_const_exprs`.
18	
19	use std::{marker::PhantomData, sync::Arc};
20	
21	use serde::{Deserialize, Deserializer, Serialize, Serializer};
22	
23	use super::model::{EmbeddingModel, ModelId};
24	use crate::runtime::error::EmbedError;
25	
26	/// What a piece of text is being embedded *as*. The same text embedded under two
27	/// purposes yields two distinct vectors, so code-vs-doc search stay separable in
28	/// one collection.
29	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
30	pub enum EmbeddingPurpose {
31		/// Embedding the code/API surface of a symbol.
32		Code,
33		/// Embedding the documentation/prose around a code object (like this).
34		Documentation,
35	}
36	
37	/// A model-branded embedding vector. The brand `M` fixes both the dimension and
38	/// the producing model at the type level; the values are length-validated
39	/// against `M::DIMENSIONS` at construction.
40	pub struct Embedding<M: EmbeddingModel> {
41		values: Arc<[f32]>,
42		
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/runtime/text/index.rs
```
1	//! The tantivy symbol/text index — the default, lightweight search surface for
2	//! finding items by name/signature.
3	//!
4	//! Owns a single replica-local tantivy directory. All indexing is synchronous
5	//! and CPU/disk-bound, so it runs on `spawn_blocking`.
6	
7	use std::path::Path;
8	use std::str::FromStr;
9	use std::sync::{Mutex, MutexGuard};
10	
11	use tantivy::{
12		Index, IndexReader, IndexWriter, TantivyDocument, TantivyError, Term, doc,
13		directory::MmapDirectory,
14		schema::{Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions},
15	};
16	
17	use heart::{ContentHash, SymbolId, Symbol, SymbolKind};
18	
19	use crate::runtime::error::TextError;
20	use crate::runtime::text::tokenizer;
21	
22	/// Heap budget for the single writer — modest, since symbol documents are tiny.
23	const WRITER_MEMORY_BYTES: usize = 50_000_000;
24	
25	/// The tantivy schema fields the symbol index is built over. Held so field
26	/// handles are resolved once, not per-operation.
27	///
28	/// Most fields are raw (untokenized) `STRING`: precise lookup works on whole
29	/// terms and dictionary regexes, and the stored fields carry everything needed
30	/// to rebuild a [`Symbol`] from a hit. The lowercased shadow fields make
31	/// contains-matching case-insensitive. On top of those, `name_tokens`/`fq_tokens`
32	/// are tokenized with the identifier analyzer ([`tokenizer`]) so a query for a
33	/// *part* of a name (`user` → `getUserById`, `http` → `HTTPServer`) matches —
34	/// which raw-string contains-matching cannot do across camel/snake boundaries.
35	/// That analyzer lives in the index's tokenizer manager and is re-registered on
36	/// every open (see [`TextIndex::open_or_create`]).
37	#[derive(Clone)]
38	pub struct TextSchema {
39		pub(crate) schema: Schema,
40		/// The [`SymbolId`] uuid — stored, and the upsert/delete key.
41		pub(crate) id: Field,
42		/// The owning package's uuid — stored.
43		pub(crate) package: Field,
44		/// The lowercase [
```

### Read: /Users/philocalyst/Projects/Backend/workspace/registry/runtime/vector/mod.rs
```
1	//! The vector/semantic runtime store (Qdrant) and the embedding types that feed
2	//! it.
3	//!
4	//! Two type-level guarantees live here:
5	//! - the collection's vector dimension is the brand's `M::DIMENSIONS`, so a
6	//!   wrong-dimension query cannot be formed, and [`Connect`] asserts the live
7	//!   collection actually has that dimension;
8	//! - semantic search is reachable only on a [`Live`] store AND only when handed
9	//!   a [`SemanticGate`], so the heavy path is never taken implicitly.
10	
11	use std::{
12		future::Future,
13		marker::PhantomData,
14		num::NonZeroUsize,
15		pin::Pin,
16		sync::Arc,
17		task::{Context, Poll},
18	};
19	
20	use futures::Stream;
21	use heart::{
22		Advisory, BackendKind, Cold, Connect, ConnectError, Cursor, Live, Retryable, Scored,
23		Symbol, SymbolId,
24	};
25	use qdrant_client::Qdrant;
26	use serde::{Deserialize, Serialize};
27	
28	pub mod cache;
29	pub mod embedding;
30	pub mod gate;
31	pub mod model;
32	pub mod similarity;
33	
34	pub use cache::{EmbeddingCache, EmbeddingKey};
35	pub use embedding::{Embedder, Embedding, EmbeddingPurpose};
36	pub use gate::SemanticGate;
37	pub use model::{EmbeddingModel, ModelId, catalog as models};
38	
39	use crate::runtime::error::VectorError;
40	
41	/// The keyset key a semantic-search [`Cursor`] resumes from: the last hit's
42	/// score paired with its id (score alone is not unique). Ordered so pagination
43	/// is stable across an eventually-consistent index.
44	///
45	/// Semantic cursors are explicitly [`Advisory`]: an ANN index has no cheap
46	/// content hash, so snapshot freshness cannot be enforced. Callers see this
47	/// in the type — `Cursor<SemanticCursorKey, Advisory>` — and understand that
48	/// completeness is best-effort across index updates.
49	pub type SemanticCursorKey = (heart::Score, SymbolId);
50	
51	/// A connected [`Semantic`] store — the form query methods live on.
52	pub type SemanticLive<M> = Semantic<M, Live>;
53	
54	/// Why a raw collection name was rej
```
