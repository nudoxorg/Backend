# The Residence Model — one local/remote split for packages, graph, and vectors

> **One line.** Locality is currently encoded four incompatible ways. Collapse
> them into one thing: every queryable datum is a **content address** (identity
> without data) whose value lives in a **`Residence`** (`Resident | Promised |
> Overlaid`), faulted-in through a single **`Resolver`** chokepoint (the
> `transport` + `ContentIo` + `bao` substrate you already built), and joined at
> query time by one **`Reconcile`** lattice-join that subsumes today's
> `merge_overlay_first`. Heavy derivation stays remote; day-to-day serving is
> local-first with lazy remote fault-in. Nothing here is new machinery — two of
> the three pillars already exist; this names the third and unifies the join.

---

## 0. The actual problem

For much of the data the system serves, some facts only a **remote** node has
(the definitive catalog, baked shards, freshly-derived embeddings, the full IR
archive) and some facts only a **local** node has (path-dependency packages on
the user's disk, IR that has *progressed* locally past the remote base, session
graphs). Three subsystems hit this:

| Subsystem | Local-only fact | Remote-only fact | Join hazard |
|---|---|---|---|
| **Package search** (`coordination/packages.rs`) | path/workspace deps on device | definitive catalog + facets | local dep must appear in results without ever being pushed remote |
| **Trustfall graph** (`registry/graph/`) | IR generations advanced locally | full archive + reverse index | a node exists on both, but the **local generation is ahead** |
| **Vector search** (`registry/vector/`) | qdrant-edge hot shards | qdrant + Voyage rerank | same as graph + sourcing |

The failure today is not missing transport. It is that **"where does this live"
has no single type**, so each subsystem invents its own answer and none of them
compose.

### 0.1 Residence is encoded (at least) four ways right now

1. `heart::access::federation::SourceRole::{Definitive, Overlay}` — federation precedence.
2. `driver::config::SourceConfig::sync_endpoint: Option<_>` — *present* ⇒ off-node peer, *absent* ⇒ served locally.
3. The `registry/vector/{local,remote}` module split — qdrant-edge vs qdrant+Voyage, chosen at the call site.
4. `registry/vector/local/depshard::RemoteRouteReason` + `remote/hedged` — the *local-first, remote-fallback, race-and-fuse* ladder, hand-written for vectors only.

Item 4 **is the whole solution, in miniature, already working for one plane.**
The plan is to lift it out of the vector plane and make it the system's residence
abstraction.

---

## 1. What is already built (two of three pillars)

**Pillar 1 — the load plane. Done.** `transport/` is the one content-addressed
transfer plane: iroh endpoint (`endpoint.rs`), `blob::{Provider, Fetcher,
TransportHash}`, `bao.rs` (outboard generate / verified-range encode-decode),
`announce.rs`, length-prefixed `frame.rs`. `heart::sync::ContentIo` is the
generic verify-before-write seam — `read/write/has/verify/max_item_bytes`, trust
anchor is content-addressing, four planes implement it (ir-vcs changes, index
packs, shard sync, sandbox goldens). `driver::sync::FederationSync` fans a
verified item across topology, generic over `ContentIo`.

**Pillar 2 — the merge plane. Done.** `heart::access::federation::Federation<S>`
= one `Definitive` base + precedence-ordered `Overlay`s; `Sourced<T>` tags every
value with `{source, role}`; `in_precedence()` / `query()` (first-hit) /
`search::merge_overlay_first` (overlay-wins). `registry::vector::SourceTag`
already carries provenance through an RRF fusion.

**Pillar 3 — the residence type. Missing.** There is no single type that says
"this datum is here / promised-elsewhere / here-but-progressed-past-a-remote-
base," and no single chokepoint that faults-in the promised case. That is the
gap this document fills.

---

## 2. Prior art, distilled

Both research threads converged on the same four-piece decomposition (EdenFS/
Sapling, git partial-clone/promisor, Buck2 remote-execution CAS, `xlb`, iroh-
blobs, bao-tree, IPFS/IPLD, casync):

