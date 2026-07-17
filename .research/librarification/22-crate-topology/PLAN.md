# PLAN: 22 — Crate Topology + Trait / Structure Inventory

**Kind:** synthesis inventory PLAN (not a master migration plan / no PR timeline)  
**Date:** 2026-07-16  
**Inputs:** research plans 01–15 (audits + designs); 16–21 empty at write time  
**Audience:** assembler / architecture plan agents  
**Companion:** [../22-crate-topology.md](../22-crate-topology.md) (same inventory body)

This PLAN is an **inventory + proposed workspace crate graph**. It does **not** prescribe PR order, timelines, or a full migration narrative. Use it to know *what crates exist*, *which traits live where*, *which libraries each crate consumes*, and *where plans disagree*.

---

## 0. How to read this document

| Section | Use |
|---|---|
| §1 Crate graph | Target package names, paths, lib vs bin |
| §2 Per-crate inventory | Responsibility, public types/traits, inter-crate deps |
| §3 Master trait catalog | Canonical trait names + home crate |
| §4 Libraries migrate-TO | External crates/APIs × consumer crate × pin |
| §5 Deletes / demotions | What leaves the product surface |
| §6 Naming glossary | INDEX / REGISTRY / moniker / generation / … |
| §7 Contradictions | Explicit collisions across research plans |
| §8 Layering rules | Hard dependency edges that must not exist |

**Current live workspace** (today, for orientation only):

| Path | Crate name | Role today |
|---|---|---|
| `workspace/heart` | `heart` | Shared vocabulary |
| `workspace/ir` | `ir` | Typed IR |
| `workspace/compiler` | `compiler` | Producers + daemon + graph + render + treesitter |
| `workspace/compiler/sandbox` | `sandbox` | Cage / worker / toolchains |
| `workspace/registry` | `registry` | PG spine, CAS, ingest, text/vector/graph runtime |
| `workspace/server` | `server` | Axum INDEX-ish HTTP + pollers |
| `workspace/gui` | `lindsey` | GPUI desktop |

**Legacy outside workspace:** root `compiler/` — demote/delete; do not grow.

---

## 1. Proposed target crate / package graph

### 1.1 Directory tree (target)

```
workspace/
├── heart/                    # lib  — vocabulary, CAS traits, typestate, progress
├── ir/                       # lib  — typed IR (+ occurrence types if kept here)
├── moniker/                  # lib  — SymbolMoniker / generation-stable coords (NEW; may start as heart module)
│
├── compiler-wire/            # lib  — postcard CompileRequest/Response (NEW extract)
├── compiler-core/            # lib  — pure pipeline; no ambient policy / daemon
├── sandbox/                  # lib  — cage, worker, toolchains, budgets (path: compiler/sandbox today)
├── compiler-daemon/          # bin  — untrusted fleet HTTP + ForgeRuntime assemble
├── producer-worker/          # bin  — sealed worker helper (exists)
│
├── graph-cold/               # lib  — GraphStore / PackageGraph over IR blobs (NEW; aka graph-over-ir)
├── text-search/              # lib  — Tantivy schema, LanguageAnalyzer, TextIndex (NEW extract)
├── vector-local/             # lib  — VectorStore + qdrant-edge + embed worker (NEW)
├── vector-remote/            # lib  — VectorStore over qdrant-client (NEW or feature of vector-local)
│
├── meta-store/               # lib  — MetaStore trait + sqlite/sqlx impls (NEW extract from registry/schema)
├── registry-local/           # lib  — desktop REGISTRY: catalog, disk CAS, local derived stores
├── index-service/            # lib+bin — remote INDEX: sqlite+S3, HTTP routes, outbox (evolves from server+registry)
├── orch/                     # lib+bin — k8s orchestration server (NEW; queue leaves Postgres)
│
├── client-core/              # lib  — sans-IO traits, routing, sync state machine (optional purity split)
├── client/                   # lib  — tokio: Routed backends, SyncEngine, HTTP INDEX, local REGISTRY wire-up
│
├── lindsey/                  # bin  — GPUI GUI (crate name stays lindsey; path may stay gui/)
│
└── (optional thin crates)
    ├── terminus-client/      # lib  — HTTP Terminus wrapper + GraphAdmission helpers
    ├── ingest/               # lib  — archive jail + directory walker (extract if shared INDEX/REGISTRY)
    └── blob/                 # lib  — BlobManifest / section layout (or keep under heart/registry-shared)
```

**Path note:** Prefer `workspace/<crate>/` flat layout for new crates. Existing nested `workspace/compiler/sandbox` may remain at that path if Buck/Cargo membership is painful; the **logical** crate name is still `sandbox`.

### 1.2 Lib vs bin matrix

| Crate | Kind | Primary consumers |
|---|---|---|
| `heart` | lib | almost everything |
| `ir` | lib | compiler-core, graph-cold, text-search, client, index-service |
| `moniker` | lib | heart/ir producers, graph-cold, text-search, client |
| `compiler-wire` | lib | compiler-daemon, orch, index-service, client |
| `compiler-core` | lib | compiler-daemon, client (trusted embed), producer-worker |
| `sandbox` | lib | compiler-core (traits), compiler-daemon, producer-worker |
| `compiler-daemon` | **bin** | k8s warm/cold pods |
| `producer-worker` | **bin** | untrusted sealed steps |
| `graph-cold` | lib | index-service, registry-local, client |
| `text-search` | lib | index-service, registry-local, client |
| `vector-local` | lib | registry-local, client (desktop) |
| `vector-remote` | lib | index-service, client (remote semantic) |
| `meta-store` | lib | index-service, registry-local, orch (job metadata) |
| `registry-local` | lib | client, lindsey |
| `index-service` | lib + **bin** | remote deployment |
| `orch` | lib + **bin** | k8s control plane |
| `client-core` | lib | client |
| `client` | lib | lindsey; CLI tools later |
| `lindsey` | **bin** | end users |
| `terminus-client` | lib | index-service (hot graph only) |
| `ingest` | lib | index-service, registry-local |
| `blob` | lib | heart consumers; optional fold into heart |

### 1.3 Dependency DAG (layer rules sketch)

```
                    ┌──────────┐
                    │  heart   │
                    └────┬─────┘
           ┌─────────────┼─────────────┐
           ▼             ▼             ▼
          ir          moniker     compiler-wire
           │             │             │
           └──────┬──────┘             │
                  ▼                    │
            compiler-core ◄── sandbox  │
                  │                    │
         ┌────────┴────────┐           │
         ▼                 ▼           │
  compiler-daemon   producer-worker    │
                                       │
  graph-cold / text-search / vector-*  │
         │              │              │
         └──────┬───────┘              │
                ▼                      │
           meta-store ◄────────────────┘
                │
     ┌──────────┼──────────┐
     ▼          ▼          ▼
registry-local  index-service  orch
     │          │          │
     └────┬─────┴────┬─────┘
          ▼          ▼
      client-core → client → lindsey
```

**Hard rule:** arrows only point **down** this DAG (or sideways within a layer). See §8.

### 1.4 Plane mapping (INDEX / REGISTRY / ORCH / SHARED)

| Plane | Crates that primarily compose it |
|---|---|
| **SHARED foundation** | heart, ir, moniker, compiler-wire, blob, compiler-core (as library), sandbox traits |
| **REGISTRY (local desktop)** | registry-local, text-search, vector-local, graph-cold, meta-store(sqlite/rusqlite), client, lindsey |
| **INDEX (remote service)** | index-service, meta-store(sqlx sqlite), text-search, vector-remote, graph-cold + terminus-client, ingest, object_store S3 |
| **ORCH (fleet)** | orch, compiler-daemon, producer-worker, compiler-wire; job SoT rows may live in INDEX sqlite |
| **CLIENT API** | client-core, client (Routed local/remote) |

---

## 2. Per-crate inventory

Each subsection: **responsibility**, **key public types/traits**, **depends on**, **research anchors**.

### 2.1 `heart` — `workspace/heart`

