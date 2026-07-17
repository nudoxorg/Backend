# LIBRARIFICATION-PLAN — nudox Greenfield Build

**Date:** 2026-07-16 · **Status:** Rev 2 — greenfield freeze (supersedes the Rev 1 migration-framed plan)
**Research corpus:** `.research/librarification/01–22` + `.research/edge-tech/00–07` (evidence trail; this document is the plan)
**Normative companion:** `.research/ir-vcs/design/IR-NATIVE-VCS-DESIGN.md` (Rev 3.2) — **authoritative for the IR plane**. Where this plan touches IR types, it defers to that document; the join points are §4, §7, §9.
**Framing:** Everything here is built greenfield. There is no Postgres to exit, no legacy protocol to alias, no dual-write window, no cutover ladder. One ideal path per concern; alternatives live only in §0.5 as closed verdicts.

---

## Table of contents

```
Part 0 — Charter
  0.1  Vision and product shape
  0.2  North-star architecture
  0.3  Glossary (authoritative)
  0.4  Decision log GD-1..GD-40 (frozen)
  0.5  Alternatives considered (closed verdicts)

Part I — Target architecture
  §1   Crate topology & dependency layering
  §2   Wire protocol & plane contracts
  §3   Storage: CAS, manifests, packs, materialization

Part II — Identity & IR
  §4   The IR plane (arena IR + change log — ir-vcs summary + joins)
  §5   Moniker & lineage (normative) + IntroId recorder integration

Part III — Engines
  §6   Incremental spine: stages, traces, deltas
  §7   Commit gate & project generations
  §8   Graph: projection, Ladybug desktop engine, cold serving
  §9   Terminus hot tier & admission
  §10  Text search: Tantivy multi-language
  §11  Vector search & local embeddings

Part IV — Planes & distribution
  §12  Compiler dual-form
  §13  Desktop toolchains & packaging
  §14  Trust & project model
  §15  Client library & sync engine (iroh data plane)
  §16  INDEX: the remote service plane
  §17  ORCH: Kubernetes orchestration + fleet cache

Part V — GUI
  §18  lindsey: product views

Part VI — Program
  §19  Build order (Waves 0–5)
  §20  Risk register
  §21  Open questions
```

---

# Part 0 — Charter

## 0.1 Vision and product shape

nudox is a **desktop-first code-intelligence product** whose GUI (`lindsey`) is the start and end of the developer experience, backed by a clean split of planes:

- The **compiler** exists in two forms from one codebase. As a binary (`compiler-daemon`) it is a stateless sealed server: source in, sealed IR artifacts out, nothing else. As a library (`compiler-core` embedded in the client) it compiles **trusted code only**: the opened project, its path dependencies, its workspace.
- The **IR plane is a VCS.** Every package generation is an arena-indexed `PackageArchive` sealed from a single-writer channel of `Change<IrAtom>` — symbol-level, invertible, content-addressed history (ir-vcs Rev 3.2). This change log is the center of the system: deltas, lineage, incremental fan-out, and Terminus publish are all projections of it.
- The **INDEX** is the remote registry: SQLite metadata + S3 CAS + HTTP `/v1` control plane. It owns the channel writer for registry packages and serves untrusted-package intelligence while clients sync.
- The **REGISTRY** is the local, in-process store inside the GUI: SQLite catalog + disk CAS + embedded Tantivy + qdrant-edge + **Ladybug graph engine**. After sync, local takes over; offline degrades to the Ready subset.
- **ORCH** is a thin orchestration server over Kubernetes primitives (Kueue, Jobs, HPA/Karpenter). No SQL queue anywhere.
- The **client** library abstracts all of this behind typestates and strong patterns: trust-branded compile, `Routed<L,R>` dual backends, a SyncEngine that pulls generation closures over **iroh** by hash-set difference, and capability tokens.
- **TerminusDB serves only the most-used packages**, admitted by a leaky-bucket scorer and fed incrementally by `GraphAtom` projections of the IR change log. Everything else answers graph queries from IR projections.
- The whole pipeline is **committed-only, changed-only**: only sealed VCS commits advance generations, and only symbols whose part-hashes changed re-embed, re-index, re-publish. Cross-generation symbol identity is a three-layer architecture — version-stripped monikers, BLAKE3 part-hashes, and confidence-tagged lineage edges from a frozen T0–T7 cascade — whose verdicts are **persisted structurally as `IntroId` continuity in the change log** (§5.6).

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
 │    │      + ladybug/ graph engine · qdrant-edge · fastembed                  │
 │    │      + channel store for first-party package lineages (ir-vcs)          │
 │    ├─► EmbeddedForge = compiler-core + TrustedForgeContext   (trusted only)  │
 │    └─► SyncEngine: want/have closure pull over iroh-blobs, BLAKE3-verified,  │
 │         atomic Ready commit                                                  │
 └───────────────┬──────────────────────────────────────────────────────────────┘
                 │ HTTPS /v1 (control)  +  iroh QUIC (data: blobs by BLAKE3)
 ┌───────────────▼──────────────── REMOTE ──────────────────────────────────────┐
 │  INDEX (StatefulSet n=1): sqlite(WAL,PVC)+Litestream→S3 · S3 cas/{blake3}    │
 │    HTTP /v1 · SearchPlanner · channel writer (LibpijulChannelStore)          │
 │    outbox→{tantivy, qdrant, terminus-hot} sinks (SymbolDelta-driven)         │
 │    PackageTierManager (leaky bucket) ── terminus-client → TerminusDB v12     │
 │    │ POST /v1/jobs                                                           │
 │    ▼                                                                         │
 │  ORCH: admit → dedup(JobKey) → warm HTTP pool | Kueue cold k8s Jobs          │
 │    └─► compiler-daemon pods (gVisor+userns+bwrap; sealed; CAS-ref I/O)       │
 │                                                                              │
 │  nudox-iroh-provider fleet (regional): serves cas/ objects to clients,       │
 │    DownloadGrant-authorized, backed by S3 + node DiskCas                     │
 └──────────────────────────────────────────────────────────────────────────────┘
```

Trust routing: the opened project (and everything inside its Jail) compiles locally through `EmbeddedForge`; every third-party dependency resolves to coordinates and is delegated to the INDEX, which serves results immediately while the SyncEngine materializes the generation closure into the local REGISTRY — after which local takes over transparently.

## 0.3 Glossary (authoritative)

| Term | Meaning |
|---|---|
| **INDEX** | Remote multi-tenant service plane: sqlite MetaStore + S3 CAS + HTTP `/v1` + derived search/graph stores + channel writer for registry packages |
| **REGISTRY** | Local desktop store: sqlite catalog + disk CAS + embedded tantivy/vectors/ladybug + generation status; offline-capable |
| **ORCH** | Orchestration server + k8s scheduling for the untrusted compile fleet (not search, not blobs) |
| **client** | Library between GUI and INDEX/REGISTRY: typestates, Routed backends, SyncEngine |
| **trusted / untrusted** | First-party sources (project roots + in-jail path deps) may compile in-process; third-party packages compile only on the sealed fleet |
| **PackageGeneration** | Immutable registry-package snapshot; identity = `GenerationStamp` (`nudox.gen.v3`, ir-vcs); the unit of sync atomicity |
| **ProjectGeneration** | First-party desktop snapshot gated on a sealed git commit; identity = `nudox.projgen.v1` (§7). Never confuse with PackageGeneration |
| **GenerationStatus** | Local readiness: Pending / Pulling / Ready / Failed — *not* an identity |
| **PackageArchive** | Sealed IR-only archive (`NdIr`) per PackageGeneration: EntryArena + links + indices (ir-vcs) |
| **Change / IrAtom / channel** | Symbol-level invertible delta / its atoms / the single-writer per-lineage log they live on (ir-vcs) |
| **IntroId** | Forever-stable IR-plane symbol identity, minted at first Insert, preserved through renames by recorder verdicts (§5.6) |
| **moniker** | Version-stripped stable symbol coordinate (SCIP-shaped); the cross-generation matcher key |
| **SymbolId** | `UUIDv5(instance_token, "intro:" ‖ package_uuid ‖ intro_bytes)` — store join only, never continuity |
| **JobKey** | Content-addressed compile job key: producer ‖ toolchain fingerprint ‖ source ‖ dep_lock (`nudox-producer/1`) |
| **BlobManifest** | Dual-hash manifest: generation identity stamp ≠ CAS storage key (ir-vcs BlobManifestV3) |
| **DepSet** | The set of (package, generation) pairs a query may see; generation-pinned |
| **closure / Ready** | `RequiredClosure(G)` per ir-vcs: files + ir_package_ref + change contents, all BLAKE3-verified locally |
| **want/have** | Manifest-driven hash-set sync negotiation |
| **hot / cold tier** | TerminusDB-admitted packages / graph served from IR projections |
| **Stage / SymbolDelta** | Incremental transform with a persistent trace / the added-removed-changed contract derived from the change log |
| **CommitGate** | Pipeline advances only on sealed VCS commits, never keystrokes |
| **ForgeContext / Cage** | Injected compile environment / OS isolation implementation |
| **SemanticGate** | Capability/quota token required for expensive semantic search |
| **Materializer / ArchiveIndex** | CAS→view trait family / external TOC (`.ndix`) enabling range-lazy member access on packs (`.ndpk`) |
| **DownloadGrant** | INDEX-signed capability: client EndpointId + hash allowlist + expiry; authorizes provider fetches |
| **nudox-iroh-provider** | Regional blob provider serving `cas/` objects over iroh, backed by S3 |
| **Fleet cache L0–L5** | Compiler fleet layers: L0 output CAS · L1 JobKey stage CAS · L2 language caches (forbidden cross-tenant) · L3 images · L4 warm process state · L5 source hardlink CAS |
| **lindsey** | The GUI binary crate (path `workspace/gui`) |

Identity stack — **never collapse these layers** (§4, §5):

```
EntryIdx<T> (arena, in-memory session)      — never serialized
  IntroId (IR plane, forever after Insert)  — change keys, blame, structural continuity
  LineageMoniker (versionless coordinate)   — matcher key, product continuity evidence
  GenerationSymbol (moniker @ generation)   — SCIP interop, index rows
  GraphDocId (versionless IRI)              — graph node key
  SymbolId (instance-salted UUID)           — store join only
  ContentBlake3 part hashes                 — what the symbol IS