| Piece | What it is | Your existing analogue |
|---|---|---|
| **① Pure content address** | identity independent of location; the id *is* the promise (Eden `ObjectId`, git OID, RE `Digest`, iroh `Hash`, CID) | `ContentHash`, `IntroId`, `StableRef`, `JobKey`, depshard `artifact_id` |
| **② Store with a *locality query*** | never binary hit/miss — always *(present ranges, missing ranges)* (iroh `LocalInfo`, RE `FindMissingBlobs`, casync local chunk-store) | *none* — `ContentIo::has()` is binary; this is a real gap |
| **③ Range-addressable verification proof, separate from data** | verify an arbitrary byte-range under a known root without the whole file (bao outboard, chunk groups, range sets) | `transport::bao` (already have it — underused) |
| **④ DAG-collection node type, distinct from leaf** | a blob that is a list of child hashes; walk = fetch node, enumerate, recurse into absent children only (iroh `HashSeq`, IPLD links, git tree, casync index) | `PackageArchive`/`NdIr` TOC, ir-vcs change deps — *implicit*, not a first-class handle |

Two findings matter most:

- **EdenFS gives Pillar 3 its exact type.** A working-copy inode is
  `Unmaterialized(ObjectId) | Materialized(LocalOverlay)`. Crucially,
  **materialization propagates *upward*: touching a leaf forces every ancestor
  out of pure-remote-pointer state**, because their child-lists now diverge from
  the remote tree they aliased. This is *precisely* your "artifacts that ARE on
  the remote have progressed on the local" case — and Eden is the only surveyed
  system that models local-edits-over-remote-base at all (git/RE/`xlb` objects
  are immutable, so they never need it).
- **`xlb` is essentially your `transport/` crate + a tiered fallback ladder** —
  Bao-verified chunked blobs over iroh resolving *local → LAN → swarm → seed →
  CDN*. It confirms Pillar 1's shape and the `hedged` ladder, and is a fetch
  primitive only (no overlay/reconcile) — so it slots under the `Resolver`, it
  is not the abstraction itself.

---

## 3. The abstraction: three planes over one address

The elegant unification the design keeps circling: **overlay-over-base
(federation, across *sources*) and local-over-remote (residence, across
*localities of one source*) are the same lattice join at two scales.** There is
one join — `reconcile` — parameterized by a precedence order and a per-identity
tip-selection rule (presence for immutable data, *generation* for versioned
data). `Sourced<T>` is to provenance what `Residence<T>` is to locality; they
compose as `Sourced<Residence<T>>`.

### 3.1 Address plane — identity without data

No new ids. Reuse `ContentHash` / `IntroId` / `StableRef` / `PackageLineageId` /
`GenerationStamp` / depshard `artifact_id`. The single rule: **locality is never
part of identity.** A content hash is location-independent — that is the whole
point (Pattern ①). `SourceRole`, `sync_endpoint`, and the `local`/`remote`
module split must stop leaking into id derivation and identity comparison.

### 3.2 Residence — the missing type (Pillar 3)

```rust
/// Where a datum addressed by `A` lives, and how to obtain its value `T`.
/// The locality analogue of `Sourced<T>`; the two compose.
pub enum Residence<A, T> {
    /// Materialized here — value present locally. (Eden: materialized inode.)
    Resident(T),

    /// Not local, but fetchable-and-verifiable from a source. The address `A`
    /// IS the promise. (Eden: unmaterialized inode; git: promisor object;
    /// iroh: `Hash` whose `LocalInfo` reports missing ranges.)
    Promised { addr: A, from: SourceId },

    /// Present locally AND locally-progressed past a remote base: a local
    /// overlay over a promised base, reconciled by generation, NOT by presence.
    /// (Eden: materialized-over-remote-tree, propagated upward.)
    Overlaid { local: T, base: A, generation: GenerationStamp },
}
```