**Responsibility:** Shared product vocabulary. Identity, content addressing, CAS trait family, Connect typestate, failure taxonomy, progress, federation, score/page, derived-store tags. **No** HTTP, **no** SQL, **no** Tantivy, **no** language producers.

| Name | Kind | Purpose (1 line) |
|---|---|---|
| `ContentHash` | type | 32-byte BLAKE3 content identity |
| `JobKey` | type | Domain-separated producer/toolchain/source/dep_lock key |
| `Id<T>`, `PackageId`, `SymbolId`, `EntryUri` | type | Branded UUID identity stack |
| `Coordinates` | type | origin/name/version package key material |
| `Connect`, `Cold`, `Live` | trait/types | Typestate open/connect pattern |
| `Cas`, `EvictableCas` | trait | First-write-wins content store |
| `MemoryCas`, `DiskCas`, `Tiered`, `NoL3` | type | CAS implementations / layering |
| `SingleFlight`, `StampedeCache` | type | Coalesce concurrent loads |
| `DerivedStore` | enum | Vector \| Graph \| Text fan-out tags |
| `ResolutionState`, `Failure`, `Phase` | type | Package lifecycle / error taxonomy |
| `Cursor`, `Score`, `Scored`, `Page` | type | Search pagination + freshness brands |
| `Progressive`, `JobProgress` | trait/type | Phased job progress for GUI |
| `Language`, `Toolchain` | type | Ecosystem tagging |
| `Federation` | type | Multi-source query precedence |
| `Generation` | type (**add**) | Sealed package-generation identity (from 08) |
| `SymbolDelta` | type (**add**) | added/removed/changed symbol set between generations |
| `BackendKind` | enum | Health probe kinds (evolve: drop Postgres-only assumption) |

**Deps:** blake3, uuid, moka (cache), serde, thiserror — keep minimal.  
**Does not depend on:** ir, compiler-*, registry-*, server, sqlx, tantivy, qdrant, axum.  
**Anchors:** 01 §10.3, 02 §8, 08 R1, 14 §2.

---

### 2.2 `ir` — `workspace/ir`

**Responsibility:** Language-agnostic typed IR: `Entry`/`Symbol` shell, kinds (function/record/module/trait), types/generics, syntax body yoke, **Occurrence** model. Pure data + serde; no I/O.

| Name | Kind | Purpose |
|---|---|---|
| `Index` / pipeline root | type | Package IR container + root_ids |
| `Entry`, `Symbol<T>` | type | Symbol shell with path + payload |
| `NudoxPath` | type | Local/External path identity in IR |
| `kind::*` | types | Function, Record, Module, TraitDef, … |
| `syntax::Occurrence`, `OccurrenceSet` | type | Reference corpus (persisted) |
| `FunctionBody` / yoke | type | In-memory body with tree cart (not CAS-persisted as C Tree) |
| `EntryContentHash` | type (**add**) | Payload fingerprint for symbol-level fan-out |

**Deps:** heart (optional; prefer keep heart-free if possible — **collision:** today IR may not depend on heart PackageId; see §7), arborium-tree-sitter, yoke, serde, strum.  
**Anchors:** 04 entire, 13 tree policy, 06 moniker layers (consumes IR for hashes).

---

### 2.3 `moniker` — NEW (`workspace/moniker` or `heart::moniker`)

**Responsibility:** Cross-generation continuity keys. Version-stripped moniker strings; generation-qualified symbols; **not** salsa-style u32s.

| Name | Kind | Purpose |
|---|---|---|
| `SymbolMoniker` | type | scheme + ecosystem + package-name + descriptors (versionless) |
| `GenerationSymbol` | type | moniker @ generation-id |
| `LineageEdge` | type | supersedes / rename / body_changed edges |
| `sig_hash`, `body_hash` | types | API vs body content digests for match tiers |
| `MonikerScheme` | enum | rust/npm/pypi/… manager schemes |

**Deps:** heart, (optionally) ir for descriptor extraction helpers.  
**Anchors:** 06 industrial §Layer A–C; 04 identity matrix; 05 academic (when filled).  
**Note:** 19-symbol-moniker-rfc is empty — this crate is a **placeholder home** until RFC lands.

---

### 2.4 `compiler-wire` — NEW extract

**Responsibility:** Single source of truth for daemon/INDEX/ORCH compile postcard types. Kills `compiler::protocol` vs `registry::protocol` twin drift.

| Name | Kind | Purpose |
|---|---|---|
| `CompileRequest` | type | coordinates, toolchain, FileBytes |
| `CompileResponse` | type | Ok{surface, references, identifiers} / Err |
| `FileBytes`, `WireFile`, `WireReference` | type | Wire file/ref payloads |
| `WireOccurrenceSet` (**add**) | type | Optional full occurrences on wire v2 |

**Deps:** heart, serde, postcard. **No** sandbox, **no** producers.  
**Anchors:** 01 §3, §8.1; 02 protocol mapping.

---

### 2.5 `compiler-core` — evolve from `workspace/compiler` lib

**Responsibility:** Pure compile pipeline: producers, surface, CST, occurrences, archive, blob_info, graph projection, render, treesitter LanguageSpec. Injected `ForgeContext` only — **no** env policy reads, **no** daemon module export.

| Name | Kind | Purpose |
|---|---|---|
| `ForgeContext` | trait | Injected cas/cage/toolchains/oracles/observer |
| `LocalForgeContext` | type | In-process / test forge |
| `TrustedForgeContext` | type | Desktop trusted assemble (require_worker=false) |
| `Producer`, `ProducerOutput`, `ExecPlan` | trait/types | Language producer lifecycle |
| `generate_with`, `PackageInput`, `GeneratedPackage` | fn/types | Full pipeline entry |
| `DocumentSink`, `emit_graph` | trait/fn | Linked-data fan-out |
| `GraphCorpus`, `project` | type/fn | IR → graph intermediate (cold path substrate) |
| `render_entry` / `Backend` | fn/trait | IR → surface syntax for GUI |
| `LanguageSpec` | type | Occurrence extractors per language |
| `OracleSet` | type | Go/Java/C# oracle paths (injected, not Buck env) |
| `SealedInput`, `ThreatTier` | type | Seal / isolation tier |
| `CstSet`, `SourceArchive`, `BlobInfo` | type | Stage outputs |

**Deps:** heart, ir, sandbox (Cage trait), compiler-wire (optional), gix only if acquisition stays (prefer **move acquisition out** — see §5), tree-sitter/arborium, ra_ap_*, language feature gates.  
**Must not depend on:** sqlx, postgres, axum, qdrant, terminus, registry schema.  
**Anchors:** 01 §8–11, 04 graph/treesitter, 08 stages (producer as Stage).

---

### 2.6 `sandbox` — `workspace/compiler/sandbox`

**Responsibility:** Isolation: `Cage`, policies, budgets, worker pool, toolchain discovery, seccomp/bwrap/landlock hooks. Binary assemble owns env reads.

| Name | Kind | Purpose |
|---|---|---|
| `Cage` | trait | Isolation backend |
| `Policy`, `ThreatTier` | type | Development vs Production, trusted vs untrusted |
| `WorkerPool`, `WorkerLang` | type | Sealed worker execution |
| `ToolchainSet` | type | Discovered toolchains + digest |
| `IsolationPolicy` | type | require_worker etc. (binary-side) |
| `Budget` / seal helpers | type | Resource limits |

**Deps:** heart (light), OS isolation crates. Used by compiler-core + daemon.  
**Anchors:** 01 §5, 12 sandbox-in-k8s (gVisor complements, not replaces).

---

### 2.7 `compiler-daemon` — bin

**Responsibility:** HTTP compile service for untrusted fleet. Assembles `ForgeRuntime`, selects cage, binds `NUDOX_*` env, materializes FileBytes, calls `generate_with`, returns wire response.

| Name | Kind | Purpose |
|---|---|---|
| `ForgeRuntime` | type | Daemon-side assembled ForgeContext |
| `main` / axum routes | bin | `POST /compile` postcard |

