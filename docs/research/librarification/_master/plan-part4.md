
---

# Part IV — Planes & distribution

## §13 Compiler dual-form

**Source:** `docs/research/librarification/01-compiler-audit.md` · **Planes:** SHARED (lib) + ORCH (bin) · **Crates:** compiler-core, compiler-wire, compiler-daemon, producer-worker, sandbox

### 13.1 Current state — halfway there already

The live compiler's critical abstractions exist and are wired: `ForgeContext` injects cage/CAS/toolchains/overrides/workers/observer without process globals; `generate_with(ctx, input)` is a pure(ish) function `(ctx, PackageInput) → GeneratedPackage` (`generate/mod.rs:96–138`); sealing is a typestate `Job::acquiring.seal(tier)` that **type-enforces network off**; stage outputs are postcard under content-addressed JobKeys (producer version ‖ toolchain digest ‖ source hash ‖ lockfile hash). `compiler-daemon` implements postcard `POST /compile`, Cold→Ready ForgeRuntime, optional disk CAS — a real sealed-server skeleton.

Specific gaps (not architectural):

1. **Ambient edges:** `ToolchainSet::from_env`, `IsolationPolicy::require_worker` env reads, `buck_resource` via `current_exe` for Go/Java/C# oracles, `NUDOX_TYPESCRIPT_ORACLE`.
2. **Sandbox** nested at `workspace/compiler/sandbox`; production isolation is Linux+bwrap only; macOS falls back to `DevPassthrough` — which is exactly the trusted desktop path.
3. **Producers uneven:** Go is the only fully `ExecPlan::Commands` sealed producer; TS/Python/Nix use WorkerPool with in-process fallback; Rust/Java/C# are "adaptive" and bypass the plan→execute template (Rust runs fully in-process, no cage).
4. **Daemon wire incomplete:** pipeline produces surface + CST + occurrences + archive + snapshot; the HTTP response returns only surface + wire references + identifier facets → fixed by protocol v2 (§2.3).
5. **Graph lowering already pure** (`project`/`emit`) → moves to graph-cold (GD-8). **Treesitter trees never stored** — intentional (GD-11). **Incrementality is package-scoped** — §7 adds the symbol layer. **`render/` is pure** — ships in the embedded library for local previews.

### 13.2 Target design

```rust
/// Desktop trusted embed — assembled once by client at project open.
pub struct TrustedForgeContext {
    cas: Tiered<DiskCas>,             // shared with REGISTRY
    cage: DevPassthrough,             // trusted: no seal; workers optional
    toolchains: ToolchainSet,         // discovered, injected (never from_env in lib)
    oracles: OracleSet,               // explicit paths (kills buck_resource)
    observer: Arc<dyn ForgeObserver>, // job progress → GUI
}
```

- Keep DAEMON-PLAN vocabulary verbatim: `ForgeContext`, `generate_with`, `SealedInput`, `JobKey`, `ThreatTier` (GD-17).
- Library form: no env reads, no daemon module export; binaries (`compiler-daemon`, `producer-worker`) own env/policy assembly.
- Fleet form: seal the adaptive producers — Rust/Java/C# get real `ExecPlan` seal or fleet-restriction until they do; TS/Python/Nix keep WorkerPool with fallback removed on Hostile tier.
- Wire: v2 full artifacts (§2.3); `Cas` transport for fleet inputs/outputs.
- Acquisition (gix clone/fetch) leaves sealed compile paths → client (trusted) and INDEX materialize (untrusted) (GD-17).

### 13.3 Implementation steps

- **S13.1** `OracleSet` injection; delete buck_resource/current_exe resolution from library paths. **S13.2** Explicit `ToolchainSet` parameter; `from_env` moves to binary mains. **S13.3** `TrustedForgeContext` assemble in client (feature `embed`). **S13.4** Daemon v2 response (= S2.4). **S13.5** Seal Rust producer (worker-isolated; RA keep-alive allowed only in TrustedForge). **S13.6** Seal Java/C# or restrict to fleet-with-flag. **S13.7** Move acquisition out of compiler-core (with §16/§17 consumers). **S13.8** Daemon `Cas` mode (= S2.5). *Acceptance:* compiler-core builds with `--no-default-features` + no env access (lint: forbid `std::env` in lib targets); fixture parity between embedded and daemon runs (same JobKey ⇒ same artifacts).

---

## §14 Desktop toolchains & packaging