```

**LineageEdge is the evidence; IntroId is the persisted verdict** (§5.6).

## 0.4 Decision log (frozen)

Every section conforms to these. GD numbers are stable across revisions so research-doc citations keep resolving; Rev 2 revises the bodies in place and adds GD-36..40.

- **GD-1 Naming.** INDEX / REGISTRY / ORCH / client / lindsey per glossary. No crate is ever named bare `registry` or `server`.
- **GD-2 SQLite everywhere there is SQL.** INDEX = sqlx 0.8 (writer pool max 1 + reader pool N), sea-query `SqliteQueryBuilder`, k8s StatefulSet `replicas: 1` + RWO PVC, Litestream v0.5.x sidecar → S3 with restore-on-boot. REGISTRY = rusqlite (bundled) on a dedicated thread. The job queue is not SQL (→ GD-19). Outbox + sink watermarks are sqlite tables drained by a single-writer poller.
- **GD-3 Graph trait naming.** `PackageGraph` = full graph API; `GraphOps` = client-facing subset (client-core).
- **GD-4 Vector.** Two layers: `VectorStore` (low-level ANN CRUD) + `VectorSearch` (app-level + SemanticGate). Embedded = **qdrant-edge**; remote = qdrant-client. Parity both sides: `jinaai/jina-embeddings-v2-base-code`, 768-d, via fastembed + ort. One vector stack; premium remote-only Voyage collection + SymbolDelta-only embeds per `.research/librarification/09b`.
- **GD-5 Catalog ⊆ MetaStore.** `Catalog` is the client-facing read surface; `MetaStore` is the spine CRUD + outbox + watermarks.
- **GD-6 client-core sans-IO split.** Pure traits, routing policy, sync state machine, typestates in client-core; tokio/reqwest/iroh wiring in client.
- **GD-7 Symbol identity per RFC-19.** Dedicated `moniker` crate. Version-stripped `LineageMoniker` (SCIP-shaped) + generation-qualified `GenerationSymbol`; BLAKE3 part hashes `sig`/`body`/`doc`/`ref`/`embed_key` with `normalizer_version`; explicit `LineageEdge` records from the frozen T0–T7 cascade; **prefer false-split over false-merge**; soft edges never skip embeds or hard identity. `PackageStemId` (version-less package continuity) in heart.
- **GD-8 Graph projection home.** `IrToGraph` + the shared edge lowerer to `(SymbolId, RelationKind, SymbolId)` live in `nudox-ir-project` (ir-vcs). Cold path re-projects from PackageArchive on cache miss. `DepLoadPolicy::StubsOnly` by default. Hot publish emits the same lowerer edges (Relation parity).
- **GD-9 Wire protocol per §2.** Control plane = HTTP/JSON `/v1` + `/v1/hello` negotiation. **Data plane = iroh-blobs** (GD-30): clients fetch CAS objects by BLAKE3 hash from the provider fleet under DownloadGrants; the INDEX never proxies blob bytes and clients never hold store credentials. Compiler plane = single postcard version in `compiler-wire` returning full sealed artifacts. ORCH plane = thin admit/status/poison. `ErrorBody` projects `heart::Failure`. Internals (sqlite, tantivy, qdrant, terminus, k8s) never appear on the public wire.
- **GD-10 BlobManifest = ir-vcs BlobManifestV3.** Dual-hash discipline (GenerationStamp ≠ CasKey); fields `ir_package_ref`, `change_set_ref`, `occurrences_ref`, `references_ref`, plus transport artifacts `archive_pack_ref`/`archive_index_ref` (excluded from identity_bytes — they are re-encodings).
- **GD-11 Tree-sitter trees are NEVER persisted.** Store zstd source + `OccurrenceSet`/`ResolvedReference`; reparse on demand. No stable Tree serialization API exists; occurrences are the reference SoT.
- **GD-12 Compression.** zstd everywhere: trained dictionaries for IR sections (dict_id in the envelope), plain zstd for source. Bulk container = `.ndpk` frame-per-file pack + `.ndix` index (§3.4) — never the sole replica; per-file CAS remains the system of record.
- **GD-13 Terminus = hot tier only.** TerminusDB v12 via HTTP behind `terminus-client` (vendored `terminusdb_schema` types); the stale crates.io `terminus-store` is banned. Admission = leaky-bucket scorer + count-min-sketch doorkeeper fed by cold-path query stats from day one; demotion pins the cold projection first. Hot publish is incremental via `Change<GraphAtom>` (GD-36).
- **GD-14 Incremental spine = hand-rolled constructive traces** in SQLite + BLAKE3 digests — not salsa-as-orchestrator (salsa allowed *inside* producers; its memo tables are never product state). `Stage` + `TraceStore` + `symbol_heads` + `SymbolDelta` per §6. Derived indexes are disposable — always rebuildable from CAS + catalog.
- **GD-15 CommitGate per §7.** `ProjectGeneration` id = `BLAKE3("nudox.projgen.v1" ‖ project_id ‖ repo_id ‖ commit_oid ‖ encode(submodule_pins))`. Detection = `notify` watches on GIT_DIR + 5 s poll net, 300 ms debounce, quiet period during rebases. Dirty trees never trigger embeddings or durable index updates; explicit preview generations use a separate TTL-GC'd id space.
- **GD-16 Trust model per §14.** `Jail` = realpath prefix set; BFS of in-jail path deps → `TrustedSourceSet`; lockfile pins → `UntrustedCoordinateSet`; canonicalize-before-jail; refuse `node_modules` roots; ignore config source redirects by default; Workspace-Trust gate precedes any toolchain execution. `Compile` exists only on `SourcePath<Trusted>` (typestate-enforced).
- **GD-17 Compiler dual-form per §12.** `ForgeContext`, `generate_with`, `SealedInput`, `JobKey`, typestate seal. Full injection surface from day one: `OracleSet` (no self-exe resource lookup), explicit `ToolchainSet` (no env reads in the library), `TrustedForgeContext` for desktop. **Every untrusted-fleet producer is sealed** (ExecPlan or worker-isolated) — there is no adaptive in-process bypass on the fleet. Acquisition (gix) lives in client/INDEX, never in sealed compile paths.
- **GD-18 Desktop toolchains per §13.** Hybrid packaging: Class-B oracles in-app or as language packs; system-discovered Class-C SDKs; JobKey toolchain components are **semantic blake3 content fingerprints from day one** (`nudox-producer/1`). MVP local languages: TypeScript, Python, Rust (if rustup present), Go (if go present). Remote-only: Java, C#, Nix. snix is GPL-3.0 and never links into the GUI binary.
- **GD-19 ORCH per §17.** Warm Deployment (direct HTTP, 503 backpressure) + Kueue cold Jobs; kube-rs + k8s-openapi; `podFailurePolicy` classifies poison into INDEX metadata; gVisor RuntimeClass + `hostUsers: false` + in-pod bwrap/landlock layered; S3 multipart/zstd artifact flow (in-cluster IAM); HPA/KEDA warm scaling; Karpenter NodePools (spot for cold). No workflow engine, no CRDs-as-queue, no message broker.
- **GD-20 Client per §15.** Typestates for monotonic concerns only (Connect, Project resolve, trust plane); per-generation sync status = runtime state + `SyncedWitness` proofs. Every query routes over a generation-pinned DepSet: local iff all members Ready, else remote while the SyncEngine pulls; offline = Ready subset only. Generations become visible atomically after full hash-verified commit. Sync = manifest want/have hash-set difference — never CRDT/row replication.
- **GD-21 Text search per §10.** One shared multi-language schema + tokenizer registration + index-format version across INDEX and REGISTRY (tantivy 0.26.1 pinned). `LanguageAnalyzer` trait over the shared subword core. A schema-version marker beside the index forces rebuild on mismatch (disposable, GD-14). Symbol ranking = multi-field BM25 + exact-match bonus + kind prior; package download-bubble fusion stays package-side.
- **GD-22 GUI per §18.** Stores-over-client: `ProjectStore`, `SymbolStore`, `SearchStore`, `JobStore`, `RegistryStore`, `NavHistory` subscribe to client; Workspace is a layout shell; gpui-component Dock/List/Table/Tree/Modal adopted. Flagship wedges: symbol lineage timeline + type-directed multi-language search. Fixtures behind `cfg(debug_assertions)`.
- **GD-23 Crate purity.** `nudox-ir` depends only on `nudox-change` (+ leaf hash crate); producers stamp heart identities in compiler-core at seal. heart gains `Generation`, `SymbolDelta`, `PackageStemId`. Shared libs stay free of plane-specific I/O. Shared identity primitives live in the leaf `content-hash` crate (GD-38).
- **GD-24 INDEX job metadata.** INDEX sqlite keeps job/poison metadata rows for observability; claim/lease semantics live only in ORCH/Kueue.
- **GD-25 Cold-first graph rollout.** Cold projection serving ships before Terminus gating (hot is an optimization). Cold query stats feed the GD-13 scorer from day one; demotion pins cold.
- **GD-26 Non-existence list.** The following are never built: SQL job queues; session multi-replica merge; ad-hoc per-handler error shapes; CST persistence; store credentials on desktops; unsealed fleet producers; parallel IR stores.
- **GD-27 Store layouts.** REGISTRY = `registry.sqlite` + sharded `cas/` + `tantivy/` + `vectors/` + `ladybug/` + `views/` + pin/LRU GC. INDEX = S3 `cas/{blake3}` + sqlite catalog. Both planes store source (zstd) + PackageArchive + occurrences per GD-10/11/12.
- **GD-28 Committed-only, changed-only.** Embeddings, text indexing, graph publish, and lineage computation run only on sealed generations (GD-15) and only for symbols whose relevant part-hash changed (GD-7/GD-14/GD-36). This is the product's central performance invariant.
- **GD-29 Materializer + ArchiveIndex.** Materialization = CAS objects + manifest + `Materializer` trait (`CasHardlink` / `LazyArchive` / `FullExtract` / `VirtualOnly`); SOCI-style external `ArchiveIndex` (`.ndix`) emitted at produce time enables range-lazy member access. No kernel FS, no FUSE, no EdenFS-class dependency; the GUI virtualizes trees in-process.
- **GD-30 iroh-blobs is the client data plane.** `BlobTransport` trait in client-core; the production impl is `IrohBlobsTransport`. A regional `nudox-iroh-provider` fleet serves `cas/` objects (backed by S3, the durable SoT) under INDEX-signed `DownloadGrant`s; self-hosted relays per region carry NAT/proxy traversal (iroh relays run over HTTPS — restrictive networks are handled *inside* the transport, not by a parallel code path). Control plane, catalog, and subscription remain HTTP. iroh-docs is never used for the catalog. Compiler-fleet I/O stays direct S3 (in-cluster IAM) — pods are not iroh clients.
- **GD-31 Fleet cache: share L0/L1/L3/L5; forbid L2 cross-tenant.** **L0** ORCH/INDEX output CAS (JobKey → sealed artifacts); **L1** stage postcard CAS via heart `Tiered` with a real `ObjectStoreCas` L3 from day one; **L3** toolchain OCI/nix image layers; **L5** node-local source hardlink CAS via Materializer. **L2** mutable language caches (`target/`, GOCACHE, Gradle) are `CacheScope::Forbidden` across untrusted tenants — RO content-addressed dep blobs only. **L4** warm process state is pod-local only. Cross-node L1 hits work from day one because JobKey toolchain fingerprints are content-based (GD-18).
- **GD-32 No versioned SQL catalog side-store.** Doltgres (and kin) rejected outright: package-metadata history is a crawl-audit concern served by INDEX sqlite + CAS snapshots, and symbol history ("what appeared between 1.0 and 1.1") is **exactly** a fold of the IR change log — `Change<IrAtom>` materialize diff is strictly stronger than `dolt_diff` rows (typed, symbol-level, content-addressed, already built). No second SQL engine, no dual-write, no cross-DB reconcile.
- **GD-33 No public-network content addressing.** No public IPFS/Kubo swarm, no Bitswap, no UnixFS/IPLD codecs. The CAS Merkle invariant (manifest root commits leaves; leaves fetch/verify independently) is already native to §3.
- **GD-34 Ladybug is the desktop graph engine.** REGISTRY graph queries run on an embedded **LadybugDB** database projected from PackageArchive links at generation-Ready (a disposable derived store per GD-14, rebuilt from IR on schema bump or corruption). The pure projection path (`nudox-ir-project`) remains the SoT lowering and the INDEX cold-serving path. No second hot graph system.
- **GD-35 No kernel-mount materialization.** Hardlink/CAS materialize + range-lazy packs cover compile and desktop; composefs/EROFS/FUSE are out.
- **GD-36 IR plane = ir-vcs Rev 3.2 (NEW).** Arena `nudox-ir`, `PackageArchive` (`NdIr`), `Change<IrAtom>` on single-writer channels (`ChannelStore`, libpijul-backed durable log), `RegistryResolver` local/remote fill, `IrToGraph` → `Change<GraphAtom>`. K1–K28 of that design are binding here. `SymbolDelta` (§6) is **derived from the applied Change**, not recomputed by map-diff.
- **GD-37 Recorder identity (NEW).** The IR recorder (prev pristine vs new arena → atoms) uses the §5 lineage cascade as its matcher: T0–T3 **Auto** matches at confidence ≥ 0.95 preserve `IntroId` (emit `Update`/`Reparent`, plus moniker change metadata); anything below emits `Delete`+`Insert` (false-split-friendly) with a soft `LineageEdge` recorded for the UI. Renames therefore keep IntroId only when the matcher is certain — K18 made operational.
- **GD-38 Leaf hash crate (NEW).** A dependency-free `content-hash` crate owns the 32-byte BLAKE3 newtype family (`ContentBlake3` and its domain-tag helpers). `heart`, `nudox-change`, and `moniker` all build their domain newtypes (`CasKey`, `GenerationStamp`, `ChangeId`, `IntroId`, part hashes…) on it. One hashing discipline: `blake3(domain_tag ‖ length-prefixed parts)`; the full domain-tag registry is Appendix A.
- **GD-39 Channel writer placement (NEW).** One writer per `(PackageLineageId, ChannelName)` (ir-vcs K26): the **INDEX** owns channels for registry packages; the **client** owns channels for first-party project packages. The sealed compiler never records — it returns a candidate arena; the channel writer materializes prev, records, applies, seals.
- **GD-40 Generation domains (NEW).** `GenerationStamp` (`nudox.gen.v3`, package plane) and ProjectGeneration (`nudox.projgen.v1`, desktop commit gate) are distinct identities with distinct domain tags. APIs never accept one where the other is meant (distinct newtypes).

## 0.5 Alternatives considered (closed verdicts)

The full evidence lives in `.research/librarification/` and `.research/edge-tech/`. These verdicts are closed; do not re-open without new evidence.

| Alternative | Verdict | Why (one line) |
|---|---|---|
| iroh-blobs data plane | **ADOPT (this revision — was Phase-2)** | BLAKE3-soulmate of the CAS: verified streaming (bao), multi-source, self-hosted relays; kills presigned-URL machinery and CDN range-GET fragility |
| HTTP presigned S3 GET as client data plane | REJECT (superseded) | Second data path with weaker verification; iroh subsumes it; S3 stays durable SoT behind providers |
| LadybugDB embedded graph | **ADOPT (this revision — was watch)** | Real query engine (Cypher, columnar) for desktop graph UX; disposable projection keeps IR as SoT |
| Upstream Kùzu | REJECT | Archived |
| Doltgres A′/B′ catalog | **REJECT (this revision — was evaluate)** | The IR change log answers every "what changed between versions" question typed and symbol-level; a second SQL engine + dual-write buys nothing |
| TerminusDB hot tier | KEEP | Only production document-graph store with immutable versioning + semantic diff/patch; behind HTTP; admission-gated |
| Second hot graph (Falkor/Neo4j/AGE) | REJECT | One hot system |
| qdrant-edge embedded vectors | KEEP | In-process, disk-resident, same filter model as remote Qdrant |
| LanceDB / USearch / sqlite-vec | REJECT | No parallel vector plane |
| SQLite single-writer + Litestream INDEX | KEEP | Writes are bursty metadata; reads cacheable; DR is a rehearsed drill |
| rqlite / LiteFS / distributed SQLite | REJECT | HA complexity unjustified at n=1 write plane |
| Kueue + k8s Jobs for cold compile | KEEP | Queue semantics belong to the scheduler, not SQL |
| Argo/Tekton, CRDs-as-queue, NATS | REJECT | Overkill / wrong object model / broker without a need |
| EdenFS / FUSE / composefs materialization | REJECT | Wrong keying model, platform tax; hardlink + range-lazy covers it |
| Public IPFS / Bitswap / CAR-CID export | REJECT | Liability + ops; the Merkle invariant is already native |
| CRDT sync engines (Electric/PowerSync/cr-sqlite) | REJECT | Content-addressed substitution (want/have) is the correct model; shapes/priority-bucket *ideas* borrowed for subset UX |
| iroh-docs catalog | REJECT | CRDT multi-writer wrong for INDEX authority |
| salsa as pipeline orchestrator | REJECT | No durable cross-process story; fine inside producers |
| WASM component pipeline steps | REJECT | Not language oracles; no current win |
| Dual-write / migration ladders of any kind | REJECT | Greenfield; versioned formats + rebuild-from-CAS is the only evolution mechanism |

---

# Part I — Target architecture

## §1 Crate topology & dependency layering

**Source:** `.research/librarification/22` + ir-vcs crate map · **Plane:** SHARED

### 1.1 Workspace

```
workspace/
├── content-hash/         # leaf — ContentBlake3 + domain-tag hashing (GD-38)
├── heart/                # lib — vocabulary, CAS traits, typestate, progress, Stage
├── nudox-change/         # lib — Atom (public unsealed), Change<A>, laws, fingerprints
├── nudox-ir/             # lib — Entry, Node, Kind, EntryIdx, Registry, EntryBuilder, IrAtom
├── nudox-ir-archive/     # lib — PackageArchive seal/open, yoke, indices (.NdIr)
├── nudox-ir-channel/     # lib — ChannelStore, InMemory + Libpijul impls (GPL boundary)
├── nudox-ir-sync/        # lib — IR wire protocol, RequiredClosure, resolvers
├── nudox-ir-project/     # lib — IrToGraph, GraphAtom, cold PackageGraph, edge lowerer
├── moniker/              # lib — LineageMoniker, part hashes, LineageEdge, T0–T7 matcher
│
├── compiler-wire/        # lib — postcard CompileRequest/Response (§2.3)
├── compiler-core/        # lib — pure pipeline; no ambient policy, no daemon
├── sandbox/              # lib — Cage, workers, toolchains
├── compiler-daemon/      # bin — untrusted fleet HTTP + ForgeRuntime
├── producer-worker/      # bin — sealed worker helper
│
├── graph-local/          # lib — LadybugGraph: embedded desktop engine over projections
├── text-search/          # lib — Tantivy schema, LanguageAnalyzer, TextIndex
├── vector-local/         # lib — VectorStore + qdrant-edge + embed worker
├── vector-remote/        # lib — VectorStore over qdrant-client
├── terminus-client/      # lib — hot-tier HTTP Terminus + typed documents
│
├── meta-store/           # lib — MetaStore trait + sqlite (sqlx & rusqlite) impls
├── blob/                 # lib — BlobManifest, envelopes, .ndpk/.ndix, Materializer
├── ingest/               # lib — archive jail + trusted directory walker
│
├── registry-local/       # lib — desktop REGISTRY composition
├── index-service/        # lib+bin — remote INDEX
├── orch/                 # lib+bin — k8s orchestration server
│
├── client-core/          # lib — sans-IO traits, routing, sync FSM, typestates
├── client/               # lib — tokio wiring: SyncEngine, HTTP, iroh, EmbeddedForge
└── gui/                  # bin — lindsey
```

### 1.2 Dependency DAG and layering rules

```
              content-hash
             ┌─────┴─────────────┐
             ▼                   ▼
       nudox-change            heart ──────────────── compiler-wire · blob
             ▼                   │
         nudox-ir ── moniker ────┤
        ┌────┴────────┐          │
        ▼             ▼          ▼
 nudox-ir-archive  nudox-ir-project   compiler-core ◄── sandbox
        ▼             │               │
 nudox-ir-channel     │        compiler-daemon · producer-worker
        ▼             │
  nudox-ir-sync   graph-local · text-search · vector-* · terminus-client
        │             │
        ▼             ▼
    meta-store ◄── ingest
        │
   ┌────┼─────────────┐
   ▼    ▼             ▼
