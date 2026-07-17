# Graph-over-IR: Cold-Path GraphStore Design

**Research date:** 2026-07-16  
**Scope:** Concrete design for serving graph ops without Terminus for the long tail of packages — load IR (and occurrences) from CAS blobs, project to an indexed in-memory graph, implement the same `GraphStore` surface as the hot Terminus adapter.  
**Feeds:** [04 IR audit](../04-ir-audit/PLAN.md) §4.5 graph-over-blobs sketch; [07 Terminus tiering](../07-terminus-tiering/PLAN.md) leaky-bucket admission + cold-path economics.  
**Non-goals:** Building code; full WOQL/GraphQL; master architecture plan; schema redesign of IR.

**Live anchors (absolute under workspace):**

| Concern | Path |
|---|---|
| `GraphStore` trait | `workspace/registry/runtime/graph/mod.rs:72–97` |
| Terminus `GraphStore` impl | same file `:591–639` |
| WOQL edge queries | same file `:538–587` |
| BFS expand | `workspace/registry/runtime/graph/expansion.rs:34–65` |
| Cross-version diff | `workspace/registry/runtime/graph/resolution.rs:29–81` |
| Structure assemble | `workspace/registry/runtime/graph/structure.rs:23–38` |
| In-memory contract tests | `workspace/registry/runtime/tests/graph_expansion.rs:24–77` |
| `GraphCorpus` / `project` | `workspace/compiler/graph/from_ir.rs:890–957` |
| Linker / stubs | `workspace/compiler/graph/link.rs:1–278` |
| Document model | `workspace/compiler/graph/model.rs:985–1094` |
| Blob manifest | `workspace/registry/blob/mod.rs:46–66` |
| `StoreLinks.graph` | `workspace/registry/metadata/mod.rs:32–43` |
| `SymbolId` / `EntryUri` | `workspace/heart/identity/symbol.rs:15–48` |
| moka stampede cache | `workspace/heart/cache/stampede.rs:48–95` |
| Server composition | `workspace/server/search/mod.rs:51–58, 128–149` |

---

## 0. Problem statement

Today the **only** production `GraphStore` is Terminus-backed `Graph<Live>` (`registry/runtime/graph/mod.rs:591`). Expansion, relatedness, and server `SymbolStore` all assume that backend. Plan 07 correctly states that Terminus cannot hold the multi-ecosystem universe; plan 04 sketches re-projecting IR into `GraphCorpus` and indexing it. Neither plan specifies:

1. exact adjacency index layouts,
2. how `SymbolId` (UUID) joins the compiler’s IRI/`uri` world,
3. memory budgets for desktop REGISTRY vs INDEX server,
4. parity with hot-tier APIs and graceful degradation,
5. when to promote/demote relative to the leaky-bucket scorer,
6. whether to re-project on load or store precomputed graph blobs.

This document is that missing design. **Cold path is the default.** Terminus is a privilege of packages with sustained graph-query demand (07).

```
                    ┌─────────────────────────────────────┐
  graph API call ──►│ TieredGraphRouter                   │
                    │  on_graph_query(weight) → scorer     │
                    └───────────┬─────────────────────────┘
              Hot? (status∈{Hot} ∧ DB ready)   else
                    │                              │
                    ▼                              ▼
           Graph<Live> Terminus          ColdGraph / GraphView
           WOQL + Document API           IR blob → project → indexes
                    │                              │
                    └────────── GraphStore ────────┘
```

Routing rule (from 07, restated): **Warming and Cooling answer from cold until Terminus is ready / after soft demotion.** Never brown out reads waiting on promotion.

---

## 1. Inventory: GraphStore operations today

### 1.1 Trait surface (read)

Declared at `registry/runtime/graph/mod.rs:72–97`:

| Method | Signature intent | Hop class | Terminus implementation |
|---|---|---|---|
| `get_occurrences(item)` | Stream of `Scored<SymbolId>` whose **declaration/signature holds** `item` | **Single-hop reverse** | WOQL: `Relation` with `kind=Occurrence`, `to=item` → yield `from` (`:594–605`) |
| `get_references(item)` | Stream of callers/users that **point at** `item` | **Single-hop reverse** | Same pattern, `kind=Reference` (`:607–617`) |
| `are_related(from, to)` | Direct edge kind if any | **Single-doc / direct edge** | WOQL: find `Relation` with matching `from`/`to`, bind `kind` (`:619–638`) |

All three are **depth-1**. They do **not** require multi-hop path search.

### 1.2 Expansion (multi-hop, not on the trait)

`Graph::expand` (`expansion.rs:67–79`) is an inherent method, not part of `GraphStore`. It uses pure `expand_via` (`:34–65`):

- Input: `origin`, `ExpansionBounds { depth, breadth }`
- Primitive: `outgoing_edges(node) → Vec<(RelationKind, SymbolId)>` (`mod.rs:564–587`)
- Semantics: BFS; at most `depth` levels; at most `breadth` edges per expanded node; score `1/depth`; multi-edge reporting allowed, expand-once.

Server `related_hits` uses depth=1, breadth=16 (`server/search/mod.rs:87–90`).

**Cold path must provide `outgoing_edges` (or equivalent) so `expand_via` is shared.** Do not reimplement BFS.

### 1.3 Pure helpers (no store)

| Helper | File | Role | Cold/hot shared? |
|---|---|---|---|
| `resolution::diff` | `resolution.rs:29–81` | Cross-version FQ/name/kind match on `heart::Symbol` slices | **Yes** — pure |
| `structure::assemble` | `structure.rs:23–38` | Join symbols + parent map → `StructureNode` | **Yes** — pure |
| `expand_via` | `expansion.rs:34–65` | Bounded BFS over a neighbor fetch | **Yes** — pure |

### 1.4 Write path (hot only)

`Graph::insert_symbols` (`mod.rs:442–494`) writes **thin** `heart::Symbol` documents (`@type Symbol`, uuid `@id`, plain/fq/kind/package/ecosystem) — **not** the compiler’s full `GraphCorpus` wave emit (`linked_data/emit.rs`). This is a **known wiring gap** (04 §4.4): runtime Relation model vs compiler Symbol-edge model diverge.

Cold path **never** writes Terminus. Writes to CAS (IR, occ, optional graph postcard) are the generate/blob pipeline’s job.

### 1.5 RelationKind ↔ compiler edges (semantic map)

`RelationKind` (`mod.rs:50–64`):

| RelationKind | Doc meaning | Compiler GraphCorpus source |
|---|---|---|
| `Member` | target is member of source | Invert `Symbol.member_of` (child→parent) → emit `(parent, Member, child)` |
| `Reference` | source references target | `Reference` documents (`source`→`target`) from `project_references` |
| `Occurrence` | target occurs in source’s decl/sig | Flatten `mentions` ∪ `takes` ∪ `returns` as `(holder, Occurrence, mentioned)` |
| `Implements` | source implements target | `Symbol.implements` + optional `Implementation` nodes (edge is enough for GraphStore) |
| `Extends` | source extends target | `Symbol.extends` |
| `ReExport` | source re-exports target | **Not projected today** — see §1.6 |

### 1.6 Gaps in today’s graph materialization

1. **No Relation documents from full GraphCorpus** on the live insert path — only thin Symbols (`insert_symbols`).
2. **`symbol_id: None` in compiler projection** (`from_ir.rs:1130`) — uploader stamp missing.
3. **`ReExport`** has no IR→graph edge yet (aliases exist on Symbol but are not edges).
4. **OccurrenceSet not in `BlobManifest`** (`blob/mod.rs:46–66`) — only legacy `references_ref`.
5. **Dual reference pipelines** (CST ResolvedReference vs OccurrenceSet) — cold path must prefer occurrences when present.

Cold design assumes the **target** end-state from 04/07: OccurrenceSet in blobs + full edge projection + stamped SymbolIds. Interim degradation is specified in the parity matrix (§8).

### 1.7 Hop-class summary for indexing

