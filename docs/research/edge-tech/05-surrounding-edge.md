# 05 — Surrounding / Breaking Edge Technologies for Nudox Librarification

**Research date:** 2026-07-16  
**Kind:** wide-but-deep survey PLAN (not a master migration plan)  
**Audience:** architecture / crate-topology / assembler agents  
**Sibling reports (do not duplicate full depth):**
- [01-doltgres](./01-doltgres/) — Doltgres as versioned SQL for INDEX/catalog
- [02-iroh](./02-iroh/) — iroh for CAS blob transport / P2P
- [03-edenfs](./03-edenfs/) — EdenFS / lazy materialization
- [04-ipfs-and-cas](./04-ipfs-and-cas/) — IPFS and content-addressed storage patterns  

**Primary product anchors:**
- [librarification/22-crate-topology](../librarification/22-crate-topology/PLAN.md) — target crate graph
- [librarification/11-sqlite-index](../librarification/11-sqlite-index.md) — INDEX without Postgres
- [librarification/07-terminus-tiering](../librarification/07-terminus-tiering.md) — hot Terminus + cold graph-over-IR
- [librarification/09-vector](../librarification/09-vector.md) — embedded vectors (qdrant-edge winner)
- [librarification/12-orchestration](../librarification/12-orchestration.md) — k8s untrusted compiles
- [librarification/13-storage](../librarification/13-storage.md) — CAS / archive / range reads
- [librarification/14-client-sync](../librarification/14-client-sync.md) — local↔remote sync typestates
- [resolution/03-exhaustive-plan](../resolution/03-exhaustive-plan.md) — consumer resolution ladder

**Verification policy:** Claims re-checked against live web / project sites on **2026-07-16**. Training data is not trusted for version numbers or project status.

---

## 0. How to read this document

| Section | Use |
|---|---|
| §1 Executive map | Problem → ranked edge candidates in one table |
| §2–§8 Per-area tech | 2026 status, problem solved, nudox fit, verdict, URLs |
| §9 Combinations | Topology-simplifying stacks (crate-graph impact) |
| §10 Moonshots | “Collapse half the design” honesty check |
| §11 Explicit rejects | Do-not-ship list with reasons |
| §12 Open questions | Decisions that need prototypes / sibling reports |
| §13 Top streamlining recommendations | Actionable shortlist for assemblers |

**Verdict key:**

| Tag | Meaning |
|---|---|
| **ADOPT** | Bring into target architecture / crate deps with high confidence |
| **BORROW** | Steal patterns, APIs, or format ideas; do not take the product whole |
| **WATCH** | Promising; pin for re-eval in 3–6 months; not on critical path |
| **REJECT** | Wrong fit, dead project, license/ops tax, or superseded by in-plan tech |
| **SIBLING** | Covered deeply in 01–04; only differential notes here |

---

## 1. Executive map: problem → edge candidates

### 1.1 Product problems (from mission)

| # | Problem | In-plan baseline (2026-07-16) |
|---|---|---|
| P1 | INDEX metadata + multi-tenant users/orgs **without Postgres** | SQLite single-writer + Litestream (11) |
| P2 | Versioned symbol/graph history without full Terminus per package | Cold graph-over-IR + hot Terminus admission (07, 20) |
| P3 | Local↔remote sync of CAS + catalog for subscribed packages | Manifest-pull / generation-pin (14); iroh/IPFS siblings |
| P4 | Efficient source tree materialization for compile | CAS + archive range reads (13); EdenFS sibling (03) |
| P5 | Incremental symbol-level pipelines, lineage | Generation + SymbolDelta (08, heart); Terminus layers hot |
| P6 | Embedded search (tantivy) + vectors | Tantivy (10); qdrant-edge local + Qdrant remote (09) |
| P7 | Orchestration of untrusted compiles on k8s | Kueue + warm pool + sandbox (12) |

### 1.2 Ranked candidates per problem

| Problem | Rank 1 | Rank 2 | Rank 3 | Avoid |
|---|---|---|---|---|
| **P1 INDEX authority** | SQLite + Litestream (plan 11) | **DoltLite** (versioned SQLite — WATCH/α) | Doltgres (sibling 01) for branchy multi-writer later | FDB Record Layer; Iceberg/Nessie as SoT; CRDT multi-master INDEX |
| **P2 Graph history** | Cold **graph-over-IR** + hot Terminus (07/20) | **LadybugDB** (Kuzu successor) as desktop cold Cypher | FalkorDB multi-graph hot alternative | Upstream **Kùzu** (archived); Neo4j desktop embed; AGE without Postgres |
| **P3 Sync** | Generation-scoped want/have (14) + **iroh** (02) | **Electric shapes** metaphor only; ostree/casync chunking | Syncthing **model** for peer folder topology | PowerSync/CR-SQLite as CAS path; WebTorrent primary |
| **P4 Materialize trees** | CAS + ZIP/archive index (13); Eden-class FUSE (03) | **composefs + erofs** (Linux immutable trees) | **SOCI/eStargz** lazy-layer analogy | Full ostree as product FS; jujutsu as package WC |
| **P5 Incremental / lineage** | Generation pin + IR delta + Terminus layers | Differential-dataflow **ideas** (local IVM) | Nix-style pure rebuild graph | Materialize SaaS for symbol events |
| **P6 Vectors / search** | **qdrant-edge** + Tantivy (09/10) | LanceDB runner-up; sqlite-vec micro | Turbopuffer remote multi-tenant only | SPFresh-as-dependency; USearch as full store |
| **P7 Compute sandbox** | Plan 12 sandbox + microVM path | **WASM components** for pure producers | gVisor for k8s RuntimeClass | WASM for full language oracles |

### 1.3 One-page streamlining thesis

**Do not replace the 22-crate planes.** The INDEX / REGISTRY / ORCH / SHARED split remains correct. Edge tech streamlines *implementations inside crates*, not the DAG:

1. **Keep INDEX = SQLite authority + S3 CAS** (plan 11 + 13). Add **DoltLite only if** generation history / branch experiment becomes a product requirement that SQLite + object store cannot express cheaply.
2. **Keep cold graph = graph-over-IR** (20). Evaluate **LadybugDB** as an *optional* local Cypher accelerator for desktop, not as Terminus replacement.
3. **Keep sync = content-hash want/have**, not CRDT row engines. Steal Electric **shapes** and PowerSync **buckets** as subscription vocabulary.
4. **Steal lazy-tree ideas** from SOCI/eStargz and composefs for compile materialization; do not adopt container snapshotters as the desktop FS.
5. **Do not open a second vector stack** beyond plan 09 (qdrant-edge / Lance fallback).
6. **Optional NATS JetStream** only for warm-path backpressure / outbox fan-out — already allowed in plan 12.
7. **WASM component model** is the interesting 2026 compute edge for *sealed pure transform steps*, not for rustc/tsc oracles.

---

## 2. Versioned data / catalogs

### 2.1 Dolt (MySQL-wire, non-gres) — differential only

**URLs:** https://www.doltdb.com · https://github.com/dolthub/dolt · https://docs.dolthub.com  

**2026 status:** Mature “Git for data” product family (Dolt MySQL-compatible, Doltgres PG-compatible, new **DoltLite** SQLite fork). Dolt proper remains a server/process with clone/push/pull/branch/merge on tables.

**Problem solved:** Branchable, mergeable SQL catalog with cell-level diffs — useful when humans (or agents) co-edit tabular metadata and need Git workflows.

**Distinct value vs Doltgres report (01):**  
- Same product family; wire protocol and SQL dialect differ (MySQL vs Postgres).  
- nudox already chose **Postgres-shaped schemas** historically and is migrating to **SQLite** for INDEX (11), not MySQL.  
- Dolt (non-gres) adds **no unique capability** for nudox beyond what Doltgres provides, and worsens dialect mismatch with any remaining sqlx Postgres muscle memory.

**Nudox fit:**

| Aspect | Fit |
|---|---|
| INDEX package catalog | Poor as primary SoT — ops process + MySQL dialect |
| Multi-tenant users/orgs | Possible with branches-as-tenants; over-engineered |
| Generation history | Attractive mentally; CAS + sqlite `package_generations` already models this |
| Desktop REGISTRY | Too heavy |

**Verdict: REJECT for direct adopt** (prefer Doltgres evaluation in sibling 01 if versioned SQL is required; prefer SQLite plan 11 as default). Document Dolt only as family context for DoltLite.

---

### 2.2 DoltLite — version-controlled SQLite (new 2026 angle)

**URLs:** https://github.com/dolthub/doltlite · https://www.dolthub.com/blog/2026-03-25-doltlite/ · https://www.dolthub.com/blog/2026-04-27-why-doltlite/  

**2026 status:** Announced **2026-03** as free/open (Apache-2.0) **fork of SQLite** with B-tree replaced by Dolt-inspired **Prolly tree**; Git-style branch/commit/diff/merge on embedded SQL. Alpha on dolthub.com product page (mid-2026). Claims: drop-in SQLite replacement, WASM builds for browser local-first, ATTACH of standard SQLite DBs alongside versioned storage. Marketed explicitly against CRDT-based SQLite sync engines (Turso / PowerSync / Electric / cr-sqlite).

**Problem solved:** Embedded SQL with **version history without a second history store** — branch experiment catalogs, time-travel queries, merge of concurrent catalog edits.