**Source:** `docs/research/librarification/18-desktop-toolchains.md` · **Plane:** GUI/REGISTRY

### 14.1 Verdict

Local desktop compilation is blocked by **resourcing**, not architecture: oracles resolve only through Buck `resources.json`, toolchains only via ambient env, and the full producer link set (RA + OXC + pyrefly + tsz + snix + host SDKs) is incompatible with a lean multi-platform GUI. Server packaging (`package.nix`) already materializes oracles beside `compiler-daemon`; desktop adopts the same **layout** with explicit **resolution** (`OracleSet` + discovered `ToolchainSet` injected at `TrustedForgeContext::assemble`).

Producer inventory: TS (OXC) and Python (pyrefly) are pure-Rust, MIT-friendly, ideal for offline local packs. Rust (ra_ap 0.0.341) is in-process, needs cargo/sysroot/proc-macro-srv, ~40 MiB+ code, multi-GiB RSS — flagship but feature-gated, eventually worker-isolated. Go/Java/C# are small Class-B oracles (binary/jar/publish dir) over large Class-C SDKs (Go ~240 MiB, JDK ~340 MiB, .NET ~700 MiB). snix is **GPL-3.0 — never links into the GUI** (GD-18).

### 14.2 Decisions

| Concern | Decision |
|---|---|
| Packaging | Hybrid: Class-B oracles in-app or language packs; system-discover Class-C SDKs (MVP); optional Nix channel; download-on-demand later |
| Size budget | Base GUI ≤ ~150 MiB compressed via Cargo/Buck features (`lang-*`) |
| MVP local languages | TypeScript, Python, Rust (if rustup present), Go (if go present) |
| Remote-only | Java, C#, Nix (GPL) — untrusted packages always fleet anyway (GD-16) |
| JobKey versioning | Toolchain component: path strings → semantic blake3 fingerprints (oracle + compiler versions) under **`nudox-producer/3`** so desktop and fleet CAS align |
| Workers | Keep oracle subprocesses; default-on worker pools for Hostile pure-Rust producers; planned rust worker; optional GPL-isolated nix worker binary |
| Platforms | darwin/linux, aarch64/x86_64 (per flake); Windows later |

Implementation steps: **S14.1** ToolchainSet discovery (rustup/go/node detection + digest fingerprints); **S14.2** `nudox-producer/3` JobKey migration (coordinated CAS epoch bump, fleet + desktop simultaneously); **S14.3** language-pack layout + loader (Class-B oracles); **S14.4** feature-gate matrix in CI (every `lang-*` combination builds); **S14.5** SDK discovery UX surfaced in GUI settings (→ §19). *Acceptance:* fresh macOS machine with rustup+node gets local TS/Python/Rust compile with zero manual config.

---

## §15 Trust & project model

**Source:** `docs/research/librarification/16-trust-project-model.md` · **Planes:** client-core/client · GUI

### 15.1 Normative algorithm

1. **Detect** project graphs under user-opened roots (manifest/workspace/lock detection per ecosystem — Cargo, npm/pnpm/yarn, Go, Python (poetry/uv/pdm/pip), Maven/Gradle, NuGet, Nix flakes).
2. **Build the Jail:** realpath prefix set of user roots ∪ explicit promotions. Canonicalize **before** jail membership checks (symlink escapes rejected).
3. **BFS path-like dependencies** that stay inside the jail → `TrustedSourceSet` (Cargo `{path}`, npm `file:/link:/workspace:`, Go `replace => ./dir`, Gradle `project(":x")`/`includeBuild`, NuGet `ProjectReference`, Nix `path:` inputs, …).
4. **Lockfile-parse registry/git pins** (after applying `[patch]`/overrides) → `UntrustedCoordinateSet` of `PinnedCoordinate`s.
5. **Never local-compile outside the jail.** Git and vendor trees are untrusted by default; `node_modules` is refused as a root; Cargo/NuGet config source redirects are ignored by default (diagnostic emitted).

Lockfiles are mandatory for generation-pinned DepSet sync; missing locks degrade to soft resolve with a quality diagnostic. Prefer native resolvers (`cargo metadata`, `go list`, …) when toolchains exist; pure parsers as fallback (diagnostic on divergence). Name collisions between path members and registry packages use disjoint identity (local key ≠ `PackageId`).

### 15.2 Types

