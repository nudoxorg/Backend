# LIBRARIFICATION-PLAN Assembly Specification

**Audience:** section-author agents and verifier agents producing the master plan.
**Assembler:** main session. **Date:** 2026-07-16.
**Final deliverable / master SoT:** `/Users/philocalyst/Projects/Backend/docs/LIBRARIFICATION-PLAN.md` — assembled by concatenating Part 0 (assembler-authored) + your section files + Part VI (assembler-authored).

> **Post-assembly note (2026-07-16):** The master plan is **live** at `docs/LIBRARIFICATION-PLAN.md`. Edge-tech session freezes are integrated as **GD-29..GD-35** (Materializer/ArchiveIndex, BlobTransport/iroh, fleet L0–L5, Doltgres A′/B′, IPFS reject, Ladybug optional, composefs Phase-2) with expansions in §0.6, §2.7–2.9, §3.5–3.10, §16.4–16.6, §17.5, §18.3–18.6, Wave 6. Edge research remains under `docs/research/edge-tech/`; do not treat edge PLANs as a second master.

## 1. Mission context

nudox is being restructured ("librarification") around two planes and one client:

- **INDEX** — the remote multi-tenant service: SQLite MetaStore + S3 CAS + HTTP `/v1` + derived search/graph stores. Serves untrusted-package intelligence. Postgres is removed entirely.
- **REGISTRY** — the local desktop store inside the GUI process: SQLite catalog + disk CAS + embedded Tantivy + embedded qdrant-edge + cold graph. Offline-capable.
- **ORCH** — thin Kubernetes orchestration server for the untrusted compile fleet (warm Deployment + Kueue cold Jobs). The Postgres job queue dies here.
- **compiler** — dual-form: a stateless sealed server binary (`compiler-daemon`, untrusted fleet) and an embedded library (`compiler-core` + `TrustedForgeContext`) that compiles **trusted** sources only (project roots, path deps, workspace members).
- **client** — the library between the GUI and everything else: typestates, `Routed<L,R>` dual backends, SyncEngine pulling generations from INDEX into REGISTRY, trust-branded compile.
- **lindsey** — the GPUI GUI, the start and end of nudox for the developer; full refactor to stores-over-client.

The pipeline becomes **heavily incremental**: only sealed git commits advance generations; only changed symbols (by part-hash) re-embed / re-index / re-publish. Symbol continuity across generations is answered by the moniker + part-hash + lineage-edge architecture (RFC-19). TerminusDB becomes a **hot tier only**, admitted by a leaky-bucket scorer; the long tail is served by graph-over-IR from CAS blobs.

## 2. Final plan structure and section assignments

Each section is authored from ONE source research doc (under `docs/research/librarification/`). Write your section to `docs/research/librarification/_master/sections/S<NN>-<slug>.md` (zero-padded two digits).