registry-local  index-service  orch
   └────┬──────────┬──┘
        ▼          ▼
    client-core → client → lindsey
```

**Forbidden edges** (enforced by a workspace-graph CI test):

| Forbidden | Why |
|---|---|
| lindsey → index-service / orch / sqlx / qdrant-client | GUI talks only through client |
| compiler-core → sqlx / axum / qdrant / tantivy / terminus / nudox-ir-channel | Compile stays pure; recording is the channel writer's job (GD-39) |
| heart → nudox-ir / compiler-* / client · nudox-ir → heart / network / SQL | Foundation is leaves-up (GD-23) |
| nudox-ir-project → terminus-client | Cold must never need hot |
| nudox-ir / nudox-change → libpijul re-exports | GPL stays behind nudox-ir-channel (ir-vcs K8) |
| registry-local → kube / orch · client → orch internals | Desktop ≠ fleet |
| graph-local → terminus-client · text-search → axum · vector-local → index-service | Libraries stay libraries |

**Feature-gated edges:** client → compiler-core (`trusted-compile`); compiler-core → heavy language deps (`lang-*`); index-service → terminus-client (`graph-hot`).

### 1.3 Master trait catalog (canonical homes)

| Trait | Home | Impls |
|---|---|---|
| `Connect` (Cold→Live), `Cas`, `EvictableCas`, `Progressive`, `Stage` | heart | existing patterns |
| `Atom`, `Change<A>` laws | nudox-change | `IrAtom` (nudox-ir), `GraphAtom` (nudox-ir-project) |
| `ChannelStore` | nudox-ir-channel | InMemoryChannelStore, LibpijulChannelStore |
| `RegistryResolver` / `AsyncRegistryResolver` | nudox-ir | LocalDisk, RemoteIndex, Composite (+ SingleFlight) |
| `Catalog` (⊆ MetaStore) | client-core | SqliteCatalog, HttpCatalog |
| `MetaStore` | meta-store | SqlxSqlite (INDEX), Rusqlite (REGISTRY) |
| `TextSearch` / `LanguageAnalyzer` | client-core / text-search | LocalTantivy, HttpSearch; per-language analyzers |
| `VectorStore` / `VectorSearch` | vector-local / client-core | QdrantEdgeLocal, QdrantRemote / LocalVectors, HttpSemantic |
| `PackageGraph` / `GraphOps` | nudox-ir-project / client-core | ColdRegistryGraph, LadybugGraph, TerminusPackageGraph, TieredPackageGraph |
| `Compile` | client-core | EmbeddedForge (Trusted only), RemoteCompile |
| `ForgeContext`, `Producer`, `DocumentSink`, render `Backend` | compiler-core | per GD-17 |
| `Cage` | sandbox | DevPassthrough, Bwrap |
| `CommitGate` | client / registry-local | §7 |
| `SemanticGate` | vector-local / client-core | quota capability |
| `Routed<L,R>` | client-core | dual-home router |
| `BlobTransport` / `BlobSink` | client-core / client | IrohBlobsTransport (production), InMemoryTransport (tests) |
| `Materializer` | blob | CasHardlink, LazyArchive, FullExtract |

New shared structures in heart: `Generation` (both domains as distinct newtypes, GD-40), `SymbolDelta`, `PackageStemId`.

---

## §2 Wire protocol & plane contracts

**Source:** `.research/librarification/21` + ir-vcs RPC mapping · **Planes:** all

### 2.1 Protocol layering (normative)

| Plane | Transport | Contents |
|---|---|---|
| **Control** | HTTP/JSON, base path `/v1`, `/v1/hello` negotiation | resolve, search, expand, sync planning, channel tips, jobs, admin |
| **Data** | iroh-blobs (QUIC; BLAKE3-verified streaming) | CAS objects by hash from the provider fleet; DownloadGrant-authorized |
| **Compiler** | postcard, `COMPILER_PROTOCOL_VERSION = 1` | CompileEnvelope; full sealed artifacts back |
| **ORCH** | HTTP/JSON `/v1` (`service: orch` in hello) | admit / status / poison; operators + INDEX only |

Internals never on the wire: SQLite schemas, Tantivy queries, Qdrant filters, WOQL, k8s Job specs, libpijul types. Desktops never hold store credentials; grants are scoped hash allowlists.

### 2.2 INDEX control-plane route table (v1)

| Group | Routes |
|---|---|
| Discovery/ops | `GET /v1/hello` · `GET /healthz` · `GET /readyz` · `GET /metrics` |
| Catalog | `POST /v1/resolve` · `GET /v1/packages/{id}` · `GET /v1/packages/{id}/generations/{latest\|stamp}` · `POST /v1/packages` · `POST /v1/packages/{id}/reindex` |
| Search/graph | `POST /v1/search` · `POST /v1/search/semantic` (ReadCap + SemanticGate) · `POST /v1/packages/search` · `POST /v1/expand` · `GET /v1/symbols/{symbol_id}` |
| Sync | `POST /v1/sync/plan` (want/have → missing hashes + providers + grant) · `POST /v1/cas/has` |
| IR channels (ir-vcs) | `GET /v1/channels/{lineage}/{channel}/tip` · `POST /v1/channels/{lineage}/{channel}/changes` (HaveChanges → GetChanges) · archive + change-contents wants resolve through `/v1/sync/plan` |
| Jobs | `POST /v1/jobs` · `GET /v1/jobs/{id}` · `POST /v1/jobs/{id}/cancel` |
| Admin | verify / rebuild / poison / unpoison under `/v1/admin/packages/{id}/…` (AdminCap) |

Body ceilings: read control 2 MiB; write control 256 KiB; sync plan 1 MiB (`have[]` ≤ 50k hashes; `HaveChanges` ≤ `MAX_HAVE_IDS = 10_000`, larger sets take the archive shortcut per ir-vcs K13). Source archives are never accepted on public INDEX JSON APIs.

### 2.3 Compiler protocol (postcard, `compiler-wire`)

```rust
pub const COMPILER_PROTOCOL_VERSION: u32 = 1;

pub enum CompileRequest {
    /// Dev/local convenience.
    Inline { coordinates: Coordinates, toolchain: ToolchainFingerprint, files: Vec<FileBytes> },
    /// Fleet: input already in object store; pod never receives bulk via ORCH.
    Cas {
        coordinates: Coordinates,
        toolchain: ToolchainFingerprint,
        input_hash: ContentBlake3,
        input_get: CasRef,
        output: OutputGrants,          // SectionPut[] + completion token
        deadline_unix_ms: Option<u64>,
    },
}

pub enum SectionRole { PackageArchive, Occurrences, References, SourcePack, ArchiveIndex, Manifest }

pub enum CompileResponse {
    Ok {
        protocol_version: u32,
        /// Sealed candidate arena: PackageArchive (NdIr) bytes or CAS ref in Cas mode.
        package_archive: ArtifactRef,
        occurrences: ArtifactRef,
        references: ArtifactRef,
        source_digests: WireSourceArchive,   // per-file path/hash/size
        moniker_materials: ArtifactRef,      // §5 emission: descriptor chains + part-hash inputs
        snapshot: ContentBlake3,             // GenerationStamp input
        uploaded: Vec<UploadedSection>,      // Cas mode: role + hash + size after PUT
    },
    Err { protocol_version: u32, failure_kind: FailureKindWire, phase: PhaseWire,
          retryable: bool, message: String },
}
```

Postcard is not protobuf: any layout change is a full version bump. Error kinds map onto `heart::FailureKind` (`sandbox_denied → Unsafe`, `timeout → Timeout` retryable, `oom → Transient once then Internal`, `input_fetch → Transient`).

**The daemon never records changes** (GD-39): it returns a sealed candidate arena; the channel writer (INDEX or client) materializes the previous pristine, runs the recorder (§5.6), applies, and seals the served PackageArchive.

### 2.4 Error body (control plane)

One `ErrorBody { code, message, failure_kind, phase, retryable, details? }` everywhere, projected from `heart::Failure`.

### 2.5 Sync plan (control) + grant (data authorization)

```json
// POST /v1/sync/plan — request
{
  "want_from": [{ "package_id": "…", "generation_stamp": "…" }],
  "have": ["<blake3 hex>", "…"],
  "limit": 512,
  "cursor": null,
  "client_endpoint_id": "<iroh EndpointId>"
}