```
Single-hop reverse (need reverse index):
  get_occurrences  ← Occurrence edges by target
  get_references   ← Reference edges by target

Single-hop forward / pair lookup:
  are_related      ← adjacency set or (from,to) map
  outgoing_edges   ← forward adjacency for BFS

Multi-hop:
  expand / related_hits ← expand_via over outgoing_edges only

Cross-version (no hop):
  resolution::diff on two symbol lists

Multi-package:
  following External / ~extern stubs may require loading dep packages
  (not a Terminus federated query — app-orchestrated; see §6)
```

---

## 2. Dual document models (must unify at the trait boundary)

There are **two** graph shapes in the tree. Cold path must make this explicit and pick the **trait boundary** as the single consumer contract.

### 2.1 Compiler / publish model (`GraphCorpus`)

```890:897:workspace/compiler/graph/from_ir.rs
pub struct GraphCorpus {
    pub packages: Vec<m::Package>,
    pub version: Option<m::PackageVersion>,
    pub symbols: Vec<m::Symbol>,
    pub implementations: Vec<m::Implementation>,
    pub references: Vec<m::Reference>,
}
```

- Identity: client-minted IRIs (`Symbol/{lang}%2F{pkg}%2F{fq}`) + logical `uri` (`lang/pkg/fq` with real `/`) — `model.rs:1007–1048`, `link.rs:66–74`.
- Edges live **on** Symbol fields (`member_of`, `implements`, `extends`, `mentions`, `takes`, `returns`) plus reified `Implementation` / `Reference`.
- Version-agnostic Symbol IRIs; membership via `PackageVersion.declares`.

### 2.2 Runtime / query model (`Relation` + uuid Symbol)

Documented assumption at `mod.rs:322–332`:

- `Symbol` docs keyed by uuid string properties
- `Relation` docs: `from` (uuid), `kind` (`RelationKind`), `to` (uuid)
- Queries in WOQL over that model

`MemoryGraph` in tests is literally `Vec<(SymbolId, RelationKind, SymbolId)>` (`graph_expansion.rs:27–29`) — **this is the minimal semantic core of GraphStore**.

### 2.3 Unification rule

| Layer | Owns | Does not own |
|---|---|---|
| CAS / IR | Source of truth for cold packages | Query indexes |
| `from_ir::project` | GraphCorpus (publish + cold intermediate) | SymbolId stamping (unless given instance token) |
| **ColdGraph indexes** | Edge list in **RelationKind + SymbolId** (MemoryGraph shape) + uri maps | WOQL |
| Terminus hot | Same edge semantics via Relation docs (target) or WOQL over Symbol fields (future) | IR blobs |
| `GraphStore` trait | SymbolId-keyed streaming API | Storage |

**Recommendation:** treat **flattened edges** `(SymbolId, RelationKind, SymbolId)` as the **shared semantic IR of GraphStore**. Both Terminus adapter and ColdGraph lower into it. Keep GraphCorpus for publish / shape / lineage membership; do not force GraphStore consumers to understand TdbLazy.

### 2.4 Target hot-path correction (out of cold scope but required for parity)

Long-term hot path should either:

- **(A)** emit Relation documents from GraphCorpus at promote time, or  
- **(B)** rewrite WOQL to walk Symbol.member_of / mentions / … fields matching GraphCorpus.

Until then, cold path implementing MemoryGraph-equivalent edges will be **ahead** of live Terminus reads for rich edges. Parity matrix marks this. Librarification should choose **(A)** when implementing gated publish (07 P2) — one lowerer, two sinks.

---

## 3. Representation choice: re-project vs precomputed GraphCorpus

### 3.1 Options

| Option | What lives in CAS | Load path | Pros | Cons |
|---|---|---|---|---|
| **A. Re-project always** | `Index` (+ OccurrenceSet) only | `postcard decode IR` → `project` → build indexes | Single source of truth; no dual invalidation; projection already pure & sorted (`from_ir.rs:909–911`) | CPU on first cold load; need occ for full Reference edges |
| **B. Precomputed GraphCorpus postcard** | IR + `graph_ref` postcard(GraphCorpus) | decode corpus → indexes (skip project) | Faster warm load; same bytes as Terminus publish input | Extra CAS object; must rebuild on projection/schema change; GraphCorpus holds heavy Shape subdocs |
| **C. Precomputed lean ColdGraphBlob** | IR + `cold_graph_ref` (edges + id maps, **no Shape**) | decode lean blob → indexes | Small; optimal for GraphStore | New type; still dual-write with IR; Shape queries need IR anyway |
| **D. Hybrid (recommended)** | IR+occ always; optional `cold_graph_ref` for warm-not-hot packages | try lean blob; else project | Best economics; demotion-friendly | Two code paths (share index builder) |

### 3.2 Recommendation: **D — Hybrid with A as mandatory baseline**

**Day-1 / MVP:** Option **A only**. Reasons:

1. Plan 04 already asserts projection is pure enough to recompute (`04` §4.5, §10 rec 4).
2. Avoid inventing a second durable graph format before GraphStore semantics stabilize.
3. Desktop REGISTRY package working sets are small enough that project-on-miss is fine with moka.
4. Terminus publish (hot) can still call `project` at promote time from the same IR — one projector.

**Day-2 / scale:** Option **C optional artifact** (not full GraphCorpus) keyed as JobKey child tag e.g. `b"cold-graph-v1"`:

```text
ColdGraphBlob {
  schema_version: u32,           // bump on RelationKind map or id scheme change
  package_uri_stem: String,      // "rust/serde"
  generation: String,            // version string
  ir_hash: ContentHash,          // must match manifest.ir_ref or reject
  occ_hash: Option<ContentHash>,
  // compact tables:
  nodes: Vec<ColdNode>,          // symbol_id, uri, kind, resolved
  edges: Vec<ColdEdge>,          // from_idx, kind, to_idx  (u32 indices)
}
```

**Do not** store full GraphCorpus postcard as the primary cold cache:

- Shape subdocuments are large and unused by GraphStore reads.
- `TdbLazy` / EntityIDFor are Terminus-oriented; lean indices are smaller and stable.
- Re-promoting to Terminus needs GraphCorpus **or** re-project from IR anyway — keep IR as source of truth.

### 3.3 Cost model (order of magnitude)

| Step | Small pkg (~1k symbols) | Large pkg (~50k symbols) |
|---|---|---|
| Decode IR postcard | 1–5 ms | 20–80 ms |
| `project` + link | 2–15 ms | 50–300 ms |
| Build ColdGraph indexes | 1–5 ms | 20–100 ms |
| Total cold miss | ~5–25 ms | ~100–500 ms |
| Warm moka hit (index walk) | <0.1–1 ms | <0.1–2 ms |
| Hot Terminus simple WOQL RTT | ~3–15 ms | ~3–15 ms (indexed) |
| Hot multi-hop WOQL | 10–500 ms | highly variable |

Aligns with 07 §B.4: **simple fetches often do not justify Terminus**; **path/closure** under sustained load does. Cold miss cost is dominated by large-package project — moka residency is the primary optimization, not precomputed blobs, until hot-set pressure forces optional ColdGraphBlob.

### 3.4 When to write optional ColdGraphBlob

Write at generate/fan-out time **iff** any of:

- package is **Warming** or recently demoted from Hot (Cooling) — expected re-query,
- IR size > threshold (e.g. >5 MB postcard) **and** graph query count > 0 in last window,
- INDEX server batch job pre-warms top-N non-hot packages overnight.

Never block generate on ColdGraphBlob. Never require it for correctness.

---

## 4. Concrete types and trait bounds

### 4.1 Module placement (recommended)

```
workspace/registry/runtime/graph/
  mod.rs              # GraphStore, Graph<Live>, RelationKind  (existing)
  expansion.rs        # expand_via (existing)
  resolution.rs       # diff (existing)
  structure.rs        # assemble (existing)
  cold/
    mod.rs            # ColdGraph, GraphView, loader
    index.rs          # adjacency builders
    lower.rs          # GraphCorpus → edge list + id maps
    cache.rs          # moka / StampedeCache integration
    deps.rs           # cross-package stub resolution policy
```

Alternatively a small `nudox-graph-cold` crate if compiler↔registry dep cycles force it; **prefer co-location** under `runtime/graph/cold` first because it implements `GraphStore` next to the trait. `from_ir::project` stays in `compiler/graph` — registry already depends on compiler artifacts via generate/blob paths; if that dep is inverted today, expose a thin `graph_project` API crate. **Open wiring detail** — not a design blocker.

