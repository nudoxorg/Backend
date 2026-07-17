# LIBRARIFICATION-PLAN — nudox Dual-Form Restructure

**Date:** 2026-07-16 · **Status:** Master plan (design freeze; supersedes per-topic drafts)
**Research corpus:** `.research/librarification/01–22` (each section cites its source doc; the research docs are the evidence trail, this document is the plan)
**Scope:** Everything — compiler library-ification, registry/server decomposition into INDEX + REGISTRY + ORCH, Postgres removal, incremental symbol-level pipeline, moniker/lineage identity, Terminus hot-tiering, embedded Tantivy/Qdrant, client library, full GUI refactor.

---

## Table of contents

```
Part 0 — Charter
  0.1  Vision and product shape
  0.2  North-star architecture
  0.3  Glossary (authoritative)
  0.4  Global Decision Log GD-1..GD-28 (frozen)
  0.5  Research-doc crosswalk

Part I — Target architecture
  §1   Crate topology & dependency layering          [22]
  §2   Wire protocol & plane contracts               [21]
  §3   Storage: CAS, blobs, compression, trees       [13]

Part II — Identity & IR
  §4   IR contract & occurrence model                [04]
  §5   Symbol identity foundations                   [05, 06]
  §6   Moniker & lineage (normative)                 [19]

Part III — Engines
  §7   Incremental spine: stages, traces, deltas     [08]
  §8   Commit gate & generations                     [17]
  §9   Graph over IR: the cold path                  [20]
  §10  Terminus hot tier & admission                 [07]
  §11  Text search: Tantivy multi-language           [10]
  §12  Vector search & local embeddings              [09]

Part IV — Planes & distribution
  §13  Compiler dual-form                            [01]
  §14  Desktop toolchains & packaging                [18]
  §15  Trust & project model                         [16]
  §16  Client library & sync engine                  [14]
  §17  INDEX: decomposition + SQLite service         [02, 11]
  §18  ORCH: Kubernetes orchestration                [12]

Part V — GUI
  §19  lindsey: full refactor & product views        [03, 15]

Part VI — Program
  §20  Unified migration phases
  §21  Risk register
  §22  Open questions & deferred work
```

---

# Part 0 — Charter

## 0.1 Vision and product shape

nudox becomes a **desktop-first code-intelligence product** whose GUI (`lindsey`) is the start and end of the developer experience, backed by a clean split of planes:

- The **compiler** exists in two forms from one codebase. As a **binary** (`compiler-daemon`) it is a stateless sealed server: it receives source, produces IR + blobs in totally contained form, and returns full artifacts — nothing else. As a **library** (`compiler-core` embedded in the client) it compiles **trusted code only**: the opened project, its path dependencies, its immediate workspace.
- The **INDEX** is the remote registry: SQLite metadata + S3 CAS + HTTP `/v1`. It serves untrusted-package intelligence and syncs generations down to clients.
- The **REGISTRY** is the local, in-process store inside the GUI: SQLite catalog + disk CAS + embedded Tantivy + embedded qdrant-edge + cold graph. After sync, the local store takes over; offline degrades gracefully to the Ready subset.
- **ORCH** is a thin orchestration server that uses Kubernetes primitives (Kueue, Jobs, HPA/Karpenter) for queueing and load-balancing instead of rolling a queue into the registry. **Postgres exits the architecture entirely.**
- The **client** library abstracts all of this away from the GUI through typestates and strong patterns: trust-branded compile, `Routed<L,R>` dual backends, a SyncEngine that pulls generation closures by hash-set difference, and capability tokens.
- **TerminusDB becomes a reality only for the most-used packages**, admitted by a leaky-bucket scorer. The vast majority of packages serve graph operations by loading IR from blobs and projecting the graph representation on demand.
- The whole pipeline becomes **heavily incremental**: only sealed git commits advance generations (never dirty working trees), and only symbols whose content actually changed re-embed, re-index, and re-publish. Cross-generation symbol identity — "when can two symbols across generations be called the same?" — is answered by the most ambitious defensible design: a three-layer architecture of version-stripped monikers, BLAKE3 part-hashes, and explicit confidence-tagged lineage edges from a frozen T0–T7 matching cascade (§6).

## 0.2 North-star architecture