```rust
pub struct Project<S>(…);            // S ∈ {Unresolved, Resolved, Indexed}
pub struct SourcePath<T>(PathBuf, PhantomData<T>);   // T ∈ {Trusted, Untrusted}
pub struct Jail { roots: BTreeSet<PathBuf> }          // realpath prefixes
pub struct ProjectGraph { members: …, path_deps: …, locks: … }
pub struct TrustedSourceSet(…); pub struct UntrustedCoordinateSet(Vec<PinnedCoordinate>);
// CompileLocally: only SourcePath<Trusted> feeds PackageInput / generate_with (GD-16)
```

A VS-Code-style **Workspace Trust gate** precedes any toolchain execution on a newly opened root (native resolvers execute build-adjacent code). Diagnostics catalogue (frozen codes): `trust::path_outside_jail`, `trust::symlink_escape`, `trust::vendor_ignored`, `trust::git_unindexed`, `resolve::missing_lock`, `resolve::multi_lock`, `resolve::config_source_replace`, `resolve::native_tool_failed`, `identity::shadow_local_registry`.

### 15.3 Implementation steps

- **S15.1** Manifest/lock detectors per ecosystem (table-driven; the research doc's Appendix A/B cheatsheets are the spec). **S15.2** Jail + canonicalization + BFS classifier. **S15.3** Lockfile parsers → PinnedCoordinate (native-resolver preference wiring). **S15.4** `Project<S>`/`SourcePath<T>` typestates in client-core; `Compile` bound to Trusted. **S15.5** Workspace Trust gate UX (→ §19 project manager). **S15.6** Acceptance test matrix T1–T10 from the research doc (virtual workspace, outside path dep, multi-root promotion, [patch], symlink escape, npm workspaces, go.work, missing lock, name shadowing).
- Prereqs: none hard; feeds §8 (package mapping), §13 (PackageInput), §16 (DepSet), §19 (trust viz).

---

## §16 Client library & sync engine

**Source:** `docs/research/librarification/14-client-sync.md` · **Crates:** client-core, client

### 16.1 Verdict

The right prior art is **content-addressed substitution** (Nix binary caches, git want/have), not bidirectional CRDT sync (PowerSync/cr-sqlite/Automerge rejected; ElectricSQL shapes and PowerSync priority buckets inform subset selection and progressive UX only). Codebase precedents to reuse: `heart::Connect` Cold/Live typestates, `Cas` + `Tiered` + `NoL3` read-through, `SingleFlight` stampede control, `SemanticGate` capability tokens, sealed `SnapshotPolicy` wire brands, `BlobManifest` dual hashing, `Progressive` job progress.

**Design stance (GD-20):** typestates for monotonic concerns (connection readiness, project resolve pipeline, trust plane); per-generation sync status is runtime state + optional `SyncedWitness` proofs. Every query routes through policy over a generation-pinned DepSet; generations become visible atomically after full hash-verified commit so mid-sync never mixes G_old and G_new.

### 16.2 Trait family and router

```rust
#[async_trait] pub trait Catalog      { async fn lookup(&self, id: PackageId) -> …; async fn generation(&self, id: PackageId) -> …; }
#[async_trait] pub trait TextSearch   { async fn search(&self, q: &TextQuery, page: &Pagination, dep: &DepSet) -> Result<Page<Scored<Symbol>>, _>; }
#[async_trait] pub trait VectorSearch { async fn search(&self, q: &VectorQuery, page: &Pagination, dep: &DepSet, gate: SemanticGate) -> …; }
#[async_trait] pub trait GraphOps     { async fn expand(&self, id: SymbolId, dep: &DepSet) -> Result<GraphSlice, _>; }
#[async_trait] pub trait Compile      { async fn compile(&self, src: &SourcePath<Trusted>) -> Result<BlobManifest, _>; }

pub struct Routed<L, R> { local: L, remote: R, sync: Arc<SyncEngine>,
                          status: Arc<GenerationStatusMap>, flights: SingleFlight<FlightKey> }
```

`Routed::search`: `sync.ensure_background(dep)`; if `status.all_ready(dep)` → local; else single-flight remote (waiters re-check local on wake — the generation may have committed while waiting). **Static generics** over enum dispatch (same reasoning as `Tiered<L3>`); erase to `Arc<dyn …>` only at the GUI boundary.

| Component | Local | Remote | Policy |
|---|---|---|---|
| Catalog | SqliteCatalog | HttpCatalog | Ready→local else remote+ensure |
| TextSearch | LocalTantivy | HttpSearch | all deps Ready → local |
| VectorSearch | LocalVectors | HttpSemantic | prefer remote until local corpus Ready; always SemanticGate |
| GraphOps | IrGraph (graph-cold) | TerminusHttp via INDEX | Ready → local IR walk |
| Compile | EmbeddedForge | RemoteCompile | **Trusted always local; Untrusted never local** |
| Blobs | DiskCas Tiered | presigned S3 GET | SyncEngine is the warmer |

Strict local-only APIs take `&[SyncedWitness]` and no `NetworkCapability` — compile-time proof of no network. SyncEngine is the write-side dual of Tiered read-through: `ensure(DepSet)` → `/v1/sync/plan` want/have → presigned pulls → BLAKE3 verify → atomic generation commit → GenerationStatusMap flip → derived-store build (Stages, §7). Offline mode: NetworkCapability absent, Ready subset only, explicit staleness surfaced.

### 16.3 Implementation steps

- **S16.1** client-core: traits, DepSet, GenerationStatusMap, Routed, typestates (pure; no tokio). **S16.2** client: HttpCatalog/HttpSearch/HttpSemantic over `/v1` (needs S2.2). **S16.3** SyncEngine v1 (plan/pull/verify/commit; needs S2.3 + S3.4). **S16.4** LocalTantivy/IrGraph/SqliteCatalog wiring over registry-local (needs S1.x extracts). **S16.5** EmbeddedForge (= S13.3) + RemoteCompile via `/v1/jobs`. **S16.6** SyncedWitness + strict local APIs. **S16.7** Progress channels → GUI (heart `Progressive`). *Acceptance:* the doc's core scenario as an integration test — query served remotely during pull, flips to local after atomic commit, offline serves Ready subset; mid-sync mixed-generation reads are impossible (assert via generation stamp on every result page).

---

## §17 INDEX: decomposition + SQLite service

**Sources:** `docs/research/librarification/02-registry-server-audit.md`, `11-sqlite-index.md` · **Plane:** INDEX · **Crates:** meta-store, index-service, ingest, blob

### 17.1 What Postgres owns today → where it goes

Six core tables + sessions; schema is sea_query-generated with no migrate files (portable by design: text CHECKs not enums, no LISTEN/NOTIFY). The only true multi-worker Postgres dependencies are `FOR UPDATE SKIP LOCKED` (job queue) and `pg_try_advisory_lock` (outbox drain) — both leave SQL entirely.

| Concern | Postgres today | Target |
|---|---|---|
| packages / parse_status / symbols | uuid PKs, jsonb facets/failure | INDEX sqlite (TEXT/BLOB + JSON) |
| jobs queue | SKIP LOCKED + leases | **ORCH/Kueue** (§18); INDEX keeps metadata rows (GD-24) |
| outbox + sink_watermarks | bigserial + advisory-lock drain | sqlite append + single-writer poller |
| sessions | PgSessionStore | optional sqlite; memory/dir locally |
| DDL/queries | PostgresQueryBuilder | SqliteQueryBuilder + sea-query-binder sqlx-sqlite |

### 17.2 SQLite operational architecture (GD-2)

- **Pattern:** one writer process, WAL mode, on a k8s StatefulSet `replicas: 1` with RWO PVC. Writer = sqlx pool `max_connections(1)` with `BEGIN IMMEDIATE` batching; readers = pool of N. `busy_timeout`, scheduled checkpoints.
- **Durability:** Litestream v0.5.x sidecar streams WAL → S3 (RPO ≈ 1 s, PITR); init container restores on boot. rqlite v10.x is the documented fallback if HA RTO demands it later; LiteFS/Turso-as-SoT/cr-sqlite rejected.
- **Throughput reality:** INDEX writes are bursty metadata + outbox appends — far below batched-WAL ceilings (10k–100k+ rows/s on NVMe). Reads dominate and are cacheable (moka stays).
- **Stack:** sqlx 0.8 (`sqlite` bundled) on INDEX; rusqlite (bundled + hooks, dedicated thread) on REGISTRY; shared DDL/Idens in meta-store (C9 resolved: dual stacks intentional). The already-present `libsqlite3-sys 0.30` finally gets used.

### 17.3 Decomposition of `workspace/registry` + `workspace/server`

- CAS (`cas/{blake3}` + `ptr/`, first-write-wins, dual-hash manifests, BLAKE3 verify) is already INDEX(S3)/REGISTRY(disk)-shaped → `blob` + heart Cas adapters (object_store for S3). Route `Store` through `Tiered` (S3.3).
- Ingest (path jail, allowlist, budgets) is a strong security boundary → `ingest` crate, reused by REGISTRY with a trusted `DirectorySource`.
- Search planes (package tantivy + symbol tantivy + qdrant + terminus client) → §11/§12/§10 crates. Dual text materializers (outbox sink + text Poller) collapse to one outbox-driven Stage.
- Server keeps: HTTP surface (→ `/v1`, §2), authz (real Principal + service auth), SearchPlanner, pollers (single-writer outbox drain — no advisory locks), `CompilerClient` (already ForgeRuntime-free) → talks ORCH.
- heart's Cold/Live Connect, deterministic ids, ResolutionState, Federation are the substrate for client (§16) — unchanged.

### 17.4 Implementation steps

- **S17.1** meta-store crate: `MetaStore` trait + Pg impl (legacy) + sqlite impl; dual-run behind a flag. **S17.2** sea-query Sqlite DDL + data migration tool (Pg → sqlite one-shot; verify row counts + spot hashes). **S17.3** Outbox drain single-writer poller (drop advisory locks). **S17.4** Litestream sidecar + restore-on-boot manifests; disaster-recovery drill in staging. **S17.5** index-service assembly: axum `/v1` + planner + sinks over meta-store; delete Postgres path. **S17.6** Session store swap. **S17.7** `BackendKind` health enum evolution (GD-23). *Acceptance:* full INDEX integration suite green on sqlite; restore-from-S3 drill under 5 min; zero Postgres references in the workspace (`grep -r sqlx-postgres` empty).

---

## §18 ORCH: Kubernetes orchestration

**Source:** `docs/research/librarification/12-orchestration.md` · **Plane:** ORCH · **Crate:** orch

### 18.1 Design (GD-19)

Split compile execution into two paths, replacing the Postgres queue:

- **Warm path:** long-lived `compiler-daemon` Deployment for interactive GUI/on-demand work. Direct HTTP with 503 backpressure; HPA (optionally KEDA 2.20) scaling. No queue in front.
- **Cold path:** bursty bulk/third-party builds as **Kubernetes Jobs admitted by Kueue v0.18.3** — ClusterQueues, ResourceFlavors (spot vs on-demand), fair sharing, preemption, WorkloadPriorityClass. Karpenter v1 NodePools provision (spot for cold).
- **ORCH server** (axum + kube-rs 4.0 + k8s-openapi 0.28): admits work, content-address dedups against INDEX (`input_hash` UNIQUE + in-flight coalescing), presigns S3 I/O, selects path, creates Jobs, reconciles outcomes into INDEX. It does NOT store blobs, do fair-share math, or implement a DAG engine.
- **Poison/retry without Postgres:** Job `backoffLimit` + `podFailurePolicy` (GA since 1.31): permanent exit codes → FailJob; disruption → Ignore; terminal poison recorded in INDEX metadata (GD-24). Idempotency is content-addressed end-to-end.
- **Sandbox layering:** gVisor RuntimeClass (minimal CPU-bound overhead) + `hostUsers: false` (userns GA in 1.36) + in-pod bwrap/landlock retained where the runtime allows.
- **Rejected:** Argo/Tekton (overkill for single-step compiles), CRDs as high-churn queue (1 MiB object cap, 8 GB etcd ceiling), NATS JetStream required-by-default (optional overflow/fan-out only: server 2.14.x, async-nats 0.49.x).
- Resource classes S/M/L/XL → flavors; artifacts flow presigned S3 multipart/zstd, never through ORCH.

### 18.2 Implementation steps

- **S18.1** orch crate: admission API (`/v1` submit/status/poison), input_hash dedup, presign issuance (needs S2.6). **S18.2** Warm-path proxying to daemon Service + backpressure semantics. **S18.3** Kueue install + ClusterQueue/Flavor manifests; Job template with podFailurePolicy + gVisor RuntimeClass + userns. **S18.4** Reconciler: Job status → INDEX rows; poison recording. **S18.5** Decommission Postgres jobs table claim path (after S17.5). **S18.6** Scale/chaos test: 10k-package backfill on spot with preemption; poison packages never retry-loop. *Acceptance:* queue semantics fully in k8s; INDEX has zero claim SQL; warm p50 latency unchanged vs today.

---

# Part V — GUI

## §19 lindsey: full refactor & product views

**Sources:** `docs/research/librarification/03-gui-audit.md`, `15-gui-references.md` · **Plane:** GUI · **Crate:** lindsey (path `workspace/gui`)

### 19.1 Current state

`lindsey` (~3.5k LOC, 12 modules, GPUI + gpui-component 0.5.x) is a structurally coherent prototype: correct boot (LogStore before tracing; Root/TitleBar wrapping one Workspace entity), established async bridge (`cx.spawn` + `async_compat::Compat` around reqwest), consistent event/global patterns (`SymbolOpenRequested`, `SemanticSearchRequested`; `AppState`, `LogStore`, `NudoxSettings`) — all preserved. But: **no project model, no trust tier, no local REGISTRY, no embedded search, no job system, no version diff, no dual-path client**; every fetch is unauthenticated HTTP to a hardcoded host. Known bugs to fix in Phase 0: `SearchPanel::run_search` filters fixtures instead of results (search_panel.rs:143–145); `/run` `graph` field never parsed so GraphView stays empty (workspace.rs:249–258); graph hover compares the wrong identifier; `auto_scroll` is a no-op; `loaded_chunks` written but unused.

### 19.2 Target architecture

Stores-over-client (GD-22): `ProjectStore`, `SymbolStore`, `SearchStore`, `JobStore`, `RegistryStore`, `NavHistory` as GPUI entities subscribing to `client`; views subscribe to stores; Workspace demoted to a layout shell (its HTTP methods deleted). Debounced multi-source search with generation tokens. Widgets: adopt gpui-component Dock (replace hand-rolled flex layout), virtualized List/Table for every long list, Tree, Modal, Toast, Editor w/ tree-sitter, form controls; imitate Zed Workspace/Pane/Item/Action patterns without vendoring GPL workspace code. Custom builds: lineage timeline, signature/docs diff, type-query builder, graph layout, trust chrome.

Reference synthesis (what each teaches): DevDocs/Dash — fuzzy keyboard-first multi-library search is THE primary surface (Dash's `searchIndex(name, type, path)` is the catalog mental model); Swift-DocC — compile-to-JSON-then-render: symbol pages consume a **versioned RenderModel** (DocC-style), not ad-hoc `kind: Value` rendering; rustdoc/docs.rs — clickable signatures, module trees, version bars; Hoogle — the UX template for **type-directed search** (nudox owns this cross-language via typed IR); Sourcegraph/GitHub — references panels, outline sidebars, file-vs-symbol finder split; JetBrains — two-tier docs (quick peek vs full page).

### 19.3 View roadmap

| Phase | Views |
|---|---|
| **P0** | Project manager with trust visualization (badge every hit with provenance — local-trusted vs INDEX); cmd-K omni-search (<10 ms local); Symbol page v1 (RenderModel); Jobs/logs panel; Settings (server_url, registry paths, SDK discovery §14); command palette; back/forward nav history; fix Phase-0 bugs; fixtures behind `cfg(debug_assertions)` |
| **P1** | Package browser; sync polish (GenerationStatus surfacing); **lineage timeline** (§6 edges; coordinate with client lineage APIs so the view is never a mock); references panel; **type-directed search mode** |
| **P2** | Version diffs (MembershipDiff/ContentDiff §9); richer graph canvas; quick-doc peek |
| **P3** | Annotations, pins, playgrounds, optional docset import |

### 19.4 Implementation steps

- **S19.1** Phase-0 bug fixes + fixture gating. **S19.2** Introduce stores + demote Workspace (client traits behind a thin shim until S16.x lands, then delete the shim). **S19.3** Dock adoption + virtualized lists. **S19.4** Project open flow: trust gate (S15.5) → resolve → DepSet display → sync progress (S16.7). **S19.5** Omni-search over Routed TextSearch. **S19.6** Symbol RenderModel schema + renderer (feeds off compiler-core `render/`). **S19.7** JobStore over `/v1/jobs` + local forge progress (replaces `nudox-indexer` subprocess + 200 ms poll thread with `background_executor().spawn_blocking`). **S19.8** Lineage timeline + type-search (P1, after S6.4/S6.5 + S11.4). *Acceptance:* P0 demo script — open project, trust-gate it, watch deps sync, search locally offline, open symbol page — passes on a cold machine.