### 4.2 Core types

```rust
/// Content-addressed identity of one package generation's graph inputs.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct GraphGenerationKey {
    pub package: PackageId,       // versioned heart id
    pub ir_hash: ContentHash,     // BlobManifest.ir_ref
    pub occ_hash: Option<ContentHash>,
    /// Instance token used to salt SymbolId (Terminus Database name today).
    pub instance_token: SmolStr,
}

/// Compact node for GraphStore (no Shape).
#[derive(Clone, Debug)]
pub struct ColdNode {
    pub symbol_id: SymbolId,
    pub uri: String,              // lang/pkg/fq  (graph model.uri)
    pub iri: String,              // Symbol/{lang}%2F{pkg}%2F{fq}
    pub fq_name: String,
    pub name: String,
    pub kind: heart::SymbolKind,  // coarse join with search, OR graph kind + map
    pub graph_kind: /* graph::model::SymbolKind or mirrored enum */,
    pub resolved: bool,           // false ⇒ stub
    pub package_name: String,     // may be ~extern or dep name
}

#[derive(Clone, Copy, Debug)]
pub struct ColdEdge {
    pub from: u32,                // index into nodes
    pub to: u32,
    pub kind: RelationKind,
}

/// Fully indexed in-memory graph for one generation of one package
/// (+ optional stub nodes for externs).
pub struct ColdGraph {
    pub key: GraphGenerationKey,
    pub nodes: Arc<[ColdNode]>,
    /// Parallel arrays / maps — see §5.
    pub by_symbol_id: HashMap<SymbolId, u32>,
    pub by_uri: HashMap<String, u32>,
    pub by_iri: HashMap<String, u32>,
    /// Forward: node → list of (kind, target_idx)
    pub out: Vec<Vec<(RelationKind, u32)>>,
    /// Reverse by kind buckets for O(degree) get_* 
    pub rev_occurrence: Vec<Vec<u32>>, // holders of node i (Occurrence)
    pub rev_reference: Vec<Vec<u32>>,  // referrers of node i (Reference)
    /// Optional: full reverse multi-kind for are_related dual lookup
    pub rev_all: Vec<Vec<(RelationKind, u32)>>,
    /// Declares list for this PackageVersion (node indices of resolved locals)
    pub declares: Vec<u32>,
    /// Retained for shape/docs on demand — not required for GraphStore.
    /// None if built from ColdGraphBlob without shapes.
    pub corpus: Option<Arc<GraphCorpus>>,
    pub index: Option<Arc<ir::entry::Index>>,
}

/// Cheap handle used by GraphStore impl; may be Arc-cloned into streams.
#[derive(Clone)]
pub struct GraphView {
    inner: Arc<ColdGraph>,
}

/// Loader + cache façade used by the tier router.
pub struct ColdGraphStore {
    blobs: Arc<dyn BlobCas>,          // get_manifest, get(hash)
    cache: StampedeCache<GraphGenerationKey, Arc<ColdGraph>>,
    /// Optional secondary cache of decoded Index only (for shape/lineage).
    ir_cache: StampedeCache<ContentHash, Arc<ir::entry::Index>>,
    instance_token: SmolStr,
    dep_policy: DepLoadPolicy,
}
```

### 4.3 Trait bounds shared with Terminus adapter

```rust
// Existing — do not fork:
pub trait GraphStore: Send + Sync {
    type Error: StoreError;
    fn get_occurrences(&self, item: SymbolId)
        -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send;
    fn get_references(&self, item: SymbolId)
        -> impl Stream<Item = Result<Scored<SymbolId>, Self::Error>> + Send;
    async fn are_related(&self, from: SymbolId, to: SymbolId)
        -> Result<Option<RelationKind>, Self::Error>;
}

// New capability trait for expansion + tier router (both adapters):
#[async_trait]
pub trait GraphExpand: GraphStore {
    async fn outgoing_edges(
        &self,
        origin: SymbolId,
    ) -> Result<Vec<(RelationKind, SymbolId)>, Self::Error>;

    fn expand(
        &self,
        origin: SymbolId,
        bounds: ExpansionBounds,
    ) -> impl Stream<Item = Result<ExpandedEdge, Self::Error>> + Send
    where
        Self: Sync,
    {
        // default body: expand_via(origin, bounds, |n| self.outgoing_edges(n))
    }
}

// Optional richer surface (07 PackageGraph sketch) — phased:
#[async_trait]
pub trait PackageGraphView: GraphExpand {
    async fn get_symbol_view(&self, id: SymbolId) -> Result<Option<SymbolView>, Self::Error>;
    async fn declares(&self, version: &str) -> Result<Vec<SymbolId>, Self::Error>;
    async fn membership_diff(&self, from_v: &str, to_v: &str) -> Result<MembershipDiff, Self::Error>;
}
```

**MVP implements only `GraphStore` + `outgoing_edges`/`expand`.** `PackageGraphView` lands with multi-version lineage UX (07 P3+).

### 4.4 Error type

Reuse `GraphError` where possible; add variants rather than a parallel error:

```rust
// conceptual additions
GraphError::Cold {
    kind: ColdGraphError,
}
enum ColdGraphError {
    ManifestMissing,
    IrDecode(String),
    OccDecode(String),
    Project(String),
    SymbolIdUnknown,      // id not in this package graph (NotFound)
    DepIrUnavailable { dependency: String },
    CacheEvictedWhileStreaming, // rare; treat as retryable
}
```

Classify `DepIrUnavailable` as **non-fatal degradation** for expand (skip edge / stub) vs hard error for explicit multi-package APIs.

### 4.5 `GraphView` as `GraphStore`

```rust
impl GraphStore for GraphView {
    type Error = GraphError;

    fn get_occurrences(&self, item: SymbolId) -> impl Stream<...> + Send {
        let hits = self.lookup_rev(item, |g, i| &g.rev_occurrence[i]);
        futures::stream::iter(hits)
    }

    fn get_references(&self, item: SymbolId) -> impl Stream<...> + Send {
        let hits = self.lookup_rev(item, |g, i| &g.rev_reference[i]);
        futures::stream::iter(hits)
    }

    async fn are_related(&self, from: SymbolId, to: SymbolId)
        -> Result<Option<RelationKind>, Self::Error>
    {
        let g = &self.inner;
        let (Some(&fi), Some(&ti)) = (g.by_symbol_id.get(&from), g.by_symbol_id.get(&to)) else {
            return Ok(None);
        };
        Ok(g.out[fi as usize]
            .iter()
            .find(|(_, t)| *t == ti)
            .map(|(k, _)| *k))
    }
}
```

Match `MemoryGraph` test contract (`graph_expansion.rs:49–77`). Prefer first matching kind if multi-edge (same as Terminus “first binding” behavior at `mod.rs:635–637`).

### 4.6 Tiered router

```rust
pub struct TieredGraph {
    tiers: Arc<PackageTierManager>,     // 07
    hot: HotGraphFactory,               // PackageId → Graph<Live> if Hot+ready
    cold: ColdGraphStore,
}

impl GraphStore for TieredGraph {
    // record weighted query on every call (07 B.2)
    // if hot_ready(package_of(item)): delegate hot
    // else: cold.load(package_of(item)).await?.method(...)
}
```

**Problem:** today’s `GraphStore` methods take only `SymbolId`, not `PackageId`. SymbolId embeds package via `EntryUri` construction, but recovering PackageId from uuid requires a reverse index (sqlite/tantivy/metadata). Practical approaches:

1. **Router at a higher layer** that already knows package context (server handlers that resolved the symbol from search) — pass `GraphView` for that package.
2. **Global id → package map** in sqlite INDEX / local metadata (11-sqlite-index).
3. Extend trait later with package-scoped stores: `PackageGraphStore` per package, federation outside.

**Recommendation for MVP:** **package-scoped stores** (`ColdGraphStore::view(package) -> GraphView`) composed by server federation (already `SourceStores` per source — `server/search/mod.rs:128`). Do not put global multi-package routing inside `GraphView`. Cross-package expand is explicit (§6).

---

## 5. In-memory index layouts