// response
{
  "want": ["h1", "h2"],
  "manifests": [ /* GenerationManifestDto */ ],
  "providers": {
    "iroh": {
      "endpoints": [
        { "endpoint_id": "…", "relay_url": "https://relay-us-east.nudox.example",
          "region": "us-east-1", "priority": 10 }
      ]
    }
  },
  "grant": {
    "v": 1,
    "client_endpoint_id": "…",
    "hashes": ["h1", "h2"],
    "exp": "2026-07-16T12:05:00Z",
    "sig": "<ed25519 over canonical bytes>"
  },
  "next_cursor": null
}
```

Rules: the INDEX never proxies blob bytes; providers verify `grant` (EndpointId binding + hash allowlist + expiry) before streaming; integrity is BLAKE3 content-hash equality on every fetch, enforced by bao verified streaming *and* re-checked at CAS put.

`/v1/hello` returns `{ service, protocol: {major, minor}, features[] }`; unknown fields are ignored on decode; major mismatch = upgrade prompt.

---

## §3 Storage: CAS, manifests, packs, materialization

**Sources:** `.research/librarification/13`, `.research/edge-tech/07` · **Planes:** SHARED/INDEX/REGISTRY

### 3.1 Invariants

Content-addressed per-file storage is the single system of record for source, IR, and extracted references. A generation manifest whose entries are BLAKE3 hashes **is** a shallow Merkle DAG: the root commits the leaves; leaves fetch and verify independently. Derived stores (tantivy, vectors, ladybug, terminus) are disposable projections.

| Item | Decision |
|---|---|
| Tree-sitter Trees | Never persisted (GD-11); `OccurrenceSet`/`ResolvedReference` are the reference corpus; reparse on demand |
| IR sections | zstd with trained dictionaries; `dict_id` recorded in the envelope codec (raw \| zstd \| zstd+dict) |
| Source | plain zstd, per-file digests |
| Bulk container | `.ndpk` frame-per-file zstd pack + `.ndix` ArchiveIndex; composable with per-file CAS, never sole replica |
| Manifest | ir-vcs `BlobManifestV3` (GD-10) |
| Footprint | ~0.6–0.9 MB compressed per 50k-LOC generation without trees; multi-version retention ≈ 5× smaller than per-version archives |

### 3.2 Manifest (normative shape)

```rust
pub struct BlobManifestV3 {
    pub package: PackageId,                  // heart UUID; joined to PackageLineageId
    pub files: NonEmpty<FileEntry>,          // path + ContentBlake3 + size
    pub ir_package_ref: CasKey,              // PackageArchive (NdIr)
    pub change_set_ref: Option<ChangeSetRef>,// channel + tip fingerprint (ir-vcs)
    pub occurrences_ref: Option<CasKey>,
    pub references_ref: Option<CasKey>,
    /// Transport artifacts — re-encodings of files[]; NOT in identity_bytes (GD-10).
    pub archive_pack_ref: Option<CasKey>,    // .ndpk
    pub archive_index_ref: Option<CasKey>,   // .ndix
    pub toolchain: Toolchain,
}
```

`GenerationStamp` identity_bytes v3 is frozen in the ir-vcs design (files + ir_package_ref + change_set_ref + references/occurrences + toolchain; no sizes; domain `nudox.gen.v3`).

### 3.3 Layouts, GC

**REGISTRY:** `<data_dir>/registry.sqlite` + sharded `cas/xx/{blake3}` + `ptr/{package-uuid}` + `tantivy/` + `vectors/` + `ladybug/` + `packs/{gen}.ndpk|.ndix` + `views/{view_id}/` + pin/LRU GC (pin levels 0 evictable · 1 recent · 2 branch tip/HEAD · 3 user pin · 4 active session).
**INDEX:** S3 `cas/{blake3}` + `ptr/` + sqlite catalog; first-write-wins section puts; BLAKE3 verify on full reads; `heart::cache::{DiskCas, Tiered}` is the read path.
**GC:** reference-counted from live generations + channel logs + pins; mark-and-sweep with a dry-run mode; the invariant test — *GC never deletes a hash reachable from any sealed generation, channel log, or live view pin* — is release-blocking.

### 3.4 ArchiveIndex (`.ndix`) + pack (`.ndpk`)

External TOC beside the pack so members are range-fetchable without unpacking (SOCI-shaped, emitted at produce/seal time, never lazily on first open):

```rust
pub struct ArchiveIndex {
    pub format_version: u32,             // 1
    pub archive_hash: CasKey,            // the .ndpk this indexes
    pub members: Vec<ArchiveMember>,
}
pub struct ArchiveMember {
    pub path: String,                    // package-relative, normalized '/'
    pub offset: u64,                     // byte offset of the member's zstd frame
    pub compressed_len: u64,
    pub uncompressed_len: u64,
    pub content_hash: ContentBlake3,     // BLAKE3 of logical file bytes
}
```

`.ndpk` = frame-per-file zstd, so each member is one independent range GET. Consume path (`LazyArchive`): look up member → if DiskCas has `content_hash`, hardlink into view; else range-fetch the frame via `BlobTransport`, decompress, verify, `DiskCas.put`, hardlink.

### 3.5 Materializer

Materialization turns a sealed generation's manifest + CAS objects into a disposable **view** (compile tree, GUI open-package, export). Views are disposable; CAS is durable.

```rust
pub struct GenerationView {
    pub generation: GenerationStamp,
    pub root: PathBuf,
    pub mode: ViewMode,
}
pub enum ViewMode { CasHardlink, LazyArchive, FullExtract, VirtualOnly }

pub enum PathSet { All, Prefixes(Vec<PathBuf>), Exact(Vec<PathBuf>) }

pub trait Materializer: Send + Sync {
    fn ensure(&self, gen: &BlobManifestV3, view: &GenerationView, paths: PathSet,
              transport: &dyn BlobTransport) -> Result<EnsureReport, MaterializeError>;
    fn drop_view(&self, view: GenerationView) -> Result<(), MaterializeError>;
}

pub struct EnsureReport {
    pub files_touched: u64, pub files_in_package: u64,
    pub bytes_fetched: u64, pub hardlinks: u64,
    pub copies: u64,     // hardlink cross-device fallback — metric, alert on ratio
}
```

**Selection policy (locked):**

```
if local_present_ratio ≥ 0.95            → CasHardlink
if pack+index present:
    expected_touch ≥ 0.70                → FullExtract    # full-tree producers
    else                                 → LazyArchive    # GUI browse: touch 0.01–0.05
if local_present_ratio ≥ 0.50            → CasHardlink (+ per-file fetch for holes)
else                                     → FullExtract
GUI browse without any tree              → VirtualOnly (stream CAS into buffers)
```

**Extraction hardening (all view modes):** reject symlinks or rewrite them to contained targets only; prefer fd-relative opens with `O_NOFOLLOW`; fleet materialize runs under an RO-tree/no-network sandbox; and re-verify every file body against its manifest BLAKE3 **after** materialize, not only at download (the gitoxide RUSTSEC-2024-0349 path-traversal class is the precedent).

**Success metrics (binding):** time-to-first-source-file p95 ≤ 200 ms LAN / ≤ 800 ms WAN; GUI single-file bytes hydrated / package size ≤ 0.05; hardlink compile hit = 0 network bytes; hash-verify failure tolerance = 0.

---

# Part II — Identity & IR

## §4 The IR plane (arena IR + change log)

**Normative:** `.research/ir-vcs/design/IR-NATIVE-VCS-DESIGN.md` Rev 3.2 (K1–K28). This section states only the joins to the rest of the system. **Do not restate or fork IR types here.**

### 4.1 Shape (summary)

- `nudox-ir`: `Entry = Symbol + Node + EntryInner`, typed `EntryIdx<T: EntryKind>`, `register_kinds!`, `EntryBuilder` tree construction, undirected `EntryLink`, `Registry<R: RegistryResolver>` multi-package resolve with remote fill.
- `nudox-ir-archive`: sealed `PackageArchive` (`NdIr`) — POD TOC + StringTable + EntryHeads + payloads + IntroIndex + NameIndex (with aliases) + TreeCSR + LinkCSR + TypeSkeletonIndex; deterministic seal ⇒ identical `CasKey`; zero-copy yoke readers (IR-only, never source).
- `nudox-change` + `nudox-ir-channel`: universal `Change<A: Atom>` envelope; `IrAtom` keyed by `IntroId` (Insert/Update/Delete/Retarget/LinkAdd/LinkRemove/Reparent), normalized within-change atom order, linear **single-writer** apply that fails closed (`ApplyError::ConcurrentWrite`); durable log = libpijul channel behind `ChannelStore` (one opaque blob per ChangeId); semantic SoT = typed pristine materialize.
- `nudox-ir-sync`: `GetChannelTip` / `HaveChanges` / `GetChanges` / `GetArchive`; `RequiredClosure` defines client Ready.
- `nudox-ir-project`: `IrToGraph` projects `Change<IrAtom>` → `Change<GraphAtom>`; cold `PackageGraph` over Registry + LinkCSR.

### 4.2 What producers emit

Producers build the arena through `EntryBuilder` and additionally stamp, per entry, the **moniker materials** (§5): descriptor chain, kind, and part-hash inputs. `compiler-core` seals fingerprints at seal time (crate purity: `nudox-ir` never sees heart types; the stamping lives in compiler-core).

### 4.3 Publish pipeline (one path, both planes)

```
producer → EntryBuilder arena (+ moniker materials)
  → channel writer (INDEX for registry pkgs, client for first-party — GD-39):
      prev = ChannelStore::materialize(tip)
      atoms = recorder(prev.pristine, arena)        # §5.6 matcher inside
      change = Change::build(atoms)                 # normalized order, ChangeId sealed
      ChannelStore::apply(change)                   # linear, fail-closed
      archive = seal(pristine) → CAS put            # deterministic CasKey
      stamp = GenerationStamp(identity_bytes v3)
      outbox.append(stamp)                          # GenerationStamp ONLY, never CasKey
      outbox.enqueue_graph(IrToGraph.project(change))
      symbol_delta = fold(change)                   # §6 fan-out contract
```

### 4.4 Ready

A client is Ready for a generation iff every hash in `RequiredClosure(G)` (files + `ir_package_ref` + reachable change-contents blobs) is present in local CAS and BLAKE3-verified. Sync of change tails vs `GetArchive` is a transport choice; Ready is the same predicate either way.

---

## §5 Moniker & lineage (normative)

**Sources:** `.research/librarification/19` (RFC freeze), `05`, `06` · **Crate:** `moniker`
Frozen: `moniker_grammar_version = 1`, `normalizer_version = 1`, `matcher_version = 1.0.0`.

Twenty years of origin-analysis literature and every industrial system (SCIP, Kythe, Unison, git rename detection) converge on: identity is **layered, not binary**; false merges are worse than false splits (history is write-once read-many); moniker equality is evidence, not continuity. nudox implements all three continuity mechanisms — moniker stability by design, content-address sameness, explicit successor edges — in that order of preference.

### 5.1 Identity lattice

| ID | Versioned? | Role |
|---|---|---|
| `LineageMoniker` | No | Cross-generation matcher key (tier-0 coordinate) |
| `GenerationSymbol` (SCIP-shaped) | Yes | Within-generation index row, SCIP interop |
| `IntroId` (ir-vcs) | No | Structural IR-plane continuity — the persisted verdict (§5.6) |
| `GraphDocId` IRI | No | Versionless graph node key |
| `PackageId` (heart) | Yes | Package instance id |
| `PackageStemId` (heart) | No | origin + name; package-level join across versions |
| `SymbolId` | Yes | `UUIDv5(instance, "intro:" ‖ pkg ‖ intro)`; per-store join only |

**Frozen rule:** never silently rewrite monikers to preserve history. Continuity is edges + moniker equality + IntroId, never mutation of IDs.

### 5.2 Moniker grammar (frozen, SCIP descriptor alphabet)

```
<LineageMoniker>   ::= <scheme> " " <ecosystem> " " <package-name> " " <descriptor>+
<GenerationSymbol> ::= <scheme> " " <ecosystem> " " <package-name> " " <version> " " <descriptor>+

<scheme>     ::= "nudox-rust" | "nudox-ts" | "nudox-go" | "nudox-java"
               | "nudox-py" | "nudox-cs" | "nudox-nix"
<ecosystem>  ::= "cargo" | "npm" | "gomod" | "maven" | "pypi" | "nuget" | "nix" | "."
<descriptor> ::= <name>"/"   (namespace/Module)   | <name>"#"  (type)
               | <name>"."   (term)               | <name>"("<disambig>?")." (method)
               | "["<name>"]" (type-param)        | "("<name>")" (parameter)
               | <name>":"   (meta)               | <name>"!"  (macro)
```

Spaces escape as double-space per SCIP. Package-name canonicalization per ecosystem: cargo as published; npm full `@scope/pkg`; gomod module path; maven `groupId/artifactId`; pypi PEP 503; nuget as published; nix flake/attr path. Package rename is a package-level lineage event. **Kind hard filter:** matching never links across incompatible descriptor kinds except explicit T5 rules. Cross-language identity is out of scope for auto lineage.

### 5.3 Content hashes (frozen)

All BLAKE3-256 via `content-hash`, domain-separated with length-prefixed tags: `H(tag, parts…) = blake3(tag ‖ le_u64(len)‖part …)`.

| Field | Tag | Canonical input |
|---|---|---|
| `sig_hash` | `sig-v1` | normalizer_version, kind tag, visibility, unqualified name, normalized signature (params by position w/ canonical types, returns, generics+bounds, sorted attrs, receiver kind). Excludes docs, body, spans |
| `body_hash` | `body-v1` | normalized token stream: drop whitespace/pure comments (doc → doc_hash), identifiers as-is, canonical string escapes. Empty body well-defined |
| `doc_hash` | `doc-v1` | doc text: NFC, trailing trim per line, LF, ≥3 blank lines → 2 |
| `ref_hash` | `ref-v1` | sorted unique outbound (+ inbound when available, else `in_absent`) intra-package lineage-moniker strings; unresolved tagged `u:` |
| `embed_key` | — | embedding cache key (drives GD-28 re-embed skip) |
| `content_hash` | `sym-v1` | full early-cutoff key: normalizer_version ‖ lineage_moniker ‖ part hashes |
| `simhash64` | — | winnowing/SimHash of body tokens — LSH blocking only, never identity |

### 5.4 Matching cascade T0–T7 (frozen thresholds)

Pipeline order is mandatory; each tier removes matched endpoints from the pools (T4 labels and T5 multi-maps exempt):

```
T0 content short-circuit → T1 moniker exact → T2 same-parent rename
→ T3 move(+rename), LSH-blocked → T4 signature-evolution labels
→ T5 split/merge/extract/inline → T6 residual bipartite + embed assist → T7 birth/death
```

| Tier | Match rule | Confidence / auto-link |
|---|---|---|
| **T0** | kind = ∧ body_hash = ∧ sig_hash = (hash join); duplicate body_hash requires parent or name equal for auto | 1.00 unique · 0.99 with parent · 0.85 soft among duplicates; auto ≥ 0.99 |
| **T1** | lineage_moniker = ∧ kind = | 0.99 sig= · 0.97 sig-compatible · 0.95 body-changed; always auto for unique keys |
| **T2** | same parent, same kind, unmatched | unique sig_hash → 0.98 · body_sim ≥ 0.85 → 0.96 · body_sim ≥ 0.70 ∧ (name_JW ≥ 0.80 ∨ ref_jaccard ≥ 0.50) → 0.93 · body_sim ≥ 0.70 alone → 0.90 (auto only if unique best ∧ margin ≥ 0.05) |
| **T3** | cross-parent move: as T2 with `body_cross_parent_auto = 0.75`, simhash blocking (hamming ≤ 3), top-k 20, margin 0.05 | `Moved`/`MovedRenamed` |
| **T4** | annotates T1/T2 edges | `SignatureEvolved` |
| **T5** | split/merge via coverage `split_cover = 0.80` | `Split`/`Merged`/`ExtractedFrom`/`InlinedInto` |
| **T6** | residual bipartite (weights: body .35 name .20 sig .15 ref .15 path .10 doc .05); embed assist ≥ 0.80 | auto ≥ 0.92 (margin 0.08) → `Related`; 0.75–0.92 soft → `MaybeSame` |
| **T7** | residual unmatched | `Added`/`Removed` |

**Global gates:** `auto_lineage_min_conf = 0.93`; `embed_skip_min_conf = 0.95`; `intro_preserve_min_conf = 0.95` (§5.6); mass-delete guard: > 40% of a package's symbols disappearing demotes all auto edges to Provisional.

### 5.5 Edge model and storage

```rust
pub enum LineageKind { Identical, SameMoniker, SameMonikerSigChanged, SameMonikerBodyChanged,
    Renamed, Moved, MovedRenamed, SignatureEvolved, BodyEdited, Split, Merged,
    ExtractedFrom, InlinedInto, Related, MaybeSame, Reexport, Copy, Manual, Added, Removed }