**Nudox fit (P1, partial P5):**

| Need | DoltLite | Plan 11 SQLite + Litestream |
|---|---|---|
| Single-writer INDEX | Yes | Yes |
| Continuous backup | Need own story | Litestream mature |
| Branch/merge catalog | **Native** | DIY (generations table + CAS) |
| sqlx / rusqlite | Unknown maturity mid-2026 | Proven |
| Multi-tenant ACL | Still app-level | Same |
| Production readiness | **Alpha** | Production pattern |

**Critical honesty:** nudox’s *real* versioning unit is **content-addressed package generation** (blobs + manifest), not mutable row history. Catalog rows are mostly append-only resolution state. Cell-level Git merges shine when two humans edit the same package metadata offline — that is **not** the INDEX write path (server-authoritative, single writer).

**Where DoltLite could still win:**
1. **Local REGISTRY experimental branches** — user tries “what if I pin serde 1.0.200 vs 1.0.210” as catalog branches without copying whole CAS.
2. **Agent/tooling** that wants `diff` of catalog SQL (DoltHub’s “git for context” narrative).
3. Long-term replacement if DoltLite becomes a boring, tested SQLite ABI with prolly-tree durability matching stock SQLite.

**Verdict: WATCH (alpha)** — do **not** block plan 11. Revisit when: (a) ABI stability + sqlx/rusqlite story clear, (b) product wants catalog branching. **BORROW** the framing: “versioned SQL is an alternative to CRDTs for multi-device” — already aligns with plan 14 reject of CRDT CAS.

---

### 2.3 Project Nessie / Apache Iceberg / LakeFS

**URLs:**  
- Nessie: https://projectnessie.org · releases through **0.108.1 (2026-06-24)**  
- Iceberg: https://iceberg.apache.org  
- LakeFS: https://lakefs.io  
- Comparison context: https://www.dremio.com/blog/data-lakehouse-versioning-comparison-nessie-apache-iceberg-lakefs/

**2026 status:**

| System | Layer | Git-like? | Primary domain |
|---|---|---|---|
| **Iceberg** | Table format (snapshots, manifests, partition specs) | Per-table snapshots | Analytics tables on object storage |
| **Nessie** | **Catalog** versioning over Iceberg (and more) | **Yes** — branches/tags/commits across *many tables* | Multi-table consistency for lakehouses |
| **LakeFS** | Object-key versioning / data version control | **Yes** — branches over object store prefixes | File/object datasets |

**Problem solved:** Isolate lakehouse ETL so readers never see half-written multi-table states; branch production data for experiments; rollback catalogs without duplicating petabytes.

**Pattern borrow for package catalogs:**

```
Lakehouse metaphor          →  nudox mapping
─────────────────────────────────────────────
Object store data files     →  CAS blobs (IR, source, trees)
Iceberg table snapshot      →  Package generation manifest
Nessie catalog commit       →  INDEX transaction publishing
  many tables atomically         many packages/generations + outbox
LakeFS branch               →  “preview INDEX” for canary ingest
```

**What to steal:**
1. **Catalog commit as multi-object atomic pointer update** — INDEX sqlite txn that advances watermarks + package rows + outbox in one commit (already plan 11), documented with Nessie vocabulary for implementers.
2. **Branch = isolated view of catalog pointers** without copying blobs (CAS makes this free).
3. **Tag = release pin** (`registry-snapshot-2026-07-01`).

**What not to steal:**
- Running a Nessie server + Spark/Flink/Trino stack for a code-intelligence product.
- Iceberg REST catalog as the client protocol for GPUI (wrong query model: analytics SQL over Parquet, not symbol search).
- LakeFS as the CAS (iroh/S3 + BLAKE3 is already content-addressed; LakeFS adds another control plane).

**Nudox fit:**

| Aspect | Verdict |
|---|---|
| Production INDEX SoT | **REJECT** product stack |
| Design vocabulary | **BORROW** heavily |
| Analytics over publish events | Optional Iceberg *export* of metrics later — not core path |

**Verdict: BORROW patterns / REJECT runtime.**

---

### 2.4 FoundationDB + Record Layer

**URLs:** https://www.foundationdb.org · https://github.com/foundationdb/fdb-record-layer · SIGMOD paper https://arxiv.org/abs/1901.04452 · docs https://foundationdb.github.io/fdb-record-layer/

**2026 status:** FoundationDB remains Apple’s battle-tested distributed KV (CloudKit). Record Layer is a **stateless Java library** providing record stores, indexes, schema evolution, Cascades-style planning, and **massive multi-tenancy** (billions of logical DBs per cluster — per-user stores). Continuations for long operations. Not a “download and go” SQLite alternative; it is **ops-heavy distributed infrastructure**.

**Problem solved:** Extreme multi-tenant structured data with ACID transactions on a shared cluster; per-tenant isolation of indexes/schema.

**Nudox fit (P1 multi-tenant):**

| Need | FDB+RL | Plan 11 SQLite |
|---|---|---|
| Multi-tenant orgs | Excellent at scale | App-level + one DB (or DB-per-tenant files) |
| Ops simplicity | **Poor** (cluster, coordinators, Java layer) | Excellent |
| Desktop REGISTRY share | Impossible (server cluster) | Same code path possible |
| Team size fit | FAANG-scale | Startup / mid |

**When it would become relevant:** multi-region INDEX with millions of tenants and CloudKit-class scale. That is not the 2026 librarification target.

**Verdict: REJECT for near/mid term.** **BORROW** only the *idea* of encapsulating tenant state (indexes included) under a single tenant prefix — maps to `org_id` column discipline or separate sqlite files per org if needed.

---

### 2.5 Materialize / differential dataflow / DBSP

**URLs:**  
- Materialize: https://materialize.com (cloud-only / BSL; Timely + Differential)  
- Differential: https://github.com/TimelyDataflow/differential-dataflow  
- Conceptual: https://materialize.com/blog/differential-from-scratch/ · DBSP literature  
- 2026 IVM landscape: RisingWave, Feldera (DBSP), Epsio, etc.

**2026 status:** Materialize is a **managed streaming DB** maintaining SQL views incrementally with strong consistency; self-host path not the default story. Differential dataflow (Rust) and DBSP remain the academic/industrial engines behind multiple products. Feldera and others productize DBSP.

**Problem solved:** Keep complex joins/aggregates **correct and fresh** as high-rate change streams arrive — without full recompute.

**Nudox mapping (P5 incremental symbol pipelines):**

| Symbol pipeline reality | Differential fit |
|---|---|
| Event rate | Package publishes are **low rate** vs ad/click streams |
| Recompute unit | Already **generation-scoped** and content-addressed |
| Views needed | Search indexes, graph edges, vector points — rebuilt from IR |
| Consistency | Generation pin is simpler than streaming frontiers |

**What is useful:**
- **Mental model:** treat `SymbolDelta` as a **Z-set** (additions + retractions with multiplicities) when applying cold→hot graph updates and Tantivy/Qdrant incremental upserts.
- **Local IVM** for GUI-derived views (e.g. “symbols in open files × dependency graph”) could use a tiny differential-inspired structure — not Materialize.

**What is not useful:**
- Shipping Materialize as infrastructure for symbol events (cost, cloud lock-in, wrong scale).
- Replacing producer pipelines with streaming SQL.

**Verdict: BORROW theory (Z-sets / incremental apply) / REJECT Materialize product.** Optional **WATCH** Feldera/DBSP only if a future analytics product needs live multi-join dashboards over publish streams.

---

### 2.6 ElectricSQL / PowerSync / cr-sqlite — 2026 INDEX authority update

**Cross-ref:** plan 14 already covers these; this section is the **2026 INDEX authority verdict** refresh.

#### ElectricSQL (post “electric-next” rewrite)

**URLs:** https://electric-sql.com · shapes: https://electric-sql.com/docs/guides/shapes  

**2026 model:**  
- **Read-path sync** from Postgres via logical replication → Electric service → clients.  
- **Shapes** = partial replication subscriptions (table + where + columns).  
- Writes go through **application APIs**, not Electric (legacy bidirectional SQLite CRDT path abandoned as primary design).  
- PGlite optional for local Postgres-in-WASM.

**INDEX verdict:** Electric assumes **Postgres primary**. nudox INDEX is moving **off** Postgres to SQLite (11). Adopting Electric would **reintroduce Postgres** solely for sync — circular.  

**BORROW:** Shape = `SubscribeSpec { packages: [...], generations: [...], sections: [ir|source|graph] }` in client-core.  
**REJECT:** Electric as INDEX transport.

#### PowerSync

**URLs:** https://www.powersync.com · sync rules docs  

**2026 model:** Backend (Postgres/Mongo/MySQL) → sync service → **local SQLite** with sync rules/buckets; offline writes via upload queue; server wins on conflict for many apps.

**INDEX verdict:** Architecture assumes **mutable app data** with write-back. Dependency packages are **pull-only, server-authoritative, immutable generations**. Upload queue is dead weight.  

**BORROW:** Bucket = resolved dependency closure at generation G; priority buckets for interactive packages.  
**REJECT:** PowerSync as product dependency.

#### cr-sqlite

**URLs:** https://github.com/vlcn-io/cr-sqlite  

**2026 model:** SQLite extension with CRDT/causal-length sets for multi-master merge.