| § | File slug | Title | Source doc | Target lines |
|---|---|---|---|---|
| 1 | S01-crate-topology | Crate Topology & Dependency Layering | 22-crate-topology.md | 350–500 |
| 2 | S02-wire-protocol | Wire Protocol & Plane Contracts | 21-wire-protocol.md | 350–500 |
| 3 | S03-storage | Storage: CAS, Blobs, Compression, Source & Trees | 13-storage.md | 350–500 |
| 4 | S04-ir-contract | IR Contract & Occurrence Model | 04-ir-audit.md | 350–500 |
| 5 | S05-identity-academic | Symbol Identity: Academic Foundations | 05-symbol-identity-academic.md | 250–350 |
| 6 | S06-identity-industrial | Symbol Identity: Industrial Practice | 06-symbol-identity-industrial.md | 250–350 |
| 7 | S07-moniker-lineage | Moniker & Lineage (Normative) | 19-symbol-moniker-rfc.md | 400–550 |
| 8 | S08-incremental | Incremental Pipeline: Stages, Traces, Deltas | 08-incremental.md | 350–500 |
| 9 | S09-commit-gate | Commit Gate & Generations | 17-commit-gate.md | 350–500 |
| 10 | S10-graph-cold | Graph over IR: Cold Path | 20-graph-over-ir.md | 350–500 |
| 11 | S11-terminus-tiering | Terminus Hot Tier & Admission | 07-terminus-tiering.md | 300–450 |
| 12 | S12-text-search | Text Search: Tantivy Multi-Language | 10-tantivy.md | 350–500 |
| 13 | S13-vector | Vector Search & Local Embeddings | 09-vector.md | 350–500 |
| 14 | S14-compiler-dual-form | Compiler Dual-Form: Sealed Server + Embedded Library | 01-compiler-audit.md | 400–550 |
| 15 | S15-desktop-toolchains | Desktop Toolchains & Packaging | 18-desktop-toolchains.md | 300–450 |
| 16 | S16-trust-project-model | Trust & Project Model | 16-trust-project-model.md | 350–500 |
| 17 | S17-client-sync | Client Library & Sync Engine | 14-client-sync.md | 400–550 |
| 18 | S18-index-decomposition | INDEX Decomposition: registry/server → services | 02-registry-server-audit.md | 400–550 |
| 19 | S19-index-sqlite | INDEX on SQLite: Ops & Migration | 11-sqlite-index.md | 300–450 |
| 20 | S20-orch | ORCH: Kubernetes Orchestration | 12-orchestration.md | 300–450 |
| 21 | S21-gui-refactor | GUI Refactor: lindsey Architecture | 03-gui-audit.md | 350–500 |
| 22 | S22-gui-product | GUI Product: Views & Reference Synthesis | 15-gui-references.md | 350–500 |

Assembler-authored (do NOT write these): Part 0 (charter, glossary, decision log), §23 Unified Migration Phases, §24 Risk Register, §25 Open Questions.

Part groupings (for your "Role" subsection context):
Part I = §1–3 (Target Architecture) · Part II = §4–7 (Identity & IR) · Part III = §8–13 (Engines) · Part IV = §14–20 (Planes & Distribution) · Part V = §21–22 (GUI) · Part VI = §23–25 (Program).

## 3. Section template (mandatory headings)

```markdown
# §<N> — <Title>

**Source research:** `docs/research/librarification/<doc>.md` · **Plane(s):** <INDEX/REGISTRY/ORCH/SHARED/GUI> · **Target crates:** <from §5 below>

## <N>.1 Role in the program
2–4 paragraphs: what this component is in the target architecture, why it exists, what breaks without it. Reference the global decisions (GD-x) it implements and the sections it feeds (→ §M).

## <N>.2 Current state / research verdict
For audit-sourced sections: the live-tree reality with file:line citations (carry them over from the source doc).
For research-sourced sections: the verdict — chosen library/algorithm/design and the evidence for it (carry over key URLs and version pins).

## <N>.3 Target design
The heart of the section. All structures, traits, typestates with Rust sketches (signatures + key fields; not full impls). Names MUST conform to §5 canonical names. State machine / dataflow diagrams as ASCII where the source doc has them. Frozen constants/thresholds where the source doc froze them.

## <N>.4 Libraries & pins
Table: library · version pin · role · notes. Only what this section's crates consume.

## <N>.5 Implementation steps
Numbered PR-sized steps `S<N>.1`, `S<N>.2`, …. Each step: **title** — what changes, in which crate/files; **prereqs** (other steps in this or other sections, by id like `S1.2`, or GD-x); **acceptance** — how we know it's done (test, invariant, build gate). These feed the unified phase plan (§23), so be precise about ordering constraints and keep steps genuinely PR-sized.

## <N>.6 Interfaces with other sections
Explicit list: "→ §M: <what crosses the boundary, which type/trait/wire message>". Every dependency claimed in <N>.5 prereqs must appear here.

## <N>.7 Risks & open questions
Carried from the source doc plus any you see during authoring. Mark each as RISK (has mitigation) or OPEN (needs a decision, and who/when).
```

