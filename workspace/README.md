Workspace crate map
===================

Nine crates, two utilities. All built with Buck2 via `nix/build/rust.bzl`.


heart
-----

The shared vocabulary every other crate imports. Defines identity (`PackageId`,
`SymbolId`, `EntryUri`, `ContentHash`), connection typestates (`Cold`/`Live` +
the `Connect` trait), ecosystem enums (`Language`, `Edition`, `Toolchain`), the
error taxonomy (`Failure`, `StoreError`, `Retryable`), symbol/search primitives
(`Symbol`, `SymbolKind`, `Score`, `Page`), the `DerivedStore` sink enum, and
the `Federation`/`Source`/`SourceId` federation model. Entry: `heart::lib`.
No logic — only contracts.


compiler
--------

Language producers and the emit pipeline. `compile/` holds one sub-module per
language (`rust`, `typescript`, `go`, `java`, `python`, `nix`), each
implementing the `Producer` trait from `compile/producer/`. The producer trait
defines the plan→cage/worker→decode lifecycle. `generate/` drives the parse
cache and orchestrates producer dispatch. `graph/` lowers `ir::Index` to
TerminusDB graph documents (`model.rs`, `from_ir.rs`, `link.rs`). `render/`
holds a Wadler-Lindig document algebra and five language backends
(`emit/rust.rs`, `go.rs`, `java.rs`, `typescript.rs`, `python.rs`). `treesitter.rs`
extracts CST snippets for implementation-level resolution.


compiler/intermediate-representation (`ir`)
-------------------------------------------

The IR schema shared between producers and consumers. Modules: `entry`
(top-level `Entry` + `Index`), `function`, `record`, `generics`, `protocols`,
`ty`, `module`, `primitives`, `parameter`, `syntax`, `kind`. The `pipeline`
module defines the ingest contract. `Option<Vec<T>>` distinguishes absent from
empty throughout — the distinction is semantically load-bearing.


registry
--------

The write-plane spine. `store/` is the content-addressed object store
(`Store<Cold>`/`Store<Live>` via `heart::Connect`). `index/` is the Postgres
global index (`GlobalStore`) — the orchestration source of truth for package
identity and `ResolutionState`. `queue/` is the poison-pill-safe Postgres job
queue with exponential-backoff retry. `coordination/` is the transactional
outbox that fans write intents to derived read stores (Qdrant / Terminus /
Tantivy). `ingest/` sanitizes untrusted archives with streaming budget and path
jail. `search/` is the package-discovery Tantivy index, polled from Postgres
watermarks. Key entry points: `registry::Store`, `registry::index::GlobalStore`,
`registry::queue::Queue`, `registry::coordination::Outbox`.


runtime
-------

The read-plane store set. `text/` is the replica-local Tantivy symbol index
(`TextIndex::open_or_create`). `vector/` is the Qdrant semantic store
(`Semantic<M, Cold/Live>`) with an `EmbeddingCache` and `SemanticGate`
capability token that gates the expensive path. `graph/` is the TerminusDB
client (`Graph<Cold/Live>`). `session/` is `PgSessionStore` — per-session
exploration graphs stored in Postgres so any gateway replica can serve any
session. Every store implements `heart::Connect`. Entry: `runtime::RuntimeError`
aggregates all per-area errors.


cas
---

Content-addressed byte store. The `Cas` trait has three implementations:
`MemoryCas` (tests), `DiskCas` (node-local BLAKE3-named files), and
`RegistryCas` (object-store backed). `tiered::Tiered` layers them
(L1 stampede cache → L2 disk → L3 registry). Keys are `heart::ContentHash`;
job-scoped keys are `heart::JobKey`. First-write-wins semantics; callers must
`invalidate` before repair.


server
------

The HTTP surface and coordination mesh. `config.rs` defines `ServerConfiguration`
with three-layer resolution (defaults → `nudox.toml` → `NUDOX_*` env vars).
`lib.rs` assembles `Server<M>`, connects all sources concurrently via
`heart::Connect`, and runs `serve()`. `http/router.rs` defines the route table
(read plane + write/admin plane). `search/` holds the `SearchPlanner` and
semantic embedder. `forge.rs` provides `ForgeRuntime<Cold/Ready>` — the owned
compile-plane capabilities (cage, CAS, toolchain set, overrides, observer) that
replace process globals. `poll.rs` runs background loops: outbox consumers,
text-index poller, package-index poller, queue worker, CAS GC. `coordination/`
has per-subsystem handlers (health, search, indexing, initialization).


util/caching
------------

Stampede-resistant caching primitives. `single_flight` coalesces concurrent
misses to one compute while propagating distinct errors to each waiter.
`stampede::StampedeCache` layers moka with XFetch probabilistic early
recomputation and stale-while-revalidate so a hot entry refreshes before it
expires. `jitter` de-synchronises batch expiries. Used by `cas` (L1 tier) and
available to any hot path.


util/sandbox
------------

Process isolation for untrusted producer execution. The `Cage` trait runs a
`SealedCommand` under a `CapabilityBudget` with cooperative `CancelToken`.
Production: `LinuxNamespaces` (bwrap + cgroup + seccomp + rlimits + Landlock).
Development: `DevPassthrough` (only constructible under `Policy::Development`).
`WorkerPool` is cage-internal for library-form producers (Nix, TypeScript,
Python). `Env` is an allowlist only — host environment is never inherited.
`Policy::from_env()` selects production vs development mode at startup.