**INDEX verdict (unchanged / strengthened):** Multi-master CRDT is the **wrong consistency model** for a global package INDEX (forked package metadata = security/incident class bug). Plan 11 already **rejects** cr-sqlite for INDEX primary.  

**Possible niche:** multi-device **user preferences / session UI state** — not catalog, not CAS. Even then, prefer simple last-write-wins or server store.

**Verdict summary for sync engines vs INDEX authority:**

| Engine | INDEX SoT | Client catalog cache | CAS blobs |
|---|---|---|---|
| Electric | REJECT | BORROW shapes | N/A |
| PowerSync | REJECT | BORROW buckets | N/A |
| cr-sqlite | REJECT | REJECT (prefs maybe) | REJECT |
| Turso Sync / embedded replicas | REJECT as SoT | BORROW UX only (14) | REJECT |
| DoltLite | WATCH α | WATCH local | N/A |
| Manifest want/have (14) | N/A | **ADOPT** | **ADOPT** |

---

## 3. Graph & knowledge stores

### 3.1 Framing: what nudox actually needs from “graph”

From plans 07 + 20 + 22:

| Tier | Role | Scale | Query shape |
|---|---|---|---|
| **Cold** | Default for most packages | Universe-scale | Load IR → project edges; trait `PackageGraph` |
| **Hot** | Sustained interactive demand | Tiny fraction | Multi-hop lineage, WOQL/GraphQL-ish, versioned commits |
| **Desktop** | Subscribed deps only | 10²–10⁴ packages | Go-to, call hierarchy, “who uses X” |

Terminus stays hot-only with admission control. Cold path is **graph-over-IR**, not “another always-on graph DB per package.”

---

### 3.2 Kùzu / KuzuDB — critical 2026 status

**URLs:** https://github.com/kuzudb/kuzu (archived) · https://kuzudb.github.io  

**2026 status (load-bearing fact):**  
- GitHub repo **archived 2025-10-10**; final release **v0.11.3**.  
- EU DMA / press confirmed **Apple acquisition of Kùzu Inc.** (agreement ~2025-10-09; public coverage early 2026).  
- Upstream **dead** for features/security; MIT license allows forks.  
- Docs remain readable; pin-only of 0.11.3 is a freeze, not a strategy.

**Problem it solved (historical):** Embedded, columnar, Cypher, DuckDB-for-graphs positioning — **exactly** the “desktop hot graph without Terminus” fantasy.

**Nudox impact:** Any design that said “ship Kuzu in lindsey” is **invalid as of Oct 2025**. Do not depend on `kuzudb/kuzu` crates/binaries for new work.

**Verdict: REJECT upstream Kuzu.** Evaluate successors (§3.3).

---

### 3.3 LadybugDB (and other Kuzu successors)

**URLs:** https://ladybugdb.com · https://github.com/LadybugDB/ladybug · narrative https://thedataquarry.com/blog/from-kuzu-to-ladybug · gdotv landscape https://gdotv.com/blog/kuzu-legacy-embedded-graph-database-landscape/

**2026 status:** Community/commercial continuation of Kuzu as **LadybugDB** — embedded columnar graph, Cypher, positioning as “DuckDB for graphs” and **graph lakehouse** (Arrow/Parquet/object store). Active development through 2026 (releases such as 0.18.x discussed in community). Other forks/names (bighorn, etc.) appeared in late-2025 chatter; **Ladybug is the serious continuity bet** per tooling vendors (gdotv IDE support).

**Problem solved:** In-process analytical property graph for desktop/agent workloads without a server.

**Nudox fit (P2 desktop / cold accelerator):**

| Criterion | LadybugDB | graph-over-IR | Terminus hot |
|---|---|---|---|
| Embedded in GPUI | **Yes (goal)** | Yes (pure Rust) | No (HTTP) |
| Cypher ergonomics | Yes | No (custom API) | WOQL/GraphQL |
| Versioned package lineage product-wide | Weak | Strong via CAS generations | Strong commits |
| Dependency risk | Fork youth / governance | Low (our code) | Ops server |
| Rust bindings quality | TBD — verify C++/FFI | Native | HTTP client |

**Recommendation shape:**

```
graph-cold (trait PackageGraph)
    ├── default: GraphOverIr (plan 20)     ← always available
    └── optional feature: LadybugBackend  ← WATCH → ADOPT if bindings + license OK
terminus-client                           ← hot remote only
```

**Does Ladybug replace cold IR + hot Terminus?** **No.**  
- Cold IR remains the **portable, versioned source of truth** for edges (works offline, diffs by generation, no second DB file per package explosion).  
- Ladybug could be a **materialized local index** over subscribed packages (like Tantivy is for text).  
- Terminus still wins for multi-tenant hot server with true commit history at INDEX scale.

**Verdict: WATCH → possible ADOPT as optional local graph index.** Prototype after Rust/FFI and license confirmation. **Do not** block graph-over-IR.

---

### 3.4 FalkorDB

**URLs:** https://www.falkordb.com · https://github.com/FalkorDB/FalkorDB · RedisGraph migration guide  

**2026 status:** Successor to **RedisGraph** (EOL ~2025). Property graph as Redis module / standalone; sparse-matrix linear algebra execution; Cypher subset; **multi-graph / multi-tenant** marketing (10k+ graphs); GraphRAG positioning; AGPL + commercial. Active 4.x line (e.g. 4.14.x mid-2026). Strong latency claims vs Neo4j in vendor and third-party benches — treat as marketing until internal bench.

**Problem solved:** Ultra-low-latency multi-tenant graph queries in a Redis-shaped ops model; GraphRAG retrieval.

**Nudox fit:**

| Aspect | Fit |
|---|---|
| Hot INDEX alternative to Terminus | Possible technically (multi-graph ≈ package graphs) |
| Versioned document commits + semantic diff | **Weaker than Terminus** (Terminus’s killer feature for lineage) |
| Desktop embed | **No** (server/Redis module) |
| License | AGPL friction for commercial desktop adjacency |
| Ops | New dependency next to Terminus — **duplicate** unless Terminus exits |

**Verdict: REJECT as Terminus replacement** unless Terminus fails ops/perf gates. **WATCH** only if GraphRAG product surface needs sparse-matrix speed and multi-graph tenancy on the server *and* Terminus cannot meet SLOs. Prefer one hot graph system.

---

### 3.5 Neo4j

**URLs:** https://neo4j.com  

**2026 status:** Incumbent property graph; broadest ecosystem (drivers, Bloom, GDS); clustering/RBAC in enterprise; community edition limitations remain a perennial issue for some features.

**Nudox fit:** Excellent general graph product; **heavy** for both desktop and lean INDEX. Cypher skills transfer. No special fit for content-addressed package generations. Dual-write with IR cold path is expensive.

**Verdict: REJECT** for core path (ecosystem size does not buy product fit). Optional future enterprise export connector only.

---

### 3.6 Memgraph

**URLs:** https://memgraph.com  

**2026 status:** In-memory, streaming-oriented, Neo4j-compatible drivers/Cypher; BSL licensing notes in 2026 comparisons; strong real-time analytics positioning.

**Nudox fit:** Hot in-memory graph for streaming updates — overkill for publish-rate symbol graphs; not embedded desktop; license caution.

**Verdict: REJECT** for core.

---

### 3.7 Apache AGE (Postgres)

**URLs:** https://age.apache.org  

**2026 status:** PostgreSQL extension; openCypher over graph stored in PG.

**Nudox fit:** Requires **Postgres** — directly conflicts with plan 11 (leave Postgres). If INDEX were staying on Postgres, AGE would be a contender for “graph in the same process as catalog.” It is not.

**Verdict: REJECT** (architecture conflict).

---

### 3.8 TypeDB

**URLs:** https://typedb.com  

**2026 status:** Strongly typed conceptual model + reasoning; niche industrial/knowledge-engineering audience; not a lightweight embed.

**Nudox fit:** Type system / reasoning is interesting academically for trait coherence queries; operationally a second specialized DB with small Rust ecosystem relative to need. IR already encodes rich structure.

**Verdict: REJECT** for storage; **BORROW** modeling ideas only if designing a formal “symbol kind ontology” later.

---

### 3.9 Dgraph

**URLs:** https://dgraph.io  

**2026 status:** Distributed graph with DQL/GraphQL heritage; horizontal scale story; operationally a cluster product.

**Nudox fit:** Scale-out graph for social-scale graphs — not our package generation graph. Ops tax high.

**Verdict: REJECT.**

---

### 3.10 RDF stores / SPARQL (generic)

**Examples:** Apache Jena, Oxigraph, RDF4J, Amazon Neptune RDF mode.

**Nudox fit:** Linked-data export of IR is already adjacent (`linked_data` emit in compiler). Full RDF triple store as query SoT duplicates Terminus/IR. **Oxigraph** (Rust) is the only mildly interesting embed for SPARQL over exported RDF — still a third query language for users.

**Verdict: BORROW** RDF *export* for interoperability; **REJECT** RDF store as primary graph engine.

---

### 3.11 ArcadeDB / Apollo note