```
 ┌─────────────────────────── DESKTOP (one process) ────────────────────────────┐
 │  lindsey (GPUI)                                                              │
 │    │ stores subscribe                                                        │
 │    ▼                                                                         │
 │  client ──── client-core (sans-IO: traits, typestates, routing, sync FSM)    │
 │    │  Routed<Local, Remote> per trait: Catalog · TextSearch · VectorSearch   │
 │    │                                    · GraphOps · Compile                 │
 │    ├─► REGISTRY (embedded): registry.sqlite + cas/ + tantivy/ + vectors/     │
 │    │      graph-cold (IR → GraphView) · qdrant-edge · fastembed              │
 │    ├─► EmbeddedForge = compiler-core + TrustedForgeContext   (trusted only)  │
 │    └─► SyncEngine: want/have manifest pull, hash-verified, atomic commit     │
 └───────────────┬──────────────────────────────────────────────────────────────┘
                 │ HTTPS /v1 (control) + presigned S3 GET (data)
 ┌───────────────▼──────────────── REMOTE ──────────────────────────────────────┐
 │  INDEX (StatefulSet n=1): sqlite(WAL,PVC)+Litestream→S3 · S3 cas/{blake3}    │
 │    HTTP /v1 · SearchPlanner · outbox→{tantivy, qdrant, terminus-hot} sinks   │
 │    PackageTierManager (leaky bucket) ── terminus-client → TerminusDB v12     │
 │    │ POST /v1/jobs                                                           │
 │    ▼                                                                         │
 │  ORCH: admit → dedup(input_hash) → warm HTTP pool | Kueue cold k8s Jobs      │
 │    └─► compiler-daemon pods (gVisor+userns+bwrap; sealed; CAS-ref I/O)       │
 └──────────────────────────────────────────────────────────────────────────────┘
```

Trust routing: the opened project (and everything inside its Jail) compiles locally through `EmbeddedForge`; every third-party dependency resolves to coordinates and is **delegated to the INDEX**, which serves results immediately while the SyncEngine materializes the generation closure into the local REGISTRY — after which local takes over transparently.

## 0.3 Glossary (authoritative)

| Term | Meaning |
|---|---|
| **INDEX** | Remote multi-tenant service plane: sqlite MetaStore + S3 CAS + HTTP `/v1` + derived search/graph stores |
| **REGISTRY** | Local desktop store: sqlite catalog + disk CAS + embedded tantivy/vectors + generation status; offline-capable |
| **ORCH** | Orchestration server + k8s scheduling for the untrusted compile fleet (not search, not blobs) |
| **client** | Library between GUI and INDEX/REGISTRY: typestates, Routed backends, SyncEngine |
| **trusted** | First-party sources (project roots + in-jail path deps); may compile in-process via TrustedForge |
| **untrusted** | Third-party/registry packages; compile only via sealed daemon/fleet; ingest jail required |
| **generation** | Immutable package snapshot identity (content-derived); the unit of sync atomicity |
| **GenerationStatus** | Local readiness: Pending / Pulling / Ready / Failed — *not* the generation id |
| **moniker** | Version-stripped stable symbol coordinate (SCIP-shaped string); the cross-generation join key |
| **SymbolId** | Instance + version-coupled UUID; intra-store join only — never cross-generation continuity |
| **JobKey** | Content-addressed compile job key: producer ‖ toolchain ‖ source ‖ dep_lock |
| **BlobManifest** | Dual-hash manifest: generation identity stamp ≠ CAS storage key |
| **DepSet** | The set of (package, generation) pairs a query may see; generation-pinned |
| **closure** | The full Ready hash set of a generation's blobs |
| **want/have** | Manifest-driven hash-set sync negotiation (Nix-binary-cache pattern) |
| **hot / cold tier** | TerminusDB-admitted packages / graph served by graph-cold over IR blobs |
| **Stage** | Incremental pure(ish) transform with a persistent constructive trace |
| **SymbolDelta** | added/removed/changed symbol sets between generations; the fan-out contract |
| **CommitGate** | Policy + machinery: the pipeline advances only on sealed VCS commits, never keystrokes |
| **ForgeContext / Cage** | Injected compile environment / OS isolation implementation |
| **SemanticGate** | Capability/quota token required for expensive semantic search |
| **plane** | INDEX / REGISTRY / ORCH / SHARED / GUI deployment concern |
| **lindsey** | The GUI binary crate (path `workspace/gui`) |

Identity stack — **never collapse these layers** (§4, §6):

```
NudoxPath (IR, package-local)
  → graph IRI (versionless)
  → LineageMoniker (versionless, product continuity)
  → GenerationSymbol (moniker @ generation)
  → SymbolId / EntryUri (versioned PackageId + instance salt — store join only)
  → ContentHash / part hashes (what the symbol IS)
```

## 0.4 Global Decision Log (frozen)

Every section conforms to these. Where a research doc disagreed, the arbitration below wins and the section notes it.