## 4. Style rules

1. Write for an implementing engineer who has NOT read the research docs. Sections must stand alone; the research doc is the citation trail, not required reading.
2. Preserve the source doc's load-bearing specifics: file:line citations, URLs, version pins, frozen constants, grammar/BNF, thresholds. Do NOT re-derive or water down frozen decisions. Do NOT invent new facts not in the source doc — if you must bridge a gap, mark it `ASSEMBLER-NOTE:`.
3. Condense literature surveys (esp. §5/§6) to what supports the frozen design in §7 (RFC-19): per-system/per-paper takeaway + URL, not full exposition.
4. Rust code blocks for all type/trait sketches. Tables for pins, matrices, enumerations. ASCII diagrams welcome.
5. Cross-reference other sections as `→ §M` using the numbers in §2 above. Never reference research doc numbers in prose (only in the Source line and citations).
6. Terminology per the glossary (§6 below). NEVER call INDEX "the server" or REGISTRY "the local index". The GUI crate is `lindsey` (path `workspace/gui`).
7. If your source doc contradicts a Global Decision (GD-x), the GD wins; note the deviation in <N>.7 as `SUPERSEDED: <what> by GD-x`.
8. No first person, no meta-commentary about "this report", no "we researched". It is a plan: imperative, declarative.
9. Length targets are in §2. Hard bounds: 250–600 lines. Dense > padded.

## 5. Canonical names (crates, traits, structures)

### 5.1 Target crates (Cargo package names)

```
heart ir moniker compiler-wire compiler-core sandbox
compiler-daemon(bin) producer-worker(bin)
graph-cold terminus-client text-search vector-local vector-remote
meta-store registry-local index-service(lib+bin) orch(lib+bin)
client-core client lindsey(bin) ingest blob
```

Layering (arrows only point down): heart → {ir, moniker, compiler-wire, blob} → {compiler-core+sandbox, graph-cold, text-search, vector-*} → meta-store → {registry-local, index-service, orch} → client-core → client → lindsey. Forbidden edges are enumerated in the source doc 22 §8.1 — §1's author reproduces them; other authors respect them.

### 5.2 Canonical trait catalog (home crate)

| Trait | Home | Notes |
|---|---|---|
| `Connect` (Cold→Live), `Cas`, `EvictableCas`, `Progressive` | heart | existing |
| `Catalog` | client-core (client-facing read subset) | Catalog ⊆ MetaStore |
| `MetaStore`, `QueueBackend` | meta-store | QueueBackend impls: `K8sJobs` (orch), `SqliteSingleWriter` (local); `PgSkipLocked` legacy-only |
| `TextSearch` | client-core (trait) / text-search (impls) | |
| `LanguageAnalyzer` | text-search | per-language query/path/boost |
| `VectorStore` | vector-local (low-level ANN CRUD) | impls: QdrantEdgeLocal, QdrantRemote, LanceLocal |
| `VectorSearch` | client-core (app-level + SemanticGate) | |
| `PackageGraph` | graph-cold (full graph API) | impls: IrBlobPackageGraph, TerminusPackageGraph, TieredPackageGraph |
| `GraphOps` | client-core (client subset of PackageGraph) | |
| `Compile` | client-core | impls: EmbeddedForge (trusted), RemoteCompile |
| `ForgeContext`, `Producer`, `DocumentSink`, render `Backend` | compiler-core | existing names, keep |
| `Cage` | sandbox | existing |
| `Stage` | heart (or small stage module) | incremental unit: digest(in)/run/output_digest |
| `CommitGate` | client / registry-local | formalized per §9 (doc 17) |
| `SemanticGate` | vector-local/client-core | capability/quota token |
| `Routed<L,R>` | client-core | local-if-Ready-else-remote combinator |

Retired names: `GraphStore` (→ PackageGraph), `SymbolClient`/`SearchClient` (GUI shims, transitional only), `BackendClient` (→ client).