### 5.1 Build algorithm

```
fn build_cold_graph(
    index: &Index,
    occ: &OccurrenceSet,          // empty set if missing
    ctx: PackageCtx,
    instance_token: &str,
    package_id: PackageId,
) -> ColdGraph {
    let corpus = project(index, occ, ctx);   // from_ir.rs:903
    lower_corpus(corpus, instance_token, package_id)
}

fn lower_corpus(...) -> ColdGraph {
    // 1. Allocate nodes for every Symbol in corpus.symbols (incl stubs)
    // 2. Stamp symbol_id:
    //      EntryUri { package: PackageId for owning pkg, path: segments }
    //      .symbol_id(instance_token)
    //    For ~extern / cross-pkg stubs: PackageId of stub's package if known,
    //    else a well-known extern PackageId or salt path with ~extern name
    // 3. Emit edges (dedup):
    //      member_of: (parent, Member, child)
    //      implements: (self, Implements, iface)
    //      extends: (self, Extends, super)
    //      mentions/takes/returns: (self, Occurrence, target)  // union
    //      Reference docs: (source, Reference, target)
    // 4. Build out / rev_* vectors
    // 5. declares from PackageVersion or all resolved local symbols
}
```

### 5.2 Layout details

**Nodes:** `Arc<[ColdNode]>` — immutable after build; shared across clones.

**Primary maps:**

| Map | Key | Value | Use |
|---|---|---|---|
| `by_symbol_id` | `SymbolId` | `u32` | GraphStore API |
| `by_uri` | `lang/pkg/fq` | `u32` | cross-store join (tantivy/qdrant) |
| `by_iri` | `Symbol/...` | `u32` | GraphCorpus / Terminus id join |

**Adjacency:**

| Structure | Element | Rationale |
|---|---|---|
| `out: Vec<Vec<(RelationKind, u32)>>` | per-node forward | `outgoing_edges`, `are_related`, BFS |
| `rev_occurrence: Vec<Vec<u32>>` | sources with Occurrence→i | `get_occurrences` without scanning all edges |
| `rev_reference: Vec<Vec<u32>>` | sources with Reference→i | `get_references` |
| `rev_all` (optional) | `(kind, source)` | debugging / multi-kind reverse |

**Edge dedup policy:** insert into `BTreeSet<(from, kind, to)>` during lower — mirrors content-addressed Reference/Implementation intent without value_hash machinery.

**Ordering:** sort each adjacency list by `(kind, target_uri)` for deterministic streams (tests pin order when they sort anyway — `graph_expansion.rs:105–107` sorts).

### 5.3 Memory estimate per package

Assume:

- `ColdNode` ≈ 120–200 B + string bytes (uri/fq ~40–80 B average) → **~250 B/node**
- Edge: 2×u32 + kind ≈ 12 B stored once; appears in `out` and one reverse list ≈ **~20 B/edge effective**
- Map overhead: ~32–48 B per node × 3 maps ≈ **~120 B/node**

| Package size | Nodes | Edges (est.) | ColdGraph RAM |
|---|---|---|---|
| Tiny (100) | 100 | 300 | ~50–80 KB |
| Medium (5k) | 5k | 20k | ~2–4 MB |
| Large (50k) | 50k | 200k | ~20–40 MB |
| Pathological (200k) | 200k | 1M | ~100–200 MB |

**If `corpus: Some(Arc<GraphCorpus>)` retained:** add ~2–5× for Shape trees — **drop corpus after lower for GraphStore-only views**; retain `Index` only if shape/docs APIs are live.

### 5.4 Identity stamping rules (critical)

`SymbolId = UUIDv5(SYMBOL, instance_token ‖ 0 ‖ package_id_str ‖ "/" ‖ segments)` (`heart/identity/symbol.rs:42–47`).

Issues from 04:

- `PackageId` **includes version** → same FQ path in v1 vs v2 ⇒ **different SymbolIds**.
- Graph `uri` is **version-agnostic**.
- Cross-version `resolution::diff` works on FQ strings, not SymbolId equality.

Cold graph for generation V must stamp ids with **that generation’s PackageId**. Cross-version UX uses `resolution::diff` / declares set-diff, not SymbolId continuity (04 §6.3).

**Stub nodes** under `~extern` (`link.rs:24, 211–213`):

- Prefer not to invent fake PackageIds that collide; use a dedicated namespace:  
  `EntryUri { package: EXTERN_PACKAGE_ID, path: segments }` where `EXTERN_PACKAGE_ID` is a well-known id for `("~extern", language)` **without version**, or salt uri string directly. Document choice in implementation.
- Cross-package External paths (`NudoxPath::External`) should stamp with **dependency’s PackageId** when the dep version is pinned in the lock/manifest; else leave as unresolved stub with deterministic id from `(lang, dep_name, fq)` string.

### 5.5 Extracting IRI from TdbLazy

Projection stores links as `TdbLazy::new_id_unchecked(iri)`. Lowering must read the id string from TdbLazy (terminusdb_schema API — use whatever accessor the vendored crate exposes; if only via JSON, lower during project with a parallel edge accumulator to avoid fighting TdbLazy).

**Implementation tip:** enhance `project` (or a sibling `project_edges`) to also return `Vec<(String /*from iri*/, RelationKind, String /*to iri*/)>` without round-tripping TdbLazy. Cleaner than scraping EntityIDFor. Prefer this small compiler-side export over reflection.

---

## 6. Cross-package stubs and dep IR loading

### 6.1 What the linker does today

- Unknown signature names → stub under `~extern` (`link.rs:194–214`).
- `NudoxPath::External { dependency, path }` → IRI under real dependency package name; stub if not in this index (`:185–191`).
- `take_stubs` materializes Unresolved symbols + bare Package nodes (`:234–277`).

Cold single-package graph **already includes stub nodes** for edges that leave the package. `get_references` / `are_related` **within** those stubs work without dep IR.

### 6.2 What requires dep IR

| Operation | Single-package stubs enough? | Needs dep IR |
|---|---|---|
| List local callers of local symbol | Yes | No |
| Edge to external trait (Implements) | Yes (edge to stub) | No for edge existence |
| Expand into dep’s members / call graph | **No** — stub has no outgoing edges | **Yes** |
| “Who implements this trait?” globally | No | Multi-package reverse index |
| Resolve stub → real shape/docs | No | Load defining package generation |

### 6.3 DepLoadPolicy

```rust
pub enum DepLoadPolicy {
    /// Never load deps; stubs are terminal. Default for desktop & MVP.
    StubsOnly,
    /// Load dep ColdGraphs when expand would step onto unresolved node
    /// with known dependency name, up to `max_packages` / `max_bytes`.
    OnDemand {
        max_packages: usize,     // e.g. 4 desktop, 16 server
        max_bytes: usize,        // e.g. 64 MB desktop, 512 MB server
        max_depth: usize,        // dep recursion depth, usually 1–2
    },
    /// Preload lockfile closure (heavy; INDEX batch jobs only).
    PreloadClosure,
}
```

**OnDemand algorithm for expand:**

```
outgoing_edges(node):
  edges = local out[node]
  if policy allows and node.resolved == false and node.package_name != "~extern":
    if let Some(dep_view) = try_load_dep(node.package_name).await:
      // map stub uri to dep's real node; return dep.out[real]
      edges.extend(remap(dep_view, node.uri))
  return edges
```

**Caching:** dep graphs enter the **same moka** keyed by their `GraphGenerationKey`. Budget enforcement: track `resident_bytes` of cold graphs; refuse new dep loads when over cap (return stub-terminal edges; metric `cold_dep_load_rejected`).

### 6.4 Cross-package reverse queries

Global “find all implementors of Trait X” is **not** a single-package ColdGraph operation and **not** free in Terminus without `cross-pkg-index` (07 topology). Options:

1. **Tantivy/sqlite reverse index** of implements edges at fan-out (preferred for INDEX).
2. Always-hot Terminus `admin/cross-pkg-index` for hot edges only.
3. Explicit multi-package API that takes a package set.

Cold GraphStore **does not** implement global reverse search. Document as intentional degradation (§8, §11).

### 6.5 Lockfile / version pinning

When loading dep IR, pick version from:

1. The dependent package’s resolved lock (if stored on manifest — **check generate lock in JobKey**; may need lock digest in metadata).
2. Else registry “latest compatible” — **dangerous for reproducibility**; avoid on INDEX.
3. Else leave stub.

**Desktop REGISTRY:** lock-accurate. **INDEX:** store dep version pins on BlobManifest or side table when available.

---

## 7. moka / StampedeCache integration and demotion

### 7.1 Cache design

Use existing `heart::cache::StampedeCache` (`stampede.rs:48–95`) — TinyLFU via moka + single-flight + XFetch SWR. Already in heart/registry/server.

```rust
// Suggested defaults — tune with metrics
const DESKTOP_COLD_GRAPH_CAP: u64 = 32;           // entries
const DESKTOP_COLD_GRAPH_TTL: Duration = Duration::from_secs(30 * 60);
const SERVER_COLD_GRAPH_CAP: u64 = 256;
const SERVER_COLD_GRAPH_TTL: Duration = Duration::from_secs(15 * 60);

// Weighter (moka weigher) preferred over entry count when available:
// weight = nodes.len() + edges.len()  or estimated_bytes
```

Prefer **byte weigher** if moka weigher API is used: cap desktop at **256 MB** cold-graph RAM, server **2–4 GB** (see §9).

Key: `GraphGenerationKey { package, ir_hash, occ_hash, instance_token }`.

**Invalidation:** content-addressed — new generation ⇒ new ir_hash ⇒ automatic miss. No explicit purge required except:

- instance_token change (rare admin),
- schema_version bump of lowerer (include in key or flush all),
- demotion does **not** need to drop cold cache (cold is the serving path!).

### 7.2 Interaction with tier demotion (07)

| Tier event | Terminus | Cold moka | StoreLinks.graph |
|---|---|---|---|
| Cold (default) | absent | fill on query | false |
| Warming | promote job running | **serve cold** | false until commit |
| Hot | serve | may keep as shadow for failover | true |
| Soft demote (Cooling) | retain DB | **serve cold** | true until hard drop |
| Hard drop | DB deleted | unchanged | false |
| Re-promote during Cooling | cancel drop | optional keep | true after catch-up |

**Do not** evict cold graphs on promotion — shadow cold enables instant failover if Terminus blips. Optional: down-weight hot packages in cold cache weigher so RAM prefers true cold tail.

### 7.3 Query weighting hooks

Every cold GraphStore method must call into the tier scorer (07 §B.2):

| Method | Weight |
|---|---|
| `get_occurrences` / `get_references` / `are_related` | `GraphTraversal = 2.0` if used as exploration; or `SimpleDocumentFetch = 0.1` if only existence check — **default 2.0** for these three (they are graph-shaped) |
| `expand` depth=1 | 2.0 |
| `expand` depth≥2 | `TransitiveClosure = 5.0` |
| IR load miss only (no graph API) | 0 (or 0.05 if counting ensure) |

Cold path **must** fill buckets — otherwise cold-only packages never promote (07 B.2 hook 2).

### 7.4 Stampede on project

`project` of a 50k-symbol package is expensive. Always load via:

```rust
cache.get_or_compute(key, || async {
    let ir = load_ir(...).await?;
    let occ = load_occ(...).await?; // empty default
    Ok(Arc::new(build_cold_graph(...)))
}).await
```

Single-flight coalesces concurrent first queries (stampede.rs design). Do **not** use raw moka `try_get_with` if error sharing is problematic — follow heart’s StampedeCache pattern.

---

## 8. Parity matrix: Hot Terminus vs Cold IR

Legend: **Full** = same results (given same stamped ids and edge lowerer); **Degraded** = partial; **N/A** = not offered; **Ahead** = cold may be richer than today’s live Terminus insert path.

| API / capability | Hot (target) | Hot (live today) | Cold IR | Notes |
|---|---|---|---|---|
| `get_occurrences` | Full | **Degraded/empty** if only `insert_symbols` without Occurrence Relations | Full if IR+edges lowered | Live WOQL expects Relation docs (`mod.rs:594–605`) |
| `get_references` | Full | Same risk | Full if OccurrenceSet→Reference projected | Without occ: empty refs (sig mentions still as Occurrence) |
| `are_related` | Full | Same risk | Full for in-package edges | Multi-kind: first match |
| `expand` / BFS | Full | Depends on outgoing Relations | Full in-package; dep expand per policy | Share `expand_via` |
| `outgoing_edges` | Full | WOQL | Full | |
| Thin symbol document fetch | Full GraphQL/Document | `insert_symbols` shape | Via `ColdNode` / optional Index | Different wire shape |
| Shape / signature payload | Terminus Symbol.shape | Not inserted by `insert_symbols` | From Index or retained corpus | Cold can be **Ahead** |
| `PackageVersion.declares` membership | Full | If published | From projection / IR paths | |
| Cross-version membership diff | Commit history + declares | Partial | Load two gens, set-diff | Cold: CAS both IRs |
| Cross-version content diff | Terminus versioned diff | Available on hot DB | IR structural / entry hash | 04 entry hashes |
| `resolution::diff` on heart Symbols | N/A (pure) | N/A | N/A | Shared pure fn |
| WOQL arbitrary queries | Full | Full | **N/A — do not reimplement** | |
| GraphQL schema queries | Full | Full | **N/A** | |
| Multi-DB federation | App-level | App-level | App-level + DepLoadPolicy | |
| Global implementors search | cross-pkg-index | Missing | **Degraded** — needs sqlite/tantivy | §6.4 |
| Time-travel to old commit | Terminus history | Yes if hot retained | Prior CAS generations only | |
| Stub nodes | Yes if published | — | Yes (`~extern`) | |
| `symbol_id` join with search | After stamp | After stamp | Stamp at lower with instance_token | Must match hot |
| Streaming backpressure | WOQL full result then stream | Same | In-memory iter — fine | |
| Soft demotion reads | — | — | Full | Hot DB may lag |

### 8.1 Graceful degradation rules

1. **Missing OccurrenceSet:** project with empty occ → no `Reference` edges; still emit Occurrence from signature mentions/takes/returns; metric `cold_refs_degraded`.
2. **Missing dep IR:** stubs terminal; expand does not fail the whole request.
3. **Unknown SymbolId:** empty stream / `None` (like no edges), not 500.
4. **IR decode failure:** `GraphError` retryable if CAS transport; permanent if corrupt.
5. **Hot partial publish:** router prefers cold if hot probe fails (07 failover).
6. **Id scheme mismatch** (instance_token): treat as empty; log error — never return wrong package’s edges.

### 8.2 Correctness oracle

Shared tests against `MemoryGraph` fixtures **and** against `GraphView` built from synthetic Index:

- Port `graph_expansion.rs` cases to construct IR → project → ColdGraph → assert same as hand-built edges.
- Golden: for a fixture package, edge multiset from ColdGraph equals edge multiset from `lower(project(ir, occ))`.
- When Terminus Relation emit lands, golden three-way: Cold ≡ Hot ≡ lowerer.

---

## 9. Memory and CPU budgets

### 9.1 Desktop REGISTRY (local)

| Resource | Budget | Rationale |
|---|---|---|
| ColdGraph moka resident | **≤ 256 MB** (weigher) or **≤ 32 graphs** | Laptops; leave room for tantivy/embeddings |
| Single graph hard cap | **≤ 128 MB** estimated; refuse build & serve stubs-only / error | Pathological packages |
| Concurrent project builds | **1–2** (single-flight per key + global semaphore) | CPU spikes |
| Dep load | `OnDemand { max_packages: 4, max_bytes: 64MB, max_depth: 1 }` | Interactive latency |
| IR-only cache | **≤ 128 MB** additional optional | Shape panels |
| Cold miss p95 target | **≤ 200 ms** medium pkg; **≤ 1 s** large | UX |
| Warm hit p95 | **≤ 5 ms** | |

### 9.2 INDEX server