Three variants, no more. Byte-range partiality (a half-fetched large blob) lives
*inside* the `Resolver`/store via Pattern ②/③, not as a fourth variant — the enum
is the logical view; the store handles "have ranges `[0..1000]`, fetch the gap."
Producibility ("this can only be *derived* remotely," the depshard R2 rule) is a
`Resolver` *outcome* (route-to-remote), not a residence *state*.

### 3.3 Load plane — the `Resolver` chokepoint (Pillar 1, generalized)

One trait, one call site — the union of Eden's `ObjectStore`/`BackingStore`,
git's `promisor_remote_get_direct`, and RE's `FindMissingBlobs`. This is where
"local miss → escalate to remote" happens *once*, and where batch-prefetch and
the `xlb`-style tier ladder bolt on.

```rust
#[async_trait]
pub trait Resolver: Send + Sync {
    type Addr: Clone + Display;      // ① content address
    type Value;

    /// Fault-in: return the value, fetching+verifying from the source when not
    /// resident, caching the result. The ONE place a local miss escalates.
    async fn resolve(
        &self,
        r: Residence<Self::Addr, Self::Value>,
    ) -> Result<Sourced<Self::Value>, ResolveError>;

    /// ② Locality probe (iroh `LocalInfo` / RE `FindMissingBlobs`): what is
    /// present, what is missing — enables one batched prefetch before a DAG walk
    /// instead of a round-trip per node.
    async fn locality(&self, addr: &Self::Addr) -> Locality;
}
```

**Locality granularity — DECIDED (§9 Q1): ranges live in transport only.** The
read plane (symbols / graph / packages) sees `Locality = Present | Absent` — a
symbol/vertex is materialized or it is not. Sub-blob `Partial{ranges}` exists
*only* in the large-blob transport `Resolver` (VM images, package archives), where
`bao`-verified partial fetch and crash-resume actually need it. So `Locality` is
two types: a degenerate read-plane enum and a range-carrying transport enum,
never one over-general type the read path pays for.

```rust
// read plane
pub enum Locality { Present, Absent }
// transport plane only
pub enum BlobLocality { Present, Partial { have: ChunkRanges }, Absent }
```

Its implementation is entirely existing parts: verify via `ContentIo::verify`,
transfer via `transport::blob::{Fetcher, Provider}`, verified partial ranges via
`transport::bao`, targets via `FederationSync`'s `RemoteTarget` derivation. The
one genuinely new build item is **② the locality query** — `ContentIo::has()`'s
binary answer must become `Locality` (present / partial-with-ranges / absent) so
partial materialization is first-class.

### 3.4 Query plane — one `Reconcile` join (Pillar 2, generalized)

`merge_overlay_first` is the *presence* special case of a general lattice join.
Generalize it so the "local progressed past remote" case has a home:

```rust
/// Join per-identity across a precedence-ordered set of `Sourced<T>` streams.
pub trait Reconcile<T> {
    type Key: Eq + Hash;
    fn key(&self, t: &T) -> Self::Key;
    /// The lattice join for one identity seen in two places.
    /// - Immutable data  → higher *precedence* wins (== today's merge_overlay_first).
    /// - Versioned data  → higher *generation* wins, regardless of precedence
    ///                      (the "local ahead of remote" case) — uses ir-vcs
    ///                      GenerationStamp ordering.
    fn winner(&self, a: Sourced<T>, b: Sourced<T>) -> Sourced<T>;
}
```

`merge_overlay_first` becomes `Reconcile` with `winner = higher precedence`.
Graph/IR uses `Reconcile` with `winner = higher GenerationStamp` — overlays are
branches (per the IR-VCS model), and the tip is picked by lineage, not by who
happens to be an "overlay."

---

## 4. How each subsystem instantiates the one abstraction

### 4.1 Packages — local deps as `Overlaid`/`Resident` over the definitive base