**Deps:** compiler-core, compiler-wire, sandbox, heart. **No** registry/Postgres.  
**Anchors:** 01 §3, 12 warm pool.

---

### 2.8 `producer-worker` — bin

**Responsibility:** Sealed multi-step producer helper process.  
**Deps:** compiler-core, sandbox.  
**Anchors:** 01 §5.5.

---

### 2.9 `graph-cold` — NEW (`graph-over-ir`)

**Responsibility:** Full `PackageGraph` / `GraphOps` / `GraphStore` over CAS IR + OccurrenceSet without Terminus. Adjacency indexes, expand, membership_diff, version_diff.

| Name | Kind | Purpose |
|---|---|---|
| `PackageGraph` | trait | Unified hot/cold graph surface |
| `IrBlobPackageGraph` | type | Cold impl: load blobs → project → adj |
| `GraphView` | type | In-memory adjacency + by_iri indexes |
| `GraphOps` | trait | Client-facing expand/related (may alias PackageGraph subset) |
| `GraphStore` | trait | Legacy registry name — converge on PackageGraph |
| `RelationKind` | enum | Member, Reference, Occurrence, Implements, Extends, ReExport |
| `MembershipDiff`, `ContentDiff` | type | Cross-generation diffs |
| `PackageCtx` | type | lang/package/version for projection |

**Deps:** heart, ir, moniker, compiler-core::graph (or move `graph/` here from compiler), moka.  
**Must not depend on:** terminus HTTP for cold path correctness.  
**Anchors:** 04 §4.5, 07 PackageGraph, 14 GraphOps local=IrGraph.

---

### 2.10 `terminus-client` — NEW thin lib

**Responsibility:** Hot-tier TerminusDB HTTP client + typed documents. Admission **signals** only; policy may live in index-service.

| Name | Kind | Purpose |
|---|---|---|
| `TerminusPackageGraph` | type | PackageGraph over WOQL/docs |
| `GraphAdmission` / score helpers | type | Leaky-bucket promotion inputs |
| Document DTOs | type | Symbol, PackageVersion, Implementation, Reference |

**Deps:** heart, reqwest, vendored terminusdb_schema. **Not** crates.io terminus-store for production.  
**Anchors:** 07 entire, 02 §5.

---

### 2.11 `text-search` — NEW extract from `registry/runtime/text` + package search

**Responsibility:** Shared Tantivy schema, identifier tokenizer, LanguageAnalyzer registry, symbol + package index open/upsert/search. Same **schema + tokenizer registration + index format major** for INDEX and REGISTRY.

| Name | Kind | Purpose |
|---|---|---|
| `TextSearch` | trait | DepSet-scoped symbol/package search |
| `TextIndex` | type | Open/create mmap index, writer budget |
| `LanguageAnalyzer` | trait | Path split, query rewrite, boosts, signature terms |
| `LanguageRegistry` | type | Analyzer map by Language |
| `IdentTokenizer` | type | Shared subword core (existing) |
| `SymbolDocument` | type | Schema row mapping |
| `TextQuery`, `SymbolKind` filters | type | Query surface |
| `LocalTantivy` | type | Desktop/REGISTRY impl |
| `PackageSearchIndex` | type | Registry package tantivy (optional same crate) |

**Deps:** heart, moniker, tantivy **0.26.1** (target; today 0.22), ir views for signature terms.  
**Anchors:** 10 entire, 02 §3, 14 TextSearch.

---

### 2.12 `vector-local` — NEW

**Responsibility:** Embedded vector store for desktop REGISTRY + local embed worker.

| Name | Kind | Purpose |
|---|---|---|
| `VectorStore` | trait | upsert/delete/search/get/count/flush/compact |
| `VectorSearch` | trait | Client-facing search with SemanticGate |
| `QdrantEdgeLocal` | type | qdrant-edge impl |
| `LanceLocal` | type | optional feature escape hatch |
| `VectorPoint`, `SearchFilter`, `SearchHit` | type | Payload + filter model |
| `EmbedStage` / embed worker | type | Stage-compatible embed (08) |
| `SemanticGate` | type | Quota capability (from registry today) |
| `StoreCapabilities` | type | Feature flags for UI |

**Deps:** heart, qdrant-edge **0.7.2**, fastembed **5.17.x** + ort, (optional) lancedb **0.31.0**.  
**Anchors:** 09 entire, 14 VectorSearch.

---

### 2.13 `vector-remote` — NEW (or feature module)

**Responsibility:** Remote Qdrant via `qdrant-client` for INDEX and client remote semantic path. Implements same `VectorStore`.

| Name | Kind | Purpose |
|---|---|---|
| `QdrantRemote` | type | qdrant-client 1.18 impl of VectorStore |
| `HttpSemantic` | type | VectorSearch over INDEX HTTP |

**Deps:** heart, qdrant-client **1.18**.  
**Anchors:** 02 §4, 09 §6.

---

### 2.14 `meta-store` — NEW extract

**Responsibility:** Catalog spine without queue: packages, parse_status, symbols, outbox, sink_watermarks, sessions. Trait + sqlite backends. **Not** the job queue (that is ORCH/k8s).

| Name | Kind | Purpose |
|---|---|---|
| `MetaStore` | trait | packages / parse_status / symbols / outbox / watermarks |
| `Catalog` | trait | lookup + generation (client-facing subset) |
| `SqliteMetaStore` / `SqlxSqliteMetaStore` | type | INDEX single-writer sqlx |
| `RusqliteCatalog` | type | Desktop REGISTRY thread-bound catalog |
| `Outbox`, `SinkWatermark` | type | Derived-store fan-out coordination |
| `PackageRow`, `ParseStatus` | type | Schema DTOs |
| `QueueBackend` | trait | **Pluggable**; INDEX uses K8sJobs; local may use SqliteSingleWriter; PgSkipLocked **legacy only** |

**Deps:** heart, sea-query (SqliteQueryBuilder), sqlx 0.8 sqlite (INDEX), rusqlite bundled (REGISTRY).  
**Anchors:** 02 §1, §11; 11 entire; 13 catalog schema sketch.

---

### 2.15 `registry-local` — NEW (desktop REGISTRY composition)

**Responsibility:** On-disk local store layout: sqlite catalog + cas/ + tantivy/ + vectors/ + generation status. Directory ingest (trusted). Rebuild derived indexes from blobs. **Not** multi-tenant HTTP server.

| Name | Kind | Purpose |
|---|---|---|
| `LocalRegistry` | type | Open root path → Live catalog+CAS+indexes |
| `GenerationStatus` | enum | Pending / Pulling / Ready / Failed |
| `GenerationStatusMap` | type | Per (package, generation) readiness |
| `Disk layout helpers` | fn | cas/, ptr/, tantivy/, vectors/, registry.sqlite |
| `LocalCatalog` | type | Catalog impl |
| `Trusted directory ingest` | fn | Walk project roots without full untrusted jail (or soft jail) |

**Deps:** heart, meta-store, text-search, vector-local, graph-cold, ingest (subset), zstd.  
**Anchors:** 13 §5, 14 dual-home, 02 REGISTRY mapping.

---

### 2.16 `index-service` — evolve `server` + remote half of `registry`

**Responsibility:** Remote INDEX: HTTP API, authz, SearchPlanner, materialize pipeline for **untrusted** packages, outbox consumers for derived stores, S3 CAS, sqlite MetaStore, optional Terminus hot tier, health. Binary entrypoint.

| Name | Kind | Purpose |
|---|---|---|
| `IndexServer` / `Server` | type | Assembled live service |
| HTTP DTOs (shared with client) | type | Search, resolve, admin, health |
| `SearchPlanner` | type | Gate issuance / multi-backend plan |
| `CompilerClient` | type | HTTP to compiler-daemon / orch |
| Materialize / coordination | type | acquire→extract→compile→emit (INDEX) |
| Text/vector/graph adapters | type | Remote backends |
| Pollers | type | Outbox drain (single-writer); **no** SKIP LOCKED job claim |
| Config `NUDOX_*` | type | Service config only |