| Resource | Budget | Rationale |
|---|---|---|
| ColdGraph resident | **2–4 GB** weigher (or 256–512 entries) | Multi-tenant tail |
| Single graph hard cap | **256 MB** | Isolate noisy packages |
| Concurrent project builds | **4–8** global | Multi-core; protect latency |
| Dep load | `OnDemand { max_packages: 16, max_bytes: 512MB, max_depth: 2 }` | |
| Optional ColdGraphBlob write | background queue | |
| Hot tier size | hundreds–low thousands packages (07) | Terminus ops |
| Cold miss p95 | **≤ 100 ms** medium (SSD/local CAS); **≤ 400 ms** remote S3 | CDN/cache IR |
| Promotion concurrency | **2–4** (07) | Terminus write RAM ~10× |

### 9.3 CPU budgeting tactics

1. **Never project on the HTTP worker without single-flight** — stampede risk.
2. **Prefer optional ColdGraphBlob** for packages with miss p99 > target.
3. **Drop Shape** from resident graph; lazy-load Index for docs.
4. **Generation pin:** only HEAD generation cached by default; historical gens load ephemerally for diff then drop (or tiny LRU).
5. **Metrics:** `cold_graph_build_ms`, `cold_graph_bytes`, `cold_graph_hit_ratio`, `cold_dep_loads`, `cold_refs_degraded`.

### 9.4 Disk (CAS)

| Object | Desktop | Server |
|---|---|---|
| IR postcard | always | always |
| OccurrenceSet | always (after 04 fix) | always |
| ColdGraphBlob | optional LRU | optional for warm-not-hot |
| Source files | as today (13-storage) | as today |

ColdGraphBlob size target: **≪ IR** (edges only) — expect 5–20% of IR size for typical packages.

---

## 10. Promote / demote relative to admission scores (link to 07)

### 10.1 State machine recap (07 §B.2)

```
Cold ──(bucket≥PROMOTE ∧ CMS)──► Warming ──(publish ok)──► Hot
Hot ──(bucket≤DEMOTE)──► Cooling ──(COOLING_DAYS)──► Cold (hard drop)
Cooling ──(traffic returns)──► Hot (or Warming if gens advanced)
```

Defaults: CAPACITY=100, PROMOTE=80, DEMOTE=20, LEAK≈1 unit/min, CMS_MIN_FREQ=3.

### 10.2 What cold path does at each state

| State | Read path | Cold cache | Promotion side effects |
|---|---|---|---|
| Cold | ColdGraph only | populate on demand | — |
| Warming | ColdGraph only | keep | Job: IR→project→Terminus Document API; do not flip Hot early |
| Hot | Terminus; cold optional shadow | optional demote weight | Incremental Δ publish (07) |
| Cooling | ColdGraph | ensure warm for this package (pin?) | Soft: stop routing hot; pin cold entry |
| Hard drop | ColdGraph | unpin | `StoreLinks.graph=false` |

**Pinning:** on transition Hot→Cooling, **pin** the package’s `GraphGenerationKey` in cold cache (moka does not pin natively — maintain `pinned: DashSet<GraphGenerationKey>` excluded from capacity accounting, capped count e.g. 64). Unpin after hard drop or re-Hot with healthy Terminus.

### 10.3 Admission scores do not change GraphStore semantics

Scorer only chooses **backend**. Results must match the parity matrix for the same generation. Tests: for a package in Hot with full Relation publish, random sample of `are_related` / `get_references` equals cold lowerer on same IR.

### 10.4 Economic gate (07 §B.4)

Promotion worth it when `(C_cold - C_hot) × Q > A_publish / T_hot`.

Cold path quality **must be high** so the product does not over-promote to paper over cold bugs. Treat cold parity tests as release-blocking for graph features.

### 10.5 `StoreLinks.graph` truth table

| links.graph | Tier intent | Actual read |
|---|---|---|
| false | Cold / Warming | cold |
| true | Hot or Cooling-before-drop | hot if probe ok else cold failover |
| true but DB missing | bug / partial demotion | cold + alert |

---

## 11. What NOT to reimplement

| Do not build on cold path | Why | Use instead |
|---|---|---|
| Full **WOQL** engine | Huge; Prolog semantics; Terminus-specific | Hot tier only |
| **GraphQL** over cold | Schema gen tied to Terminus | Hot / separate REST |
| **terminus-store** embed (crates.io 0.21.5 stale) | Format drift vs server v12 (07 A.1) | HTTP hot or IR cold |
| Second **BFS** distinct from `expand_via` | Duplication; test drift | `expansion::expand_via` |
| Parallel **RelationKind** enum | Trait is shared | existing enum |
| **LinkML** schema path | Live schema is TerminusDBModel derive (07 A.3) | model.rs |
| Live **tree-sitter Tree** storage for graph | Graph uses OccurrenceSet/IR (04) | reparse only for non-graph |
| Global **federated multi-hop** as GraphStore default | Unbounded cost | DepLoadPolicy + explicit APIs |
| **Branch-per-version** Terminus model for cold | Cold has CAS gens | PackageVersion + IR |
| Full **Shape** in hot path of every query | Memory | lazy Index |
| Reimplement **resolution ladder** | Already in generate/resolve | consume OccurrenceSet |
| Custom **JSON-LD** for cold queries | Unnecessary | ColdNode / heart Symbol |

**Do reuse:**

- `from_ir::project`, `Linker`, `SymbolTable`
- `expand_via`, `resolution::diff`, `structure::assemble`
- `StampedeCache` / moka
- `RelationKind`, `GraphStore`, `GraphError`
- BlobManifest + CAS get paths
- PackageTierManager (07) when implemented

---

## 12. Blob contract changes (dependencies on 04)

Cold path MVP can ship with **empty OccurrenceSet** (signature Occurrence edges only). Full Reference parity needs:

1. **`occurrences_ref: ContentHash`** on `BlobManifest` (04 rec 2) — or replace `references_ref`.
2. Deterministic postcard of `OccurrenceSet` (already designed with sorted files — `occurrence.rs` stats note).
3. Generate pipeline already builds occurrences (`generate/occurrences.rs`) — wire into manifest.

Optional later:

4. `cold_graph_ref: Option<ContentHash>` for ColdGraphBlob.
5. `entry_fp_ref` for incremental (04 appendix 20) — not required for cold GraphStore.

---

## 13. End-to-end algorithms

### 13.1 Load package graph

```
async fn load_view(store, package_id, instance_token) -> GraphView {
    let man = store.get_manifest(package_id).await?;
    let key = GraphGenerationKey {
        package: package_id,
        ir_hash: man.ir_ref,
        occ_hash: man.occurrences_ref, // Option when landed
        instance_token: instance_token.clone(),
    };
    let graph = store.cache.get_or_compute(key, || async {
        // optional: if man.cold_graph_ref { decode lean; verify ir_hash; return }
        let bytes = store.cas.get(man.ir_ref).await?;
        let index: Index = postcard::from_bytes(&bytes)?;
        let occ = match man.occurrences_ref {
            Some(h) => postcard::from_bytes(&store.cas.get(h).await?)?,
            None => OccurrenceSet::default(),
        };
        let meta = store.package_meta(package_id)?; // language, name, version
        let ctx = PackageCtx {
            language: meta.language,
            package: meta.name,
            version: Some(meta.version),
        };
        let cold = build_cold_graph(&index, &occ, ctx, &instance_token, package_id);
        // cold.corpus = None; cold.index = None;  // default GraphStore-slim
        Ok(Arc::new(cold))
    }).await?;
    GraphView { inner: graph }
}
```

### 13.2 Promote (hot) using same lowerer

```
on_promote(package):
  ir, occ, ctx = load_inputs(package)
  corpus = project(&ir, &occ, ctx)
  stamp symbol_id on each symbol (uploader)
  // Target path:
  docs = wave_emit(corpus) + relation_docs(lower_edges(corpus))
  terminus.bulk_insert(docs, overwrite=true)
  StoreLinks.graph = true
```

Cold and hot **share** `project` + edge lowerer. Divergence is a bug.

### 13.3 Soft demote

```
on_soft_demote(package):
  tier = Cooling
  pin_cold(package.head_key)
  // stop routing to Terminus
  // do not delete DB yet
```

### 13.4 Cross-version membership (cold)