pub struct LineageEdge {
    pub edge_id: ContentBlake3,            // H(canonical edge bytes) — CAS/dedup
    pub from: SymbolEndpoint, pub to: SymbolEndpoint,   // generation-qualified
    pub kind: LineageKind, pub tier: u8, pub confidence: f32,
    pub decision: Decision,                // Auto | Soft | Manual | Provisional
    pub features: FeatureVector,           // body/name/sig/ref/path/doc/embed sims + git_rename_score
    pub evidence: Vec<Evidence>,           // HashEquality, MonikerEqual, GitFileRename, AuthorPatch, …
    pub matcher_version: semver::Version,
    pub moniker_grammar_version: u32, pub normalizer_version: u32,
    pub created_at: Timestamp,
}
```

`SymbolEndpoint` carries stem + version + PackageId + instance id + IntroId + moniker + SCIP string + graph IRI, so every consumer joins on its own layer. Storage: SQLite `lineage_edges` on both planes + CAS blobs for evidence; Terminus receives Auto edges for hot packages only. Auto edges permit embed reuse (≥ 0.95); Soft edges surface as "maybe related" and never skip embeds or merge identities; Manual edges override everything and are never GC'd; Provisional edges await corroboration. Reexports are alias edges, not origin.

### 5.6 Recorder integration — the matcher writes the change log (GD-37)

The IR recorder computes atoms between `prev` pristine and the new arena. Its identity assignment **is** the T0–T3 cascade run over `(prev symbols, new arena symbols)`:

```rust
pub enum RecorderVerdict {
    /// Auto match, confidence ≥ intro_preserve_min_conf (0.95):
    /// same IntroId survives — emit Update (payload/moniker change) and/or Reparent.
    Continue { intro: IntroId, edge: LineageEdge },
    /// Below threshold or unmatched: false-split-friendly.
    /// New arena entry gets a fresh bootstrapped IntroId; the old one is Deleted.
    Fresh { deleted: Option<IntroId>, edge: Option<LineageEdge> },  // Soft edge for UI only
}
```

Consequences (normative):

1. A **rename** detected at T2 with confidence ≥ 0.95 emits `Update` under the same `IntroId` — K18 ("renames do not change IntroId") is *earned* by matcher certainty, never assumed.
2. A **move** (T3 ≥ 0.95) emits `Reparent` (+ `Update` if payload changed) under the same IntroId.
3. Everything weaker becomes `Delete` + `Insert` with fresh IntroId and, at 0.75–0.95, a **Soft** LineageEdge so the UI can still show "maybe the same" — but structural history stays split (false-split preferred; a later Manual edge can join lineages without rewriting the log).
4. T5 split/merge never merges IntroIds; it records multi-edges over distinct intros.
5. `LineageEdge` rows and `Change<IrAtom>` are written in the same seal transaction — the evidence and the verdict cannot drift.

---

# Part III — Engines

## §6 Incremental spine: stages, traces, deltas

**Source:** `.research/librarification/08` · **Planes:** REGISTRY + INDEX/ORCH

The pipeline spine is a hand-rolled constructive-trace store: SQLite + BLAKE3 digests, the same digest discipline as content-addressed build systems (GD-14).

### 6.1 Hash layers

```
sealed commit / registry publish
   └─► Change<IrAtom> applied on channel (§4.3)
         └─► SymbolDelta = fold(change) ──┬─► render  ─┬─► constructive traces
                                          ├─► embed    │    (sqlite + CAS)
                                          ├─► tantivy  ┤
                                          ├─► ladybug  ┤   (desktop)
                                          └─► terminus ┘   (hot only, GraphAtoms)
```

| Layer | Key | Purpose |
|---|---|---|
| L0 tree | git commit/tree OID | gate work (§7) |
| L1 file | blob OID / ContentBlake3 | skip unchanged files |
| L2 package IR | `GenerationStamp` | producer cache |
| **L3 symbol** | `(IntroId, content_hash)` | **primary early-cutoff for fan-out** |
| L4 stage | `H(stage_id ‖ tool ‖ L3)` | per-downstream memo |

Part hashes refine L3: embed depends on `embed_key`; graph edge rewrite depends on `ref_hash`; a docs-only edit re-embeds and re-indexes text but skips edge rewrites.

### 6.2 SymbolDelta (heart) — derived from the change log

```rust
pub struct SymbolDelta {
    pub package: PackageLineageId,
    pub from: Option<ChangeSetFingerprint>,
    pub to: ChangeSetFingerprint,
    pub added: Vec<IntroId>,
    pub removed: Vec<IntroId>,
    pub changed: Vec<SymbolChange>,          // unchanged implicit
}
pub struct SymbolChange {
    pub intro: IntroId,
    pub old_hash: ContentBlake3, pub new_hash: ContentBlake3,
    pub old_parts: SymbolPartHashes, pub new_parts: SymbolPartHashes,
}
```

Computed by **folding the applied `Change<IrAtom>`** (Insert → added, Delete → removed, Update → changed with before/after part hashes) — never by re-diffing symbol maps. Consumption: tantivy add/delete/replace; vectors embed/upsert/delete; ladybug node/edge upserts; terminus `GraphAtom` publish; render write/delete.

### 6.3 Trace store (SQLite, both planes)

```sql
CREATE TABLE symbol_heads (
  generation BLOB NOT NULL,          -- GenerationStamp
  intro_id BLOB NOT NULL,
  content_hash BLOB NOT NULL, part_sig BLOB, part_body BLOB, part_refs BLOB,
  ir_blob BLOB NOT NULL,             -- CasKey of PackageArchive
  PRIMARY KEY (generation, intro_id));
CREATE INDEX symbol_heads_by_hash ON symbol_heads(content_hash);

CREATE TABLE stage_traces (
  stage_id TEXT NOT NULL, input_digest BLOB NOT NULL, output_digest BLOB NOT NULL,
  output_blob BLOB NOT NULL, tool_digest BLOB NOT NULL, created_at INTEGER NOT NULL,
  PRIMARY KEY (stage_id, input_digest, tool_digest));   -- idempotent re-runs
```

**Seal protocol (atomic):** insert `running` → apply change + write CAS + staging rows → SymbolDelta → fan-out stages (idempotent via traces PK) → transaction: flip `sealed`, promote staging → symbol_heads. Crash mid-run leaves `running`; the next process reaps or restarts.

### 6.4 Stage trait

```rust
pub trait Stage: Send + Sync {
    type In: StageInput; type Out: StageOutput;
    fn stage_id(&self) -> &'static str;        // stable, versioned ('embed.v1')
    fn tool_digest(&self) -> ContentBlake3;    // model/config fingerprint — global invalidation lever
    fn input_digest(&self, input: &Self::In) -> ContentBlake3;   // pure, deterministic
    fn run(&self, input: &Self::In) -> Result<Self::Out, StageError>;
    fn output_digest(&self, out: &Self::Out) -> ContentBlake3;
}
pub fn run_stage<S: Stage>(traces: &dyn TraceStore, cas: &dyn Cas, stage: &S, input: &S::In)
    -> Result<S::Out, StageError>  // trace hit → CAS decode; miss → run, put, record
```

Desktop and fleet share the same trace shape: SQLite in the app data dir vs SQLite on the INDEX PVC; DiskCas vs S3 with the same keys — same symbol hash ⇒ one embed blob cluster-wide.

**Acceptance (the worked example):** editing one symbol and committing re-embeds exactly one symbol; the E2E test asserts stage-run counts.

---

## §7 Commit gate & project generations

**Source:** `.research/librarification/17` · **Plane:** REGISTRY (desktop), mirrored by INDEX publish

### 7.1 Identity (frozen)

```
project_generation_id = blake3("nudox.projgen.v1" ‖ project_id ‖ repo_id ‖ commit_oid ‖ encode(sorted submodule pins))
```

- `commit_oid` alone is insufficient (multi-root workspaces sharing a monorepo commit are different trust planes); `tree_oid` is recorded defensively (empty commits) but lineage follows the commit graph.
- Branch names are never part of the key; two branches at one commit share one sealed generation.
- Detached HEAD is allowed and indexed; unborn branch → no durable generation. Amend/rebase create new OIDs — sealed generations are never mutated; parent selection walks **sealed first-parent ancestors**, falling back to full rebuild.
- Worktrees share CAS by content with per-project active-HEAD pointers. Submodules default to pin-only.
- Ephemeral "Index now" previews live in a separate id space (`ephemeral_generations`), TTL-GC'd, and never enter durable publish paths.

### 7.2 Detection & dirty policy

`notify` watches on `GIT_DIR` (HEAD, refs/, packed-refs) + 5 s poll safety net; 300 ms debounce; longer quiet period during rebase sequences; `post-commit`/`post-rewrite` hooks are optional accelerators. Dirty trees never trigger embeddings or durable index updates; the GUI shows a dirty badge and serves the last sealed commit. After OID tree-diff (gix), a `CompositePackageMapper` (Cargo, npm/pnpm/yarn, Go, Python, Maven/Gradle, NuGet, Nix) over-approximates dirty packages; lockfile-only commits refresh the untrusted DepSet via the SyncEngine without re-producing first-party IR.

### 7.3 Schema (extends §6.3)

```sql
CREATE TABLE generations (
  id INTEGER PRIMARY KEY, generation_uid BLOB NOT NULL UNIQUE,   -- nudox.projgen.v1
  project_id BLOB NOT NULL, repo_id BLOB NOT NULL,
  commit_oid BLOB NOT NULL, tree_oid BLOB NOT NULL,
  parent_id INTEGER REFERENCES generations(id), parent_commit BLOB,
  branch_name TEXT, status TEXT NOT NULL,          -- running|sealed|failed|superseded
  kind TEXT NOT NULL DEFAULT 'durable',            -- durable|ephemeral
  full_rebuild INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, sealed_at INTEGER, stats_json TEXT);
CREATE UNIQUE INDEX generations_project_commit
  ON generations(project_id, commit_oid) WHERE kind='durable';

CREATE TABLE project_active ( project_id BLOB PRIMARY KEY,
  generation_id INTEGER REFERENCES generations(id),
  head_commit BLOB, dirty_flag INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL);

CREATE TABLE generation_submodules ( generation_id INTEGER NOT NULL, path TEXT NOT NULL,
  commit_oid BLOB NOT NULL, PRIMARY KEY (generation_id, path));
```

`parent_id` = the generation used for tree-diff/recording (nearest **sealed** ancestor). Crash recovery: `running` past TTL → failed (or restart if commit_oid == HEAD); idempotency via traces PK.

---

## §8 Graph: projection, Ladybug desktop engine, cold serving

**Sources:** `.research/librarification/20`, `.research/edge-tech/05` · **Crates:** nudox-ir-project, graph-local

### 8.1 One lowerer, three consumers

`nudox-ir-project` owns the single edge lowerer from PackageArchive links/tree to `(SymbolId, RelationKind, SymbolId)`:

```rust
pub enum RelationKind { Member, Reference, Occurrence, Implements, Extends, ReExport }