**Deps:** heart, meta-store, text-search, vector-remote, graph-cold, terminus-client, compiler-wire, ingest, object_store (S3), axum, moka, governor.  
**Must not depend on:** lindsey, GPUI, rusqlite desktop paths.  
**Anchors:** 02 §7, §9; 11 INDEX; 12 completion notify; 14 wire §6.

---

### 2.17 `orch` — NEW orchestration server

**Responsibility:** Thin control plane for untrusted compile fleet: admit jobs, warm vs cold path select, CAS dedup, presign S3, create Kueue Jobs / call warm Service, poison-pill recording into INDEX sqlite, **no** business search.

| Name | Kind | Purpose |
|---|---|---|
| `OrchServer` | type | axum + kube controllers |
| `JobAdmission` | type | input_hash dedup + in-flight coalesce |
| `PathSelector` | type | warm HTTP vs cold Kueue |
| `QueueBackend::K8sJobs` | impl | Job claim API redesign |
| `PoisonRecord` | type | INDEX-backed permanent fail |
| Resource class map | type | S/M/L/XL → flavors |

**Deps:** heart, compiler-wire, meta-store (job rows) or INDEX HTTP, kube **4.0**, k8s-openapi **0.28**, object_store, governor. Optional async-nats **0.49.1**.  
**Must not depend on:** compiler-core language crates (pods run compiler-daemon image).  
**Anchors:** 12 entire BOM §9.

---

### 2.18 `ingest` — extract or keep module

**Responsibility:** Untrusted archive sanitization (path jail, allowlist, budgets) + trusted directory walker for REGISTRY.

| Name | Kind | Purpose |
|---|---|---|
| `extract_archive` | fn | Jail + budget extract |
| `DirectorySource` | type | Trusted local project walk |
| Limits / allowlists | type | Security choke points |

**Deps:** heart (paths), minimal.  
**Anchors:** 02 §6.

---

### 2.19 `blob` — optional extract from `registry/blob`

**Responsibility:** BlobManifest dual-hash discipline, section layout, emit helpers. Pure types shared by INDEX/REGISTRY/client sync.

| Name | Kind | Purpose |
|---|---|---|
| `BlobManifest` | type | generation stamp + section refs + file list |
| `Section` refs | type | ir, references, source digests |
| `manifest_cas_key` | fn | CAS key vs generation identity split |
| Envelope codec | type | raw \| zstd \| zstd+dict (13) |

**Deps:** heart.  
**Anchors:** 02 §2.4, 13 §6.

---

### 2.20 `client-core` — optional purity split

**Responsibility:** Sans-IO: trait definitions for Catalog/TextSearch/VectorSearch/GraphOps/Compile, sync state machine transitions, routing policy pure functions, typestates Trusted/Untrusted, SyncedWitness.

| Name | Kind | Purpose |
|---|---|---|
| `Catalog`, `TextSearch`, `VectorSearch`, `GraphOps`, `Compile` | traits | Dual-backend family |
| `Routed<L,R>` | type | Local-if-ready else remote + single-flight |
| `SyncPlan`, `SyncEvent` | type | Manifest-pull control |
| `Trusted`, `Untrusted` | markers | Compile API only on Trusted |
| `SourcePath<T>` | type | Trust-branded paths |
| `SyncedWitness` | type | Proof generation Ready for local-only APIs |
| `Project` typestate | Unresolved→Resolved→Indexed | Open pipeline |
| `NetworkCapability`, `LocalComputeCapability` | tokens | Capability pattern |

**Deps:** heart, moniker, ir (DTOs only). **No** tokio required if pure.  
**Anchors:** 14 §4–5, §8–9.

---

### 2.21 `client` — NEW primary application library

**Responsibility:** Tokio runtime wiring: SyncEngine, HTTP INDEX client, local REGISTRY open, CAS tiered, progress channels for GUI. Replaces GUI’s direct `reqwest` + ad-hoc DTOs.

| Name | Kind | Purpose |
|---|---|---|
| `Client<S>` | type | Cold/Live Connect facade |
| `SyncEngine` | type | ensure(DepSet), pull, verify, atomic commit |
| `HttpCatalog`, `HttpSearch`, … | type | Remote impls |
| `EmbeddedForge` | type | Compile via compiler-core TrustedForge |
| `RemoteCompile` | type | Compile via INDEX/ORCH |
| `DepSet` | type | Resolved {(package, generation)} |
| Progress → GUI | channels | Align heart Progressive |

**Deps:** client-core, heart, registry-local, meta-store, text-search, vector-local/remote, graph-cold, compiler-core (feature `embed`), compiler-wire, reqwest.  
**Must not depend on:** index-service binary internals, orch, postgres, lindsey.  
**Anchors:** 14 entire, 03 §9.4, 15 stores.

---

### 2.22 `lindsey` — `workspace/gui` (crate name `lindsey`)

**Responsibility:** GPUI presentation only. Stores subscribe to `client`; no raw INDEX URLs in views long-term.

| Name | Kind | Purpose |
|---|---|---|
| `ProjectStore`, `SymbolStore`, `JobStore`, `RegistryStore` | type | GPUI Entity model layer |
| Views | modules | search, symbol, graph, jobs, project, settings |
| `SymbolClient` etc. | trait shim | Thin re-export of client traits if needed |

**Deps:** client, heart (types), gpui, gpui-component.  
**Must not depend on:** index-service, registry (server), sqlx-postgres, compiler-daemon.  
**Anchors:** 03, 15.

---

### 2.23 Incremental spine types (home: `heart` + small `stage` module or `meta-store`)

Not necessarily a separate crate day one; inventory of structures from 08:

| Name | Home | Purpose |
|---|---|---|
| `Stage` | heart or `stage` crate | digest(in)/run/output_digest pipeline unit |
| `StageInput`, `StageOutput` | same | Canonical bytes for digests |
| `TraceStore` | meta-store / registry-local | stage_traces SQLite |
| `symbol_heads` table | meta-store | Per-generation symbol content hashes |
| `SymbolDelta` | heart | Fan-out contract |
| `CommitGate` | client / registry-local | **Conceptual:** only sealed git commits advance generations (08); may be type `CommitGate` or policy fn — **not fully sketched as trait in research** |
| `run_stage` | same as Stage | Constructive-trace rebuilder |

**Anchors:** 08 §8–9. **Note:** `CommitGate` is used as a product concept; formal trait name is unresolved (§7).

---

## 3. Master trait catalog

Canonical names for the assembler. Prefer **one** trait name per concern; aliases listed when research used several.

| Trait | Home crate | Purpose | Primary impls |
|---|---|---|---|
| `Connect` | heart | Cold→Live open | MetaStore, Cas stores, Client |
| `Cas` | heart | Content-addressed get/put | MemoryCas, DiskCas, ObjectStoreCas |
| `EvictableCas` | heart | Honest delete | DiskCas, Tiered layers |
| `Catalog` | client-core / meta-store | Package/generation lookup | SqliteCatalog, HttpCatalog |
| `MetaStore` | meta-store | Full INDEX spine CRUD + outbox | SqlxSqlite, (legacy Pg) |
| `QueueBackend` | meta-store / orch | Job claim/complete | `K8sJobs`, `SqliteSingleWriter`, legacy `PgSkipLocked` |
| `TextSearch` | client-core / text-search | Symbol/package text search | LocalTantivy, HttpSearch |
| `LanguageAnalyzer` | text-search | Per-language query/path/boost | RustAnalyzer, TsAnalyzer, … |
| `VectorStore` | vector-local | Low-level ANN CRUD | QdrantEdgeLocal, QdrantRemote, LanceLocal |
| `VectorSearch` | client-core | App semantic search + gate | LocalVectors, HttpSemantic |
| `GraphOps` | client-core | Expand / related for UI | IrGraph, TerminusHttp |
| `PackageGraph` | graph-cold | Full package graph API | IrBlobPackageGraph, TerminusPackageGraph, TieredPackageGraph |
| `GraphStore` | **converge → PackageGraph** | Legacy registry name | same |
| `Compile` | client-core | Compile Trusted sources | EmbeddedForge, RemoteCompile |
| `ForgeContext` | compiler-core | Injected compile environment | Local, Trusted, Daemon ForgeRuntime |
| `Producer` | compiler-core | Language surface builder | RustProducer, … |
| `Cage` | sandbox | Isolation | DevPassthrough, Bwrap, … |
| `DocumentSink` | compiler-core | Graph document emission | Terminus sink, no-op |
| `DerivedStore` | heart (enum) | Fan-out channel tags | used by outbox |
| `Stage` | heart / stage | Incremental stage memo | EmbedStage, TantivyStage, … |
| `Progressive` | heart | Progress reporting | jobs, SyncEngine |
| `SemanticGate` | vector-* / registry runtime | Semantic quota capability | token issuance |
| `SymbolClient` / `SearchClient` | lindsey shim | GUI-facing; prefer client traits | Http*, Local*, Composite |
| `Backend` (render) | compiler-core/render | IR → surface syntax | per-lang emit |
| `CommitGate` | **TBD** | Seal generation only on commit | policy in client/local pipeline |