```
fn membership_diff(store, pkg, v1, v2) -> MembershipDiff {
  let a = load_view(pkg@v1).declares_uris();
  let b = load_view(pkg@v2).declares_uris();
  // set-diff on uri (version-agnostic), not SymbolId
}
```

---

## 14. Implementation roadmap

| Phase | Deliverable | Depends | Exit criteria |
|---|---|---|---|
| **C0** | `lower_edges(GraphCorpus) → Vec<(iri, RelationKind, iri)>` + unit tests | compiler graph | golden edges on fixture IR |
| **C1** | `ColdGraph` + `GraphView: GraphStore` + `outgoing_edges` | C0, SymbolId stamp helper | `graph_expansion` tests pass on GraphView |
| **C2** | `ColdGraphStore` CAS load + StampedeCache | C1, BlobManifest | load real blob fixture end-to-end |
| **C3** | Tier router stub (always cold) wired in server `SourceStores` | C2 | GUI/search expand works without Terminus |
| **C4** | DepLoadPolicy::StubsOnly documented; OnDemand experimental | C2 | metrics for stub hits |
| **C5** | occurrences_ref in manifest + full Reference edges | 04 blob work | parity refs vs live resolve |
| **C6** | Optional ColdGraphBlob | C2 + size metrics | miss p99 improvement on large pkgs |
| **C7** | Integrate PackageTierManager (07 P1–P2) | C3 + 07 | promote/demote routing |
| **C8** | Hot Relation emit from same lowerer | C0 + 07 P2 | Cold≡Hot golden |

**P0 of 07** (“PackageGraph trait + IrBlobPackageGraph MVP”) **is this document’s C0–C3.**

---

## 15. Test plan

### 15.1 Unit

| Test | Assert |
|---|---|
| Lower member_of | parent Member child edge exists both ways in indexes |
| Lower mentions | Occurrence reverse index lists holder |
| Lower Reference | only policy-passing occ become Reference edges (`from_ir.rs:1159–1173`) |
| Stub ~extern | unresolved node; edge present; no panic |
| are_related none | Ok(None) |
| expand bounds | reuse expansion.rs cases with ColdGraph neighbor fetch |
| Deterministic build | same IR bytes ⇒ same edge multiset order after sort |

### 15.2 Contract

Reuse `MemoryGraph` expectations in `graph_expansion.rs` — either generalize tests over `impl GraphStore` or dual-run.

### 15.3 Integration

| Test | Assert |
|---|---|
| Fixture package blob → get_references non-empty when occ present | |
| Cache single-flight | N concurrent loads ⇒ 1 project |
| Demotion pin | package remains warm-hit after synthetic pressure |
| SymbolId matches EntryUri::symbol_id | search join |

### 15.4 Non-tests (explicit)

- Do not require live Terminus for cold CI.
- Do not full-scan multi-crate universe in unit tests.

---

## 16. Worked example (Rust micro-package)

IR (conceptual):

```
mod m { fn f(x: u32) -> String { g() } fn g() {} }
```

After project:

- Symbols: `m`, `m::f`, `m::g`, stubs for `u32`/`String` if not in index
- `m::f.member_of → m`
- `m::f` Occurrence→ `u32`, `String` (takes/returns)
- Reference: `m::f` → `m::g` (call) if occ anchored

Cold edges:

```
(m, Member, m::f)
(m, Member, m::g)
(m::f, Occurrence, u32_stub)
(m::f, Occurrence, String_stub)
(m::f, Reference, m::g)
```

Queries:

- `get_references(m::g)` → `[m::f]`
- `get_occurrences(u32_stub)` → `[m::f]`
- `are_related(m::f, m::g)` → `Some(Reference)`
- `expand(m, depth=1)` → Member edges to f,g

---

## 17. Security and multi-tenancy

- Cold path reads only CAS objects the caller can already access (same authz as blob fetch).
- instance_token must be the **tenant’s** graph instance — wrong token ⇒ wrong SymbolIds ⇒ join failures (safe fail-closed).
- Do not execute arbitrary queries; fixed GraphStore methods only.
- Cap expand breadth/depth at API layer (server already uses depth 1 / breadth 16 for related_hits).

---

## 18. Open questions and risks

1. **Recover PackageId from SymbolId alone** for tier routing — need metadata reverse map (11) or package-scoped APIs only?
2. **heart::SymbolKind vs graph::SymbolKind** mapping incomplete (04 appendix 16) — which does ColdNode store?
3. **EXTERN_PACKAGE SymbolId** scheme — well-known PackageId vs uri-salted uuid?
4. **Should insert_symbols be replaced before or with cold path?** Cold may ship “ahead” of hot; product risk if hot empty Relations.
5. **TdbLazy id extraction** vs parallel edge list from project — implementability on vendored terminusdb-rs.
6. **Exact RAM of GraphCorpus vs lean blob** on real crates (serde, tokio, kubernetes) — measure before enabling ColdGraphBlob.
7. **Multi-tenant fairness** of server cold cache — weigher vs per-tenant caps?
8. **Versionless stem PackageId** (04/05/06) — would simplify cross-version SymbolId; cold must track decision.
9. **ReExport edges** — product need? If yes, define projection from aliases / re-export IR.
10. **OccurrenceSet default when only references_ref exists** — bridge legacy ResolvedReference → weak Reference edges?
11. **Compiler/registry crate dependency direction** for calling `project` from runtime.
12. **Whether Hot should keep shadow ColdGraph** always (RAM) or only on probe failure (latency).

---

## 19. File atlas (implementation targets)

| Path | Action |
|---|---|
| `workspace/registry/runtime/graph/mod.rs` | Keep trait; maybe `GraphExpand` |
| `workspace/registry/runtime/graph/cold/*` | **New** ColdGraph stack |
| `workspace/compiler/graph/from_ir.rs` | Export edge list / keep project pure |
| `workspace/compiler/graph/link.rs` | Shared IRI helpers (already public) |
| `workspace/registry/blob/mod.rs` | occurrences_ref (+ optional cold_graph_ref) |
| `workspace/registry/metadata/mod.rs` | StoreLinks.graph semantics unchanged |
| `workspace/heart/cache/stampede.rs` | Reuse as-is |
| `workspace/server/search/mod.rs` | Wire Tiered/Cold into SourceStores |
| `workspace/registry/runtime/tests/graph_expansion.rs` | Generalize over GraphView |
| 07 PackageTierManager (future) | Scoring + routing |

---

## 20. Executive summary

The live `GraphStore` trait is small and **single-hop** (`get_occurrences`, `get_references`, `are_related`); multi-hop exploration is pure `expand_via` over `outgoing_edges`. The Terminus adapter queries a **Relation(from, kind, to)** model keyed by `SymbolId`, while the compiler’s `GraphCorpus` stores **IRI-linked Symbol fields** plus reified Reference/Implementation documents. Cold path unifies them by **lowering GraphCorpus into the same edge multiset** `MemoryGraph` already uses in tests, after stamping `SymbolId`s with the instance token and versioned `PackageId`.

**Default representation:** always keep IR (+ OccurrenceSet) in CAS; **re-project with `from_ir::project` on cache miss**; build compact forward/reverse indexes; optionally later persist a **lean ColdGraphBlob** (edges only, not full GraphCorpus) for warm-not-hot large packages. Do **not** make Terminus or full GraphCorpus postcard the cold source of truth.

**Identity:** Graph `uri` is version-agnostic; `SymbolId` is not. Cross-version ops use uri/FQ set-diff and `resolution::diff`, never SymbolId equality. Stubs under `~extern` and External deps are first-class nodes; **dep IR loading is policy-gated** (default stubs-only) so expand cannot unbounded-crawl the universe.

**Caching:** `StampedeCache`/moka keyed by `(PackageId, ir_hash, occ_hash, instance_token)` with byte budgets (~256 MB desktop, ~2–4 GB server). Demotion **pins** cold graphs; promotion does not require cold eviction. Cold queries **must** feed the 07 leaky-bucket scorer so packages can promote.

**Parity:** implement full GraphStore on cold; accept degraded Reference edges without OccurrenceSet; do **not** reimplement WOQL/GraphQL. Long-term hot publish must emit the **same lowerer edges** as Relations or cold will remain ahead of live Terminus reads (today’s `insert_symbols` is thin).