- **GD-1 Naming.** INDEX / REGISTRY / ORCH / client / lindsey per glossary. Target crates never reuse bare `registry` or `server`.
- **GD-2 Postgres exits entirely.** INDEX = SQLite via sqlx 0.8 (writer pool max 1 + reader pool N), sea-query `SqliteQueryBuilder`, k8s StatefulSet `replicas: 1` + RWO PVC, Litestream v0.5.x sidecar → S3 with restore-on-boot. REGISTRY = rusqlite (bundled) on a dedicated thread. The job queue leaves SQL entirely (→ GD-19). No SKIP LOCKED, no advisory locks; outbox + sink watermarks remain as sqlite tables. rqlite v10.x is the documented HA fallback only.
- **GD-3 Graph trait naming.** `PackageGraph` = full graph API (home: graph-cold); `GraphOps` = client-facing subset (client-core); `GraphStore` is retired after migration.
- **GD-4 Vector.** Two layers kept: `VectorStore` (low-level ANN CRUD) + `VectorSearch` (app-level + SemanticGate). Embedded = **qdrant-edge 0.7.2**; remote = qdrant-client 1.18. Same model + dimension + metric on both sides: `jinaai/jina-embeddings-v2-base-code`, 768-d, via fastembed 5.17.x + ort. LanceDB 0.31.0 is the feature-gated escape hatch. Local vectors ship in the desktop MVP wave; `Routed` degrades to remote until local is Ready.
- **GD-5 Catalog ⊆ MetaStore.** `Catalog` is the client-facing read surface; `MetaStore` is the spine CRUD + outbox + watermarks.
- **GD-6 client-core sans-IO split adopted.** Pure traits, routing policy, sync state machine, and typestates live in client-core; tokio/reqwest wiring lives in client.
- **GD-7 Symbol identity per RFC-19.** Dedicated `moniker` crate. Version-stripped `LineageMoniker` (SCIP-shaped) + generation-qualified `GenerationSymbol`; BLAKE3 part hashes `sig`/`body`/`doc`/`ref`/`embed_key` with `normalizer_version`; explicit `LineageEdge` records from the frozen T0–T7 cascade; **prefer false-split over false-merge**; soft edges never skip embeds or hard identity. `PackageStemId` (version-less package continuity) added to heart. Producers stamp monikers + hash materials; `generate` seals fingerprints. Lineage storage = SQLite + CAS on both planes; Terminus receives Auto edges only for hot packages.
- **GD-8 Graph projection home.** `GraphCorpus` + `project()` + the shared edge lowerer to `(SymbolId, RelationKind, SymbolId)` move to graph-cold; compiler-core depends on graph-cold for its emit stage. Cold path re-projects from IR blobs on cache miss (no persisted GraphCorpus postcard in MVP; lean ColdGraphBlob optional later). `DepLoadPolicy::StubsOnly` by default. Hot publish must emit the same lowerer edges (Relation parity) in the promotion program of work.
- **GD-9 Wire protocol per §2.** Control plane = HTTP/JSON `/v1` + `/v1/hello` negotiation; data plane = presigned object-store GETs against BLAKE3 CAS only (closure-scoped); compiler plane = single postcard **v2** in `compiler-wire` returning FULL artifacts (surface + occurrences + references + archive digests + snapshot); ORCH plane = thin admit/status/poison. `ErrorBody` projects `heart::Failure`. `registry::protocol` is deleted. Internals (sqlite, tantivy, qdrant, terminus, k8s) never appear on the public wire. The desktop never holds store credentials.
- **GD-10 BlobManifest** keeps the dual-hash discipline (generation stamp ≠ CAS key) and gains `occurrences_ref`.
- **GD-11 Tree-sitter trees are NEVER persisted** (INDEX or REGISTRY). Store zstd source + OccurrenceSet/ResolvedReference; reparse on demand. This *is* the efficient encoding of "store resolved trees": no stable Tree serialization API exists (upstream maintainers advise storing source), and docs.rs stores archives, not parse trees. Deliberate, evidence-backed deviation from the product brief's phrasing.
- **GD-12 Compression.** zstd everywhere: trained dictionaries for IR sections (dict_id recorded in the envelope), plain zstd for source. Seekable transfer packs optional/deferred; FastCDC deferred until large binary blobs appear.
- **GD-13 Terminus = hot tier only.** TerminusDB v12.0.6 via HTTP behind `terminus-client` (vendored `terminusdb_schema` types); crates.io `terminus-store` (0.21.5, 2 years stale) is banned from production. Admission = leaky-bucket scorer + count-min-sketch doorkeeper, fed by cold-path query stats from day one; demotion pins the cold graph first; the cold path is the always-correct fallback.
- **GD-14 Incremental spine = hand-rolled constructive traces** in SQLite + BLAKE3 digests — NOT salsa-as-orchestrator (salsa 0.28 allowed *inside* producers). `Stage` trait + `TraceStore` + `symbol_heads` + `SymbolDelta` per §7; symbol-level early cutoff via GD-7 part hashes. Derived indexes are disposable — always rebuildable from blobs + catalog.
- **GD-15 CommitGate per §8.** Durable generation id = `BLAKE3("nudox.gen.v1" ‖ project_id ‖ repo_id ‖ commit_oid [‖ submodule pins])`. Detection = `notify` watches on GIT_DIR (HEAD, refs/, packed-refs) + 5 s poll safety net, 300 ms debounce, longer quiet period during rebases. Dirty working trees NEVER trigger embeddings or durable index updates; explicit preview generations use a separate TTL-GC'd id space. `CompositePackageMapper` over-approximates dirty packages; lockfile-only commits refresh the untrusted DepSet without re-producing first-party IR. Multi-root = multiple RepoBindings.
- **GD-16 Trust model per §15.** `Jail` = realpath prefix set of user roots ∪ promotions; BFS of in-jail path dependencies → `TrustedSourceSet`; lockfile pins (post-patch) → `UntrustedCoordinateSet`; canonicalize-before-jail; refuse `node_modules` roots; ignore Cargo/NuGet config redirects by default; a VS-Code-style Workspace Trust gate precedes any toolchain execution. `Compile` exists only on `SourcePath<Trusted>` (typestate-enforced). Untrusted always compiles on the fleet.
- **GD-17 Compiler dual-form per §13.** Keep the DAEMON-PLAN vocabulary (`ForgeContext`, `generate_with`, `SealedInput`, `JobKey`, typestate seal). Complete the injection surface: `OracleSet` (kills buck_resource/current_exe), explicit `ToolchainSet` (kills `from_env` in the library), `TrustedForgeContext` for desktop. Seal the adaptive producers (Rust/Java/C#) for the untrusted fleet. Daemon returns full artifacts (GD-9). `render/` ships in the embedded library. Acquisition (gix) moves out of sealed compile paths into client/INDEX.
- **GD-18 Desktop toolchains per §14.** Hybrid packaging: Class-B oracles in-app or as language packs; system-discover Class-C SDKs for MVP; optional Nix channel; download-on-demand later. The JobKey toolchain component migrates from path strings to semantic blake3 fingerprints under `nudox-producer/3`. MVP local languages: TypeScript, Python, Rust (if rustup present), Go (if go present). Remote-only: Java, C#, Nix. snix is GPL-3.0 and must never link into the GUI binary.
- **GD-19 ORCH per §18.** Warm Deployment (direct HTTP, 503 backpressure) + Kueue v0.18.3 cold Jobs; kube-rs 4.0 + k8s-openapi 0.28; `podFailurePolicy` (GA) classifies poison, recorded into INDEX; gVisor RuntimeClass + `hostUsers: false` + in-pod bwrap/landlock layered; presigned S3 multipart/zstd artifact flow; HPA/KEDA 2.20 warm scaling; Karpenter v1 NodePools (spot for cold). NO Argo/Tekton, NO CRDs-as-queue, NATS optional-off.
- **GD-20 Client per §16.** Typestates for monotonic concerns only (Connect, Project resolve, trust plane); per-generation sync status = runtime state + optional `SyncedWitness` proofs (no `Store<Syncing>` typestate). Every query routes over a generation-pinned DepSet: local iff all members Ready, else remote while the SyncEngine pulls in the background; offline = Ready subset only. Generations become visible atomically after full hash-verified commit. Sync = manifest want/have hash-set difference — NOT CRDT/row-replication (ElectricSQL/PowerSync/cr-sqlite rejected for this domain).
- **GD-21 Text search per §11.** One shared multi-language schema + tokenizer registration + index-format major across INDEX and REGISTRY. `LanguageAnalyzer` trait for path splitting / query rewrite / boosts / signature terms over the shared subword core. Target tantivy 0.26.1 (minimum 0.24.2) via one-shot controlled reindex; a schema-version marker file forces reindex on mismatch. Symbol ranking = multi-field BM25 + exact-match bonus + kind prior; package download-bubble fusion stays package-side in index-service.
- **GD-22 GUI per §19.** Stores-over-client: `ProjectStore`, `SymbolStore`, `SearchStore`, `JobStore`, `RegistryStore`, `NavHistory` entities subscribe to client; Workspace demoted to a layout shell; gpui-component Dock/List/Table/Tree/Modal adopted. P0 = project manager + trust visualization, cmd-K omni-search, symbol page v1 (versioned RenderModel, DocC-style), jobs/logs, settings, palette, nav history. Flagship P1 wedges: **symbol lineage timeline** + **type-directed multi-language search**. Fixtures behind `cfg(debug_assertions)`.
- **GD-23 Crate purity.** `ir` must NOT depend on heart (producers stamp heart identities in compiler-core). heart gains `Generation`, `SymbolDelta`, `PackageStemId`; `BackendKind` evolves off its Postgres variant. Shared libs (heart, ir, moniker, compiler-wire, blob) stay free of plane-specific I/O.
- **GD-24 INDEX job metadata.** INDEX sqlite keeps job/poison metadata rows for observability; claim/lease semantics live ONLY in ORCH/Kueue. Traces are sqlite-only — no new Postgres anywhere, including k8s trace stores.
- **GD-25 Cold-first graph rollout.** The cold PackageGraph ships before Terminus gating (hot is an optimization). Cold query stats feed the GD-13 scorer from day one. Demotion pins cold; a hot-probe failure fails over to cold.
- **GD-26 Deletions.** Root legacy `compiler/` frozen then deleted; `registry::protocol`; the SKIP LOCKED queue design; PgSessionStore-as-only-sessions; Qdrant-server-as-local-path; Terminus-for-all; CstSet demoted (OccurrenceSet is the reference SoT); the buck_resource embed path; GUI release fixtures; `BackendClient` as the sole data path.
- **GD-27 Store layouts.** REGISTRY = `registry.sqlite` + sharded `cas/` + `tantivy/` + `vectors/` + pin/LRU GC. INDEX = S3 `cas/{blake3}` + sqlite catalog. Sync = manifest-driven hash-set difference. Both planes store source (zstd) + IR + occurrences per GD-11/12.
- **GD-28 Committed-only, changed-only.** Embeddings, text indexing, graph publish, and lineage computation run ONLY on sealed generations (GD-15) and ONLY for symbols whose relevant part-hash changed (GD-7/GD-14). This is the product's central performance invariant.

## 0.5 Research-doc crosswalk

| § | Source docs | § | Source docs |
|---|---|---|---|
| §1 | 22-crate-topology | §11 | 10-tantivy |
| §2 | 21-wire-protocol | §12 | 09-vector |
| §3 | 13-storage | §13 | 01-compiler-audit |
| §4 | 04-ir-audit | §14 | 18-desktop-toolchains |
| §5 | 05-academic, 06-industrial | §15 | 16-trust-project-model |
| §6 | 19-symbol-moniker-rfc | §16 | 14-client-sync |
| §7 | 08-incremental | §17 | 02-registry-server-audit, 11-sqlite-index |
| §8 | 17-commit-gate | §18 | 12-orchestration |
| §9 | 20-graph-over-ir | §19 | 03-gui-audit, 15-gui-references |
| §10 | 07-terminus-tiering | §20–22 | synthesis |

---

# Part I — Target architecture

## §1 Crate topology & dependency layering

**Source:** `.research/librarification/22-crate-topology.md` · **Plane:** SHARED

### 1.1 Target workspace

Today's five live crates (`heart`, `ir`, `compiler`+`sandbox`, `registry`, `server`, plus `lindsey`) split into a layered workspace:

```
workspace/
├── heart/                # lib — vocabulary, CAS traits, typestate, progress
├── ir/                   # lib — typed IR + Occurrence model (heart-free, GD-23)
├── moniker/              # lib — LineageMoniker, part hashes, LineageEdge (§6)
│
├── compiler-wire/        # lib — postcard CompileRequest/Response v2 (§2.3)
├── compiler-core/        # lib — pure pipeline; no ambient policy, no daemon
├── sandbox/              # lib — Cage, workers, toolchains (path may stay nested)
├── compiler-daemon/      # bin — untrusted fleet HTTP + ForgeRuntime
├── producer-worker/      # bin — sealed worker helper (exists)
│
├── graph-cold/           # lib — PackageGraph over IR blobs; GraphCorpus + project()
├── text-search/          # lib — Tantivy schema, LanguageAnalyzer, TextIndex
├── vector-local/         # lib — VectorStore + qdrant-edge + embed worker
├── vector-remote/        # lib — VectorStore over qdrant-client
├── terminus-client/      # lib — hot-tier HTTP Terminus + typed documents
│
├── meta-store/           # lib — MetaStore trait + sqlite (sqlx & rusqlite) impls
├── blob/                 # lib — BlobManifest, section layout, zstd envelopes
├── ingest/               # lib — archive jail + trusted directory walker
│
├── registry-local/       # lib — desktop REGISTRY composition
├── index-service/        # lib+bin — remote INDEX
├── orch/                 # lib+bin — k8s orchestration server
│
├── client-core/          # lib — sans-IO traits, routing, sync FSM, typestates
├── client/               # lib — tokio wiring: SyncEngine, HTTP, EmbeddedForge
└── gui/                  # bin — lindsey (crate name stays)
```

Root-level legacy `compiler/` is frozen immediately and deleted at the end of Wave 1 (GD-26).

### 1.2 Dependency DAG and layering rules

```
                    heart
        ┌────────────┼──────────────┐
        ▼            ▼              ▼
       ir         moniker      compiler-wire        blob
        └─────┬──────┘                │
              ▼                       │
   graph-cold ◄── compiler-core ◄── sandbox
        │              │
        │     ┌────────┴────────┐
        │     ▼                 ▼
        │  compiler-daemon  producer-worker
        │
  text-search · vector-local · vector-remote · terminus-client
        │
        ▼
    meta-store  ◄── ingest
        │
   ┌────┼─────────────┐
   ▼    ▼             ▼
registry-local  index-service  orch
   └────┬──────────┬──┘
        ▼          ▼
    client-core → client → lindsey
```

**Forbidden edges** (enforced in Cargo/Buck review):

| Forbidden | Why |
|---|---|
| lindsey → index-service / orch / sqlx / qdrant-client | GUI talks only through client |
| compiler-core → sqlx / postgres / axum / qdrant / tantivy / terminus | Compile stays pure; fan-out is Stage consumers elsewhere |
| heart → ir / compiler-* / client · ir → heart / network / SQL | Foundation is leaves-up (GD-23) |
| graph-cold → terminus-client | Cold must never need hot |
| registry-local → kube / orch · client → orch internals | Desktop ≠ fleet; compile via HTTP API only |
| sandbox → registry / server · text-search → axum · vector-local → index-service | Libraries stay libraries |
| anything → root legacy `compiler/` | Dead tree |

**Feature-gated edges:** client → compiler-core (`embed`/`trusted-compile`); compiler-core → heavy language deps (`lang-rust`, `lang-nix`, …); vector-local → lancedb (`lance`); index-service → terminus-client (`graph-hot`); heart → telemetry (server feature).

### 1.3 Plane × crate matrix

| Crate | SHARED | REGISTRY | INDEX | ORCH | GUI |
|---|:-:|:-:|:-:|:-:|:-:|
| heart / ir / moniker / blob | ● | ● | ● | ●(heart) | ○ |
| compiler-wire | ● | | ● | ● | |
| compiler-core + sandbox | ● | ● embed | | image | |
| compiler-daemon / producer-worker | | | | ● | |
| graph-cold / text-search | ● | ● | ● | | ○ |
| vector-local | | ● | | | ○ |
| vector-remote / terminus-client | | | ● | | ○ via client |
| meta-store / ingest | ● | ● | ● | ○ | |
| registry-local | | ● | | | ○ |
| index-service / orch | | | ● | ● | |
| client-core / client | ● | ● | ● | | ● |
| lindsey | | | | | ● |

### 1.4 Master trait catalog (canonical homes)

| Trait | Home | Impls |
|---|---|---|
| `Connect` (Cold→Live), `Cas`, `EvictableCas`, `Progressive` | heart | existing |
| `Catalog` (⊆ MetaStore, GD-5) | client-core | SqliteCatalog, HttpCatalog |
| `MetaStore`, `QueueBackend` | meta-store | SqlxSqlite, Rusqlite; K8sJobs / SqliteSingleWriter (PgSkipLocked legacy-only) |
| `TextSearch` / `LanguageAnalyzer` | client-core (trait) / text-search (impls) | LocalTantivy, HttpSearch; per-language analyzers |
| `VectorStore` / `VectorSearch` | vector-local / client-core | QdrantEdgeLocal, QdrantRemote, LanceLocal / LocalVectors, HttpSemantic |
| `PackageGraph` / `GraphOps` (GD-3) | graph-cold / client-core | IrBlobPackageGraph, TerminusPackageGraph, TieredPackageGraph |
| `Compile` | client-core | EmbeddedForge (Trusted only), RemoteCompile |
| `ForgeContext`, `Producer`, `DocumentSink`, render `Backend` | compiler-core | existing names kept (GD-17) |
| `Cage` | sandbox | DevPassthrough, Bwrap, … |
| `Stage` | heart (small `stage` module) | EmbedStage, TantivyStage, GraphStage, RenderStage, LineageStage |
| `CommitGate` | client / registry-local | §8 |
| `SemanticGate` | vector-local / client-core | quota capability |
| `Routed<L,R>` | client-core | dual-home router |

Retired names: `GraphStore` (→ PackageGraph), `SymbolClient`/`SearchClient` (transitional GUI shims only), `BackendClient` (→ client).

New shared structures in heart: `Generation`, `SymbolDelta`, `PackageStemId`; `BackendKind` loses its Postgres assumption (GD-23).

### 1.5 Current → target rename map

| Today | Target |
|---|---|
| `workspace/compiler` (lib) | compiler-core (daemon module removed from lib) |
| `workspace/compiler/protocol.rs` + `workspace/registry/protocol.rs` | compiler-wire (single source, GD-9) |
| `workspace/compiler` daemon bin | compiler-daemon |
| `workspace/compiler/sandbox` | sandbox (path may stay nested; logical name flat) |
| `workspace/registry` (monolith) | meta-store + registry-local + text-search + vector-* + graph-cold + ingest + blob + parts of index-service |
| `workspace/server` | index-service (+ DTOs → compiler-wire/client) |
| `workspace/gui` | lindsey over client (unchanged path) |
| Postgres `jobs` / SKIP LOCKED | orch + INDEX metadata rows (GD-24) |

### 1.6 Implementation steps

- **S1.1** Extract `compiler-wire` from the twin protocol modules; dual re-export from old paths for one release. *Acceptance:* both daemons/clients compile against the single crate; byte-identical postcard for v1 shapes.
- **S1.2** Extract `meta-store` (schema + queries behind `MetaStore`) from `registry/schema` with the Postgres impl marked legacy. *Acceptance:* server boots against trait; no behavior change.
- **S1.3** Extract `text-search` from `registry/runtime/text` + package tantivy. **S1.4** Extract `graph-cold` skeleton and move `compiler/graph/{model,from_ir,link}` into it (GD-8). **S1.5** Extract `blob` + `ingest`.
- **S1.6** Create `client-core`/`client`/`registry-local`/`vector-local`/`vector-remote`/`moniker`/`orch` empty-but-wired crates with layering CI checks (a `cargo deny`-style edge lint or a workspace-graph test).
- **S1.7** Freeze root `compiler/`; delete after Wave 1. *Acceptance:* no workspace member references it.

Prereqs: none external; S1.4 requires S1.1 landed for daemon build. Everything else in this plan assumes §1's crates exist as homes.

---

## §2 Wire protocol & plane contracts

**Source:** `.research/librarification/21-wire-protocol.md` · **Planes:** all

### 2.1 Protocol layering (normative)

| Plane | Transport | Contents |
|---|---|---|
| **Control** | HTTP/JSON, base path `/v1`, `/v1/hello` negotiation | resolve, search, expand, sync planning, jobs, admin |
| **Data** | Presigned object-store GET/PUT only | BLAKE3 CAS blobs; closure-scoped; client verifies hashes |
| **Compiler** | postcard, `COMPILER_PROTOCOL_VERSION = 2` | CompileEnvelope; full artifacts back |
| **ORCH** | HTTP/JSON `/v1` (`service: orch` in hello) | admit / status / poison; operators + INDEX only |

Internals never on the wire: SQLite schemas, Tantivy queries, Qdrant filters, WOQL, k8s Job specs. Desktop clients never receive store credentials — presigned URLs only.

### 2.2 INDEX control-plane route table (v1)

| Group | Routes |
|---|---|
| Discovery/ops | `GET /v1/hello` · `GET /healthz` · `GET /readyz` · `GET /metrics` |
| Catalog | `POST /v1/resolve` (coordinates → ids + generations + sync pointers) · `GET /v1/packages/{id}` · `GET /v1/packages/{id}/generations/{latest\|hash}` · `POST /v1/packages` · `POST /v1/packages/{id}/reindex` |
| Search/graph | `POST /v1/search` · `POST /v1/search/semantic` (ReadCap + SemanticGate) · `POST /v1/packages/search` · `POST /v1/expand` · `GET /v1/symbols/{symbol_id}` · sessions GET/PUT |
| Sync | `POST /v1/sync/plan` (want/have → missing hashes + presigned URLs) · `POST /v1/cas/has` · `POST /v1/cas/presign` |
| Jobs | `POST /v1/jobs` · `GET /v1/jobs/{id}` · `POST /v1/jobs/{id}/cancel` |
| Admin | verify / rebuild / poison / unpoison under `/v1/admin/packages/{id}/…` (AdminCap) |

Body ceilings: read control 2 MiB; write control 256 KiB; sync plan 1 MiB (server caps have[] ≈ 50k hashes); compiler postcard 256 MiB (fleet prefers CAS refs). Source archives are never accepted on public INDEX JSON APIs.

### 2.3 Compiler protocol v2 (postcard, `compiler-wire`)

```rust
pub const COMPILER_PROTOCOL_VERSION: u32 = 2;

pub enum CompileRequest {
    /// Dev/local: today's shape.
    Inline { coordinates: Coordinates, toolchain: Toolchain, files: Vec<FileBytes> },
    /// Fleet: input already in object store; pod never gets bulk via ORCH.
    Cas {
        coordinates: Coordinates, toolchain: Toolchain,
        input_hash: [u8; 32], input_get_url: String,
        output: OutputGrants,               // SectionPut[] + completion callback/token
        deadline_unix_ms: Option<u64>,
    },
}

pub enum SectionRole { Ir, References, Occurrences, ArchiveIndex, Manifest }

pub enum CompileResponse {
    Ok {
        protocol_version: u32,
        surface: Vec<u8>,               // postcard ir::entry::Index
        references: Vec<WireFile>,      // v1 layout byte-identical
        occurrences: Vec<u8>,           // NEW: postcard OccurrenceSet
        archive: WireSourceArchive,     // NEW: per-file path/hash/size digests
        identifiers: Vec<String>,
        snapshot: [u8; 32],             // NEW: generation stamp input
        uploaded: Vec<UploadedSection>, // Cas mode: role+hash+size after PUT
    },
    Err { protocol_version: u32, kind: String, message: String,
          failure_kind: FailureKindWire, phase: PhaseWire, retryable: bool },
}
```

Postcard is not protobuf: any layout change is a full version bump; `WireFile`/`WireReference`/`WireTarget` and the ReferenceKind u8 table stay byte-identical to live v1. Error kinds map onto `heart::FailureKind` (`sandbox_denied → Unsafe`, `timeout → Timeout` retryable, `oom → Transient once then Internal`, `input_fetch → Transient`, …).

### 2.4 Error body (control plane)

One `ErrorBody { code, message, failure_kind, phase, retryable, details? }` shape everywhere, projected from `heart::Failure` — `server/error.rs` and `heart/error/failure.rs` are the source of truth; ad-hoc handler errors are replaced.

### 2.5 Compatibility

`/v1/hello` returns `{ service, protocol: {major, minor}, features[] }`. Shipped desktops negotiate: unknown fields ignored on decode (serde), unknown feature = degrade, major mismatch = upgrade prompt. Live flat routes get `/v1` aliases for one release, then die. Deprecations announced in release notes with explicit windows.

### 2.6 Implementation steps

- **S2.1** Create `compiler-wire` with v1-byte-compatible types (= S1.1). **S2.2** `/v1/hello` + ErrorBody on index-service; alias old routes. **S2.3** `POST /v1/sync/plan` + `cas/has`/`cas/presign` backed by object_store presign. **S2.4** Compiler v2 `Ok` full artifacts (occurrences/archive/snapshot) behind version negotiation; BlobBuilder consumes occurrences (GD-10). **S2.5** `Cas` transport mode for fleet. **S2.6** ORCH `/v1` (submit/status/poison) + INDEX `/v1/jobs` forwarding. **S2.7** Drop route aliases; freeze OpenAPI surface map.
- *Acceptance at each step:* wire golden tests (postcard byte snapshots for compiler plane; JSON schema snapshots for control plane); a lagging-client simulation test for hello negotiation.

---

## §3 Storage: CAS, blobs, compression, source & trees

**Source:** `.research/librarification/13-storage.md` · **Planes:** SHARED/INDEX/REGISTRY

### 3.1 Verdict

Content-addressed per-file storage is the single system of record for source, IR, and extracted references — already embodied in `BlobManifest`, `Store` (`cas/` + `ptr/`), `SourceArchive`, and `heart::cache::DiskCas`. External prior art confirms the shape: docs.rs (catalog vs blob; ZIP+range only for non-dedupable HTML), Nix narinfo (signed manifests over CAS for sync), Dash/Zeal (sqlite + payload for offline UX), Sourcegraph SCIP (semantic indexes are not CSTs), and tree-sitter maintainers (store compressed source, there is **no stable Tree serialization API** as of 2026).

### 3.2 Decisions

| Item | Decision |
|---|---|
| Tree-sitter Trees | **Never persisted** (GD-11). Persist `OccurrenceSet`/`ResolvedReference`; reparse on demand (cheap at package/file grain) |
| IR sections | zstd with **trained dictionaries**; `dict_id` recorded in the envelope codec (raw \| zstd \| zstd+dict) |
| Source | plain zstd, per-file digests (`SourceArchive`/`FileDigest` exist) |
| Transfer packs | optional seekable-zstd bulk packs for cold sync — never the only replica; deferred (GD-12) |
| Chunking | FastCDC deferred until large binary blobs appear |
| Manifest | `BlobManifest` dual-hash (generation stamp ≠ CAS key) + NEW `occurrences_ref` (GD-10) |
| Footprint | ~0.6–0.9 MB compressed per 50k-LOC generation without trees; multi-version retention ≈ 5× smaller than per-version archives |

### 3.3 Layouts

**REGISTRY (desktop):** `<data_dir>/registry.sqlite` + sharded `cas/xx/{blake3}` + `ptr/{package-uuid}` + `tantivy/` + `vectors/` + pin/LRU GC (pin levels 0–4, §8.4).
**INDEX (remote):** S3 `cas/{blake3}` + `ptr/` + sqlite catalog; first-write-wins section puts; BLAKE3 integrity verify on full reads. `heart::cache::{DiskCas, Tiered}` becomes the actual path used by the store (today it exists but `registry::store::Store` bypasses it).

**Sync** is manifest-driven hash-set difference (want/have → presigned GETs, §2.2), identical in shape to Nix binary-cache substitution. Blob GC: reference-counted from live generations + pins; INDEX-side GC is a Wave-4 deliverable (explicitly unsolved today).

### 3.4 Implementation steps

- **S3.1** `blob` crate: envelope codec (raw|zstd|zstd+dict) + dictionary training job over IR corpus. **S3.2** Add `occurrences_ref` to BlobManifest + emit path (needs S2.4). **S3.3** Route `registry::store::Store` reads through `Tiered` CAS. **S3.4** REGISTRY disk layout + pin/GC tables in registry-local (with §8's pin levels). **S3.5** INDEX GC: mark-and-sweep from catalog + pins, dry-run mode first. *Acceptance:* round-trip property tests per envelope; GC never deletes a hash reachable from any sealed generation (invariant test).