### 3.1 Trait → crate ownership (quick index)

```
heart:          Connect, Cas, EvictableCas, Progressive, DerivedStore(enum)
compiler-core:  ForgeContext, Producer, DocumentSink, render::Backend
sandbox:        Cage
text-search:    TextSearch (impls), LanguageAnalyzer
vector-local:   VectorStore
client-core:    Catalog, TextSearch, VectorSearch, GraphOps, Compile, Routed
graph-cold:     PackageGraph (+ GraphOps impl)
meta-store:     MetaStore, QueueBackend
orch:           QueueBackend::K8sJobs
```

### 3.2 Key non-trait structures (inventory)

| Structure | Home | Purpose |
|---|---|---|
| `BlobManifest` | blob/heart | Generation section map |
| `Generation` / `GenerationStatus` | heart / registry-local | Identity vs sync readiness |
| `DepSet` | client-core | Resolved dependency set for queries |
| `SymbolMoniker` / `LineageEdge` | moniker | Continuity |
| `SymbolDelta` | heart | Incremental fan-out |
| `JobKey` / `ContentHash` | heart | CAS keys |
| `GraphCorpus` | compiler-core or graph-cold | Projection intermediate |
| `OccurrenceSet` | ir | Reference corpus |
| `OracleSet` | compiler-core | External oracle paths |
| `StoreLinks` | meta-store | Which derived stores hold a generation |
| `PackageTierManager` | index-service / terminus-client | Hot admission state machine |
| `SyncEngine` | client | Manifest-pull sync |
| `Routed<L,R>` | client-core | Dual-home router |

---

## 4. Libraries / APIs to migrate TO

### 4.1 Master table (consumer × library × pin)

| Library / API | Pin / version (from research) | Consumer crate(s) | Role | Notes |
|---|---|---|---|---|
| **sqlx** (sqlite) | 0.8, features sqlite + bundled | meta-store, index-service | INDEX MetaStore | Dual pools: writer max 1, reader N |
| **sea-query** | 0.32 (eval 1.x) + SqliteQueryBuilder | meta-store | DDL/queries | Drop PostgresQueryBuilder for INDEX |
| **sea-query-binder** | 0.7 sqlx-sqlite | meta-store | Bindings | was sqlx-postgres |
| **rusqlite** | bundled + hooks | registry-local, meta-store desktop | REGISTRY catalog | Dedicated thread |
| **libsqlite3-sys** | 0.30 bundled | already in registry | — | Use or drop; currently inert |
| **Litestream** | v0.5.x (patched ≥0.5.2; e.g. 0.5.14 class) | ops for index-service | Continuous WAL → S3 | Sidecar + init restore; not a Rust dep |
| **rqlite** | v10.x | **fallback only** | HA INDEX | Not primary |
| **object_store** | enable S3 features | index-service, orch, heart Cas adapter | S3 CAS | INDEX cas/{blake3} |
| **zstd** / seekable zstd | pin at impl | blob, registry-local, index-service | Compression + transfer packs | dict_id in envelope |
| **tantivy** | **0.26.1** target (min 0.24.2); today **0.22** | text-search | Symbol/package FTS | Shared schema local+remote |
| **qdrant-edge** | **0.7.2** | vector-local | Embedded ANN | Primary desktop |
| **qdrant-client** | **1.18** | vector-remote, index-service | Remote ANN | Keep |
| **lancedb** | **0.31.0** | vector-local (optional feature) | Escape hatch | Runner-up |
| **fastembed** | **5.17.x** + **ort** | vector-local | Local embeddings | jina-embeddings-v2-base-code 768-d |
| **moka** | **0.12.x** | heart, graph-cold, index-service, client | Caches / stampede | Already in tree |
| **governor** | **0.7** today → **0.10.4** when convenient | index-service, orch, vector gate | Rate limits | Not hot-tier promotion sole policy |
| **gix** (gitoxide) | existing pin | client / acquisition (not sealed pods) | Clone/fetch, tree diff for CommitGate | Move off compiler-core sealed path |
| **postcard** | existing | compiler-wire, CAS stage values | Binary wire + stage cache | |
| **blake3** | existing | heart | ContentHash | |
| **kube** (kube-rs) | **4.0.0** | orch | Controllers + Job create | |
| **k8s-openapi** | **0.28** | orch | Typed k8s | |
| **Kueue** | **v0.18.3** | cluster (orch creates Jobs) | Cold admission | Ops not Rust crate |
| **KEDA** | **v2.20.x** optional | warm pool | Scaling | |
| **Karpenter** | **v1.x** (~1.13–1.14) | cluster | Nodes/spot | |
| **Kubernetes** | **1.36.x** preferred; ≥1.31 | cluster | userns, podFailurePolicy | |
| **async-nats** | **0.49.1** optional | orch | Overflow/events | Not required v1 |
| **NATS Server** | **v2.14.3** optional | cluster | Same | |
| **gVisor runsc** | node RuntimeClass | compiler-daemon pods | Sandbox layer | |
| **reqwest** | ^0.12 | client, terminus-client, index-service | HTTP | |
| **axum** | existing | index-service, orch, compiler-daemon | HTTP servers | |
| **terminusdb** server | **v12.0.6** | ops / hot tier | Graph DB | HTTP only |
| **terminusdb_schema** | vendored TERMINUS_REV | terminus-client | Types | Not crates.io terminus-store |
| **terminus-store** crates.io | 0.21.5 | **experiments only** | Stale vs v12 | Do not production-embed |
| **count-min-sketch-rs** | 0.1.1 | index-service admission | CMS doorkeeper | 07 |
| **dashmap** | existing-class | admission / caches | Concurrent maps | |
| **arborium / tree-sitter** | arborium 2.18 class | compiler-core, ir | Parse | No Tree persistence |
| **ra_ap_*** | existing | compiler-core rust feature | Rust HIR | Feature-gate desktop |
| **scip** (format/docs) | reference only | moniker design | Prior art | Not necessarily a runtime dep |
| **gpui / gpui-component** | lindsey pins; component ~0.5.1 observed | lindsey | UI | 15 |
| **salsa** | 0.28 | **optional inside producers only** | Not pipeline spine | 08 decision: hand-rolled traces |
| **rkyv** | 0.8.x if needed | optional IR mmap | Not default tree storage | 13 |

### 4.2 Explicitly not primary (reject / demote as product dependency)

| Item | Why |
|---|---|
| Postgres as INDEX SoT | Replaced by sqlite + Litestream; queue → k8s |
| `FOR UPDATE SKIP LOCKED` job queue | ORCH/Kueue |
| `pg_try_advisory_lock` | Single-writer sqlite / leader election |
| Qdrant-only local assumption | qdrant-edge + trait; remote remains Qdrant server |
| Terminus for all packages | Hot tier only; cold = graph-cold |
| LiteFS / Turso Cloud as INDEX SoT | 11 reject |
| Argo / Tekton | 12 overkill |
| Electric / PowerSync full sync | Wrong domain (14); use manifest-pull |
| Salsa as cross-process pipeline | 08 |
| FastCDC day one | 13 |
| Persist live tree-sitter Trees | 04, 13 Option A |