`search_packages` already folds `federation().in_precedence()` and calls
`merge_overlay_first`. Change: the device's path/workspace deps become a
**local-only overlay source** whose `PackageSearchIndex` entries are `Resident`;
the definitive catalog is `Promised`. `Reconcile` (presence) merges them. A local
dep never syncs upstream (it *can't* — Pattern: identity-without-data means the
remote can hold the *address* for a public dep it has, but a private path-dep is
`Resident`-only and simply never gets a `Promised` twin). No push, no leak.

### 4.2 Trustfall graph — DAG walk with `Promised` neighbours + generation reconcile

This is the payoff for "traverse the DAG of points in packages." The
`IrTrustfallAdapter` neighbour methods (`members`, `lineage`, `type_refs`,
`usages`, `occ_target`) return **`Residence<StableRef, GraphVertex>` handles**,
not eager vertices. The walk resolves *only the nodes actually stepped into*
(Eden fault-in; git promisor lazy walk). A `StableRef` that points into a package
whose archive is `Promised` gets faulted-in through the `Resolver`; a package
whose local IR generation is ahead resolves `Overlaid` and `Reconcile` picks the
local tip by `GenerationStamp`. `execute_graph_query`'s current `Unsupported`
stub is where this lands — the adapter becomes residence-aware before the
Trustfall schema is fully wired, so the graph spans local+remote transparently.

### 4.3 Vectors — `hedged` *is* this; refactor onto the shared trait

`registry/vector/remote/hedged.rs` (race local + remote, RRF-fuse, never drop
`remote_omitted`, `SourceTag` provenance) and `depshard` (`ArtifactFetcher`,
`RemoteRouteReason`, `DepManifestEntry` verified by BLAKE3) are the prototype.
Refactor: `ArtifactFetcher` → `Resolver`, `SourceTag` → `Sourced`, the
hedge/RRF-fuse → a `Reconcile` whose `winner` is rank-fusion. The `local`/`remote`
module split stops being a hard-coded call-site choice and becomes "which
`Residence` did the address resolve to." `hotset` admission is unchanged — it is
the *cache policy* behind `resolve`, i.e. which `Promised` shards get promoted to
`Resident`.

---

## 5. The DAG-materialization substrate (Pillar 1 details you already own)

For the physical "ship packages / IR / smolvm images and fault-in sub-parts"
layer, adopt the iroh-blobs shape wholesale — it is the most directly reusable
off-the-shelf stack and matches what `transport/` half-built:

- **`HashSeq` as the collection node (Pattern ④).** A package's intro set, an IR
  archive TOC, or a VM image's layer list is a blob-of-hashes. Walk = fetch the
  `HashSeq`, diff its children against `locality()`, fetch only the absent ones.
  This is the concrete form of "traverse the DAG of the points in packages."
  `PackageArchive`/`NdIr`'s TOC becomes (or is fronted by) a `HashSeq`.
- **`bao-tree` chunk-group outboards (Pattern ③).** A multi-GB VM image or
  package archive is fetched in ~16 KiB verified increments, streamed to disk,
  resumed after a crash by re-requesting only missing chunk-ranges, never
  re-hashed in full. `transport::bao` already does this — wire it into the
  `Resolver`'s partial path.
- **`BlobTicket`-style capability.** `{EndpointAddr + Hash + Format}` is the
  portable "this hash, reachable here" token — the on-wire form of a
  `Promised{addr, from}`. `announce.rs` already carries the announce half.
- **Tags/`TempTag` as GC roots.** A `Resident` value is a pinned root; eviction
  (hotset, depshard evict) is untagging + reap.

One plane serves all three payload kinds (packages, IR-VCS changes, smolvm
containers) because all three are already content-addressed DAGs behind
`ContentIo` — the `Resolver` is their common fault-in.

---

## 6. Where "local progressed past remote" is actually resolved

The subtle case gets a precise mechanism, not a heuristic. IR-VCS already has
`GenerationStamp`, generations, and overlays-as-branches. So:

1. Local edit advances a package's IR → new `GenerationStamp`, local channel tip
   ahead of the base it was pulled from. (Eden: leaf materializes.)