**ArcadeDB** (https://arcadedb.com): multi-model (graph/document), Apache-2.0, active 2026 marketing as Neo4j alternative / Kuzu migration target. Java-centric. **REJECT** for Rust desktop core; optional server experiment only.

**“Apollo”:** No leading embedded graph product under that name in 2026 graph rankings; likely confusion with **Apollo GraphQL** (federation — orthogonal) or **ArcadeDB**. GraphQL federation is **REJECT** for INDEX (wrong problem). If GraphQL is desired as a *query façade* over Terminus, that is a thin API choice, not a storage technology.

---

### 3.12 Synthesis: can embedded graph collapse cold IR + hot Terminus?

| Proposal | Collapses cold? | Collapses hot? | Honest answer |
|---|---|---|---|
| Ladybug/Kuzu-class only | Partial (materialized) | No (no multi-tenant versioned server story like Terminus) | **No** — adds optional local index |
| Falkor/Neo4j only | No | Replaces Terminus poorly (lineage/versioning) | **No** without losing Terminus strengths |
| graph-over-IR only | Yes for queries | No for heavy interactive | **Yes for cold** (already plan) |
| Terminus for everything | Wasteful | Yes | **No** at universe scale (07) |

**Streamlining win:** Keep **two tiers**, optionally add **Ladybug as third local cache** — but that is *more* crates unless Ladybug stays behind the same `PackageGraph` trait (then crate count stays flat).

---

## 4. Sync & transport

### 4.1 NATS JetStream

**URLs:** https://nats.io · https://docs.nats.io/nats-concepts/jetstream  

**2026 status:** Mature embedded persistence for NATS; streams, consumers, exactly-once processing patterns; official multi-language clients including Rust; used widely for cloud-native eventing. Litestream v0.5 even lists NATS JetStream as a replica target type (plan 11 adjacency).

**Problem solved:** Durable pub/sub, work queues, fan-out, backpressure without Kafka-class ops.

**Nudox fit (P7 + outbox):**

| Use | Fit |
|---|---|
| Cold Job queue (replace k8s Jobs) | **No** — plan 12 prefers Kueue |
| Warm-path backlog / scale signals | **Yes optional** |
| INDEX transactional outbox fan-out to search/vector workers | **Yes optional** |
| Client sync bus | **No** — clients should pull manifests over HTTP(S)/iroh |

Plan 12 already: *“Optional NATS only if warm-path backpressure or fan-out events need a durable bus.”*

**Verdict: ADOPT optionally** as orch/index infrastructure; not on client critical path. Do not make NATS required for MVP INDEX.

---

### 4.2 WebTransport / HTTP/3

**URLs:**  
- MDN WebTransport (Baseline **March 2026**): https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API  
- IETF draft WebTransport over HTTP/3: https://datatracker.ietf.org/doc/draft-ietf-webtrans-http3/  
- HTTP/3: widespread CDN/browser support by 2026  

**2026 status:** WebTransport is **Baseline** in major browsers (March 2026). Useful for multiplexed streams + datagrams over QUIC. Native desktop (GPUI/reqwest) still primarily HTTP/1.1–2; HTTP/3 support in Rust ecosystem exists (quinn/h3) but is not free.

**Problem solved:** Lower latency multiplexing, independent streams (avoid head-of-line), unreliable datagrams for progress telemetry.

**Nudox fit (P3):**

| Channel | Priority |
|---|---|
| Blob bulk transfer desktop↔INDEX | HTTP/2 range GETs + iroh (02) first |
| Progress / job telemetry | WebTransport datagrams **nice** for browser clients; GPUI can use plain HTTP SSE/WS |
| Multiplex many small catalog reads | HTTP/2 sufficient |

**Verdict: BORROW later** for browser clients and telemetry; **not** a 2026 streamlining lever for Rust desktop. Prefer finishing generation-pull protocol on boring HTTPS.

---

### 4.3 rsync / zsync / rclone

**URLs:** rsync classic; zsync (HTTP range + rolling checksums); rclone https://rclone.org  

**Problem solved:** Efficient tree/file sync over dumb transports; cloud object ↔ disk bridging (rclone).

**Nudox fit:**  
- **rsync algorithm** inspiration for delta of *mutable* trees — but package generations are **immutable CAS**; want/have on hashes dominates.  
- **zsync** interesting only if publishing **mutable large files** without chunk CAS — we should prefer content-defined chunking in CAS (13) instead.  
- **rclone** useful as **ops tool** for admin mirroring of S3 buckets, not as client library.

**Verdict: BORROW rsync mental model for docs; REJECT as client protocol.** rclone **ADOPT as ops**, not product.

---

### 4.4 Git protocol for metadata

**Problem solved:** Efficient negotiation of commits/trees; packfiles; partial clone.

**Nudox fit:**  
- Package **manifests** could be stored as a git repo of YAML/JSON (moonshot §10).  
- Git protocol is optimized for source trees with renames/deltas — our blobs are already content-addressed; pack negotiation is a subset of want/have.  
- Running git servers for multi-tenant ACL is a product you do not want (forges are hard).

**Verdict: BORROW pack negotiation ideas; REJECT git server as INDEX.** jujutsu/gitoxide may still help **local** tooling (§5).

---

### 4.5 Syncthing model

**URLs:** https://syncthing.net  

**2026 status:** Mature P2P continuous file sync; device IDs, folder shares, introducer nodes, optional untrusted encrypted peers; no central cloud required.

**Problem solved:** Multi-device folder consistency without a vendor cloud.

**Nudox-relevant mechanics:**
1. **Device identity + explicit trust** (pair before share).  
2. **Global index of file versions** exchanged as metadata first; data pulled second.  
3. **Introducer** topology for mesh growth.  
4. Folder-level share ACLs.

**Mapping:**

| Syncthing | nudox |
|---|---|
| Device ID | Client installation identity |
| Folder | Project subscription set |
| File version vectors | Package generation pins |
| Block pulls | CAS hash pulls (iroh/HTTP) |

**Mismatch:** Syncthing is **multi-writer file sync** with conflict copies. nudox deps are **single-writer (INDEX) multi-reader**. Do not run Syncthing under the hood.

**Verdict: BORROW trust + metadata-first index exchange; REJECT embedding Syncthing.**

---

### 4.6 WebTorrent / BitTorrent v2 for package blob swarms

**URLs:** https://webtorrent.io · https://github.com/webtorrent/webtorrent · BitTorrent v2 spec  

**2026 status:** WebTorrent still active for browser/desktop streaming via WebRTC + classic BitTorrent bridges; npm package maintained; desktop app exists. BitTorrent v2 (hash trees / piece layers) improves large-tree integrity. **Not** a standard corporate package CDN replacement.

**Problem solved:** Peer bandwidth offload for popular immutable blobs; swarm resilience.

**Nudox fit (P3):**

| Aspect | Assessment |
|---|---|
| Popular crate IR shared across devs | Swarm helps **theoretically** |
| Enterprise firewalls / compliance | **Hostile** to random P2P |
| Hash alignment | BT piece hashes ≠ BLAKE3 CAS unless carefully mapped |
| Overlap with iroh (02) | **High** — iroh is the intentional modern CAS P2P bet |
| Seeding ethics / legal | Users seed third-party code continuously — product UX risk |

**Verdict: REJECT as primary.** **WATCH** only as research after iroh path exists; if iroh already provides peer fetch of BLAKE3 CIDs, WebTorrent is redundant. Prefer **iroh** (sibling 02) for any desktop swarm story.

---

### 4.7 HTTP range / CDN patterns (refresh)

Already central in plan 13 (docs.rs archive + Range). Edge reminder: lazy materialization for packages should look like **SOCI/eStargz** (§5.3) more than like BitTorrent.

**Verdict: ADOPT** (already in plan 13).

---

## 5. FS / materialization (beyond EdenFS)

**Cross-ref:** sibling 03 for EdenFS full depth. This section covers **other** 2026 angles.

### 5.1 gitoxide advanced checkout

**URLs:** https://github.com/GitoxideLabs/gitoxide · used by Jujutsu as Git backend  

**2026 status:** Pure-Rust Git implementation; performance-oriented; library-first. Powers serious tools (jj). Checkout/index APIs continue to mature for embedding.

**Problem solved:** Programmatic, fast Git object access and working-tree materialization without shelling to `git`.

**Nudox fit:**  
- If any metadata or source is stored in Git-compatible object form, gitoxide is the right Rust library.  
- For **CAS package trees**, gitoxide is optional — only if we deliberately choose Git tree encoding.  
- Useful for **consumer project** integration (open a user’s git worktree, enumerate files for resolution plan).

**Verdict: ADOPT as library** for Git-interop and consumer tree walking; **not** required as package CAS format.

---

### 5.2 Jujutsu (jj) working copies

**URLs:** https://github.com/jj-vcs/jj · https://docs.jj-vcs.dev/latest/working-copy/  

**2026 status:** Rapidly growing Git-compatible VCS; **working-copy-as-a-commit**; automatic snapshot; colocated git backend via gitoxide; production-usable for many developers.

**Problem solved:** Simplify WC mental model; conflicts as first-class; multi-workspace.

**Nudox fit (P4):**

| Idea | Use in nudox |
|---|---|
| WC is a commit | Materialized compile tree could be a **snapshotted generation view** |
| Operation log | Debug “what did sync change” |
| Multi-workspace | Multiple project checkouts sharing store |

**Do not:** make users install jj; make package store a jj repo.

**Verdict: BORROW WC-as-snapshot semantics for local materialization state machine; REJECT jj as dependency of REGISTRY.**

---

### 5.3 Sapling without full Eden

**URLs:** https://github.com/facebook/sapling  

**2026 status:** Meta’s Git-compatible client; pairs with EdenFS for huge monorepos; can operate in more normal modes for smaller repos.

**Nudox fit:** Without Eden, Sapling is “another Git client.” Eden path is sibling 03. Limited streamlining if not already in Meta ecosystem.

**Verdict: REJECT** as product dependency; read Sapling/Eden papers for sparse checkout UX only (**BORROW**).

---

### 5.4 eStargz / SOCI — lazy container images as package tree analogy

**URLs:**  
- eStargz / stargz-snapshotter: https://github.com/containerd/stargz-snapshotter (v0.18.x as of early 2026)  
- SOCI: https://github.com/awslabs/soci-snapshotter · production on EKS/Fargate; 2026 arxiv https://arxiv.org/html/2607.06868v1  
- Industry notes: Grab engineering 2026 lazy loading comparison  

**2026 status:**

| Format | Approach | Compatibility |
|---|---|---|
| **eStargz** | Recompress layer so each file is its own gzip member; TOC in layer | Still valid OCI; works with stargz-snapshotter |
| **SOCI** | **External** zTOC index via OCI referrers; original layers untouched | Better for existing images; parallel pull; production AWS |

**Key lesson (Grab et al.):** Lazy loading **redistributes** download cost; SOCI can preserve app start latency better than eStargz by index design.

**Package tree analogy for nudox:**

```
OCI image layer          →  package generation archive (ZIP/tar+zstd)
SOCI zTOC / eStargz TOC  →  archive member offset index (plan 13)
Lazy snapshotter mount   →  FUSE/Eden-like materialization (03) OR
                            on-demand Range GET + local CAS put
First open of file X     →  fetch only member X (+ parents if tree walk)
```

**Streamlining:** Implement **SOCI-shaped external index** next to each generation archive in object storage:

```
cas/blobs/<blake3>           # archive
cas/indexes/<blake3>.ztoc    # member offsets + compression checkpoints
```

Compiler/materializer asks for paths → index → range fetch → local CAS.

**Verdict: BORROW aggressively (format-level)**; **REJECT** running containerd snapshotters on developer laptops as the primary mechanism.

---

### 5.5 erofs + composefs

**URLs:**  
- composefs: https://github.com/containers/composefs  
- ostree composefs notes: https://ostreedev.github.io/ostree/composefs/  
- EROFS kernel docs; Fedora Atomic / bootc adoption trajectory 2024–2026  

**2026 status:** composefs combines **EROFS metadata** + **overlayfs** + content-addressed data objects for **truly read-only** trees with fs-verity integrity. Default direction for Fedora Atomic Desktops / CoreOS / bootc world. ostree integration matured; still treat some paths as evolving.

**Problem solved:** Immutable, integrity-verified, deduplicated filesystem trees for OS and container rootfs — mount without unpacking full trees into mutable directories.

**Nudox fit (P4 Linux compile materialization):**

| Need | composefs/erofs |
|---|---|
| Mount package source tree RO for sandbox compile | **Excellent on Linux** |
| macOS desktop (primary GPUI target) | **Weak / unavailable** as first-class |
| Per-package generation switch | Natural (mount different metadata roots, shared objects) |
| Cross-platform product | Need fallback path |

**Recommended posture:**
- **Linux CI / k8s compile pods:** WATCH→ADOPT composefs-style mounts for toolchain+source inputs when sandbox design allows.  
- **macOS local REGISTRY:** keep CAS + ordinary directory materialization or Eden-class approach (03).  
- Share **object naming** with CAS (blake3) so Linux mounts and macOS copies use same store layout.

**Verdict: BORROW design; ADOPT on Linux sandbox path when ready; not a cross-platform panacea.**

---

### 5.6 ostree / casync (refresh vs plan 14)

Already in plan 14 as content-addressed distribution cousins. 2026: ostree remains core of atomic desktops; casync still niche but correct intellectually.

**Verdict: BORROW** chunk + index ideas; **REJECT** ostree as user-facing package manager for nudox packages.

---

## 6. Compute / sandbox

### 6.1 WebAssembly component model + WASI 0.3

**URLs:**  
- WASI: https://github.com/WebAssembly/WASI (v0.3.0 tagged **2026-06-11** per releases listing)  
- Component model: https://github.com/WebAssembly/component-model  
- Wasmtime; Bytecode Alliance  
- 2026 state writeups: Uno Platform “State of WebAssembly 2025 and 2026”; various WASI 0.3 async explainers  

**2026 status:**  
- **WASI 0.2** modular WIT worlds in production at edge (Cloudflare, Fastly, Fermyon class).  
- **WASI 0.3** brings **native async** (`future`/`stream`) into the component model — major usability jump.  
- **WASI 1.0** still the stabilization horizon (late 2026 / 2027 narratives).  
- Component model enables polyglot composition with capability-based imports (no ambient FS/net unless granted).

**Problem solved:** Portable, capability-safe execution of untrusted *modules* with tiny cold start — denser than containers for pure compute.

**Nudox fit (P7 producers):**

| Workload | WASM fit |
|---|---|
| Pure IR transforms (link, moniker normalize, graph project) | **Excellent** |
| Tree-sitter wrappers | Possible if ports/WASI FS granted |
| Full rustc / javac / tsc oracle | **Poor** (size, WASI gaps, existing native toolchains) |
| Multi-tenant plugin producers from third parties | **Excellent** long-term |
| Replace k8s sandbox for heavy compiles | **No** |

**Streamlining proposal:**

```
producer-worker
  ├── native sealed steps (today) — languages, oracles
  └── wasm component steps (new) — pure, auditable, fast cold start
        host: wasmtime
        grants: read CAS inputs, write output dir, no net
```

This can **collapse** some producer versioning pain (ship component digest as JobKey material) without replacing plan 12’s microVM story for untrusted full toolchains.

**Verdict: WATCH→ADOPT for pure pipeline stages; REJECT as sole sandbox for language oracles.**

---

### 6.2 gVisor / Kata — 2026 differential only

**Cross-ref:** plan 12 owns the deep sandbox decision.  

**2026 notes:**  
- AI-agent sandboxing boom reinforces **microVM (Kata/Firecracker) vs gVisor** as standard menu.  
- gVisor: userspace kernel, good density, syscall compatibility limits.  
- Kata: VM isolation, higher overhead, stronger boundary.  
- k8s RuntimeClass still the integration point.

**New angle for nudox:** none that changes plan 12’s hybrid warm/cold + sealed worker model. WASM (§6.1) is the genuinely new 2026 layer *above* containers for pure steps.

**Verdict: DEFER to plan 12; no change.** Mention agent-sandbox SIG docs only as ecosystem confirmation.

---

### 6.3 Nix as content-addressed world

**URLs:** https://nixos.org · Nix store model  

**Problem solved:** Pure builds, cryptographic store paths, reproducible toolchains, extensive binary cache graph.

**Nudox adjacency:**  
- `JobKey` / CAS already Nix-rhymes.  
- Desktop toolchains research (18) likely touches Nix.  
- Using Nix **as the product package DB** would force users into Nix UX — wrong for multi-language docs intelligence.

**Borrow:**  
- Input-hash → output-hash memoization (already CAS).  
- Transparent binary caches (HTTP NAR-like).  
- Sandbox purity principles for producers.

**Verdict: BORROW purity & store ideas; REJECT Nix as INDEX/REGISTRY.** Optional: support Nix as a **consumer project** ecosystem later.

---

## 7. Search / vectors — only breaking angles vs plan 09

Plan 09 already selected **qdrant-edge** (winner) + **LanceDB** runner-up + jina embeddings via fastembed. This section only records **new or alternative** 2026 edges.

### 7.1 USearch

**URLs:** https://github.com/unum-cloud/usearch  

**What it is:** Extremely fast similarity search library (C++/Rust bindings), not a full payload DB.

**Angle:** Could accelerate custom indices; **does not** replace filterable payload store.

**Verdict: REJECT as primary VectorStore;** optional ANN backend experiment only.

---

### 7.2 LanceDB (delta note)

Already runner-up in 09. 2026: strong embedded multimodal/columnar story; lakehouse adjacency.

**New angle:** If Ladybug/graph lakehouse paths store embeddings beside graph in columnar form, Lance might unify — **speculative**. Do not fork plan 09.

**Verdict: unchanged — runner-up ADOPT if Edge fails.**

---

### 7.3 sqlite-vec

**URLs:** https://github.com/asg017/sqlite-vec  

**2026 status:** Small C extension; vec0 virtual tables; runs everywhere SQLite runs (incl. WASM narratives); “fast enough” not SOTA ANN at huge scale.

**Angle for nudox:** INDEX already sqlite — **co-locate tiny vector tables** for admin/demo? Desktop REGISTRY could put toy vectors in sqlite — but plan 09 needs **10⁴–10⁶** with filters and <500MB; sqlite-vec is better for **small** per-user sets.

**Verdict: WATCH for micro-scale / SQL-joined filters; REJECT replacing qdrant-edge.**

---

### 7.4 Turbopuffer + SPFresh

**URLs:** https://turbopuffer.com · docs mention **SPFresh** incremental vector index  

**2026 status:** Managed object-storage-native search; multi-tenant SaaS economics; SPFresh for incremental indexing with high recall targets. Not OSS engine to embed.

**Angle:** Remote INDEX alternative to self-hosted Qdrant for **namespace-heavy multi-tenant** semantic search if ops cost dominates.

**Verdict: WATCH as possible remote vendor;** does not remove need for local edge. **REJECT** as desktop embed. Do not change plan 09 local choice.

---

### 7.5 Synthesis vectors

**No streamlining that collapses Tantivy + vectors into one magical system** is production-ready for nudox’s dual text/semantic UX in 2026. Hybrid retrieval remains two indexes + fusion — already planned.

---

## 8. Cross-cutting: orchestration-adjacent edge (brief)

Already deep in plan 12. Edge confirmations only:

| Tech | 2026 note | Verdict |
|---|---|---|
| Kueue | v0.18.x class; fair sharing | ADOPT (12) |
| KEDA | No sqlite scaler | Optional warm HPA |
| NATS | See §4.1 | Optional |
| Agent sandbox + gVisor/Kata | Ecosystem noise, same tech | DEFER 12 |

---

## 9. Combinations that simplify 22-crate topology

### 9.1 Recommended “lean edge” stack (does not explode crates)

```
┌─────────────────────────────────────────────────────────────┐
│ SHARED: heart, ir, moniker, compiler-wire, blob             │
└─────────────────────────────────────────────────────────────┘
┌──────────────────┐  ┌──────────────────┐  ┌────────────────┐
│ registry-local   │  │ index-service    │  │ orch           │
│ sqlite catalog   │  │ sqlite+Litestream│  │ Kueue+warm     │
│ DiskCas          │  │ S3 Cas           │  │ sandbox/native │
│ tantivy          │  │ tantivy          │  │ optional NATS  │
│ qdrant-edge      │  │ qdrant-client    │  │ optional WASM  │
│ graph-over-ir    │  │ graph-over-ir    │  │   components   │
│ optional Ladybug │  │ terminus-client  │  └────────────────┘
└────────┬─────────┘  └────────┬─────────┘
         │                     │
         └──────── client ─────┘
              want/have + HTTPS
              optional iroh (02)
```

**Crate impact:** **Zero new top-level crates** if Ladybug/WASM stay behind existing traits (`PackageGraph`, producer step trait). iroh may justify `blob` transport feature flags rather than new packages.

### 9.2 Combination A — “Desktop smart, server classic” (preferred)

| Layer | Choice |
|---|---|
| INDEX meta | SQLite + Litestream (11) |
| INDEX blobs | S3 + archive index (SOCI-like) (13) |
| INDEX hot graph | Terminus admission (07) |
| Local graph | graph-over-IR + optional Ladybug |
| Local vectors | qdrant-edge (09) |
| Sync | Generation want/have (14) + optional iroh (02) |
| Materialize | Range+CAS; Eden-class optional (03); composefs on Linux CI |
| Orch | Kueue + sandbox (12); WASM pure steps |

**Simplifies:** Clear authority; no CRDT; no second SQL dialect; cold graph portable.

### 9.3 Combination B — “Versioned SQL experiment”

| Layer | Choice |
|---|---|
| INDEX meta | **DoltLite** or Doltgres (01) instead of stock SQLite |
| Everything else | Same as A |

**When:** Product requirement for branch/merge of catalog and time-travel SQL.  
**Cost:** Alpha risk (DoltLite) or ops process (Doltgres); sqlx story; backup tools differ from Litestream.  
**Crate impact:** `meta-store` grows a backend; still one trait.

### 9.4 Combination C — “P2P CAS mesh” (post-MVP)

| Layer | Choice |
|---|---|
| Blob transport | iroh (02) among desktops + INDEX gateway |
| Optional | BitTorrent-class swarm **rejected** unless iroh insufficient |
| Catalog | Still server-authoritative sqlite |

**Simplifies bandwidth;** does not simplify security/ACL design (harder).

### 9.5 Combination D — “Graph monorepo” (reject)

Ladybug/Falkor/Terminus **only** — drop IR cold path.  

**Why not:** Loses generation-portable graph, offline purity, and CAS-diff lineage. Violates 07/20.

### 9.6 Combination E — “Lakehouse INDEX” (reject)

Iceberg + Nessie + Athena/Spark for package catalog.  

**Why not:** Interactive symbol search/graph is not lakehouse SQL; ops alien; client sync worse.

### 9.7 Combination F — “Everything CRDT” (reject)

cr-sqlite + Electric + Automerge catalog.  

**Why not:** Plan 14 — multi-writer package metadata is a footgun; CAS already handles immutable merge (hash equality).

### 9.8 Topology edit suggestions (only if edge ADOPTs land)

| If we adopt… | Topology change |
|---|---|
| Ladybug optional | Feature on `graph-cold`, not new crate |
| WASM producers | Feature/module under `compiler-core` or `producer-worker` |
| iroh | Feature on `blob` / `client` transport |
| DoltLite | `meta-store` backend feature |
| composefs helper | Linux-only module under `sandbox` |
| NATS | Optional dep of `orch` + `index-service` outbox dispatcher |

**Do not add crates:** `nessie-client`, `electricsql`, `webtorrent-sys`, `falkor-client` (unless Terminus is removed by a future ADR).

---

## 10. Moonshots — “could collapse half the design?”

### 10.1 “INDEX is just a git/dolt repo of manifests + CAS”

**Pitch:** Every package generation is a commit; tree points at CAS hashes; clients `git fetch` metadata; blobs from S3/iroh.

**What collapses:** Custom catalog schema? Multi-tenant SQL? Outbox table?

**What does not:**
- ACL / org / billing / session graphs still need structured store or forge-class product.  
- Search/vector/graph indexes still derived.  
- Job orchestration still k8s.  
- Git multi-tenant hosting is a company (GitHub).  
- Partial subscribe of “shapes” is awkward in raw git.

**Honest verdict:** **Beautiful for open-source mirror mode**; **insufficient for multi-tenant SaaS INDEX**. Could be an **export format** (`nudox clone --git-metadata`) without being the SoT. Dolt/DoltLite are the SQL-shaped version of this moonshot — same limits.

**Score:** 3/10 collapse · **BORROW as export / offline mirror**

---

### 10.2 “Everything is a CRDT”

**Pitch:** Local-first package DB; merge anywhere; no server authority.

**Fatal issues:**
- Security: malicious merge of package provenance.  
- Identity: who published serde 1.0.210?  
- Compiles must be deterministic on **pinned** generations — CRDT “merge both” is wrong.  
- Plan 14 already closed this.

**Score:** 0/10 collapse · **REJECT**

---

### 10.3 “Only object storage + Athena/DuckDB”

**Pitch:** No INDEX sqlite; catalog is Parquet in S3; query with Athena/DuckDB; CAS alongside.

**Works for:** Analytics, batch audit, data science over ecosystem.

**Fails for:**  
- Transactional outbox + resolution state machine.  
- Low-latency GUI catalog lookups offline.  
- Multi-tenant authz at row level without a catalog service.  
- Client sync protocol needs a session/API surface anyway → you reinvent INDEX.

**Score:** 2/10 collapse · **BORROW for analytics lake export only**

---

### 10.4 “Terminus for all graphs + kill graph-over-IR”

**Pitch:** One system; simpler mental model.

**Fails:** Universe-scale cost; demotion/rehydration; desktop offline; plan 07 evidence.

**Score:** 1/10 · **REJECT**

---

### 10.5 “Embedded Ladybug/Kuzu everywhere; kill Terminus”

**Pitch:** Same embed on server (many files) + desktop.

**Fails:** Multi-tenant ops, versioned commit product features, hot admission, HA. Server wants a real service. Kuzu upstream dead; Ladybug young.

**Score:** 2/10 · **REJECT as sole graph**

---

### 10.6 “iroh + CAS + no INDEX API”

**Pitch:** Pure P2P package universe.

**Fails:** Discovery, trust, search, billing, compile orchestration, malicious content.

**Score:** 1/10 · **REJECT** (iroh as transport under INDEX is fine)

---

### 10.7 “Nix is the registry”

**Pitch:** Export all packages as Nix derivations; use binary cache.

**Fails:** Multi-language non-Nix users; GUI product; Windows; search UX.

**Score:** 1/10 · **REJECT as product core**

---

### 10.8 “WASM components replace compiler daemon fleet”

**Pitch:** Every producer is a component; edge runs them.

**Partial truth:** Pure steps yes; oracles no. Still need orch for heavy jobs.

**Score:** 4/10 partial · **WATCH hybrid**

---

## 11. Explicit rejects (consolidated)

| Technology | Reason |
|---|---|
| **Upstream Kùzu** | Archived Oct 2025; Apple acquisition; no security maintenance |
| **Neo4j / Memgraph / Dgraph as core** | Ops+license+weight; no CAS-native generation model |
| **Apache AGE** | Requires Postgres; conflicts with plan 11 |
| **TypeDB as store** | Niche; heavy; low leverage on IR |
| **FalkorDB as default hot graph** | AGPL/ops; weaker versioning than Terminus; duplicate |
| **Nessie/Iceberg/LakeFS as INDEX SoT** | Wrong runtime domain; good metaphors only |
| **FoundationDB Record Layer** | Ops scale mismatch |
| **Materialize for symbols** | Cloud IVM for low-rate events; lock-in |
| **ElectricSQL / PowerSync / cr-sqlite for INDEX or CAS** | Wrong authority model; Postgres gravity; multi-master danger |
| **WebTorrent primary swarm** | Compliance + overlap with iroh; hash mismatch |
| **Syncthing embedded** | Multi-writer files; not package authority |
| **rsync as client protocol** | Inferior to hash want/have for CAS |
| **jj/Sapling as package WC manager** | User/VCS confusion; wrong layer |
| **containerd snapshotters on desktop** | Analogy only |
| **USearch / sqlite-vec as primary vectors** | Incomplete vs plan 09 winner |
| **Turbopuffer as desktop** | SaaS remote only |
| **“Everything CRDT”** | Security + pin semantics |
| **Athena-only INDEX** | No transactional control plane |
| **Git forge as multi-tenant INDEX** | Product scope explosion |
| **WASM for full oracles** | Toolchain reality |
| **Dolt MySQL-wire** | Dialect mismatch; no edge over Doltgres/SQLite |

---

## 12. Open questions

### 12.1 Must answer before adopting

1. **LadybugDB:** Production Rust bindings? License of all deps? Single-writer file locking vs GPUI threads? Memory at 10⁴ packages?  
2. **DoltLite:** sqlx/rusqlite compatibility? Litestream-equivalent backup? Write throughput vs stock SQLite for INDEX outbox? Alpha exit criteria?  
3. **iroh vs HTTPS-only (sibling 02):** Is P2P worth ACL + NAT complexity for v1?  
4. **composefs in sandbox pods:** Does plan 12 cage allow custom mounts + erofs kernel modules on node images?  
5. **Terminus SLO:** Do we have numbers that would trigger Falkor evaluation?  
6. **WASM producers:** Which pipeline stages are pure enough to componentize first (graph project? moniker normalize?)?

### 12.2 Product / policy

7. Multi-tenant model: single sqlite vs db-per-org vs FDB-class future?  
8. Are catalog branches a user-facing feature (DoltLite motivation) or only generation pins?  
9. Enterprise air-gap: must swarm/P2P be compile-time disabled?  
10. Linux-only materialization accelerators acceptable behind trait, or does macOS parity gate every FS feature?

### 12.3 Cross-report dependencies

11. Finalize Doltgres (01) vs SQLite (11) vs DoltLite — **one** versioned-SQL story.  
12. EdenFS (03) vs SOCI-index+Range vs composefs — pick **desktop** vs **CI** materialization matrices.  
13. IPFS (04) vs iroh (02) vs plain S3 — one blob transport narrative for client docs.

### 12.4 Research spikes (small)

| Spike | Exit criterion |
|---|---|
| Ladybug load of 1 graph projected from 50 package IRs | Query latency + RSS vs graph-over-IR |
| SOCI-like index for existing ZIP CAS layout | p95 single-file open without full download |
| Wasmtime component for `from_ir` graph project | Deterministic output hash = native |
| DoltLite branch of toy catalog schema | Merge conflict UX vs generation pins |

---

## 13. Top streamlining recommendations

### 13.1 Do now (aligns with existing plans; edge-informed)

1. **Keep plan 11 SQLite INDEX** — do not derail for lakehouse or FDB.  
2. **Keep plan 14 manifest want/have** — reject CRDT engines for CAS/catalog authority.  
3. **Implement SOCI-like external archive indexes** in storage layout (13) — highest ROI lazy materialization without Eden.  
4. **Preserve graph-over-IR as cold default** (20); Terminus hot-only (07).  
5. **Stay with qdrant-edge + Tantivy** (09/10); no new vector product.  
6. **Shape/bucket vocabulary** in client-core for subscriptions (Electric/PowerSync **words only**).  
7. **Optional NATS** only when outbox/warm path proves need (12).

### 13.2 Do next (edge bets worth spikes)

8. **LadybugDB spike** behind `PackageGraph` — potential desktop Cypher without Terminus.  
9. **WASM component spike** for pure producer steps — density + reproducibility.  
10. **composefs/erofs** on Linux compile pods — immutable inputs.  
11. **iroh** evaluation per sibling 02 for blob swarm among desktops **after** HTTPS path works.  
12. **DoltLite watchlist** — re-evaluate if catalog branching becomes a roadmap item.

### 13.3 Explicitly do not do

13. Do not rebuild INDEX on Nessie/Iceberg/Athena.  
14. Do not embed Syncthing/WebTorrent/Electric.  
15. Do not depend on archived Kuzu.  
16. Do not add Neo4j/Falkor alongside Terminus “just in case.”  
17. Do not collapse planes in 22-crate topology for moonshot purity.

---

## 14. Per-tech verdict card index (quick lookup)

| Tech | Area | Verdict | Primary URL |
|---|---|---|---|
| Dolt (MySQL) | Catalog | REJECT | https://www.doltdb.com |
| DoltLite | Catalog | WATCH α | https://github.com/dolthub/doltlite |
| Doltgres | Catalog | SIBLING 01 | (see 01) |
| Nessie | Catalog versioning | BORROW | https://projectnessie.org |
| Iceberg | Table format | BORROW | https://iceberg.apache.org |
| LakeFS | Object versioning | BORROW | https://lakefs.io |
| FoundationDB RL | Multi-tenant | REJECT | https://foundationdb.github.io/fdb-record-layer/ |
| Materialize / DD | IVM | BORROW theory / REJECT product | https://materialize.com |
| ElectricSQL | Sync | BORROW shapes / REJECT | https://electric-sql.com |
| PowerSync | Sync | BORROW buckets / REJECT | https://www.powersync.com |
| cr-sqlite | Sync | REJECT INDEX | https://github.com/vlcn-io/cr-sqlite |
| Kùzu upstream | Graph | REJECT (archived) | https://github.com/kuzudb/kuzu |
| LadybugDB | Graph embed | WATCH→ADOPT? | https://ladybugdb.com |
| FalkorDB | Graph server | REJECT default | https://www.falkordb.com |
| Neo4j | Graph | REJECT core | https://neo4j.com |
| Memgraph | Graph | REJECT | https://memgraph.com |
| Apache AGE | Graph | REJECT | https://age.apache.org |
| TypeDB | Knowledge | REJECT store | https://typedb.com |
| Dgraph | Graph | REJECT | https://dgraph.io |
| RDF stores | Graph | BORROW export | (Oxigraph etc.) |
| ArcadeDB | Graph | REJECT core | https://arcadedb.com |
| NATS JetStream | Transport | ADOPT optional | https://nats.io |
| WebTransport/H3 | Transport | BORROW later | MDN WebTransport |
| rsync/zsync | Transport | BORROW ideas | — |
| rclone | Ops | ADOPT ops | https://rclone.org |
| Git protocol | Meta | BORROW | — |
| Syncthing | Sync model | BORROW model | https://syncthing.net |
| WebTorrent/BT2 | Swarm | REJECT primary | https://webtorrent.io |
| gitoxide | FS/Git | ADOPT lib | https://github.com/GitoxideLabs/gitoxide |
| Jujutsu | WC model | BORROW | https://github.com/jj-vcs/jj |
| Sapling | VCS | REJECT dep | https://github.com/facebook/sapling |
| eStargz | Lazy tree | BORROW | https://github.com/containerd/stargz-snapshotter |
| SOCI | Lazy tree | BORROW | https://github.com/awslabs/soci-snapshotter |
| erofs+composefs | Immutable FS | ADOPT Linux path | https://github.com/containers/composefs |
| WASM components | Sandbox | WATCH→ADOPT pure | https://github.com/WebAssembly/WASI |
| gVisor/Kata | Sandbox | DEFER 12 | https://gvisor.dev |
| Nix | CAS world | BORROW purity | https://nixos.org |
| USearch | Vectors | REJECT primary | https://github.com/unum-cloud/usearch |
| LanceDB | Vectors | Runner-up (09) | https://lancedb.com |
| sqlite-vec | Vectors | WATCH micro | https://github.com/asg017/sqlite-vec |
| Turbopuffer/SPFresh | Vectors | WATCH remote SaaS | https://turbopuffer.com |
| iroh | Blobs | SIBLING 02 | (see 02) |
| EdenFS | Materialize | SIBLING 03 | (see 03) |
| IPFS | CAS | SIBLING 04 | (see 04) |
| Terminus | Hot graph | ADOPT hot (07) | https://terminusdb.com |

---

## 15. Mapping back to product problems (detailed)

### 15.1 P1 — INDEX metadata + multi-tenant without Postgres

**Best path:** Plan 11 (SQLite + Litestream + app-level tenancy).  
**Edge upgrades:** DoltLite if branches matter; Nessie vocabulary for multi-table publish atomicity; FDB only at mythical scale.  
**Anti-path:** Electric (brings Postgres back), AGE, lakehouse SoT.

### 15.2 P2 — Versioned symbol/graph history without full Terminus

**Best path:** graph-over-IR cold + Terminus hot admission (07/20).  
**Edge upgrades:** Ladybug local materialization; differential Z-set apply for deltas.  
**Anti-path:** Kuzu-dead-end, Neo4j-everywhere, kill cold IR.

### 15.3 P3 — Local↔remote CAS + catalog sync

**Best path:** Generation manifests + want/have (14); HTTPS range; optional iroh (02).  
**Edge upgrades:** shapes/buckets vocabulary; Syncthing trust model for multi-device user identity later.  
**Anti-path:** CRDT engines, WebTorrent primary, PowerSync writeback.

### 15.4 P4 — Efficient source tree materialization

**Best path:** CAS archive + member index (13); optional Eden (03).  
**Edge upgrades:** SOCI/eStargz index design; composefs on Linux CI; jj WC-as-snapshot state machine.  
**Anti-path:** Full ostree client as product; snapshotter on macOS GPUI.

### 15.5 P5 — Incremental symbol pipelines + lineage

**Best path:** Generation + SymbolDelta + Terminus layers + IR declare diffs.  
**Edge upgrades:** Z-set mental model; WASM pure stages with content-addressed component digests.  
**Anti-path:** Materialize SaaS; full streaming graph DB for publish events.

### 15.6 P6 — Embedded search + vectors

**Best path:** Plan 09/10.  
**Edge upgrades:** sqlite-vec only for tiny auxiliary; Turbopuffer if remote ops economics demand.  
**Anti-path:** Replacing Edge mid-flight without trait boundary breach.

### 15.7 P7 — Untrusted compiles on k8s

**Best path:** Plan 12.  
**Edge upgrades:** WASM pure steps; composefs inputs; optional NATS.  
**Anti-path:** WASM-only fleet; CRDT job state.

---

## 16. Suggested ADR stubs (for assemblers)

### ADR-E05-1: Catalog authority remains single-writer SQLite  
**Status:** Proposed  
**Decision:** Do not introduce CRDT or lakehouse catalog SoT in 2026 librarification.  
**Consequences:** Litestream backup path; shapes are app-level; DoltLite deferred.

### ADR-E05-2: Cold graph remains IR-projected; optional embedded Cypher later  
**Status:** Proposed  
**Decision:** graph-over-IR mandatory; Ladybug optional feature; Terminus hot-only.  
**Consequences:** Kuzu removed from any drafts; trait stability more important than Cypher.

### ADR-E05-3: Lazy package materialization via archive indexes  
**Status:** Proposed  
**Decision:** SOCI-like external indexes for generation archives; FUSE/Eden optional.  
**Consequences:** Object store layout gains `indexes/`; compiler materializer API takes path lists.

### ADR-E05-4: No P2P blob swarm in v1 client  
**Status:** Proposed  
**Decision:** HTTPS(+optional iroh later); no WebTorrent.  
**Consequences:** Simpler compliance; bandwidth cost on INDEX/CDN.

### ADR-E05-5: WASM components for pure producer steps only  
**Status:** Proposed  
**Decision:** Native toolchains remain for oracles; Wasmtime for pure transforms.  
**Consequences:** Dual runtime in producer-worker; JobKey includes component digest.

---

## 17. Appendix A — Problem × tech matrix (compact)

|  | P1 | P2 | P3 | P4 | P5 | P6 | P7 |
|---|---|---|---|---|---|---|---|
| DoltLite | W | | | | w | | |
| Nessie/Iceberg/LakeFS | B | | B | | B | | |
| FDB RL | R | | | | | | |
| Materialize | | | | | B | | |
| Electric/PowerSync/cr-sqlite | R | | B | | | | |
| LadybugDB | | W | | | w | | |
| Falkor/Neo4j/… | | R | | | | | |
| NATS | | | o | | | | O |
| WebTransport | | | b | | | | |
| Syncthing model | | | B | | | | |
| WebTorrent | | | R | | | | |
| gitoxide/jj | | | | B | | | |
| eStargz/SOCI | | | | B | | | |
| composefs/erofs | | | | A_L | | | A_L |
| WASM components | | | | | A | | A |
| qdrant-edge (09) | | | | | | A | |
| sqlite-vec | | | | | | w | |
| Turbopuffer | | | | | | w_r | |
| iroh (02) | | | S | | | | |
| EdenFS (03) | | | | S | | | |
| IPFS (04) | | | S | | | | |

Legend: A=adopt, A_L=adopt Linux, B=borrow, b=borrow later, W=watch, w=weak watch, w_r=watch remote, O=optional adopt, R=reject, S=sibling.

---

## 18. Appendix B — Citation & source log (2026-07-16)

Non-exhaustive; major sources used in research:

**Versioned data**  
- https://www.dolthub.com/blog/2026-03-25-doltlite/  
- https://www.dolthub.com/blog/2026-04-27-why-doltlite/  
- https://github.com/dolthub/doltlite  
- https://projectnessie.org/releases/  
- https://projectnessie.org/iceberg/iceberg/  
- https://www.dremio.com/blog/data-lakehouse-versioning-comparison-nessie-apache-iceberg-lakefs/  
- https://arxiv.org/abs/1901.04452  
- https://foundationdb.github.io/fdb-record-layer/  
- https://materialize.com/blog/differential-from-scratch/  
- https://electric-sql.com/docs/intro  
- https://www.powersync.com/  
- https://github.com/vlcn-io/cr-sqlite  

**Graph**  
- https://github.com/kuzudb/kuzu (archived banner / v0.11.3)  
- https://ladybugdb.com/ · https://github.com/LadybugDB/ladybug  
- https://gdotv.com/blog/kuzu-legacy-embedded-graph-database-landscape/  
- https://www.falkordb.com/  
- https://age.apache.org · https://neo4j.com · https://memgraph.com · https://dgraph.io · https://typedb.com  

**Sync / transport**  
- https://docs.nats.io/nats-concepts/jetstream  
- https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API  
- https://datatracker.ietf.org/doc/draft-ietf-webtrans-http3/  
- https://syncthing.net/  
- https://webtorrent.io/  

**FS / materialization**  
- https://github.com/containerd/stargz-snapshotter  
- https://github.com/awslabs/soci-snapshotter  
- https://arxiv.org/html/2607.06868v1  
- https://github.com/containers/composefs  
- https://ostreedev.github.io/ostree/composefs/  
- https://github.com/jj-vcs/jj  
- https://docs.jj-vcs.dev/latest/working-copy/  
- https://github.com/GitoxideLabs/gitoxide  

**Compute**  
- https://github.com/WebAssembly/WASI  
- https://gvisor.dev/  
- https://nixos.org  

**Vectors**  
- https://github.com/asg017/sqlite-vec  
- https://turbopuffer.com/docs/vector  
- https://qdrant.tech/documentation/edge/ (plan 09)  
- https://lancedb.com  

**Internal**  
- `docs/research/librarification/{07,09,11,12,13,14,20,22}`  
- `docs/research/resolution/03-exhaustive-plan.md`  
- `docs/research/edge-tech/01–04` (siblings)

---

## 19. Appendix C — Glossary (edge × nudox)

| Term | Meaning here |
|---|---|
| **Shape** | Electric-style partial subscription; → client SubscribeSpec |
| **Bucket** | PowerSync-style sync priority set; → dependency closure |
| **Z-set** | Differential dataflow multiset with +/− weights; → SymbolDelta apply |
| **zTOC** | SOCI external table of contents for lazy gzip/layer reads |
| **eStargz** | Seekable tar.gz image format for lazy container pull |
| **composefs** | EROFS metadata + overlay + CAS objects for immutable trees |
| **Prolly tree** | Probabilistic B-tree used by Dolt/DoltLite for content-addressed SQL storage |
| **Hot/cold graph** | Terminus admitted vs IR-projected (07/20) |
| **Want/have** | Content-hash negotiation for CAS sync (14) |
| **Generation** | Sealed package build identity (heart / 08) |

---

## 20. Document control

| Field | Value |
|---|---|
| ID | edge-tech/05-surrounding-edge |
| Date | 2026-07-16 |
| Lines target | 900–1600 |
| Companion identical copy | `05-surrounding-edge/PLAN.md` |
| Supersedes | (none) |
| Does not supersede | librarification 07–14, 20, 22; edge 01–04 |
| Next actions | Spikes in §12.4; ADRs in §16 for assembler intake |

---

## 21. Final synthesis (read last)

The surrounding edge landscape in mid-2026 is rich in **metaphors** and thin in **drop-in replacements** for nudox’s already-good plan stack (SQLite INDEX, CAS, graph-over-IR, Terminus hot, qdrant-edge, Kueue).

**Three technologies actually threaten to simplify implementation effort:**

1. **SOCI/eStargz-style archive indexes** — make lazy materialization real without EdenFS day one.  
2. **LadybugDB** — only serious embedded Cypher path after Kuzu’s death; still optional.  
3. **WASM components (WASI 0.3)** — shrink pure producer surface and improve sealability.

**One technology family to keep firmly out of the authority path:**

- **CRDT / multi-master SQL sync** (Electric legacy ideas, cr-sqlite, PowerSync writeback) for packages.

**One moonshot that is honest as an export, dishonest as SoT:**

- **Git/Dolt repo of manifests + CAS.**

Everything else is either **sibling-owned** (Doltgres, iroh, Eden, IPFS), **ops-optional** (NATS), or **reject**.

The 22-crate topology does **not** need a rewrite for edge fashion. It needs **feature flags and trait backends** where spikes succeed — and ruthless rejection where edge tools solve last decade’s multiplayer note-taking problem instead of content-addressed code intelligence.

---

*End of 05 — Surrounding / Breaking Edge Technologies.*