---

## 5. What gets deleted or demoted

| Item | Action | Reason / plane |
|---|---|---|
| Root legacy `compiler/` | **DELETE / freeze** | Live tree is `workspace/compiler` |
| `compiler` lib exporting `daemon` | **Demote** to compiler-daemon only | 01 |
| Duplicated `registry::protocol` | **DELETE** after compiler-wire | Twin drift |
| Postgres-only query builders as sole path | **Demote** | Dual then sqlite-only INDEX |
| `jobs` table as INDEX multi-worker queue | **DELETE** from INDEX design | ORCH |
| `PgSessionStore` as only session | **Demote**; Memory/Dir local | 02 |
| `libsqlite3-sys` unused | **Use in meta-store or drop** | Dead weight |
| Dual text materializers (outbox + text::Poller) | **Collapse** to one | 02 risk |
| Qdrant as sole vector path for desktop | **Demote** to remote INDEX | 09 Edge |
| Terminus as mandatory for graph UX | **Demote** to hot cache | 07 |
| crates.io `terminus-store` production path | **DELETE from plan** | Stale |
| GUI fixtures in release | **cfg(debug)** only | 03 |
| GUI `BackendClient` as only data path | **Demote** behind client | 03, 14 |
| LocalIndexPanel fire-and-forget subprocess as architecture | **Demote** → JobStore + Compile trait | 03, 15 |
| Package-ranking downloads bubble for symbols | **Demote** | 10 |
| CstSet as long-term persisted reference SoT | **Demote** → OccurrenceSet | 04 |
| Adaptive unsealed Java/C# as production untrusted | **Finish seal or restrict** | 01 |
| `buck_resource` embed path | **DELETE** for library form | OracleSet |
| Acquisition/gix inside sealed compile pods | **Move** to client/INDEX | 01 |
| Postgres `BackendKind` as required health | **Evolve** enum | 02 |
| Full ElectricSQL-style row sync | **Out of scope** | 14 |
| Tree projection as system of record | **Optional cache only** | 13 |

### 5.1 Table demotion map (Postgres → targets)

| Table / concern | INDEX | REGISTRY | DELETE |
|---|---|---|---|
| packages | sqlite | sqlite subset | |
| parse_status | sqlite | sqlite | |
| symbols | sqlite | sqlite | |
| outbox | sqlite | sqlite | |
| sink_watermarks | sqlite | sqlite | |
| sessions | optional sqlite | memory/dir | PgSessionStore |
| jobs | metadata only / ORCH | simple local queue optional | SKIP LOCKED design |

---

## 6. Naming glossary

| Term | Meaning (authoritative for librarification) |
|---|---|
| **INDEX** | Remote multi-tenant service plane: sqlite MetaStore + S3 CAS + HTTP API + derived search/graph; serves untrusted package intelligence |
| **REGISTRY** | Local desktop store: sqlite catalog + disk CAS + embedded Tantivy/vectors + generation status; offline-capable |
| **ORCH** | Orchestration server + k8s scheduling for untrusted compile fleet (not search) |
| **client** | Library between GUI and INDEX/REGISTRY; typestate + Routed backends + SyncEngine |
| **heart** | Shared vocabulary crate (not a plane) |
| **trusted** | First-party / user project sources allowed to compile **in-process** with TrustedForge; no untrusted archive assumptions |
| **untrusted** | Third-party / registry packages; compile only via sealed daemon/fleet; ingest jail required |
| **generation** | Immutable package snapshot identity: BlobManifest generation stamp + CAS closure; unit of sync atomicity |
| **GenerationStatus** | Local readiness: Pending / Pulling / Ready / Failed — **not** the same as content generation id |
| **moniker** | Version-stripped stable symbol coordinate (SCIP-inspired string); primary cross-generation join key |
| **SymbolId** | Instance + version-coupled UUID; **intra-store join**, not cross-generation continuity |
| **graph IRI** | Versionless Symbol/Package path URI used in GraphCorpus |
| **JobKey** | Content-addressed compile job domain key (producer‖toolchain‖source‖dep_lock) |
| **ContentHash** | BLAKE3 of bytes; CAS object identity |
| **BlobManifest** | Dual-hash manifest: generation identity vs CAS storage key |
| **DepSet** | Set of (package, generation) a query may see |
| **Routed** | Combinator: local if Ready else remote |
| **hot tier** | Packages admitted to TerminusDB under GraphAdmission |
| **cold tier** | Graph served by graph-cold over IR blobs |
| **Stage** | Incremental pure(ish) transform with constructive trace |
| **SymbolDelta** | added/removed/changed symbols between generations |
| **CommitGate** | Policy: pipeline advances on sealed VCS commit, not keystrokes |
| **ForgeContext** | Injected compile environment (cas, cage, toolchains) |
| **Cage** | OS/process isolation implementation |
| **SemanticGate** | Capability/quota token for expensive semantic search |
| **plane** | INDEX / REGISTRY / ORCH / SHARED deployment concern |
| **lindsey** | GUI binary crate name (path may be `workspace/gui`) |

### 6.1 Identity stack (do not collapse)

```
NudoxPath (IR, package-local)
  → graph IRI (versionless, cross-version)
  → SymbolMoniker (versionless, product continuity)
  → GenerationSymbol (moniker @ generation)
  → SymbolId / EntryUri (versioned PackageId + instance salt — store join only)
  → ContentHash / entry hashes (what the symbol *is*)
```

---

## 7. Contradictions / unresolved collisions

List for humans; do **not** silently pick winners in code until assembler decides.

| ID | Collision | Side A | Side B | Why it matters |
|---|---|---|---|---|
| C1 | **Graph trait name** | `GraphOps` (14 client) | `PackageGraph` (07) / `GraphStore` (registry) | Three names, one concern — pick PackageGraph as full API, GraphOps as client subset, or alias |
| C2 | **Vector trait split** | `VectorStore` (09 low-level) | `VectorSearch` (14 app-level + gate) | Keep both; document layering — do not merge blindly |
| C3 | **Catalog vs MetaStore** | `Catalog` thin client (14) | `MetaStore` full spine (02/11) | Catalog ⊆ MetaStore; clarify methods |
| C4 | **client crate split** | `client-core` + `client` (14) | single `client` modules (14 pragmatic alt) | Packaging overhead vs purity |
| C5 | **moniker crate vs heart module** | dedicated `moniker` crate | heart identity expand | 19 RFC empty |
| C6 | **PackageId versioned forever?** | Keep versioned PackageId (04 status quo) | Need package **stem id** for continuity (04 gap, 06) | SymbolId cannot equal across versions without stem |
| C7 | **GUI trait names** | `SymbolClient`/`SearchClient` (03) | `Catalog`/`TextSearch`/… (14) | GUI should depend on client traits, not invent parallel set long-term |
| C8 | **graph code home** | Stay in compiler-core (01) | Move to graph-cold (04/07) | Avoid compiler-core depending on storage; pure project() can live either place |
| C9 | **sqlx vs rusqlite** | INDEX sqlx (11) | REGISTRY rusqlite (11) | Dual stacks intentional — share schema/Idens only |
| C10 | **QueueBackend on MetaStore** | Trait on meta-store (02) | Jobs only in orch (12) | INDEX may keep job **metadata** rows without claim SQL |
| C11 | **Trace store Postgres** | 08 mentions Postgres option for k8s traces | 11/02 drop Postgres INDEX | Prefer sqlite traces on INDEX PVC; no new Postgres |
| C12 | **Warm path queue** | Direct HTTP (12 rec) | NATS optional (12) | Default no NATS |
| C13 | **CommitGate as trait** | Named in mission / 08 concept | No formal trait sketch in plans | Define trait vs policy function |
| C14 | **gix in compiler** | Still in compile/vcs (01) | Acquisition belongs client/INDEX (01 rec) | Move timing |
| C15 | **IR depends on heart?** | Keep ir free of PackageId | Producers need Coordinates/PackageId | Boundary of ir vs heart types |
| C16 | **Occurrence wire completeness** | Daemon returns thin response (01) | INDEX needs occurrences (01/04) | Wire v2 vs side-channel CAS |
| C17 | **Local vectors v1** | Full Edge embed (09) | “may remain remote-only v1” (14 §7.5) | Product phasing |
| C18 | **BackendKind enum** | Includes Postgres (heart) | Postgres removed | Rename/evolve |
| C19 | **Package search vs symbol search crates** | Both under text-search | Keep package ranking in index-service only | Fusion differs (10) |
| C20 | **TieredPackageGraph location** | terminus-client vs index-service vs graph-cold | 07 places router with manager | Admission state is INDEX, cold impl is graph-cold |
| C21 | **sandbox path** | Nested under compiler (today) | Flat workspace/sandbox | Build system only |
| C22 | **lindsey path vs name** | path `gui`, name `lindsey` | rename path | Cosmetic |
| C23 | **Salsa inside RA producer** | Allowed (08) | Not pipeline spine | OK if contained |
| C24 | **Transfer pack required?** | Optional (13) | Some sync UX assumes packs | v1 hash-set pull enough |