2. Materialization **propagates upward** (Eden's rule): any `StableRef` whose
   target subtree changed forces its enclosing archive/`HashSeq` node to
   `Overlaid` — its child-hash list now diverges from the promised base.
3. At query time, `Reconcile::winner` for versioned data selects the higher
   `GenerationStamp`, so the local tip wins over the remote base for exactly the
   progressed lineages, while untouched lineages stay `Promised` and are served
   from remote. No full local copy; only the diverged path is `Resident`.

This is the one place a new invariant is required: **materialization must
propagate up the enclosure/`HashSeq` DAG**, or a stale parent will keep serving a
remote child that the local tip has replaced.

---

## 7. Type sketch — how it all composes

```rust
// A query result is provenance-tagged AND locality-tagged; they compose.
type Answer<T> = Sourced<Residence<Addr, T>>;

// Serving a symbol/graph/package query:
//   1. fan the query across federation sources (Pillar 2)  -> Vec<Sourced<Stream<Residence<..>>>>
//   2. reconcile per-identity across sources (§3.4)        -> winning Sourced<Residence<..>>
//   3. resolve the winner's residence (Pillar 1, §3.3)     -> faults-in Promised, verifies
//   4. page/rank as today
//
// Heavy derivation (embed, compile, bake, reverse-index) is a Resolver policy
// outcome (`route-to-remote`), never attempted on a light local node — the
// depshard R2 rule, generalized.
```

The "dynamically clever" property the request asked for: the *same* address
resolves to whichever residence it is *right now* — `Resident`, `Promised`, or
`Overlaid` — and the query code never branches on locality. It folds
`federation`, reconciles, and resolves; the `Resolver` and `Reconcile` instances
carry all the local/remote cleverness. Add a new subsystem = pick an `Addr`, a
`Resolver`, and a `Reconcile::winner`; the join and the transport are free.

---

## 8. Roadmap (maps onto existing code)

| Phase | Deliverable | Touches |
|---|---|---|
| **R0** | `Residence<A,T>` + `Locality` + `Resolver` + `Reconcile` traits in `heart::access` (beside `Federation`/`Sourced`). No behaviour change. | `heart/access/` |
| **R1** | `ContentIo::has()` → `locality() -> Locality{Present\|Partial{ranges}\|Absent}`; wire `bao` verified-range fetch into a reference `Resolver` over `transport::blob`. | `heart/sync.rs`, `transport/`, `driver/sync.rs` |
| **R2** | Refactor vector `hedged`/`depshard`/`SourceTag` onto `Resolver`/`Reconcile`/`Sourced` — prove the abstraction on the plane that already has the pattern. Zero user-visible change. | `registry/vector/{local,remote}` |
| **R3** | `merge_overlay_first` → `Reconcile` (presence). Package search local-dep overlay source (`Resident`). | `driver/search/mod.rs`, `coordination/packages.rs` |
| **R4** | `IrTrustfallAdapter` neighbour methods return `Residence<StableRef, GraphVertex>`; lazy fault-in walk; `Reconcile` by `GenerationStamp`; unblock `execute_graph_query`. | `registry/graph/` |
| **R5** | Upward materialization invariant on the enclosure/`HashSeq` DAG for `Overlaid`; `HashSeq` front for `PackageArchive` TOC; one plane for packages + IR-VCS + smolvm payloads. | `ir-vcs/`, `transport/` |

R0–R2 are pure consolidation (fold existing behaviour under the new names, no
new capability) and de-risk the model before it touches graph/IR in R4–R5.

---

## 9. Open questions / sign-offs

1. ~~**Locality granularity.**~~ **DECIDED**: ranges in transport only; read
   plane is `Present|Absent`. See §3.3.
2. **Reconcile cost for graph.** Per-identity generation comparison across
   sources during a DAG walk — does it need the reverse index hydrated, or can it
   ride the channel-tip stamp cheaply? (Affects whether R4 needs R-index first.)
3. **`xlb` — adopt or mirror?** It is a thin, on-point Bao-over-iroh tier ladder.
   Vendor it under the `Resolver`, or keep `transport/` bespoke and borrow only
   the tier-ladder shape? (Immutable-only, so it can't be the whole `Resolver`.)
4. **Producibility as capability.** Should `route-to-remote-for-derivation` be a
   typed `ResolveError::MustDeriveRemotely{reason}` (generalizing
   `RemoteRouteReason`) so callers can enqueue a bake uniformly? Recommend yes.

---

## Appendix A — R4 in depth: the federated IR graph

R4 is the hardest and least-precedented phase, so it gets a full design. The
short version: **the current graph adapter is single-package; a real walk crosses
package boundaries via `StableRef`, and that boundary is exactly the residence
boundary.** Everything else follows.

### A.1 The boundary is already typed — it's `StableRef`

`IrTrustfallAdapter` (`registry/graph/trustfall_adapter.rs`) borrows *one*
`IrView` (one package at one channel tip) and its neighbour methods already hand
back **addresses, not materialized vertices**:

| Method | Returns | Crosses packages? | Residence |
|---|---|---|---|
| `members(intro)` | `Iterator<IntroId>` | no — same view | intra-package, resident-with-view |
| `lineage(intro)` | `Option<IntroId>` | no | intra-package |
| `type_refs(intro)` | `Vec<StableRef>` | **yes** iff `TypeRefWire::Foreign` | resolve `sref.package` |
| `occ_target(occ)` | `StableRef` | **yes** iff target pkg ≠ view pkg | resolve `sref.package` |
| `usages(target)` | `Vec<IntroId>` (owners *in this view*) | **incoming — see A.3** | remote-authoritative |

`TypeRefWire::{Same(IntroId), Foreign(StableRef)}` *already* draws the intra-vs-
cross line. So the walk classification is free: `Same`/`members`/`lineage` stay
in the current view (Pillar-free); a `Foreign(StableRef)` is a `Residence` keyed
by `sref.package` — `Resident` (local IR held), `Overlaid` (local generation
ahead of a remote base), or `Promised` (only on the remote).

### A.2 Generation reconciliation selects *which view* answers

When a foreign package is present both as `Overlaid` (local tip ahead) and
`Promised` (remote base), `Reconcile::winner` by `GenerationStamp` picks the
local tip's `IrView`. This is already threaded: `ReverseIndexKey.channel_tip` is
"the `GenerationStamp`/`CasKey` of the generation," so a reverse index is keyed
per generation and cross-generation walks are cache-safe by construction. R4 adds
`reconciled_view(pkg) -> Sourced<Arc<IrView>>` over a `Federation<GraphSource>`,
where `GraphSource::view_of(pkg) -> Residence<PackageAddr, Arc<IrView>>`.

### A.3 The deep asymmetry: outgoing is local, incoming is remote

This is the sharpest concrete instance of your whole split, and it falls straight
out of the data model:

- **Outgoing edges** (`members`, `lineage`, `type_refs`, `occ_target`) are stored
  *on the owner's entry*. If you hold the owner's package view, you can answer
  them locally — no remote needed.
- **Incoming edges** (`usages` — who calls/mentions me) are **distributed across
  every caller's package**. The current `usages(target)` only finds callers
  *within the one view*. The global caller set lives in other packages'
  occurrence frames, and **only the remote holds every package's reverse index**.
  A local node can see callers *only among packages it happens to hold*.

So `usages` is intrinsically **remote-authoritative**, while outgoing traversal
is **local-first**. That is not a wart to paper over — it *is* the clean split
the request asked for, made precise:

```
usages(sref) = remote_global_reverse(sref)         // Promised — the authoritative fan-in
             ⊍ local_overlay_callers(sref)          // Resident/Overlaid — packages progressed locally
reconciled by (caller StableRef, GenerationStamp, confidence)   // union, dedup, local-tip wins
```

`Reconcile` handles the union/dedup; the "confidence" tiebreak covers a local
overlay resolving an edge at `Oracle` confidence where the remote base only had
`Import`.

### A.4 Fault-in vs push-down = the heavy-compute-stays-remote knob

`heart::Query.routing.reach` (already in the wire query) is the budget that
decides how a `Promised` frontier is handled — this is where "keep heavy
computation on the remote" becomes a typed policy, not a vibe:

- **Small reach / bounded neighbourhood → fault-in.** Batch-probe the missing
  foreign intros (one `locality()` call, RE-`FindMissingBlobs`-style), fetch them
  as a `HashSeq` sub-range over `transport::bao` into an ephemeral local
  `IrView`, and walk locally. Cheap, interactive, day-to-day.
- **Unbounded reach, or `usages` on a hot symbol (huge fan-in) → push-down.**
  Ship the seed `StableRef` + query to the remote node, which has the full
  archive and a hot global reverse index, and return the *projected* result set.
  This is Buck2-RE's "run the consuming action remotely against CAS, avoiding the
  round trip." The local node then reconciles the remote projection with its
  local overlays.

`routing.hot_packages[]` feeds the same decision: a hot package is one worth
promoting `Promised → Resident` (via the vector plane's existing `hotset`
admission machinery, §4.3 — identical "promote under budget" policy, reused).

### A.5 Why Trustfall makes fault-in free

When `execute_graph_query`'s `Unsupported` stub is finally wired, the Trustfall
`Adapter::resolve_neighbors` iterators become exactly these lazy
`Residence`-resolving edge walks. **Trustfall is pull-based**: it only advances an
edge iterator for neighbours the query actually consumes. That is git-promisor
laziness for free — a query materializes *only* the promised nodes it touches,
never the whole reachable DAG. The laziness of the query engine and the laziness
of fault-in are the same laziness. This is a strong reason to wire the real
adapter residence-aware *before* the schema, not after.

### A.6 R4 build order

1. `residence_of(PackageLineageId) -> Residence` + `reconciled_view` (generation-
   wins) over `Federation<GraphSource>`. (Pure Pillar-2/3 composition.)
2. Collapse the five neighbour methods into one
   `neighbours(intro) -> impl Iterator<Item = Edge>` where `Edge` carries a
   `StableRef` + edge-kind, so intra/cross classification is one match, not five
   call sites.
3. Federate `usages`: remote-global reverse (Promised) ⊍ local-overlay callers,
   reconciled (A.3). **Needs a new remote surface**: a global reverse-index query
   endpoint (`usages_of(StableRef) -> Page<Sourced<StableRef>>`).
4. Reach-budget policy (A.4): fault-in vs push-down driven by
   `Query.routing.{reach, hot_packages}`.
5. Wire `execute_graph_query` to Trustfall over the lazy `Residence` iterators
   (A.5) — the payoff.

### A.7 R4 open sub-questions

- **Usages: remote endpoint or shard fault-in?** Expose `usages_of` as a remote
  query (recommended — fan-in is unbounded, don't transfer whole reverse shards),
  or fault-in reverse-index shards as `Promised` and answer locally (keeps compute
  local, transfers more)? Recommend the endpoint for incoming, fault-in for
  outgoing — matching A.3's asymmetry.
- **Ephemeral faulted-in view lifetime.** Reuse `hotset` admission/eviction from
  the vector plane for promoted foreign views — same budgeted `Promised→Resident`
  cache, don't invent a second one.
- **Occurrence confidence in reconcile.** Confirm `(generation, confidence)`
  ordering is the right tiebreak when local overlay and remote base disagree on an
  edge's confidence (A.3).

---

## 10. One-line architecture

**One content address; three residences (`Resident | Promised | Overlaid`); one
`Resolver` that faults-in the promised case over the `transport`+`ContentIo`+`bao`
substrate you already have; one `Reconcile` lattice-join that subsumes
`merge_overlay_first` and adds generation-wins for the "local progressed past
remote" case — so packages, Trustfall graph, and vectors are three instances of
the same local/remote split, and adding a fourth is picking an `Addr`, a
`Resolver`, and a `winner`.**