pub trait PackageGraph {
    fn outgoing_edges(&self, id: &SymbolId) -> Result<EdgeIter<'_>, GraphError>;
    fn expand(&self, seed: &SymbolId, via: &[RelationKind], depth: u8) -> Result<GraphSlice, GraphError>;
    fn are_related(&self, a: &SymbolId, b: &SymbolId) -> Result<bool, GraphError>;
    fn membership_diff(&self, other: &GenerationStamp) -> Result<MembershipDiff, GraphError>;
}
```

| Consumer | Feed | Plane |
|---|---|---|
| `ColdRegistryGraph` | on-demand projection over Registry + LinkCSR (bounded BFS; `DepLoadPolicy::StubsOnly`) | INDEX serving + universal correctness reference |
| `LadybugGraph` | `GraphProjectionStage` folds SymbolDelta/atoms into an embedded Ladybug DB | REGISTRY (desktop queries) |
| Terminus hot | `Change<GraphAtom>` from `IrToGraph` | INDEX (admitted packages, §9) |

Identity: graph node key is the versionless IRI; SymbolId is versioned — cross-version operations use IRI/moniker set-diff, never SymbolId equality. Stubs for external deps are first-class nodes; dep IR loading is policy-gated so expand cannot crawl the universe.

### 8.2 LadybugGraph (GD-34)

```rust
/// Embedded desktop graph engine. A disposable projection (GD-14):
/// schema_version bump or corruption ⇒ drop directory, re-project from IR.
pub struct LadybugGraph {
    db: ladybug::Database,                  // <data_dir>/ladybug/
    schema_version: u32,                    // marker table; mismatch ⇒ rebuild
}

/// Stage: input = SymbolDelta + archive view; output = ladybug upserts.
pub struct GraphProjectionStage;
// node tables: Symbol(symbol_id, iri, intro, moniker, kind, generation)
// rel  tables: one per RelationKind, endpoints FK Symbol
```

Queries the GUI needs (expand, references panel, membership trees, path-between) run as Cypher against Ladybug with the same `PackageGraph` contract; a parity suite asserts LadybugGraph ≡ ColdRegistryGraph on fixtures — parity is release-blocking, keeping the projection the semantic SoT.

**Caching (cold path):** StampedeCache/moka keyed `(PackageId, ir_ref, occ_ref, instance_token)`; byte budgets ≈ 256 MB desktop / 2–4 GB server; single-flight builds. Cold queries feed the §9 scorer from day one.

---

## §9 Terminus hot tier & admission

**Source:** `.research/librarification/07` · **Plane:** INDEX · **Crate:** terminus-client

TerminusDB v12 (DFRNT-stewarded) is the only production document-graph store with immutable versioning + semantic JSON diff/patch — matching lineage needs. It lives strictly behind HTTP (`terminus-client`, vendored `terminusdb_schema` types); write cost ≈ O(Δ triples) per commit fits incremental symbol publish; rollups are scheduled on hot packages to bound read-side layer depth.

- **Admission:** leaky bucket per package (`PackageTierManager`): cold-path graph queries drip tokens; sustained demand crosses the promotion threshold; a count-min-sketch doorkeeper filters one-hit wonders. Promotion never happens under one-shot load (acceptance-tested).
- **Promotion:** materialize generation documents + lowerer edges (Relation parity with §8) + Auto lineage edges into Terminus; flip `StoreLinks.graph`.
- **Incremental publish:** the outbox drains `Change<GraphAtom>` projections (§4.3) — only changed documents write per generation.
- **Demotion:** pin the cold projection → verify cold serves → clear bit → delete branch per retention. Demotion is loss-free by construction (cold ≡ hot on the parity suite).
- `TieredPackageGraph` (hot if admitted + healthy, else cold) lives in index-service.

---

## §10 Text search: Tantivy multi-language

**Sources:** `.research/librarification/10`, `10b` · **Planes:** INDEX + REGISTRY · **Crate:** text-search

- **One shared schema** for all languages (tantivy 0.26.1 pinned): identifier subword core (`IdentTokenizer`), signature terms, path-prefix facet `/lang/{eco}`, prefix-ngram autocomplete field, popularity/quality fast fields, `ecosystem` STRING filter. A schema-version marker beside the index forces rebuild on mismatch; tokenizer chains are registered identically on every open (registration is not on disk).
- **`LanguageAnalyzer` trait:** path separators (`::` vs `.` vs `/`), signature term extraction, query rewrite, kind priors and boost tables per language, over the shared subword core.
- **Symbol ranking:** tiered query (exact STRING → regex contains → subtoken AND), fused as multi-field BM25 + exact-match bonus + kind prior. Package download-bubbles never apply to symbol queries.
- **Package discovery:** `EcosystemAdapter` (associated consts + `Lexicon`) + `PackageSignals` + free `compute_quality` (no `dyn`) + one `retrieve_and_rank` shared by local replica and remote. Intent NAVIGATE vs EXPLORE; percentile popularity per ecosystem (never one global download floor). Locality is a desktop `LocalEnrichment` sidecar (used-before / in-project soft boosts + path/fork package injection — never uploaded, never a Tantivy scope).
- **Incremental:** a SymbolDelta-driven Stage upserts/deletes by IntroId-stable doc ids (GD-28).
- DepSet scoping: `TextSearch::search(q, page, dep: &DepSet)` filters to generation-pinned membership on both planes.

Adopted 0.24–0.26 features: CompactDoc (desktop memory), lazy scorers (as-you-type latency), string fast-field TopDocs ordering, `minimum_number_should_match`, NgramTokenizer `prefix_only` autocomplete, PhrasePrefixQuery, feature-gated stemmer, configurable merge threads.

*Acceptance:* relevance regression suite per language; identical results local vs remote for identical DepSets; NDCG goldens for navigate/explore/typosquat.

---

## §11 Vector search & local embeddings

**Source:** `.research/librarification/09`, `09b` · **Crates:** vector-local, vector-remote

- **Embedded engine: qdrant-edge** (in-process, disk-resident, same filter/payload model as remote Qdrant via qdrant-client). One vector stack, both planes, one model: `jinaai/jina-embeddings-v2-base-code` (768-d, Apache-2.0, 8K context) via fastembed + ort (CPU default; Metal/CoreML EP later). Same model + dimension + metric local and remote — single-index identity; model upgrades ship as versioned collections.
- **Layout:** one Edge shard per workspace project; payload fields `language`, `package`, `kind`, `symbol_id` participate in ANN filtering (not post-filter). Scalar/binary quantization under a **500 MB RAM ceiling**; WAL durability (desktops force-quit).
- **`EmbedStage`:** input digest = `embed_key` (§5.3); `tool_digest` = model+revision+dims fingerprint — bumping the model invalidates globally *by design*. Re-embed skip across lineage edges only at confidence ≥ 0.95 (GD-7).
- **`SemanticGate`** capability gates local CPU quota and remote premium collections; `Routed` prefers remote until the local corpus is Ready.

*Acceptance:* local/remote result-parity harness on a fixture corpus; RAM ceiling test at 10⁶ vectors; kill-9 durability test.

---

# Part IV — Planes & distribution

## §12 Compiler dual-form

**Source:** `.research/librarification/01` · **Crates:** compiler-core, compiler-wire, compiler-daemon, producer-worker, sandbox

### 12.1 Design

```rust
/// Desktop trusted embed — assembled once by client at project open.
pub struct TrustedForgeContext {
    cas: Tiered<DiskCas>,              // shared with REGISTRY
    cage: DevPassthrough,              // trusted: no seal; workers optional
    toolchains: ToolchainSet,          // discovered, injected — the library never reads env
    oracles: OracleSet,                // explicit paths — no self-exe resource lookup
    observer: Arc<dyn ForgeObserver>,  // job progress → GUI
}
```

- Vocabulary: `ForgeContext`, `generate_with(ctx, input) → GeneratedPackage`, `SealedInput`, `JobKey`, `ThreatTier`, typestate seal (`Job::acquiring.seal(tier)` type-enforces network off).
- **Library form:** pure — no env reads, no daemon module; binaries own env/policy assembly. CI lint: `std::env` forbidden in lib targets.
- **Fleet form:** every producer is sealed — `ExecPlan::Commands` through the Cage, or worker-isolated (`producer-worker`) for in-process oracles (rust-analyzer). There is no in-process fallback on Hostile tier; a producer that cannot yet run sealed does not run on the fleet.
- **Producers emit** `EntryBuilder` arenas + moniker materials (§4.2); render (`render/`) is pure and ships in the embedded library for local previews.
- **Recording is not the compiler's job** (GD-39): daemons return sealed candidate artifacts; channel writers record/apply/seal.
- Acquisition (gix clone/fetch) lives in client (trusted) and INDEX materialize (untrusted) — never in sealed compile paths.
- Sandbox layering on the fleet: gVisor RuntimeClass + `hostUsers: false` + in-pod bwrap/landlock (§17). Desktop trusted path uses `DevPassthrough` by design.

*Acceptance:* compiler-core builds `--no-default-features` with no env access; fixture parity embedded vs daemon (same JobKey ⇒ same artifacts, byte-identical archives by deterministic seal).

---

## §13 Desktop toolchains & packaging

**Source:** `.research/librarification/18` · **Plane:** GUI/REGISTRY

| Concern | Decision |
|---|---|
| Packaging | Hybrid: Class-B oracles (small binaries/jars) in-app or as language packs; system-discovered Class-C SDKs (Go/JDK/.NET); optional Nix channel; download-on-demand later |
| Size budget | Base GUI ≤ ~150 MiB compressed via `lang-*` features |
| MVP local languages | TypeScript (OXC), Python (pyrefly) — pure-Rust, ideal offline; Rust (ra_ap, if rustup present, feature-gated, worker-isolated); Go (if go present) |
| Remote-only | Java, C#, Nix — untrusted packages always compile on the fleet anyway (GD-16); snix is GPL-3.0 and never links into the GUI |
| JobKey toolchains | Semantic blake3 **content fingerprints** (oracle + compiler versions) under `nudox-producer/1` from day one — desktop and fleet CAS align without any epoch migration |
| Workers | Oracle subprocesses kept; default-on worker pools for Hostile pure-Rust producers |
| Platforms | darwin/linux, aarch64/x86_64 |

*Acceptance:* fresh macOS machine with rustup+node gets local TS/Python/Rust compile with zero manual config; every `lang-*` feature combination builds in CI.

---

## §14 Trust & project model

**Source:** `.research/librarification/16` · **Planes:** client-core/client · GUI

### 14.1 Normative algorithm

1. **Detect** project graphs under user-opened roots (manifest/workspace/lock detection per ecosystem).
2. **Build the Jail:** realpath prefix set of user roots ∪ explicit promotions; canonicalize **before** membership checks (symlink escapes rejected).
3. **BFS path-like dependencies** inside the jail → `TrustedSourceSet` (Cargo `{path}`, npm `file:/link:/workspace:`, Go `replace => ./dir`, Gradle `project(":x")`, NuGet `ProjectReference`, Nix `path:` inputs).
4. **Lockfile-parse registry/git pins** (after `[patch]`/overrides) → `UntrustedCoordinateSet` of `PinnedCoordinate`s.
5. **Never local-compile outside the jail.** Git/vendor trees untrusted by default; `node_modules` refused as a root; Cargo/NuGet config source redirects ignored by default (diagnostic emitted).

Lockfiles are mandatory for generation-pinned DepSet sync; missing locks degrade to soft resolve with a quality diagnostic. Prefer native resolvers (`cargo metadata`, `go list`) when toolchains exist; pure parsers otherwise (diagnostic on divergence). Path members and registry packages with colliding names use disjoint identity.

### 14.2 Types

```rust
pub struct Project<S>(…);                             // S ∈ {Unresolved, Resolved, Indexed}
pub struct SourcePath<T>(PathBuf, PhantomData<T>);    // T ∈ {Trusted, Untrusted}
pub struct Jail { roots: BTreeSet<PathBuf> }          // realpath prefixes
pub struct TrustedSourceSet(…);
pub struct UntrustedCoordinateSet(Vec<PinnedCoordinate>);
// Compile exists only on SourcePath<Trusted> (GD-16)
```

A Workspace-Trust gate precedes any toolchain execution on a newly opened root. Frozen diagnostic codes: `trust::path_outside_jail`, `trust::symlink_escape`, `trust::vendor_ignored`, `trust::git_unindexed`, `resolve::missing_lock`, `resolve::multi_lock`, `resolve::config_source_replace`, `resolve::native_tool_failed`, `identity::shadow_local_registry`.

*Acceptance:* the T1–T10 matrix (virtual workspace, outside path dep, multi-root promotion, `[patch]`, symlink escape, npm workspaces, go.work, missing lock, name shadowing) is release-blocking.

---

## §15 Client library & sync engine

**Source:** `.research/librarification/14`, `.research/edge-tech/02` · **Crates:** client-core, client

### 15.1 Stance

The model is **content-addressed substitution** (want/have hash-set difference), not bidirectional replication. Typestates for monotonic concerns (connection readiness, project resolve, trust plane); per-generation sync status is runtime state + `SyncedWitness` proofs. Every query routes through policy over a generation-pinned DepSet; generations become visible atomically after full hash-verified commit — mid-sync never mixes G_old and G_new.

### 15.2 Trait family and router

```rust
#[async_trait] pub trait Catalog      { async fn lookup(&self, id: PackageId) -> …; }
#[async_trait] pub trait TextSearch   { async fn search(&self, q: &TextQuery, page: &Pagination, dep: &DepSet) -> …; }
#[async_trait] pub trait VectorSearch { async fn search(&self, q: &VectorQuery, page: &Pagination, dep: &DepSet, gate: SemanticGate) -> …; }
#[async_trait] pub trait GraphOps     { async fn expand(&self, id: SymbolId, dep: &DepSet) -> …; }
#[async_trait] pub trait Compile      { async fn compile(&self, src: &SourcePath<Trusted>) -> Result<BlobManifestV3, _>; }

pub struct Routed<L, R> { local: L, remote: R, sync: Arc<SyncEngine>,
                          status: Arc<GenerationStatusMap>, flights: SingleFlight<FlightKey> }