---

## 8. Dependency layering rules

Hard rules for Cargo.toml reviews and Buck graphs.

### 8.1 Forbidden edges

| Forbidden | Rationale |
|---|---|
| `lindsey` → `index-service` / `server` / `orch` | GUI talks only through `client` |
| `lindsey` → `sqlx` / postgres / qdrant-client directly | Use client + local crates |
| `compiler-core` → `sqlx` / `postgres` / `registry` schema | Compile stays pure |
| `compiler-core` → `axum` / daemon | Daemon is separate bin |
| `compiler-core` → `qdrant*` / `tantivy` / `terminus` | Fan-out is Stage consumers elsewhere |
| `heart` → `ir` / `compiler-*` / `client` | heart is leaves-up foundation |
| `ir` → `compiler-core` / network / SQL | IR is data |
| `sandbox` → `registry` / `server` | Isolation only |
| `graph-cold` → `terminus-client` | Cold must not need hot |
| `registry-local` → `kube` / `orch` | Desktop ≠ fleet |
| `client` → `orch` internals | Compile via HTTP API only |
| `text-search` → `axum` | Library only |
| `vector-local` → `index-service` | No server coupling |
| Any crate → root legacy `compiler/` | Dead tree |

### 8.2 Allowed but feature-gated

| Edge | Feature / note |
|---|---|
| `client` → `compiler-core` | `embed` / `trusted-compile` feature for desktop |
| `compiler-core` → heavy lang deps | `lang-rust`, `lang-nix`, … for binary size |
| `vector-local` → `lancedb` | `lance` feature escape hatch |
| `index-service` → `terminus-client` | `graph-hot` feature |
| `heart` → telemetry | server feature only |

### 8.3 Plane composition rules

1. **INDEX binary** may link: heart, ir, moniker, meta-store, text-search, vector-remote, graph-cold, terminus-client, ingest, blob, compiler-wire, object_store — **not** lindsey, **not** rusqlite-only desktop paths required.
2. **REGISTRY (in client process)** may link: heart, registry-local, text-search, vector-local, graph-cold, compiler-core(embed), meta-store(rusqlite) — **not** kube, **not** Kueue.
3. **ORCH binary** may link: heart, compiler-wire, kube, meta-store/job API — **not** language producers.
4. **compiler-daemon image** may link: compiler-core + all lang features needed + sandbox — **not** sqlite INDEX schema.
5. **Shared libraries** (heart, ir, moniker, compiler-wire, blob) must remain free of plane-specific I/O.

### 8.4 Trust boundary rules

| Rule | Detail |
|---|---|
| `Compile` only for `SourcePath<Trusted>` | Type system enforces (14) |
| Untrusted packages | RemoteCompile / daemon only |
| Ingest jail | Required for INDEX archives; optional soft for trusted dirs |
| SemanticGate | Required for VectorSearch remote; local may still gate CPU |
| NetworkCapability | Local-only APIs refuse without offline mode |

### 8.5 Generation consistency rules

| Rule | Detail |
|---|---|
| Query over DepSet S | Never mix generations mid-sync |
| Local takeover | Only when GenerationStatus::Ready for all members of S |
| CAS first-write-wins | ContentHash equality ⇒ share blobs |
| Derived indexes | Disposable; rebuild from blobs + catalog |
| Symbol moniker joins | Version-stripped; not SymbolId equality across gens |

---

## 9. Cross-walk: research plan → crates

| Plan | Primary crate impacts |
|---|---|
| 01 compiler-audit | compiler-core, compiler-wire, compiler-daemon, sandbox, TrustedForge |
| 02 registry-server | meta-store, index-service, registry-local, blob, ingest, heart |
| 03 gui-audit | lindsey stores, client traits |
| 04 ir-audit | ir, graph-cold, moniker, OccurrenceSet policy |
| 05 academic moniker | moniker (when filled) |
| 06 industrial moniker | moniker, LineageEdge, SCIP-shaped strings |
| 07 terminus tiering | terminus-client, PackageGraph, graph-cold, GraphAdmission |
| 08 incremental | Stage, SymbolDelta, TraceStore, CommitGate concept, gix feed |
| 09 vector | vector-local, vector-remote, VectorStore |
| 10 tantivy | text-search, LanguageAnalyzer, tantivy 0.26 |
| 11 sqlite-index | meta-store, Litestream ops, sqlx sqlite |
| 12 orchestration | orch, QueueBackend::K8sJobs, compiler-daemon deploy |
| 13 storage | blob envelopes, zstd, registry layout, no tree persist |
| 14 client-sync | client-core, client, Routed, SyncEngine, trust markers |
| 15 gui-references | lindsey views/roadmap only (no new backend crates) |
| 16–21 | empty at synthesis time — reserved for trust model, commit-gate trait, toolchains, moniker RFC, graph-over-ir detail, wire protocol |

---

## 10. Suggested Cargo package names (stable strings)

Prefer these `package.name` values (paths may differ):

```
heart
ir
moniker                 # optional
compiler-wire
compiler-core
sandbox
compiler-daemon         # bin package
producer-worker         # bin package
graph-cold
terminus-client         # optional
text-search
vector-local
vector-remote           # or vector-local feature "remote"
meta-store
registry-local
index-service
orch
client-core             # optional
client
lindsey
ingest                  # optional
blob                    # optional
```

Avoid reusing bare `registry` and `server` for the **target** architecture without renaming — they conflate INDEX and REGISTRY. Migration may keep old package names temporarily behind aliases.

---

## 11. Minimal public API surface checklist (assembler)

When a crate is “done enough” for dual-form:

| Crate | Must export |
|---|---|
| heart | ContentHash, JobKey, Cas, Connect, Generation, SymbolDelta, Failure taxonomy |
| ir | Index, Entry, OccurrenceSet, NudoxPath |
| moniker | SymbolMoniker, GenerationSymbol, LineageEdge |
| compiler-wire | CompileRequest, CompileResponse |
| compiler-core | ForgeContext, generate_with, Producer, project/GraphCorpus, render_entry |
| graph-cold | PackageGraph, IrBlobPackageGraph |
| text-search | TextSearch, TextIndex, LanguageAnalyzer |
| vector-local | VectorStore, QdrantEdgeLocal |
| meta-store | MetaStore, Catalog, Outbox types |
| registry-local | LocalRegistry, GenerationStatus |
| client | Client, SyncEngine, Routed backends, Trusted/Untrusted Compile |
| index-service | HTTP surface stable under protocol v1 |
| orch | Job admission API + k8s Job templates |
| lindsey | Stores over client only |