### 5.3 Key structures

`ContentHash`, `JobKey`, `PackageId`, `PackageStemId` (NEW, heart), `SymbolId`, `Coordinates`, `Generation` (NEW, heart), `GenerationStatus` (registry-local), `SymbolDelta` (heart), `BlobManifest` (+ NEW `occurrences_ref`), `DepSet` (client-core), `SymbolMoniker`/`LineageMoniker`, `GenerationSymbol`, `LineageEdge`, part-hashes (`sig`/`body`/`doc`/`ref`/`embed_key` + `normalizer_version`) (moniker), `GraphCorpus`, `GraphView`, `RelationKind`, `MembershipDiff` (graph-cold), `OracleSet`, `SealedInput`, `TrustedForgeContext` (compiler-core), `ToolchainSet` (sandbox), `Jail`, `TrustedSourceSet`, `UntrustedCoordinateSet`, `PinnedCoordinate`, `Project<Unresolved|Resolved|Indexed>`, `SourcePath<Trusted|Untrusted>` (client-core/trust), `SyncEngine`, `SyncedWitness`, `TraceStore`, `symbol_heads`, `RepoBinding`, `CompositePackageMapper` (commit gate), `PackageTierManager`, `GraphAdmission` (index-service/terminus-client), `LocalRegistry`, `IdentTokenizer`, `TextIndex`, `SymbolDocument`, `VectorPoint`, `EmbedStage`.

### 5.4 Identity stack (do not collapse — reproduce where relevant)

```
NudoxPath (IR, package-local)
  → graph IRI (versionless, cross-version)
  → SymbolMoniker / LineageMoniker (versionless, product continuity)
  → GenerationSymbol (moniker @ generation)
  → SymbolId / EntryUri (versioned PackageId + instance salt — store join only)
  → ContentHash / part hashes (what the symbol IS)
```

## 6. Glossary (authoritative)

| Term | Meaning |
|---|---|
| **INDEX** | Remote multi-tenant service plane: sqlite MetaStore + S3 CAS + HTTP `/v1` + derived stores |
| **REGISTRY** | Local desktop store: sqlite catalog + disk CAS + embedded tantivy/vectors + generation status |
| **ORCH** | Orchestration server + k8s scheduling for the untrusted compile fleet |
| **client** | Library between GUI and INDEX/REGISTRY: typestates, Routed backends, SyncEngine |
| **trusted** | First-party sources (project roots + in-jail path deps); may compile in-process via TrustedForge |
| **untrusted** | Third-party/registry packages; compile only via sealed daemon/fleet |
| **generation** | Immutable package snapshot identity (content-derived); unit of sync atomicity |
| **GenerationStatus** | Local readiness: Pending/Pulling/Ready/Failed (NOT the generation id) |
| **moniker** | Version-stripped stable symbol coordinate (SCIP-inspired); cross-generation join key |
| **hot tier / cold tier** | TerminusDB-admitted packages / graph-cold over IR blobs |
| **Stage / SymbolDelta / CommitGate** | Incremental unit / changed-symbol contract / commit-sealed advancement policy |
| **want/have** | Manifest-driven hash-set sync negotiation (Nix-binary-cache pattern) |
| **closure** | The full Ready hash set of a generation's blobs |
| **plane** | INDEX / REGISTRY / ORCH / SHARED / GUI deployment concern |

## 7. Global Decision Log (FROZEN — sections must conform)