**Phasing:** C0 edge lowerer → C1 GraphView → C2 CAS+cache → C3 server always-cold → C5 occ in manifest → C7 tier manager → C8 hot Relation parity.

---

## 21. Concrete recommendations (actionable)

1. **Ship cold GraphStore before gating Terminus** — default path is cold; hot is optimization (07).
2. **Define shared edge lowerer** from GraphCorpus (or project sibling) to `(SymbolId, RelationKind, SymbolId)`; both tiers consume it.
3. **MVP = re-project from IR + empty-or-real OccurrenceSet**; skip precomputed GraphCorpus postcard.
4. **Indexes:** `by_symbol_id`, `by_uri`, forward `out`, `rev_occurrence`, `rev_reference`.
5. **Reuse `expand_via`**; add `outgoing_edges` on a small `GraphExpand` trait.
6. **Package-scoped views** for MVP routing; join global SymbolId→package via metadata/sqlite later.
7. **Stamp SymbolId at lower time** with instance_token; never leave None in ColdNode.
8. **DepLoadPolicy::StubsOnly default**; OnDemand only with hard caps.
9. **Budget:** 256 MB desktop / 2–4 GB server cold RAM; single-flight builds.
10. **Add occurrences_ref to BlobManifest** for Reference parity (04).
11. **Wire cold query weights into 07 scorer** on day one of tiering.
12. **Pin cold graph on Cooling**; failover to cold if hot probe fails.
13. **Do not implement WOQL on cold.**
14. **Fix hot Relation materialization** in the same program of work as promotion (C8) so parity is real.
15. **Generalize graph_expansion tests** to run against GraphView.

---

## 22. Appendix A — RelationKind lowerer pseudocode

```text
fn emit_edges(corpus: &GraphCorpus) -> BTreeSet<(String, RelationKind, String)> {
  let mut e = BTreeSet::new();
  for s in &corpus.symbols {
    let from = iri_of(s);
    if let Some(parent) = s.member_of {
      e.insert((iri(parent), Member, from)); // parent has member child
    }
    for t in &s.implements { e.insert((from, Implements, iri(t))); }
    for t in &s.extends    { e.insert((from, Extends, iri(t))); }
    for t in s.mentions.iter().chain(&s.takes).chain(&s.returns) {
      e.insert((from, Occurrence, iri(t)));
    }
  }
  for r in &corpus.references {
    e.insert((iri(r.source), Reference, iri(r.target)));
  }
  // Implementation nodes: redundant with implements edge for GraphStore;
  // optional extra metadata API later.
  e
}
```

Evidence for field inventory: `model.rs:1039–1045`, `from_ir.rs:1136–1141`, `project_references` at `from_ir.rs:1154–1185`.

---

## 23. Appendix B — Alignment with plan 04 sketch

04 appendix 19 pseudocode:

```text
GraphView::from_blobs(ir, occ, ctx) -> GraphView
  .neighbors / .expand / .are_related / .version_diff
```

This design maps:

| 04 sketch | This design |
|---|---|
| `GraphView::from_blobs` | `ColdGraphStore::load_view` |
| neighbors | `outgoing_edges` + rev indexes |
| expand | `expand_via` |
| are_related | `GraphStore::are_related` |
| version_diff | membership_diff on uri + `resolution::diff` |

04’s simpler “keep GraphCorpus intermediate” is accepted; persistence choice refined to **lean optional blob**, not full corpus postcard.

---

## 24. Appendix C — Alignment with plan 07 IrBlobPackageGraph

07 synthesis trait methods vs this design:

| 07 PackageGraph | Mapping |
|---|---|
| `get_symbol` | ColdNode / Index lookup (phase PackageGraphView) |
| `query_symbols` | **Not cold graph** — tantivy/sqlite |
| `transitive_closure` | `expand` with high depth + edge kind filter |
| `declares` | ColdGraph.declares |
| `membership_diff` | uri set-diff two gens |
| `symbol_content_diff` | IR entry hash / structural diff (04) |
| `latest_generation` | metadata |

07 correctly says cold must implement the trait **well** — this document’s parity matrix and C0–C3 are that commitment for the **GraphStore** subset; full PackageGraph is phased.

---

## 25. Appendix D — Occurrence assertion policy (copy)

From live `from_ir.rs:1159–1173` / 04 appendix 13:

```
Assert Reference only if:
  role == Reference
  ∧ anchored
  ∧ confidence >= Index
  ∧ kind ∈ {FunctionCall, MethodCall, TypeReference, MacroInvocation, Import}
  ∧ enclosing.is_some()
```

Cold lowerer must use the **same** policy by calling `project` (not re-deriving) so hot/cold Reference sets match.

---

## 26. Appendix E — Score constants for streams

Live hot path uses `direct_score() = 1.0` for depth-one relations (`mod.rs:421–425`). Expansion uses `1/depth` (`expansion.rs:48–49`). Cold must use the same scoring functions for stream parity with GUI ranking.

---

## 27. Appendix F — Anti-goals checklist (review gate)

Before merging cold path code, reject PRs that:

- [ ] Add WOQL subset “just for cold”
- [ ] Store live tree-sitter trees for graph
- [ ] Key cold cache by package name without ir_hash
- [ ] Return SymbolId without instance_token salt
- [ ] Load unbounded dep closures on expand
- [ ] Block generate on Terminus or ColdGraphBlob
- [ ] Diverge RelationKind meaning between MemoryGraph tests and lowerer

---

## 28. Appendix G — Metrics catalog

| Metric | Type | Labels |
|---|---|---|
| `nudox_cold_graph_hit` | counter | result=hit\|miss |
| `nudox_cold_graph_build_seconds` | histogram | |
| `nudox_cold_graph_resident_bytes` | gauge | |
| `nudox_cold_graph_nodes` | histogram | |
| `nudox_cold_refs_degraded` | counter | reason=no_occ |
| `nudox_cold_dep_load` | counter | result=ok\|reject\|missing |
| `nudox_graph_tier_route` | counter | tier=hot\|cold\|failover |
| `nudox_graph_query_weight` | counter | weight_class |

---

## 29. Appendix H — Glossary

| Term | Meaning |
|---|---|
| Cold path | Graph ops served from IR/CAS without Terminus |
| Hot path | Terminus-backed GraphStore |
| GraphCorpus | Compiler projection output (`from_ir::project`) |
| ColdGraph | Indexed in-memory graph for one generation |
| GraphView | Arc handle implementing GraphStore |
| Lowerer | GraphCorpus → RelationKind edge multiset |
| Stub | Unresolved symbol node (`resolved=false`) |
| instance_token | Salt for SymbolId (Database name today) |
| Warming/Cooling | Tier states (07) |

---

---

## 30. Edge note — LadybugDB (optional acceleration)

**Status:** 2026-07-16 pointer from [edge-tech/05-surrounding-edge](../../edge-tech/05-surrounding-edge/PLAN.md). Does **not** change GraphStore-over-IR MVP.

**Unified map:** [edge-tech/00-DECISIONS.md](../../edge-tech/00-DECISIONS.md)

| Item | Decision |
|---|---|
| **MVP cold path** | **GraphStore over IR/CAS** (this plan) — mandatory |
| **LadybugDB** | **Optional acceleration layer** only (desktop Cypher / embedded index) behind feature on `graph-cold` |
| **Not a replacement** | Ladybug does **not** supersede IR projection, occurrence policy, or cold trait parity with hot Terminus |
| **Upstream Kùzu** | **Reject** (archived); only successor evaluation |
| **Doltgres SQL graph** | **Reject** as cold/hot GraphStore primary — see [07 alternatives](../07-terminus-tiering/PLAN.md), [01-doltgres](../../edge-tech/01-doltgres/PLAN.md) |

**Adoption gate:** Rust/FFI + license OK → spike behind `PackageGraph`; measure vs in-memory cold BFS. Until then, ship pure IR cold path.

**Topology:** [22-crate-topology optional backends](../22-crate-topology/PLAN.md) · hot tier: [07-terminus-tiering](../07-terminus-tiering/PLAN.md)

---

*End of plan. Line citations refer to the live tree under `/Users/philocalyst/Projects/Backend/workspace` as of 2026-07-16.*