```

`Routed::search`: `sync.ensure_background(dep)`; if `status.all_ready(dep)` → local; else single-flight remote (waiters re-check local on wake). Static generics over enum dispatch; erase to `Arc<dyn …>` only at the GUI boundary.

| Component | Local | Remote | Policy |
|---|---|---|---|
| Catalog | SqliteCatalog | HttpCatalog | Ready→local else remote+ensure |
| TextSearch | LocalTantivy | HttpSearch | all deps Ready → local |
| VectorSearch | LocalVectors | HttpSemantic | prefer remote until local corpus Ready; always SemanticGate |
| GraphOps | LadybugGraph | TieredPackageGraph via INDEX | Ready → local |
| Compile | EmbeddedForge | RemoteCompile | Trusted always local; Untrusted never local |
| Blobs | DiskCas Tiered | iroh provider fleet | SyncEngine is the warmer |

Strict local-only APIs take `&[SyncedWitness]` and no `NetworkCapability` — compile-time proof of no network. Offline mode: NetworkCapability absent, Ready subset only, staleness surfaced.

### 15.3 BlobTransport (GD-30)

```rust
#[async_trait]
pub trait BlobTransport: Send + Sync {
    /// Fetch each hash; stream verified logical bytes into `sink`.
    /// MUST reject incomplete/corrupt data before sink.finalize.
    async fn fetch(&self, hashes: &[ContentBlake3], ctx: &FetchCtx, sink: &dyn BlobSink)
        -> Result<FetchReport, FetchError>;

    /// Verified sub-blob range — LazyArchive members inside a pack (§3.4).
    /// Distinct from fetch-of-hashes: bao proves the range against the pack's root hash.
    async fn fetch_range(&self, hash: ContentBlake3, range: Range<u64>, ctx: &FetchCtx)
        -> Result<Bytes, FetchError>;

    fn cancel(&self) {}
}

pub trait BlobSink: Send + Sync {
    fn begin(&self, hash: ContentBlake3, size_hint: Option<u64>)
        -> Result<Box<dyn BlobWrite>, SinkError>;
}
pub trait BlobWrite: Send {
    fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), SinkError>;
    fn finalize(self: Box<Self>, expected: ContentBlake3) -> Result<(), SinkError>;
}

pub struct FetchCtx {
    pub grant: DownloadGrant,          // INDEX-signed; EndpointId-bound
    pub providers: ProviderSet,        // from SyncPlanResponse
    pub progress: ProgressTx,
    pub cancel: CancellationToken,
}
pub struct FetchReport {
    pub completed: Vec<ContentBlake3>,
    pub failed: Vec<(ContentBlake3, FetchError)>,
    pub bytes_by_path: BytesByPath,    // direct | relay — observability of NAT traversal
}
```

Production impl: **`IrohBlobsTransport`** — `ContentHash ↔ iroh Hash` are byte-identical BLAKE3; bao verified streaming means corruption is rejected mid-stream, then the CAS put re-verifies. Provider selection: LAN/site peer → regional `nudox-iroh-provider` by priority → relay path (iroh-internal; relays are self-hosted per region and carry restrictive-network traversal over HTTPS — there is no separate HTTP blob path in our code). `InMemoryTransport` exists for tests. `fetch_range` serves `LazyArchive` members against the pack object.

**v1 engineering requirements (from edge 02 — promotion makes these mandatory, not spike criteria):**

- **Version pin:** iroh core is stable; `iroh-blobs` is younger — pin one line at Wave-3 start (conservative stable vs current canary) and record it; the pin decides which fetch APIs the transport may use.
- **Hash-parity CI:** byte-identity of `ContentBlake3` ↔ iroh `Hash` asserted on empty/1B/1KiB/1MiB/16MiB−1 fixtures + a golden vector. Providers serve **raw logical bytes** — compression envelopes stripped before hashing; JobKey's length-prefixed hashing is never confused with content keys.
- **Outboards:** generate `cas/{hash}.outboard` at ingest for blobs ≥ 1 MiB; lazy-compute below.
- **Provider registry:** INDEX table `iroh_providers(endpoint_id, region, relay_url, weight, healthy)` + heartbeats; each replica holds a persistent SecretKey for a stable EndpointId.
- **Desktop Endpoint lifecycle:** on-demand — created when SyncEngine needs it, dropped after 60–120 s idle (an always-on Endpoint holds a home-relay socket: battery/network drain); SecretKey persisted in the OS keychain; iroh runs on a dedicated Tokio runtime inside `client` with a channel bridge — never on the GPUI thread.

Rules: the transport never flips GenerationStatus (SyncEngine commits after the full closure is in DiskCas); SingleFlight coalesces concurrent fetches of one hash and **cancels pending wants for hashes that land locally first** (cancel-on-local-hit lives above the transport); re-fetch of a present hash is a no-op.

### 15.4 SyncEngine loop

```
ensure(DepSet):
  for each generation not Ready:
    plan  = POST /v1/sync/plan (want/have, client_endpoint_id)
    tail? = HaveChanges/GetChanges when a prior generation is Ready     # ir-vcs delta path
    transport.fetch(plan.want, FetchCtx{grant, providers}, DiskCasSink)
    verify RequiredClosure(G) complete + BLAKE3-verified                # §4.4
    atomic generation commit → GenerationStatus Ready → SyncedWitness
    Stages rebuild derived indexes (§6): tantivy, vectors, ladybug
    Materializer.ensure for open views (§3.5) on demand
```

Views are demand-driven: generation Ready closes the CAS; GUI open-package materializes `PathSet::Prefixes` or goes VirtualOnly; trusted compile needing a dep tree gets CasHardlink/LazyArchive under job scratch.

```rust
pub struct ClientRuntime {
    pub sync: Arc<SyncEngine>,
    pub transport: Arc<dyn BlobTransport>,
    pub materializer: Arc<dyn Materializer>,
    pub cas: Tiered<DiskCas>,
}
```

*Acceptance:* the core scenario as an integration test — query served remotely during pull, flips to local after atomic commit, offline serves Ready subset; mixed-generation reads impossible (generation stamp asserted on every result page); provider-kill mid-fetch retries onto another provider/relay and still Ready-commits.

---

## §16 INDEX: the remote service plane

**Sources:** `.research/librarification/02`, `11` · **Crates:** meta-store, index-service, ingest, blob

### 16.1 Ops architecture (GD-2)

- **One writer process**, WAL mode, k8s StatefulSet `replicas: 1` + RWO PVC. Writer = sqlx pool `max_connections(1)` with `BEGIN IMMEDIATE` batching; readers = pool of N; `busy_timeout`; scheduled checkpoints.
- **Durability:** Litestream sidecar streams WAL → S3 (RPO ≈ 1 s, PITR); init container restores on boot. Disaster recovery is a rehearsed, CI-scheduled drill (< 5 min restore), not a hope.
- **Throughput reality:** INDEX writes are bursty metadata + outbox appends — far below batched-WAL ceilings (10k–100k rows/s on NVMe). Reads dominate and are cacheable (moka).
- **Stack:** sqlx (bundled sqlite) on INDEX; rusqlite (bundled, dedicated thread) on REGISTRY; shared DDL/Idens in meta-store — dual stacks are intentional (async service vs synchronous desktop).

### 16.2 Composition

| Concern | Home |
|---|---|
| packages / parse_status / symbols / lineage_edges | INDEX sqlite (TEXT/BLOB + JSON), sea-query DDL |
| Channel writer (registry packages) | `LibpijulChannelStore` on the PVC (GD-39); apply lock per lineage/channel |
| jobs | ORCH/Kueue (§17); INDEX keeps metadata rows for observability (GD-24) |
| outbox + sink_watermarks | sqlite append + single-writer poller; sinks = tantivy, qdrant, terminus (GraphAtoms), render |
| sessions | sqlite table, single replica — no merge machinery |
| CAS | S3 `cas/{blake3}` via object_store + heart `Tiered` |
| Ingest | `ingest` crate: path jail, allowlist, budgets — the security boundary for untrusted archives |
| HTTP | axum `/v1` (§2.2), real Principal + service auth, SearchPlanner |
| Blob data plane | `nudox-iroh-provider` fleet + DownloadGrant signing (key in KMS; rotation by `grant.v`) |

*Acceptance:* full INDEX integration suite green; restore drill < 5 min; the workspace contains no non-sqlite SQL driver.

---

## §17 ORCH: Kubernetes orchestration + fleet cache

**Sources:** `.research/librarification/12`, `.research/edge-tech/06` · **Crate:** orch

### 17.1 Two execution paths (GD-19)

- **Warm path:** long-lived `compiler-daemon` Deployment for interactive work. Direct HTTP with 503 backpressure; HPA/KEDA scaling. No queue in front.
- **Cold path:** bursty bulk builds as k8s Jobs admitted by **Kueue** — ClusterQueues, ResourceFlavors (spot vs on-demand), fair sharing, preemption, WorkloadPriorityClass; Karpenter NodePools provision (spot for cold).
- **ORCH server** (axum + kube-rs): admits work, dedups by `JobKey` (UNIQUE + in-flight coalescing), issues S3 I/O credentials to pods (in-cluster IAM — pods are not iroh clients), selects path, creates Jobs, reconciles outcomes into INDEX. It does not store blobs or implement a DAG engine.
- **Poison/retry:** Job `backoffLimit` + `podFailurePolicy`: permanent exit codes → FailJob; disruption → Ignore; terminal poison recorded in INDEX metadata. Idempotency is content-addressed end-to-end.
- **Sandbox:** gVisor RuntimeClass + `hostUsers: false` + in-pod bwrap/landlock. Resource classes S/M/L/XL → flavors; artifacts flow S3 multipart/zstd, never through ORCH.

### 17.2 Fleet cache L0–L5 (GD-31)

```
│ L0  ORCH/INDEX output CAS     JobKey → sealed artifacts (archive/occ/refs)  │
│ L1  Stage CAS                 JobKey stage → postcard, via heart Tiered      │
│     └─ L3 tier = ObjectStoreCas("cas/stage/") — real from day one           │
│ L2  Language build caches     FORBIDDEN across untrusted tenants            │
│ L3  Toolchain image layers    nix2container / OCI layers, pinned digests    │
│ L4  In-process warm state     RA RootDatabase, worker pools — pod-local     │
│ L5  Source materialization    node-local DiskCas + Materializer hardlinks   │
```

```rust
pub enum CacheScope {
    GlobalStage,   // fleet-shared JobKey stages + L0 (default server)
    NodeLocal,     // node DiskCas only; no remote stage put/get
    Forbidden,     // disable stage cache (forensics / correctness A/B)
}