- **GD-1 Naming.** INDEX / REGISTRY / ORCH / client / lindsey as per glossary. Target crates never reuse bare `registry` / `server`.
- **GD-2 Postgres exits entirely.** INDEX = SQLite via sqlx 0.8 (writer pool max 1 + reader pool), sea-query SqliteQueryBuilder, k8s StatefulSet replicas:1 + PVC, Litestream v0.5.x sidecar → S3, restore-on-boot. REGISTRY = rusqlite (bundled) on a dedicated thread. Job queue leaves SQL entirely (→ GD-19). No SKIP LOCKED, no advisory locks; outbox + watermarks stay as tables in sqlite. rqlite is the documented HA fallback only.
- **GD-3 Graph trait naming.** `PackageGraph` = full API in graph-cold; `GraphOps` = client-core subset; `GraphStore` retired after migration.
- **GD-4 Vector.** `VectorStore` (low-level) + `VectorSearch` (app + SemanticGate) both kept. Embedded = qdrant-edge 0.7.2; remote = qdrant-client 1.18. **Parity model** both sides: `jinaai/jina-embeddings-v2-base-code` 768-d via fastembed 5.17.x + ort (CPU required; CoreML/CUDA optional). **Premium remote-only** second collection (`voyage-code-3`, never score-mixed with Jina) per `09b-retrieval-pipeline-plan.md`. EmbedStage is **SymbolDelta-only** on sealed gens (`embed_key` input_digest; 09b §16). Desktop: on_disk + scalar quant ladder (09b §17). LanceDB 0.31.0 = feature-gated escape hatch. Local vectors ship in the desktop MVP wave (not blocking client MVP; Routed degrades to remote until Ready).
- **GD-5 Catalog ⊆ MetaStore.** Catalog is the client-facing read surface; MetaStore is the INDEX/REGISTRY spine CRUD + outbox + watermarks.
- **GD-6 client-core sans-IO split adopted.** Pure traits + routing + sync state machine + typestates in client-core; tokio/reqwest wiring in client.
- **GD-7 Symbol identity per RFC-19.** Dedicated `moniker` crate. Version-stripped `LineageMoniker` (SCIP-shaped) + generation-qualified symbols; BLAKE3 part hashes `sig`/`body`/`doc`/`ref`/`embed_key` with `normalizer_version`; explicit `LineageEdge` records from the frozen T0–T7 cascade; prefer false-split over false-merge; soft edges never skip embeds or hard identity. `PackageStemId` (version-less package continuity) added to heart. Producers must stamp monikers + hash materials; `generate` seals fingerprints. Lineage storage = SQLite INDEX + CAS; Terminus receives Auto edges only for hot packages.
- **GD-8 Graph projection home.** `GraphCorpus` + `project()` (+ the shared edge lowerer to `(SymbolId, RelationKind, SymbolId)`) move to graph-cold; compiler-core depends on graph-cold for its emit stage. Cold path re-projects from IR blobs on cache miss (no persisted GraphCorpus postcard in MVP; lean ColdGraphBlob optional later). `DepLoadPolicy::StubsOnly` default. Hot publish must emit the same lowerer edges (Relation parity) in the promotion program of work.
- **GD-9 Wire protocol per doc 21.** Control plane = HTTP/JSON `/v1` + `/v1/hello` negotiation; data plane = presigned object-store GETs against BLAKE3 CAS only (closure-scoped); compiler plane = single postcard v2 in `compiler-wire` returning FULL artifacts (surface + occurrences + references + archive refs); ORCH plane = thin admit/status/poison. `ErrorBody` projects `heart::Failure`. `registry::protocol` deleted. Internals (sqlite, tantivy, qdrant, terminus, k8s) never on the public wire. Desktop never holds store credentials.
- **GD-10 BlobManifest** keeps dual-hash discipline (generation stamp ≠ CAS key) and gains `occurrences_ref`.
- **GD-11 Tree-sitter trees are NEVER persisted** (INDEX or REGISTRY). Store zstd source + OccurrenceSet/ResolvedReference; reparse on demand. This is the efficient encoding of "store resolved trees": no stable Tree serialization API exists; docs.rs stores archives. (Deviation from the product brief is intentional and evidence-backed.)
- **GD-12 Compression.** zstd everywhere: trained dictionaries for IR sections (dict_id in envelope), plain zstd for source. Seekable transfer packs optional/deferred; FastCDC deferred.
- **GD-13 Terminus = hot tier only.** v12.0.6 via HTTP behind `terminus-client`; vendored terminusdb_schema types; crates.io terminus-store banned from production. Admission = leaky-bucket scorer (+ count-min-sketch doorkeeper) fed by cold-path query stats from day one; demotion pins the cold graph first; cold path is the always-correct fallback.
- **GD-14 Incremental spine = hand-rolled constructive traces** in SQLite + BLAKE3 digests (NOT salsa-as-orchestrator; salsa allowed inside producers). `Stage` trait + `TraceStore` + `symbol_heads` + `SymbolDelta` per doc 08; symbol-level early cutoff via RFC-19 part hashes. Derived indexes are disposable — rebuild from blobs + catalog.
- **GD-15 CommitGate per doc 17.** Generation identity = `BLAKE3(project ‖ repo ‖ commit_oid [‖ submodule pins])`. Detection = `notify` watches on GIT_DIR (HEAD, refs, packed-refs) + 5s poll net, 300ms debounce, rebase quiet period. Dirty working trees NEVER trigger embeddings or durable index updates (explicit TTL-GC'd preview generations use a separate id space). `CompositePackageMapper` over-approximates dirty packages; lockfile-only commits refresh untrusted DepSet without re-producing first-party IR. Multi-root = multiple RepoBindings.
- **GD-16 Trust model per doc 16.** `Jail` = realpath prefix set of user roots ∪ promotions; BFS in-jail path deps → `TrustedSourceSet`; lockfile pins (post-patch) → `UntrustedCoordinateSet`; canonicalize-before-jail; refuse node_modules roots; ignore Cargo/NuGet config redirects by default; VS-Code-style Workspace Trust gate before executing toolchains. `Compile` exists only on `SourcePath<Trusted>` (typestate-enforced). Untrusted always compiles on the fleet.
- **GD-17 Compiler dual-form per doc 01.** Keep DAEMON-PLAN vocabulary (`ForgeContext`, `generate_with`, `SealedInput`, `JobKey`, typestate seal). Complete the injection surface: `OracleSet` (kill buck_resource/current_exe), explicit `ToolchainSet` (kill from_env in lib), `TrustedForgeContext` for desktop. Seal the adaptive producers (Rust/Java/C#) for the untrusted fleet. Daemon returns full artifacts (GD-9). `render/` ships in the embedded library. Acquisition (gix) moves out of sealed compile paths → client/INDEX.
- **GD-18 Desktop toolchains per doc 18.** Hybrid packaging: Class-B oracles in-app or language packs; system-discover Class-C SDKs for MVP; optional Nix channel; download-on-demand later. JobKey toolchain component migrates from path strings to semantic blake3 fingerprints under `nudox-producer/3`. MVP local languages: TypeScript, Python, Rust (if rustup), Go (if go). Remote-only: Java, C#, Nix. snix is GPL-3.0 and must never link into the GUI binary.
- **GD-19 ORCH per doc 12.** Warm Deployment (direct HTTP, 503 backpressure) + Kueue v0.18.3 cold Jobs; kube-rs 4.0 + k8s-openapi 0.28; podFailurePolicy (GA) for poison classification recorded into INDEX; gVisor RuntimeClass + `hostUsers:false` + in-pod bwrap/landlock layered; presigned S3 multipart/zstd artifact flow; HPA/KEDA 2.20 warm scaling, Karpenter v1 nodes. NO Argo/Tekton, NO CRD-as-queue, NATS optional-off.
- **GD-20 Client per doc 14.** Typestates for monotonic concerns only (Connect, Project resolve, trust plane); per-generation sync status = runtime state + optional `SyncedWitness` (no `Store<Syncing>` typestate). Every query routes over a generation-pinned DepSet: local iff all members Ready, else remote while SyncEngine pulls; offline = Ready subset. Generations become visible atomically post hash-verify. Sync = manifest want/have hash-set difference — NOT CRDT/row-replication (Electric/PowerSync rejected).
- **GD-21 Text search per doc 10.** One shared multi-language schema + tokenizer registration + index-format major across INDEX and REGISTRY. `LanguageAnalyzer` trait for path split/query rewrite/boosts/signature terms over the shared subword core. Target tantivy 0.26.1 (min 0.24.2) via one-shot controlled reindex; schema-version marker file, forced reindex on mismatch. Symbol ranking = multi-field BM25 + exact-match bonus + kind prior; package download-bubble fusion stays package-side in index-service.
- **GD-22 GUI per docs 03+15.** Stores-over-client: `ProjectStore`, `SymbolStore`, `SearchStore`, `JobStore`, `RegistryStore`, `NavHistory` entities subscribe to client; Workspace demoted to layout shell; gpui-component Dock/List/Table/Tree/Modal adopted. P0 = project manager + trust viz, cmd-K omni-search, symbol page v1 (versioned RenderModel, DocC-style), jobs/logs, settings, palette, nav history. P1 = package browser, sync polish, lineage timeline, references panel, type-directed search. Fixtures cfg(debug_assertions) only. Flagship wedges: lineage timeline + type-directed multi-language search.
- **GD-23 Crate purity.** `ir` must NOT depend on heart (producers stamp heart identities in compiler-core). heart gains `Generation`, `SymbolDelta`, `PackageStemId`; `BackendKind` evolves off Postgres variants. Shared libs (heart, ir, moniker, compiler-wire, blob) stay free of plane-specific I/O.
- **GD-24 INDEX job metadata.** INDEX sqlite keeps job/poison metadata rows for observability; claim/lease semantics live ONLY in ORCH/Kueue. Traces are sqlite-only (no new Postgres anywhere).
- **GD-25 Cold-first graph rollout.** Cold PackageGraph ships before Terminus gating (hot is an optimization). Cold query stats feed the GD-13 scorer from day one. Demotion pins cold; hot probe failure fails over to cold.
- **GD-26 Deletions.** Root legacy `compiler/` frozen then deleted; `registry::protocol`; SKIP LOCKED queue design; PgSessionStore-as-only-sessions; Qdrant-server-as-local-path; Terminus-for-all; CstSet demoted to OccurrenceSet as reference SoT; buck_resource embed path; GUI release fixtures; `BackendClient` as sole data path.
- **GD-27 Store layouts.** REGISTRY = `registry.sqlite` + sharded `cas/` + `tantivy/` + `vectors/` + pin/LRU GC. INDEX = S3 `cas/{blake3}` + sqlite catalog. Sync = manifest-driven hash-set difference. Both planes store source (zstd) + IR + occurrences per GD-11/12.
- **GD-28 Committed-only, changed-only.** Embeddings, text index, graph publish, and lineage run ONLY on sealed generations (GD-15) and ONLY for symbols whose relevant part-hash changed (GD-7/GD-14). This is the product's central performance invariant.

## 8. Author procedure

1. Read this spec fully.
2. Read your source doc fully (`docs/research/librarification/<doc>.md`).
3. Optional: consult `docs/research/librarification/_master_exec_summaries.txt` for neighbor-section context (do not read other full docs).
4. Write your section file at the exact path in §2, following the §3 template and §4 style.
5. Return the structured summary (schema provided in your task prompt). Do not return the section text itself.

## 9. Verifier lenses (for the verify wave; authors may self-check)

- V-NAME: no retired/banned names outside "retired" callouts; glossary conformance; crate names exact.
- V-XREF: every `→ §M` points at the right section; every interface is reciprocated in the counterpart's §M.6 (or flagged).
- V-GD: no section contradicts a GD; deviations marked SUPERSEDED.
- V-COVER: section captures its source doc's decisions, structures, pins, steps, risks (checked doc-vs-section).
- V-PHASE: implementation steps' prereqs form a DAG; no cycles; no orphan prereqs.