---

## 12. Open questions (inventory, not migration plan)

1. Finalize trait names: PackageGraph vs GraphOps vs GraphStore (C1).  
2. Package stem id: new type in heart or moniker?  
3. moniker as crate vs module (C5).  
4. client-core split worth the package? (C4).  
5. Formal `CommitGate` trait signature (C13).  
6. Local vectors in MVP vs remote-only (C17).  
7. Wire v2 occurrences vs CAS side-channel (C16).  
8. graph/ module physical move timing (C8).  
9. Whether `blob` and `ingest` are crates or modules of meta-store/registry-local.  
10. INDEX job metadata table retained vs pure k8s Job objects (C10).  
11. snix GPL in desktop feature matrix.  
12. RA keep-alive only in TrustedForge (library form) vs stateless pods.  

---

## 13. Executive inventory summary

**Target topology** splits today’s five live crates into a layered workspace: foundation (`heart`, `ir`, `moniker`), pure compile (`compiler-core`, `sandbox`, `compiler-wire` + two bins), derived intelligence libraries (`text-search`, `vector-local`/`remote`, `graph-cold`, `terminus-client`), storage spine (`meta-store`, `blob`, `ingest`), planes (`registry-local`, `index-service`, `orch`), and application (`client`, `lindsey`).

**Master traits** cluster as: storage (`Cas`, `MetaStore`, `Catalog`, `QueueBackend`), search (`TextSearch`, `LanguageAnalyzer`, `VectorStore`/`VectorSearch`), graph (`PackageGraph`/`GraphOps`), compile (`ForgeContext`, `Producer`, `Compile`, `Cage`), incremental (`Stage`, `SymbolDelta`, CommitGate policy), and client routing (`Routed`, capability tokens).

**Migrate-to stack:** sqlite/sqlx + Litestream (INDEX), rusqlite (REGISTRY), tantivy 0.26, qdrant-edge 0.7.2 + qdrant-client 1.18, zstd envelopes, gix at acquisition/commit-gate, kube-rs 4 + Kueue 0.18 for ORCH, moka/governor retained, Terminus v12 HTTP hot-only.

**Deletes/demotes:** Postgres queue/SKIP LOCKED, dual protocols, daemon-in-lib, Qdrant-only local, Terminus-for-all, root legacy compiler, release fixtures as architecture, tree-sitter Tree persistence.

**Unresolved:** trait naming collisions (C1–C3), PackageId stem, client purity split, local vector MVP, formal CommitGate — see §7.

---

## Appendix A — Plane × crate matrix

| Crate | SHARED | REGISTRY | INDEX | ORCH | GUI |
|---|:-:|:-:|:-:|:-:|:-:|
| heart | ● | ● | ● | ● | ● |
| ir | ● | ● | ● | | ○ |
| moniker | ● | ● | ● | | ○ |
| compiler-wire | ● | | ● | ● | |
| compiler-core | ● | ● embed | | image | |
| sandbox | ● | ● embed | | image | |
| compiler-daemon | | | | ● | |
| graph-cold | ● | ● | ● | | ○ |
| terminus-client | | | ● | | |
| text-search | ● | ● | ● | | ○ |
| vector-local | | ● | | | ○ |
| vector-remote | | | ● | | ○ via client |
| meta-store | ● | ● | ● | ○ | |
| registry-local | | ● | | | ○ |
| index-service | | | ● | | |
| orch | | | | ● | |
| client | ● | ● | ● | | ● |
| lindsey | | | | | ● |
| ingest | ● | ● | ● | | |
| blob | ● | ● | ● | ○ | ○ |

● = primary link; ○ = transitive/types only.

---

## Appendix B — Current → target rename cheat sheet

| Today | Target |
|---|---|
| `workspace/heart` | heart (keep) |
| `workspace/ir` | ir (keep) |
| `workspace/compiler` (lib) | compiler-core (− daemon) |
| `workspace/compiler/protocol` | compiler-wire |
| `workspace/compiler` daemon bin | compiler-daemon |
| `workspace/compiler/sandbox` | sandbox (keep path or flatten) |
| `workspace/registry` (monolith) | meta-store + registry-local + pieces of index-service + text-search + vector-* + graph-cold + ingest + blob |
| `workspace/server` | index-service (+ client DTOs → client) |
| `workspace/gui` / lindsey | lindsey (client-backed) |
| root `compiler/` | delete/freeze |
| Postgres jobs | orch + INDEX metadata |
| Qdrant local assumption | vector-local (Edge) |
| Terminus always-on | terminus-client hot + graph-cold |

---

## Appendix C — Source plan checklist

| Input | Used |
|---|---|
| 01-compiler-audit | §2.5–2.8, §5, OracleSet, dual-form |
| 02-registry-server-audit | meta-store, INDEX/REGISTRY map, heart inventory |
| 03-gui-audit | lindsey, client shims |
| 04-ir-audit | ir, graph-cold, identity |
| 06-symbol-identity-industrial | moniker layers |
| 07-terminus-tiering | PackageGraph, admission, terminus-client |
| 08-incremental | Stage, SymbolDelta, CommitGate concept |
| 09-vector | VectorStore, qdrant-edge pins |
| 10-tantivy | LanguageAnalyzer, tantivy 0.26 |
| 11-sqlite-index | sqlx/rusqlite/Litestream |
| 12-orchestration | orch BOM |
| 13-storage | blob, zstd, no trees |
| 14-client-sync | client traits, Routed, trust |
| 15-gui-references | lindsey views only |
| 05, 16–21 | partial/empty — flagged |
| edge-tech 01–05 | § optional backends (below); no DAG restructure |

---

## Optional future crates / backends (edge-tech)

**Status:** additive only (2026-07-16). Does **not** restructure the 22-crate DAG or plane split.

**Unified map:** [edge-tech/00-DECISIONS.md](../../edge-tech/00-DECISIONS.md)

| Optional piece | Lives in / beside | When | Verdict | Source |
|---|---|---|---|---|
| **`BlobTransport` trait** | `client` / `blob` | Design now | **Adopt interface**; HTTP default | [02-iroh](../../edge-tech/02-iroh/PLAN.md), [14](../14-client-sync/PLAN.md) |
| **`HttpBlobTransport`** | `client` | v1 | **Adopt** (presigned S3 + INDEX) | 14, 21 |
| **`IrohBlobsTransport`** | `client` (feature) | Phase 2 | **Optional** LAN/multi-source | 02 |
| **`Materializer`** | `blob` / `registry-local` / orch | v1 sparse | CAS→tmpdir/hardlink views | [03-edenfs](../../edge-tech/03-edenfs/PLAN.md), [13](../13-storage/PLAN.md) |
| **`ArchiveIndex` (SOCI-like)** | `blob` | Near-term ROI | External TOC + range reads | [05](../../edge-tech/05-surrounding-edge/PLAN.md), 13 |
| **composefs/EROFS helper** | `orch` / sandbox path | Linux pods later | Optional immutable mounts | 03, 05 |
| **`catalog-doltgres` service** | separate service (not INDEX SoT) | post-Doltgres-1.0 if needed | **Metadata (+symbols B′) only** | [01-doltgres](../../edge-tech/01-doltgres/PLAN.md) |
| **`graph-cold` + `ladybug` feature** | `graph-cold` | after FFI/license spike | Optional desktop Cypher | 05, [07](../07-terminus-tiering/PLAN.md), [20](../20-graph-over-ir/PLAN.md) |
| **CID/CAR export adapter** | `blob` boundary | interop only | Optional; no Kubo | [04-ipfs-and-cas](../../edge-tech/04-ipfs-and-cas/PLAN.md) |
| **WASM pure-step workers** | `compiler-core` / sandbox | later | Pure producers only | 05 |

**Do not add as first-class crates/planes:** EdenFS, public IPFS, iroh-docs catalog, second vector stack, second hot graph (Falkor/Neo4j), Doltgres-as-Terminus.

---

*End of 22-crate-topology inventory.*