pub struct SharedCasConfig {
    pub scope: CacheScope,
    pub l1_capacity: u64,
    pub l2_root: Option<PathBuf>,
    pub l3: Option<RemoteCasConfig>,
    pub write_l3: bool,
    pub verify_sample_rate: f32,
    pub stage_epoch: String,        // poison rotation "e0", "e1", …
}
```

Lookup order: L0 outputs complete? → return (superset skip, L1 never consulted) → admit warm|cold → inside pod: L5 materialize → L1 stage hits → produce → upload → L0 record. Cross-node L1 hits work from day one because toolchain fingerprints are content-based (GD-18).

**L2 policy (normative):** shared RW `target/`/Gradle home on hostPath is forbidden; per-job emptyDir is the default; RO content-addressed dep blobs ride as L5/image. **Security:** L3 stage CAS is written by the compiler service account only; sealed package code has no raw IAM; decode failure quarantines the prefix or bumps `stage_epoch` — never mutate L3 in place.

*Acceptance:* second pod with the same JobKey hits L1-remote after first success; Forbidden scope disables puts; no shared `target/` mounts pass template lint; 10k-package backfill on spot with preemption never retry-loops poison.

---

# Part V — GUI

## §18 lindsey: product views

**Sources:** `.research/librarification/03`, `15` · **Crate:** lindsey (path `workspace/gui`)

### 18.1 Architecture

Stores-over-client (GD-22): `ProjectStore`, `SymbolStore`, `SearchStore`, `JobStore`, `RegistryStore`, `NavHistory` are GPUI entities subscribing to `client`; views subscribe to stores; Workspace is a layout shell with no I/O. Debounced multi-source search with generation tokens. Widgets: gpui-component Dock, virtualized List/Table for every long list, Tree, Modal, Toast, Editor w/ tree-sitter, form controls; Zed's Workspace/Pane/Item/Action patterns imitated without vendoring GPL code. Custom builds: lineage timeline, signature/docs diff, type-query builder, graph canvas, trust chrome.

Reference synthesis: DevDocs/Dash — fuzzy keyboard-first multi-library search is *the* primary surface; Swift-DocC — symbol pages consume a **versioned RenderModel**, never ad-hoc kind rendering; rustdoc/docs.rs — clickable signatures, module trees, version bars; Hoogle — the UX template for type-directed search (nudox owns this cross-language via typed IR); Sourcegraph — references panels, outline sidebars; JetBrains — two-tier docs (quick peek vs full page).

### 18.2 View roadmap

| Phase | Views |
|---|---|
| **P0** | Project manager with trust visualization (badge every hit with provenance); cmd-K omni-search (<10 ms local); Symbol page v1 (RenderModel); Jobs/logs panel; Settings (server_url, registry paths, SDK discovery §13); command palette; back/forward nav history |
| **P1** | Package browser; sync surfacing (GenerationStatus); **lineage timeline** (§5 edges — real client APIs, never a mock); references panel; **type-directed search mode** |
| **P2** | Version diffs (MembershipDiff + change-log fold); richer graph canvas (Ladybug-powered); quick-doc peek |
| **P3** | Annotations, pins, playgrounds |

*Acceptance:* P0 demo on a cold machine — open project, trust-gate it, watch deps sync, search locally offline, open symbol page.

---

# Part VI — Program

## §19 Build order (Waves 0–5)

Greenfield construction order. Steps reference the ir-vcs PR plan (PR-0..PR-9) where the IR plane is the deliverable. Each wave has an exit gate; lanes within a wave are parallel workstreams; no wave starts exit-gated work before the previous gate is green.

### Wave 0 — Foundations

| Lane | Deliverables |
|---|---|
| Leaf + laws | `content-hash` crate (domain-tag hashing, Appendix A) · `nudox-change` (**PR-0**: unsealed Atom, Change, laws, DomainKey; GenerationStamp/CasKey/ChangeId newtypes; outbox type-gate) |
| Shared vocab | heart: Generation newtypes (GD-40), SymbolDelta, PackageStemId, Stage module, Cas/Tiered |
| Skeletons | compiler-wire, meta-store, blob, ingest crate shells + workspace-graph CI lint (forbidden edges live from day one) |

**Exit:** toy-atom inverse/commute laws green; type system rejects CasKey where a stamp is required; layering lint enforced.

### Wave 1 — IR plane + identity vocabulary

| Lane | Deliverables |
|---|---|
| IR core | `nudox-ir` (**PR-1**: Entry/Node/EntryIdx/Registry/EntryBuilder/links; Module/Record/Field/Function/Type kinds; IrAtom + order normalizer; linear apply table) |
| Archive | `nudox-ir-archive` (**PR-2**: NdIr layout, indices, type-skeleton goldens, deterministic seal) · manifest + GenerationStamp (**PR-3**) |
| Moniker | `moniker` crate: grammar, parser/printer, per-language descriptor extractors; normalizers (sig/body/doc/ref) in compiler-core seal path; determinism tests |
| First producers | TS (OXC) + Python (pyrefly) emit EntryBuilder arenas + moniker materials |

**Exit:** fixture packages seal deterministic PackageArchives (identical CasKey on double-run) carrying monikers + part hashes; mmap name lookup within budget.

### Wave 2 — Change plane, spine, gate, trust

| Lane | Deliverables |
|---|---|
| Channels | InMemory ChannelStore (**PR-4**: single-writer lock, fail-closed errors, unrecord, property tests) · LibpijulChannelStore (**PR-5**: opaque per-ChangeId blobs; materialize equality vs memory) |
| Recorder | §5.6 recorder with T0–T3 matcher; LineageEdge writes in the seal transaction; mass-delete guard |
| Spine | TraceStore tables · sinks-as-Stages · symbol_heads at seal · SymbolDelta from change fold · one-symbol E2E test |
| Gate + trust | CommitGate watcher + generations schema + CompositePackageMapper + ephemeral previews · Jail/BFS/lockfile parsers/typestates + T1–T10 matrix |

**Exit:** editing one symbol and committing re-embeds/re-indexes exactly that symbol; concurrent channel apply fails closed; dirty trees provably trigger nothing; trust matrix green.

### Wave 3 — Remote planes

| Lane | Deliverables |
|---|---|
| INDEX | meta-store sqlite impls · outbox single-writer poller · Litestream + DR drill · index-service `/v1` assembly · channel writer hosting · sessions |
| IR sync | Resolvers + `ensure_package` (**PR-6**) · Have/Want + RequiredClosure Ready (**PR-7**) · `/v1/sync/plan` + grants |
| Data plane | **Pre-flight (gates this lane):** iroh-blobs version pin + hash-parity CI suite · then `nudox-iroh-provider` fleet + relays + DownloadGrant signing/verification |
| ORCH | admission API + JobKey dedup · warm path · Kueue + Job templates (gVisor/userns/podFailurePolicy) · reconciler + poison · fleet L0/L1 (`ObjectStoreCas`) · scale/chaos test |
| Compiler fleet | daemon wire (full artifacts) · sealed producers (Go ExecPlan; Rust/Java/C# worker-isolated or fleet-gated) · Cas transport mode |
| Cold graph | `nudox-ir-project` cold PackageGraph (**PR-8**) serving all INDEX graph reads; query stats → scorer |

**Exit:** two-node sync reaches Ready with full closure verification over iroh; fleet compile E2E (submit → sealed pod → L0 record → INDEX rows); graph reads served cold; restore drill < 5 min.

### Wave 4 — Desktop

| Lane | Deliverables |
|---|---|
| Client | client-core traits/DepSet/Routed/typestates · HTTP backends · SyncEngine (plan/fetch/verify/commit) · IrohBlobsTransport · SyncedWitness · progress channels |
| REGISTRY | disk layout + pin/GC · rusqlite catalog · first-party channel store · view GC |
| Graph local | **Pre-flight (gates this lane):** Ladybug verification spike — Rust FFI binding quality, dependency license audit, single-writer locking vs GPUI concurrency, RSS @ 10⁴ packages · then GraphProjectionStage → LadybugGraph · parity suite vs ColdRegistryGraph |
| Text | shared schema + LanguageAnalyzers (Rust/TS/Python/Go) · symbol fusion ranking · SymbolDelta Stage · EcosystemAdapter Phase-0 (descriptions, downloads, eco filter, retrieve_and_rank shared) |
| Vector | vector-remote + qdrant-edge local · fastembed worker · EmbedStage · SemanticGate + routing |
| Materializer | trait + CasHardlink + FullExtract + `.ndpk`/`.ndix` codec + emit-at-seal + LazyArchive over iroh range |
| Compile | TrustedForgeContext assembly (`trusted-compile`) · ToolchainSet discovery · language packs · EmbeddedForge/RemoteCompile |

**Exit:** remote-serves-while-syncing flips to local atomically; offline Ready subset; local/remote search parity on identical DepSets; hardlink view verifies every path hash; LazyArchive opens 1-of-N files with bytes ≪ pack size.

### Wave 5 — Product

| Lane | Deliverables |
|---|---|
| GUI | stores · Dock/virtualization · project open flow (trust gate → resolve → DepSet → sync progress) · omni-search · RenderModel symbol pages · JobStore · lineage timeline + type-directed search |
| Hot tier | terminus-client · PackageTierManager (leaky bucket + CMS) · TieredPackageGraph · promote/demote with Relation parity · GraphAtom outbox sink · lineage Auto-edge publish · rollups |
| Lineage v2 | T4/T5 · T6 embed-assist · Manual patch API + UI soft-edge surfacing |
| Ops tail | INDEX GC (mark-and-sweep + invariant test) · fleet L5 Materializer in daemon · L2-Forbidden enforcement · IAM/NetworkPolicy · security review |

**Exit:** P0 GUI demo on a cold machine; Terminus admits only under sustained load and demotes loss-free; lineage timeline shows real T0–T5 edges across ≥ 3 generations of a fixture package; fleet L1 hit on second pod, same JobKey.

### Cross-wave rules

1. **Derived stores are disposable** (GD-14): any schema change ships as rebuild-from-CAS, never in-place migration.
2. **Lanes may pre-land dark code behind features at any time**; exit-gated integration cannot.
3. **Rejects stay rejected** (§0.5) — even as "temporary" shortcuts.
4. **The ir-vcs gates are load-bearing:** no Wave-3 sync without PR-4's fail-closed apply; no recorder without PR-1's normalizer.

## §20 Risk register

| # | Risk | Sev | Mitigation |
|---|---|---|---|
| R1 | qdrant-edge is young — API churn or perf gaps at 10⁶ vectors | H | `VectorStore` trait isolates; remote fallback via Routed is always live; RAM-ceiling bench in CI |
| R2 | Terminus Prolog-core ops opacity | M | HTTP-only boundary; cold projection is the SoT; demotion loss-free by construction |
| R3 | False lineage merges poison history (write-once read-many) | H | Frozen thresholds prefer false-split; auto ≥ 0.93; intro-preserve ≥ 0.95; embed reuse ≥ 0.95; mass-delete guard; Manual overrides |
| R4 | SQLite single-writer INDEX write ceiling at crawl scale | M | Batched IMMEDIATE writes ≥ 10k rows/s measured; queue is not SQL; channel apply is per-lineage serialized anyway |
| R5 | Litestream restore fails when needed | H | CI-scheduled restore drill; release-blocking |
| R6 | iroh relay/provider ops cost or instability | M | Self-hosted regional relays; provider fleet is stateless over S3; `bytes_by_path` metrics; provider-kill chaos test in CI |
| R7 | Sealing Rust producer stalls (RA in-process complexity) | M | Worker isolation (`producer-worker`); trusted desktop path unaffected; fleet gate holds until sealed |
| R8 | Ladybug fork maturity / license drift | M | Disposable projection (rebuild from IR); parity suite vs ColdRegistryGraph is release-blocking; the trait boundary keeps the engine swappable |
| R9 | GUI scope explosion | M | P0 scope frozen in §18.2; stores land one at a time |
| R10 | Trust jail bypass (symlinks, config redirects) | H | Canonicalize-before-jail; refusal defaults; diagnostics; T1–T10 release-blocking |
| R11 | Commit-gate watcher misses (packed-refs races, rebases) | M | Poll net + debounce + quiet period; hooks as accelerators only |
| R12 | snix GPL contamination of GUI | H | Never linked; license CI check; nix is remote-only |
| R13 | Embedding model swap invalidates all vectors | M | `tool_digest` global invalidation is designed; versioned collection names |
| R14 | Moniker grammar gaps (overloads, Nix attrs) | M | grammar_version field; method disambiguator reserved; per-language extractors snapshot-tested |
| R15 | Hardlink cross-device silently degrades to copies | M | Same-FS detection at view create; copy-ratio metric alert; LazyArchive preferred for large gens |
| R16 | ArchiveIndex skew (index for wrong pack bytes) | H | `archive_hash` inside index; member hash verified on every put; packs immutable |
| R17 | Fleet L1 poison (wrong postcard under valid JobKey) | H | Compiler-only IAM; first-write-wins immutability; decode quarantine; `stage_epoch` rotation; `verify_sample_rate` |
| R18 | Recorder mis-verdict corrupts IntroId continuity | H | Verdicts only at ≥ 0.95 on T0–T3; below = split (recoverable via Manual edge); change log is invertible — unrecord + re-record is a defined operation |
| R19 | Kueue/k8s learning curve | M | ORCH stays thin; warm path is plain HTTP; poison semantics are GA k8s features |

## §21 Open questions

1. **PackageStemId canonicalization for forks/renames** — registry rename events create new lineage roots pending an explicit package-rename patch mechanism (lineage v2).
2. **T6 embed-assist thresholds** — frozen values (0.92/0.75/0.80) need recalibration against a real corpus before T6 Auto edges default on.
3. **Producer touch fraction** per language — determines LazyArchive vs FullExtract defaults for compile views.
4. **DownloadGrant crypto details** — signing-key rotation cadence; hash-set vs manifest-root binding.
5. **Provider topology** — colocated-with-INDEX vs separate edge fleet; multi-region priority algorithm; office-LAN peer-assist policy.
6. **Bao outboard storage** — persist next to CAS vs recompute on provider; cost tradeoff.
7. **INDEX job-metadata retention** — poison/history row lifetime before S3 archival.
8. **ColdGraphBlob** (lean persisted edges for warm-not-hot packages) — only if re-projection profiling demands it.
9. **Windows desktop** — post-MVP; Materializer hardlink/copy parity on NTFS.
10. **Docset import** (Dash/Zeal) — P3 idea, unvalidated demand.
11. **Cross-language lineage** (generated bindings) — out of scope for auto matching; Manual edges only.
12. **verify_sample_rate** production default (0/1%/5%) vs CPU cost.
13. **Ladybug projection rebuild latency** on cold start / schema bump for large subscribed sets — quantify; may motivate checkpointed projections.
14. **Enterprise UDP-blocked fraction** — how many desktops end up relay-bound (sizes the relay fleet; relays ride HTTPS/443 so correctness is unaffected, cost and latency are).

---

## Appendix A — Domain-tag registry

Every BLAKE3 preimage is domain-tagged. One registry; collisions are a review-blocking error.

| Tag | Owner | Hashes |
|---|---|---|
| `nudox.gen.v3` | ir-vcs | `GenerationStamp` identity_bytes (package plane) |
| `nudox.projgen.v1` | §7 | ProjectGeneration id (desktop commit gate) |
| `nudox-change/v1` | nudox-change | `ChangeId` envelope |
| `nudox.intro.v1` | nudox-ir | `IntroId` bootstrap |
| `nudox.cset.v1` | nudox-change | `ChangeSetFingerprint` |
| `nudox.link.v1` | nudox-change | `LinkDomainKey` |
| `nudox.tyskel.v1` | nudox-ir-archive | type-skeleton fingerprint |
| `nudox.author.v1` | nudox-change | `AuthorId` |
| `sig-v1` / `body-v1` / `doc-v1` / `ref-v1` / `sym-v1` | moniker | part hashes + `content_hash` |
| `nudox-producer/1` | compiler-core | `JobKey` (producer ‖ toolchain fingerprint ‖ source ‖ dep_lock) |

---

*End of LIBRARIFICATION-PLAN Rev 2. Evidence trail: `.research/librarification/` (22 docs) + `.research/edge-tech/` (00–07). IR plane normative: `.research/ir-vcs/design/IR-NATIVE-VCS-DESIGN.md` Rev 3.2.*
