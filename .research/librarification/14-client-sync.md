# Client Sync Typestates: Local REGISTRY vs Remote INDEX

**Research date:** 2026-07-16  
**Scope:** Client-library architecture for progressive, generation-consistent sync of content-addressed IR/source/catalog from a remote INDEX into a local REGISTRY; typestate and capability patterns; dual-backend routing; wire protocol; offline semantics.  
**Audience:** librarification architecture / client crate design.

---

## 0. Problem statement

nudox is becoming a multi-language code-intelligence platform with a desktop GUI (GPUI, Rust) consuming a new **`client` library** evolved out of today's axum `server` crate. The client's job is to **erase the local/remote distinction** for every dual-responsibility component, using typestates and other strong patterns.

### Semantics (authoritative)

1. A developer assigns a **PROJECT** → **trusted** → compiled **locally** by an embedded compiler library (project root + path deps + workspace members).
2. The project's third-party **DEPENDENCIES** are **untrusted** → delegated to the remote **INDEX**.
3. The INDEX **serves queries immediately** while **syncing** underlying artifacts (content-addressed IR/source/tree blobs + catalog rows) into the local **REGISTRY** (SQLite + disk).
4. Once a generation is fully synced, the **local store takes over transparently**.

### Dual-home components

| Component | Local home | Remote home |
|---|---|---|
| Compile | Embedded compiler / forge | Remote compiler fleet |
| Text search | Embedded Tantivy | Remote Tantivy / server search API |
| Vector search | Embedded store (or none) | Remote Qdrant |
| Graph ops | Local IR blobs / embedded graph | Remote Terminus |
| Package metadata | Local SQLite catalog | Remote INDEX API |
| Blobs / CAS | Local DiskCas + L1 | Object store / INDEX blob URLs |

### Existing codebase precedents (load-bearing)

| Pattern | Location | What it teaches |
|---|---|---|
| `Cold` / `Live` + `Connect` | `workspace/heart/connection.rs` | Monotonic readiness typestate |
| `Cas` + `Tiered<L3>` + `NoL3` | `workspace/heart/cache/mod.rs`, `tiered.rs` | Trait + local/remote tiers + ZST absence |
| `SingleFlight` | `workspace/heart/cache/single_flight.rs` | Stampede-safe coalescing |
| `SemanticGate` | `workspace/registry/runtime/vector/gate.rs` | Consumable capability token |
| Sealed `SnapshotPolicy` + wire brand | `workspace/heart/cursor.rs` | Policy brand survives encode |
| `BlobManifest` dual hashes | `workspace/registry/blob/mod.rs` | Generation stamp ≠ CAS key |
| `Store<Cold/Live>` | `workspace/registry/store.rs` | Connect-gated object store |
| `ForgeRuntime<Cold/Ready>` | `workspace/compiler/daemon/forge.rs` | Cold→Ready assembly |
| `Progressive` / `JobProgress` | `workspace/heart/progress.rs` | Phased progress surface |
| GUI `BackendClient` | `workspace/gui/src/backend.rs` | Today: pure HTTP remote; no local registry |

---

## 1. Prior art: remote-serves-while-syncing → local-takeover

### 1.1 Turso / libSQL embedded replicas

**Docs (verified 2026-07-16):** [Embedded Replicas](https://docs.turso.tech/features/embedded-replicas/introduction)

**How it works:**

1. Local file = main database (`url`).
2. Remote primary = sync source (`syncUrl`).
3. **Reads always from local replica.**
4. **Writes go to remote primary** by default (not local-first); optional `offline: true` enables local writes.
5. After a successful write, the initiating replica sees data immediately (**read-your-writes**), even without `sync()`.
6. Other replicas see data on next `sync()` or at `syncInterval`.

**Sync granularity:** page-level (4 KiB frames). Docs note unexpected frame counts on btree splits, dirty WAL regeneration after server restart, and full re-sync if local files are wiped.

**Consistency model:**

- Single-writer primary (default embedded-replica mode).
- Eventual consistency across replicas; RYW on the writer replica.
- **Not** a multi-master CRDT story for the classic path.

**Turso Sync (2026 successor):** [Turso Sync benchmark post](https://turso.tech/blog/sync-benchmark) (2026-04) documents a rewrite using **logical CDC** with explicit `push()` / `pull()` instead of page replication — claimed up to 312× faster RYW and 18× less bandwidth. Docs now recommend Turso Sync for new projects that need sync; Embedded Replicas remain fully supported.

**Maturity for nudox:**

| Aspect | Fit |
|---|---|
| Remote-first-ish → local reads | **Good UX analogy** (local microsecond reads after sync) |
| Our immutability + generation pin | **Poor direct fit** — Turso syncs mutable SQL pages/rows; we sync content-addressed manifests |
| Conflict story | Irrelevant for us if INDEX is authoritative and deps are pull-only |
| Crate maturity | libSQL clients mature; Turso Sync is the newer path — do not depend on it as a product for package IR |

**Takeaway:** Steal the **UX contract** (local reads after background pull; status surface; periodic + on-demand sync). Do **not** adopt page-level SQL replication for IR blobs.

### 1.2 Local-first / offline-first sync engines

#### ElectricSQL (2026)

Sources: [ElectricSQL docs](https://electric-sql.com/docs/intro), [Shapes guide](https://electric-sql.com/docs/guides/shapes), [AGENTS.md architecture notes](https://github.com/electric-sql/electric/blob/main/AGENTS.md), [Neon guide](https://neon.com/guides/electric-sql).

**Architecture:**

- Postgres → Electric sync service (Elixir) via logical replication.
- Clients consume **Shapes** over **HTTP** (CDN-friendly).
- **Unidirectional read path:** Postgres → client. Writes go through the application's existing API (not through Electric).
- Old bidirectional SQLite model was replaced by **read-path Electric + client DB for optimistic UX**.

**Relevance:** Electric's post-rewrite model is closer to ours than classic local-first CRDT engines: **server-authoritative, one-way, shape-scoped partial sync**. Shapes ≈ "subscribe to the subset of catalog + package generations this project needs."

**Mismatch:** Electric still targets **mutable relational rows** with continuous changelog streaming. Our primary payload is **immutable content-addressed blobs** keyed by BLAKE3; catalog rows are secondary and generation-pinned.

#### PowerSync

Sources: [PowerSync](https://www.powersync.com/), [Sync Rules / buckets](https://docs.powersync.com/llms.txt), [QueryPlane architecture overview](https://queryplane.com/blog/powersync-offline-first-sync/).

**Architecture:**

- Server-side CDC (Postgres WAL, Mongo change streams, etc.).
- Client embeds SQLite; app reads/writes local SQL.
- **Sync Rules** partition data into **buckets** (global + parameterised).
- Bidirectional: local mutations upload; server rules decide download scope.

**Relevance:** Bucket model is a useful metaphor for **project-scoped sync sets** (bucket = resolved dependency set at generation G). Priority-sync-for-buckets is exactly progressive availability UX.

**Mismatch:** Designed for **mutable offline-first apps** with write-back. Our dependency plane is **pull-only and server-authoritative**; write-back CRDT machinery is dead weight.

#### cr-sqlite / Automerge / Ditto

Sources: [cr-sqlite intro](https://vlcn.io/docs/cr-sqlite/intro), [vlcn design notes](https://vlcn.io/blog/libsql-gdoc).

CRDTs solve **multi-writer convergence under partition**. Our constraints:

- IR/source blobs are **immutable once hashed** (`ContentHash` / BLAKE3 in `heart/content.rs`).
- `BlobManifest` is a **server-emitted generation** (`registry/blob/emit.rs` → outbox with generation).
- Client never authors dependency IR; trusted local project is a **separate trust plane**.

**Verdict: CRDTs are overkill.** A simple **manifest-pull protocol** (want/have over content hashes) dominates.

### 1.3 Content-addressed sync (the right family)

#### Nix binary caches / substituters

Sources: [Nix binary cache substituter](https://nix.dev/manual/nix/2.33/package-management/binary-cache-substituter.html), [NixOS Wiki Binary Cache](https://wiki.nixos.org/wiki/Binary_Cache).

**Protocol loop:**

1. Need store path / NAR hash H.
2. For each substituter: `HEAD /<hash>.narinfo`.
3. On 200: fetch NAR; verify; unpack into store.
4. On 404: try next substituter.

**Properties we want:**

- Content-addressed, **trustless-at-hash** (integrity is the hash).
- Lazy substitution: only pull what a build/query needs.
- Multi-source routing (local disk → LAN cache → remote).

**Mapping to nudox:**

| Nix | nudox |
|---|---|
| `.narinfo` | `BlobManifest` + catalog row for package@generation |
| NAR | section blobs under `cas/{blake3}` |
| substituter list | Local DiskCas → optional LAN → INDEX/object store |
| path closure | generation closure (manifest + all section hashes) |

#### Git pack protocol (want/have)

Source: [gitprotocol-pack](https://git-scm.com/docs/gitprotocol-pack).

**Three phases:**

1. Server advertises refs/capabilities.
2. Client sends `want` lines (desired objects) + `have` lines (already-local); server ACKs commons.
3. Server sends a pack of **only missing objects** (with deltas).

**Mapping:** Our pull loop is Git-shaped without deltas (or with optional casync-style chunking later):

```
client:  HAVE { local ContentHash set for generation G }
client:  WANT { hashes in Manifest(G) \ HAVE }
server/store: stream blobs for WANT
client:  verify blake3; stage; commit generation atomically
```

#### ostree / casync

Sources: [casync README](https://github.com/systemd/casync), [casync blog](https://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html), [OSTree related projects](https://ostreedev.github.io/ostree/related-projects/).

- **OSTree:** content-addressed objects over HTTP; optional precomputed deltas; many small files hurt CDNs.
- **casync:** content-defined chunking + index; CDN-friendly large chunks; rsync∩git hybrid.

**For v1:** whole-blob GET by hash is enough (sections already content-addressed in `registry/blob`). Chunk-level delta is a later optimisation for large source archives.

#### IPFS bitswap

Want/have block exchange over a swarm. Relevant only if peer-to-peer edge caches appear; not v1.

### 1.4 Media / offline UX patterns

| Product pattern | Application to nudox |
|---|---|
| Spotify offline playlist | Mark dependency set "available offline"; progressive download; grey-out unsynced |
| Steam depot delta | Generation-level patch: only missing hashes; progress by bytes + items |
| Browser offline cache | Query works offline if generation complete; surface "partial / offline degraded" |

**UX contract for GUI:**

1. Search/graph always returns **something** when network is up (remote-first for unsynced).
2. Background sync fills local REGISTRY; status bar shows per-package and aggregate progress.
3. When generation G is **committed**, subsequent queries for that dep set never hit the network for those blobs.
4. Offline: only committed local generations; clear `OfflineMode` status; no silent stale remote fallback.

---

## 2. Codebase precedents in detail

### 2.1 Connection typestate: `Cold` → `Live`

```1:26:workspace/heart/connection.rs
//! Our typestate markers. We of course want to avoid connections or attempted
//! sends to a database that isn't actually alive.
// ...
pub struct Cold;
pub struct Live;
pub trait Connect: Sized {
    type Live;
    async fn connect(self) -> Result<Self::Live, ConnectError>;
}
```

**Properties:**

- Monotonic: Cold → Live (no Live → Cold type transition; reconnect builds a new Cold).
- Methods that need a live backend live only on `Store<Live>` (see `registry/store.rs:97+`).
- Same pattern: `ForgeRuntime<Cold/Ready>`, `Semantic<M, Cold/Live>`, `Graph<Cold/Live>`, `Queue`, `Outbox`.

**Client lesson:** Use typestates for **configuration readiness and trust planes**, not for **transient sync percentages**.

### 2.2 Tiered CAS as dual-home prototype

From `workspace/heart/cache/mod.rs` and `tiered.rs`:

- Trait `Cas`: `get` / `put` / `put_keyed` (first-write-wins).
- `Tiered<L3>`: L1 StampedeCache → L2 DiskCas → L3 remote.
- Absence of L3 is **type-level** `NoL3`, not `Option`.
- Read promotes upward; write durable tiers first.
- `EvictableCas` only on tiers that can honestly delete.

This is already a **read-through cache** for immutable blobs. Client sync generalises it:

```
QueryRouter = Tiered-for-queries:
  if generation committed locally → LocalTextSearch / LocalCatalog / LocalGraph
  else → Remote + enqueue SyncJob
Blob path = existing Tiered CAS
```

### 2.3 Single-flight / stampede

`SingleFlight<K>` (`workspace/heart/cache/single_flight.rs`):

- Leader computes; waiters re-check store after `Notify`.
- Errors not shared as `Arc<E>` (preserves concrete error types).
- Used by `StampedeCache` with XFetch + SWR.

**Client application:** coalesce concurrent remote queries for the same `(PackageId, Generation, QueryKind)` and concurrent blob downloads for the same `ContentHash`.

### 2.4 Capability: `SemanticGate`

```22:46:workspace/registry/runtime/vector/gate.rs
/// An explicit, audited opt-in to (heavy) semantic search.
/// Deliberately not Copy/Clone: one issuance authorizes one query.
pub struct SemanticGate { reason: &'static str }
impl SemanticGate {
    pub fn issue(reason: &'static str) -> Self { ... }
    pub fn for_readiness() -> Self { ... }
}
```

Consumed **by value** in semantic search paths. Planner is the sole user-facing issuance site.

**Client generalisation:**

| Token | Authorises |
|---|---|
| `NetworkCapability` | Remote INDEX / blob fetches (offline mode withholds) |
| `LocalComputeCapability` | Local compile / local embed / local index rebuild |
| `SemanticGate` (existing) | Expensive vector path |
| `SyncedWitness<G>` | Generation-pinned local-only query path that assumes complete closure |

### 2.5 Sealed policies + wire survival: `Cursor`

`Cursor<K, P: SnapshotPolicy>` brands Enforced vs Advisory with sealed traits; wire envelope carries `PolicyTag` so brands survive encode/decode (`workspace/heart/cursor.rs`).

**Client lesson:** When a capability or snapshot brand must cross process boundaries (disk cache of cursors, IPC to GUI), **serialise an explicit discriminant**, not only `PhantomData`.

### 2.6 Generation identity: `BlobManifest`

Two deliberately distinct hashes (`registry/blob/mod.rs`):

1. **Generation-stamp** (`identity_bytes` → `ContentHash`) — "same logical snapshot?"
2. **CAS key** (`manifest_cas_key` postcard hash) — "where is the wire blob stored?"

Emit path (`blob/emit.rs`): put sections → put manifest → outbox append with **generation**.

**Client sync unit of atomicity = one generation of one package** (manifest + full section closure). Partial section downloads must not mark the generation available for local takeover.

### 2.7 Progress

`Progressive` trait + `ProgressStatus` + `JobProgress` (`heart/progress.rs`) already model phased package jobs. Client `SyncEvent` should align with this vocabulary so GUI panels reuse progress widgets.

### 2.8 GUI today

`workspace/gui/src/backend.rs` is a pure `reqwest` remote client. No local REGISTRY, no sync status, no typestate. The client library is the migration target that lets GPUI stop knowing about HTTP endpoints.

---

## 3. Read-through semantics

### 3.1 Policy algorithm

For every read `Q` against a **resolved dependency set** `S = {(P_i, G_i)}`:

```
fn route(Q, S):
  if all (P_i, G_i) in S are LocallyCommitted:
      return Local.execute(Q @ S)
  if NetworkCapability available:
      spawn_or_join SyncEngine.ensure(S)   // single-flight per generation
      return Remote.execute(Q @ S)         // INDEX serves immediately
  else:
      return OfflineDegraded(Local.partial(Q, S), missing=S \ committed)
```

**Key invariant:** Remote and local answers for the same `(Q, S)` must be **generation-consistent**. Never mix IR from G_old with catalog rows from G_new mid-sync.

### 3.2 Atomic visibility of generations

**Staging model:**

```
cas/staging/{download-id}/{hash}   # incomplete blobs
cas/{hash}                         # committed content-addressed objects
catalog.generations: (package_id, generation) → status ∈ {Pending, Pulling, Ready, Failed}
```

**Commit rule (all-or-nothing):**

1. Download all hashes in `Manifest(P, G).closure()`.
2. Verify each BLAKE3.
3. Transactionally:
   - move/link staging → durable CAS,
   - insert catalog rows for that generation,
   - set status = `Ready`,
   - emit `SyncEvent::GenerationReady { package, generation }`.

Until step 3 commits, **routers must treat the generation as not local** even if some blobs already exist (shared across packages via content-addressing).

**Partial reuse:** If hash H already exists in durable CAS (shared with another package), count it toward the closure without re-download. That is Nix-style substitution, not partial generation visibility.

### 3.3 Generation-pinned queries

A search session binds to a **`DepSet`**:

```rust
struct DepSet {
    /// Project lockfile / resolution result.
    members: Vec<(PackageId, ContentHash /* generation stamp */)>,
    /// Fingerprint of the whole set for cache keys.
    set_id: ContentHash,
}
```

All query results and cursors for that session carry `set_id` + snapshot (reuse `Cursor` + `ContentHash` snapshot pattern). If a newer generation appears on the INDEX, the session does **not** silently upgrade; user/app must re-resolve.

### 3.4 Cache stampede / single-flight

Two coalescing domains:

| Domain | Key | Leader work |
|---|---|---|
| Query | `(set_id, query_hash, backend)` | One remote HTTP/gRPC call |
| Blob | `ContentHash` | One GET + verify + put |
| Generation | `(PackageId, generation)` | Full pull loop to Ready |

Reuse `heart::cache::SingleFlight` and `StampedeCache` — already production-tested in-process.

### 3.5 Staleness rules

| Data class | Staleness policy |
|---|---|
| CAS blob at hash H | Immutable forever; no TTL needed |
| Catalog "package P → generation G" pointer | Server-authoritative; refresh on resolve / TTL for discovery UI only |
| Search index local | Valid only for committed generations; rebuild or segment-per-generation |
| Semantic vectors | Same generation pin; Advisory cursors (existing) |

**Never:** serve local search hits from G_old while claiming resolution S includes G_new.

### 3.6 Offline mode

```rust
enum Connectivity {
    Online,
    Degraded { since: Instant, last_error: ConnectError },
    Offline,
}

struct OfflinePolicy {
    /// If true, never attempt remote (airplane mode).
    force_offline: bool,
    /// Local-only generations still queryable.
    allow_partial_local: bool,
}
```

When offline:

- Trusted project compile/search still works (local sources).
- Dependency queries limited to `Ready` generations.
- GUI shows explicit missing packages (Steam-like "not downloaded").

---

## 4. Typestate + strong-pattern catalog

### 4.1 What belongs in the type system vs runtime

| Concern | Mechanism | Why |
|---|---|---|
| Configured vs connected store | Typestate `Cold`/`Live` | Monotonic, matches `Connect` |
| Project resolve pipeline | Typestate `Unresolved`→`Resolved`→`Indexed` | Monotonic user workflow |
| Trust plane | Type param `Trusted` / `Untrusted` | Compile API must not exist on untrusted |
| Network / local-compute permission | Capability ZST tokens | Runtime + audit, like `SemanticGate` |
| Sync % / pulling / ready | **Runtime enum** + optional **witness** | Non-monotonic (can fail, retry); status changes often |
| Generation complete for local-only path | `SyncedWitness` (runtime-minted proof) | Not a permanent type state of the whole Client |

### 4.2 Why **not** `Store<Syncing>` / `Store<Synced>` as typestates

Typestates shine when transitions are **monotonic, rare, and API-defining**. Sync status is:

- **Per-generation**, not per-store (store can be Live while 30 packages are Pulling).
- **Non-monotonic** (Ready → Failed → Pulling on corruption repair).
- **High-frequency** (GUI progress ticks).

Encoding that as `Client<Syncing>` forces either:

- one global sync state (false), or
- an explosion of type parameters, or
- constant type transitions that fight the borrow checker and async tasks.

**Recommended split:**

```
Client / RegistryStore / IndexClient : Cold|Live typestates
SyncEngine                          : runtime status map
SyncedWitness { package, generation }: capability for local-only APIs that require Ready
```

This mirrors the existing world: `Store` is Live even while outbox consumers lag; readiness is probed separately.

### 4.3 Project typestate pipeline

```rust
pub struct Unresolved;
pub struct Resolved;
pub struct Indexed;

pub struct Project<S> {
    root: PathBuf,
    // ...
    _state: PhantomData<S>,
}

impl Project<Unresolved> {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ProjectError> { ... }

    /// Parse manifests, resolve path/workspace members (trusted).
    pub async fn resolve(self, toolchain: &Toolchain) -> Result<Project<Resolved>, ProjectError> { ... }
}

impl Project<Resolved> {
    /// Trusted local sources only.
    pub fn trusted_sources(&self) -> &[SourceRoot] { ... }

    /// Untrusted third-party coordinates after lock/resolve.
    pub fn dependency_set(&self) -> &DepSet { ... }

    /// Kick INDEX sync for deps; may run concurrently with remote queries.
    pub async fn ensure_deps(
        self,
        client: &Client<Live>,
    ) -> Result<Project<Indexed>, ProjectError> { ... }
}

impl Project<Indexed> {
    /// Local+routed query surface bound to this project's DepSet.
    pub fn session(&self) -> Session<'_> { ... }
}
```

### 4.4 Trust boundary as type parameter

```rust
pub enum Trusted {}
pub enum Untrusted {}

pub struct SourcePath<T> {
    path: PathBuf,
    _trust: PhantomData<T>,
}

pub trait CompileLocally {
    async fn compile_locally(&self, forge: &ForgeRuntime<Ready>) -> Result<BlobManifest, CompileError>;
}

impl CompileLocally for SourcePath<Trusted> { ... }

// Intentionally no CompileLocally for SourcePath<Untrusted>.
impl SourcePath<Untrusted> {
    pub async fn delegate(
        &self,
        index: &IndexClient<Live>,
        net: NetworkCapability,
    ) -> Result<RemoteHandle, IndexError> { ... }
}
```

This is stronger than runtime checks: **untrusted compile is unrepresentable**.

### 4.5 Capability tokens (generalising SemanticGate)

```rust
#[must_use]
pub struct NetworkCapability {
    reason: &'static str,
}

#[must_use]
pub struct LocalComputeCapability {
    reason: &'static str,
}

/// Proof that generation G of package P is fully Ready in the local REGISTRY.
/// Minted only by SyncEngine after atomic commit. Not Clone (or Clone if cheap
/// re-check is preferred — see tradeoff below).
#[must_use]
pub struct SyncedWitness {
    package: PackageId,
    generation: ContentHash,
}

impl SyncedWitness {
    /// Prefer re-validate on use for long-lived witnesses.
    pub fn revalidate(&self, reg: &RegistryStore<Live>) -> Result<(), SyncError> { ... }
}
```

**Clone tradeoff:** `SemanticGate` is non-Clone to prevent double use of expensive ops. `SyncedWitness` is a **status proof**, not a one-shot auth — `Clone` + revalidate-on-use is fine; or store as `Arc<GenerationReady>` with generation map.

### 4.6 Sealed traits + `#[non_exhaustive]`

Follow `SnapshotPolicy` sealed module pattern for:

- `SyncPhase` (if only client may implement Progressive for sync)
- Backend tags for routing
- Protocol capability negotiation enums

Wire DTOs:

```rust
#[non_exhaustive]
#[derive(Serialize, Deserialize)]
pub struct ManifestDto {
    pub package: PackageId,
    pub generation: ContentHash,
    pub sections: Vec<SectionDto>,
    // v2 fields optional with defaults
    #[serde(default)]
    pub features: u32,
}
```

Desktop clients ship less often than INDEX; **additive, non_exhaustive, version negotiation** is mandatory (see §6).

### 4.7 sans-IO / hexagonal

Source: [Firezone sans-IO](https://www.firezone.dev/blog/sans-io); pattern used by quinn-proto, str0m, etc.

**Recommendation for nudox client:**

| Layer | Style |
|---|---|
| Sync pull state machine (want/have, commit rules) | **sans-IO core** (`client-core`) — pure transitions, no tokio |
| Query routing policy | Pure functions over status maps |
| HTTP/gRPC, disk, SQLite, GPUI bridges | `client-tokio` (or `client` with runtime feature) |

**Why partial sans-IO (not everything):**

- Full sans-IO for Tantivy/SQLite is impractical (those APIs are inherently IO-bound and already async).
- Sync **protocol** and **routing decisions** are pure enough to unit-test without network — high ROI.
- Firezone's lesson: abstract time + I/O effects at the protocol boundary; do not force the whole app into pure state machines.

**Crate split:**

```
nudox-client-core   # DepSet, Manifest, SyncMachine, RouteDecision, traits (no tokio)
nudox-client        # Tokio drivers, SQLite registry, HTTP index, Tiered CAS, SyncEngine
nudox-client-gpui   # optional: SyncEvent → GPUI entity updates (or keep in gui crate)
```

Alternatively a **single crate** with features `runtime`, `sqlite`, `http` if packaging cost dominates — but keep **modules** separated as if they were crates so sans-IO core stays testable.

### 4.8 Async + progress for GUI

```rust
pub enum SyncEvent {
    Started { package: PackageId, generation: ContentHash },
    BlobProgress { hash: ContentHash, bytes: u64, total: Option<u64> },
    GenerationProgress { package: PackageId, done: u32, total: u32 },
    GenerationReady { package: PackageId, generation: ContentHash },
    GenerationFailed { package: PackageId, generation: ContentHash, error: String },
    Connectivity(Connectivity),
    QueueDepth(usize),
}

// GUI subscription
fn subscribe(&self) -> broadcast::Receiver<SyncEvent>;
// or
fn events(&self) -> impl Stream<Item = SyncEvent> + '_;
```

Align `GenerationProgress` with `heart::Progressive` so shared UI code can treat sync jobs like index jobs.

GPUI note: GPUI owns state in `App`; async work should land via channels/executor patterns Zed uses, updating entities on the main context — the client library should emit events, not call GPUI directly ([gpui](https://www.gpui.rs/), [Zed gpui README](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md)).

---

## 5. Dual-backend trait family

### 5.1 Design: traits + `Routed<L, R>` combinator

```rust
#[async_trait]
pub trait Catalog {
    async fn lookup(&self, id: PackageId) -> Result<Option<PackageRow>, CatalogError>;
    async fn generation(&self, id: PackageId) -> Result<Option<ContentHash>, CatalogError>;
}

#[async_trait]
pub trait TextSearch {
    async fn search(
        &self,
        q: &TextQuery,
        page: &Pagination,
        dep: &DepSet,
    ) -> Result<Page<Scored<Symbol>>, SearchError>;
}

#[async_trait]
pub trait VectorSearch {
    async fn search(
        &self,
        q: &VectorQuery,
        page: &Pagination,
        dep: &DepSet,
        gate: SemanticGate,
    ) -> Result<Page<Scored<Symbol>>, SearchError>;
}

#[async_trait]
pub trait GraphOps {
    async fn expand(&self, id: SymbolId, dep: &DepSet) -> Result<GraphSlice, GraphError>;
}

#[async_trait]
pub trait Compile {
    async fn compile(&self, src: &SourcePath<Trusted>) -> Result<BlobManifest, CompileError>;
}
```

Local and Remote implement each. The router:

```rust
pub struct Routed<L, R> {
    local: L,
    remote: R,
    sync: Arc<SyncEngine>,
    status: Arc<GenerationStatusMap>,
    flights: SingleFlight<FlightKey>,
}

impl<L: TextSearch, R: TextSearch> TextSearch for Routed<L, R> {
    async fn search(&self, q: &TextQuery, page: &Pagination, dep: &DepSet)
        -> Result<Page<Scored<Symbol>>, SearchError>
    {
        self.sync.ensure_background(dep);
        if self.status.all_ready(dep) {
            return self.local.search(q, page, dep).await;
        }
        // single-flight remote
        match self.flights.enter(FlightKey::query(dep, q)) {
            Ticket::Leader(_g) => self.remote.search(q, page, dep).await,
            Ticket::Waiter(n) => {
                // await + re-check local first (may have committed while waiting)
                wait_lost_wakeup_safe(n).await;
                if self.status.all_ready(dep) {
                    self.local.search(q, page, dep).await
                } else {
                    self.remote.search(q, page, dep).await
                }
            }
        }
    }
}
```

### 5.2 Contrast: enum dispatch

```rust
enum Backend {
    Local(LocalText),
    Remote(RemoteText),
    Routed(Routed<LocalText, RemoteText>),
}
```

**Pros:** simple, object-friendly.  
**Cons:** loses per-backend type state; match noise; harder to compose with `Connect` typestates.

**Recommendation:** **static generics** (same as `Tiered<L3>` and heart's "no boxed Cas" choice). Erase only at the GUI/FFI boundary if needed (`Arc<dyn TextSearch>` behind a narrow facade).

### 5.3 Trait family table

| Component | Local impl | Remote impl | Routing policy |
|---|---|---|---|
| Catalog | `SqliteCatalog` | `HttpCatalog` / Index API | Local if pointer Ready; else remote + ensure |
| TextSearch | `LocalTantivy` | `HttpSearch` | All deps Ready → local; else remote |
| VectorSearch | `LocalVectors` or `Unsupported` | `HttpSemantic` + gate | Prefer remote unless fully local embed cache; always require SemanticGate |
| GraphOps | `IrGraph` over local blobs | `TerminusHttp` | Ready → local IR walk; else remote |
| Compile | `EmbeddedForge` | `RemoteCompile` (fleet) | **Trusted always local**; Untrusted never local |
| Cas/Blobs | `DiskCas` + L1 | Presigned S3 / Index blob GET | Existing Tiered; sync engine is the warmer |

### 5.4 Where witnesses enter

```rust
impl LocalTantivy {
    /// Strict local-only API: requires proof of readiness.
    pub async fn search_local(
        &self,
        q: &TextQuery,
        page: &Pagination,
        dep: &DepSet,
        _w: &[SyncedWitness], // one per package or a set witness
    ) -> Result<Page<Scored<Symbol>>, SearchError> { ... }
}
```

`Routed` does **not** need a witness (it decides dynamically). Callers that want **compile-time guarantee of no network** take `search_local` + witnesses + no `NetworkCapability`.

### 5.5 Generalising Tiered CAS

```
TieredCas:     L1 mem → L2 disk → L3 object store     (bytes by hash)
RoutedQuery:   local derived store → remote API       (structured query)
SyncEngine:    warms L2 + local derived stores from L3/INDEX
```

SyncEngine is the **write-side dual** of Tiered read-through: it populates local tiers from remote so future reads never leave the machine.

---

## 6. Wire protocol: client ↔ INDEX

### 6.1 Today

- Server: axum HTTP + JSON for search/admin (`workspace/server/http/`).
- Compiler daemon wire: **postcard** for compile (`registry/protocol.rs`) — not JSON.
- Blobs: content-addressed in object store (`cas/{blake3}`, `ptr/{package-id}`).

GUI client: JSON over HTTP to localhost.

### 6.2 Recommended split (v1)

| Concern | Transport | Why |
|---|---|---|
| Catalog resolve, search, graph, compile-request | **HTTP/JSON (or JSON+postcard hybrid)** | Debuggable; matches server; human tools |
| Bulk IR / source / section blobs | **Direct object-store GET (presigned URLs)** | Offload INDEX; CDN; range requests later |
| Sync control plane | HTTP: `GET /v1/manifest/{package}/{generation}`, `POST /v1/sync/plan` | Small control messages |
| Progress to GUI | In-process channels (same process) | Desktop embeds client |

**Why not gRPC/tonic for v1:**

- Desktop + existing axum surface already JSON.
- Blobs should not go through an application RPC layer if S3-compatible storage exists.
- gRPC shines for bi-di streaming microservices; our sync is primarily **pull of immutable objects**.
- Revisit gRPC if INDEX becomes multi-service mesh with strong schema needs — not blocking for client library.

Sources on tonic: mature ([tonic](https://docs.rs/tonic)), but protocol versioning + desktop packaging cost is real for a first cut.

### 6.3 Manifest-pull protocol (sketch)

```
POST /v1/resolve
  body: { coordinates[], toolchain }
  → { dep_set_id, members: [{ package, generation, manifest_url, blob_base }] }

GET  /v1/manifest/{package}/{generation}
  → BlobManifestDto { files[], ir_ref, references_ref, toolchain, generation_stamp }

POST /v1/sync/plan
  body: { have: [ContentHash], want_from: [{ package, generation }] }
  → { want: [ContentHash], urls: { hash → presigned_url }, expires_at }

GET  {presigned_url}   # object store
  → bytes; client verifies ContentHash::of_bytes
```

Optional later: pack endpoint that multiplexes many small blobs (Git pack / casync) if HTTP/2 request overhead dominates.

### 6.4 Auth

| Surface | Auth |
|---|---|
| INDEX API | Bearer / session token (existing Principal model) |
| Presigned blob GET | URL signature; short TTL; no long-lived blob credentials on client |
| Local REGISTRY | OS file permissions; optional encryption-at-rest (Turso-style key) for shared machines |

### 6.5 Versioning / compat for shipped desktop

1. **`Accept: application/vnd.nudox.v1+json`** (or `X-Nudox-Protocol: 1`).
2. INDEX returns `protocol_min` / `protocol_max` on `/health` or `/v1/hello`.
3. Client refuses to talk if outside window; GUI prompts update.
4. DTOs `#[non_exhaustive]`; unknown fields ignored via serde.
5. Deprecation window: N desktop releases or ≥ 6 months before removing a field.
6. Generation-stamp encoding (`identity_bytes`) must remain stable independently of serde schema bumps — already designed that way in `BlobManifest`.

### 6.6 BlobTransport / iroh Phase-2

> **Full design:** [edge-tech/02-iroh §11 Expanded Phase-2 integration design](../../edge-tech/02-iroh/PLAN.md). Wire fields: [21-wire-protocol](../21-wire-protocol/PLAN.md) (`SyncPlanResponse.providers`).

v1 data plane is **HTTPS presigned GET only**. Phase 2 does **not** change the SyncEngine control loop (resolve → plan → fetch → verify → atomic commit). It only plugs an alternate fetch implementation behind a trait.

**Trait placement:** `BlobTransport` in `client-core` (or `client`), not in network-heavy form inside `heart`:

```rust
#[async_trait]
pub trait BlobTransport: Send + Sync {
    /// Pull hashes into local CAS staging. Desktop needs fetch only (no put).
    async fn fetch(
        &self,
        hashes: &[ContentHash],
        ctx: &FetchCtx,
        sink: &dyn BlobSink,
    ) -> Result<FetchReport, FetchError>;
}

struct HttpPresignTransport { /* providers.http / urls[] from sync plan */ }
struct IrohBlobsTransport { /* optional; iroh Endpoint + Downloader */ }
```

| Concern | v1 | Phase 2 |
|---|---|---|
| Control plane | `POST /v1/sync/plan` HTTP | **Same** (+ optional `client_endpoint_id`, `providers.iroh`) |
| Default transport | `HttpPresignTransport` | still default; flag `sync.transport = http \| iroh \| auto` |
| Optional transport | — | `IrohBlobsTransport` (LAN / regional provider assist) |
| Integrity | Client BLAKE3 verify | Same; Bao streaming verify is additive |
| Commit / Ready | SyncEngine + DiskCas | Unchanged |
| Subscription / “package updated” | HTTP poll / later SSE | **Stays HTTP** — not iroh-gossip as primary |
| Catalog authority | INDEX | INDEX — **not** iroh-docs |

**Auto fallback:** if iroh dial/UDP/relay fails, complete residual hashes via HTTPS presign automatically. iroh must never block offline-capable product progress when S3 URLs exist.

**Desktop Endpoint:** lazy / on-demand during sync (not always-on) for battery; no Endpoint required after generation is Ready.

**Explicit non-goals for client-sync:** iroh-docs as package catalog; gossip as primary subscription; dual FsStore+DiskCas as SoT; desktop upload of INDEX CAS.

---

## 7. Sync engine design

### 7.1 Responsibilities

1. Accept `ensure(DepSet)` from routers / project flow.
2. For each `(package, generation)` not Ready: fetch manifest → plan want/have → download → verify → atomic commit.
3. Emit `SyncEvent`s.
4. Honour connectivity and prioritisation (interactive package first).
5. Maintain single-flight per generation and per hash.

### 7.2 Prioritisation (Steam / Spotify progressive)

1. **Interactive:** packages currently in the open symbol view / search hits.
2. **Critical path:** direct dependencies of the active project.
3. **Background:** transitive closure, low priority.
4. **Idle:** prefetch popular versions (optional, disk-budgeted).

### 7.3 Disk budget / GC

- Content-addressed sharing across generations reduces growth.
- GC: refcount or mark generations still in any DepSet / user pin; delete unreferenced CAS objects.
- Mirror server CAS GC ideas from `server/poll.rs` comments / GC loops — client-side simpler (LRU of generations).

### 7.4 Failure / resume

- Staging is restart-safe: incomplete downloads discarded or resumed via HTTP Range if store supports it.
- Failed generation stays Failed until retry; does not flip Ready.
- Integrity mismatch: delete staging object; never promote.

### 7.5 Interaction with local indexes

When generation becomes Ready:

1. Insert catalog rows.
2. Enqueue local Tantivy segment update for that package generation.
3. Optional: local vector embed job (gated; expensive — may remain remote-only for v1).
4. Graph: either materialise adjacency from IR or query IR on demand.

Local index updates should themselves be **generation-scoped** so a query over DepSet S never sees symbols outside S.

---

## 8. Recommended client architecture (synthesis)

### 8.1 Crate layout

```
workspace/
  client-core/     # pure: types, sync state machine, routing decisions, traits
  client/          # tokio: SyncEngine, SQLite REGISTRY, HTTP INDEX, CAS wiring
  gui/             # GPUI; depends on client, not on server HTTP DTOs directly
  heart/           # shared vocabulary (existing)
  registry/        # server-side + shared blob/manifest types (extract shared DTOs carefully)
```

**Pragmatic alternative:** one `client` crate with modules `core`, `sync`, `registry_local`, `index_remote`, `route` if Buck/Cargo overhead is painful — preserve the purity boundary with `#![cfg]` tests that compile core without tokio.

### 8.2 Typestate map (final)

| State | Encoding | Justification |
|---|---|---|
| Store/client connected | `Cold`/`Live` typestate | Matches heart::Connect |
| Project open pipeline | `Unresolved`/`Resolved`/`Indexed` typestate | Monotonic UX flow |
| Source trust | `Trusted`/`Untrusted` type param | Compile API safety |
| Network / compute / semantic | Capability tokens | Runtime policy + audit |
| Per-generation sync | `enum GenerationStatus` | Dynamic, non-monotonic |
| Local-only guarantee | `SyncedWitness` | Optional proof, not Client state |
| Cursor freshness | Existing `Enforced`/`Advisory` | Already correct |

### 8.3 Sync protocol summary

- Unit: **package generation** = manifest + section closure.
- Mechanism: **want/have over ContentHash**, pull via presigned URLs.
- Atomic commit before local takeover.
- Remote serves queries until commit.
- Offline degrades to Ready subset.

### 8.4 Why not full Electric/PowerSync/Turso

Those products optimise **mutable collaborative state**. nudox dependency intelligence is **immutable content-addressed artifacts + authoritative catalog**. Manifest-pull is simpler, more correct, and aligns with existing `BlobManifest` / CAS design.

Borrow:

- Electric: **shapes / subset sync** → DepSet as shape.
- PowerSync: **buckets + priority** → progressive ensure.
- Turso: **local read latency after sync** + status UX.
- Nix/Git: **actual wire mechanics**.

---

## 9. Full Rust API sketches

The following is compilable-looking sketch code (~250 lines) consolidating the design. Names align with heart where possible.

```rust
//! nudox-client public surface (sketch)

use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use heart::{
    cache::{Cas, SingleFlight, Ticket},
    connection::{Cold, Connect, Live},
    content::ContentHash,
    package::Coordinates,
    progress::{Percent, Progressive},
    ConnectError, PackageId, Scored, Symbol, Toolchain,
};
use tokio::sync::broadcast;

// ── Trust & project typestates ───────────────────────────────────────────────

pub enum Trusted {}
pub enum Untrusted {}
pub enum Unresolved {}
pub enum Resolved {}
pub enum Indexed {}

pub struct SourcePath<Trust> {
    pub path: PathBuf,
    _trust: PhantomData<Trust>,
}

impl SourcePath<Trusted> {
    pub fn new(path: PathBuf) -> Self {
        Self { path, _trust: PhantomData }
    }
}

// ── Capabilities ─────────────────────────────────────────────────────────────

#[must_use = "NetworkCapability authorises remote IO"]
pub struct NetworkCapability {
    pub reason: &'static str,
}

#[must_use]
pub struct LocalComputeCapability {
    pub reason: &'static str,
}

/// Proof a generation is Ready locally. Minted only by SyncEngine::commit.
#[derive(Clone, Debug)]
pub struct SyncedWitness {
    pub package: PackageId,
    pub generation: ContentHash,
}

// ── Dependency set (generation pin) ──────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DepMember {
    pub package: PackageId,
    pub generation: ContentHash, // generation-stamp hash
}

#[derive(Clone, Debug)]
pub struct DepSet {
    pub members: Vec<DepMember>,
    pub set_id: ContentHash,
}

impl DepSet {
    pub fn fingerprint(members: Vec<DepMember>) -> Self {
        let mut h = ContentHash::builder();
        for m in &members {
            h.update(m.package.as_uuid().as_bytes());
            h.update(m.generation.as_bytes());
        }
        let set_id = h.finalize();
        Self { members, set_id }
    }
}

// ── Sync status (runtime, not typestate) ─────────────────────────────────────

#[derive(Clone, Debug)]
pub enum GenerationStatus {
    Absent,
    Pending,
    Pulling { done: u32, total: u32 },
    Ready,
    Failed { message: String },
}

#[derive(Clone, Debug)]
pub enum Connectivity {
    Online,
    Degraded { detail: String },
    Offline,
}

#[derive(Clone, Debug)]
pub enum SyncEvent {
    Started { package: PackageId, generation: ContentHash },
    GenerationProgress { package: PackageId, done: u32, total: u32 },
    BlobProgress { hash: ContentHash, bytes: u64, total: Option<u64> },
    GenerationReady { package: PackageId, generation: ContentHash },
    GenerationFailed { package: PackageId, generation: ContentHash, error: String },
    Connectivity(Connectivity),
}

// ── Config & client facade ───────────────────────────────────────────────────

pub struct ClientConfig {
    pub registry_path: PathBuf,
    pub cas_path: PathBuf,
    pub index_base_url: String,
    pub auth_token: Option<String>,
    pub force_offline: bool,
    pub disk_budget_bytes: u64,
}

pub struct Client<S = Live> {
    inner: Arc<ClientInner>,
    _state: PhantomData<S>,
}

struct ClientInner {
    // Concrete types omitted: SqliteCatalog, DiskCas, HttpIndex, SyncEngine, ...
    events: broadcast::Sender<SyncEvent>,
    flights: SingleFlight<FlightKey>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum FlightKey {
    Generation(PackageId, ContentHash),
    Blob(ContentHash),
    Query(ContentHash /* set_id */, ContentHash /* q */),
}

impl Client<Cold> {
    pub fn open(config: ClientConfig) -> Result<Self, ConnectError> {
        // open sqlite, disk cas, build http client — no network probe yet
        todo!()
    }
}

impl Connect for Client<Cold> {
    type Live = Client<Live>;

    async fn connect(self) -> Result<Self::Live, ConnectError> {
        // probe INDEX /health unless force_offline
        todo!()
    }
}

impl Client<Live> {
    pub fn project(&self, path: PathBuf) -> Result<Project<Unresolved>, ProjectError> {
        Project::open(path)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SyncEvent> {
        self.inner.events.subscribe()
    }

    pub fn network(&self, reason: &'static str) -> Option<NetworkCapability> {
        // None if offline / force_offline
        Some(NetworkCapability { reason })
    }

    pub fn local_compute(&self, reason: &'static str) -> LocalComputeCapability {
        LocalComputeCapability { reason }
    }

    pub fn catalog(&self) -> RoutedCatalog<'_> { todo!() }
    pub fn text_search(&self) -> RoutedTextSearch<'_> { todo!() }
    pub fn graph(&self) -> RoutedGraph<'_> { todo!() }
    pub fn sync(&self) -> &SyncEngine { todo!() }
}

// ── Project pipeline ─────────────────────────────────────────────────────────

pub struct ProjectError;
pub struct Project<S> {
    root: PathBuf,
    dep_set: Option<DepSet>,
    _state: PhantomData<S>,
}

impl Project<Unresolved> {
    pub fn open(root: PathBuf) -> Result<Self, ProjectError> {
        Ok(Self { root, dep_set: None, _state: PhantomData })
    }

    pub async fn resolve(
        self,
        _toolchain: &Toolchain,
        _local: LocalComputeCapability,
    ) -> Result<Project<Resolved>, ProjectError> {
        // parse Cargo.toml / package.json / etc.; path deps trusted
        todo!()
    }
}

impl Project<Resolved> {
    pub fn trusted_roots(&self) -> Vec<SourcePath<Trusted>> { todo!() }
    pub fn dependency_set(&self) -> &DepSet { self.dep_set.as_ref().unwrap() }

    pub async fn ensure_indexed(
        self,
        client: &Client<Live>,
        net: Option<NetworkCapability>,
    ) -> Result<Project<Indexed>, ProjectError> {
        let deps = self.dependency_set().clone();
        client.sync().ensure(&deps, net).await.map_err(|_| ProjectError)?;
        Ok(Project { root: self.root, dep_set: self.dep_set, _state: PhantomData })
    }
}

impl Project<Indexed> {
    pub fn session<'a>(&'a self, client: &'a Client<Live>) -> Session<'a> {
        Session { client, deps: self.dep_set.as_ref().unwrap() }
    }
}

pub struct Session<'a> {
    client: &'a Client<Live>,
    deps: &'a DepSet,
}

impl Session<'_> {
    pub async fn search_text(&self, q: &str) -> Result<Vec<Scored<Symbol>>, SearchError> {
        self.client.text_search().search(q, self.deps).await
    }
}

// ── Dual backends + router ───────────────────────────────────────────────────

pub trait Catalog: Send + Sync {
    fn lookup(
        &self,
        id: PackageId,
    ) -> impl std::future::Future<Output = Result<Option<PackageRow>, CatalogError>> + Send;
}

pub trait TextSearch: Send + Sync {
    fn search(
        &self,
        q: &str,
        deps: &DepSet,
    ) -> impl std::future::Future<Output = Result<Vec<Scored<Symbol>>, SearchError>> + Send;
}

pub struct LocalCatalog { /* sqlite */ }
pub struct RemoteCatalog { /* http */ }
pub struct LocalText { /* tantivy */ }
pub struct RemoteText { /* http */ }

pub struct PackageRow {
    pub id: PackageId,
    pub generation: ContentHash,
    pub name: String,
}

pub struct CatalogError;
pub struct SearchError;
pub struct SyncError;
pub struct CompileError;

pub struct Routed<L, R> {
    local: L,
    remote: R,
    status: Arc<StatusMap>,
    sync: Arc<SyncEngine>,
    flights: Arc<SingleFlight<FlightKey>>,
    net: Connectivity,
}

pub struct StatusMap; // package+generation → GenerationStatus

impl StatusMap {
    pub fn all_ready(&self, deps: &DepSet) -> bool { todo!() }
    pub fn witnesses(&self, deps: &DepSet) -> Option<Vec<SyncedWitness>> { todo!() }
}

impl<L: TextSearch, R: TextSearch> TextSearch for Routed<L, R> {
    async fn search(&self, q: &str, deps: &DepSet) -> Result<Vec<Scored<Symbol>>, SearchError> {
        self.sync.ensure_background(deps);
        if self.status.all_ready(deps) {
            return self.local.search(q, deps).await;
        }
        match self.net {
            Connectivity::Offline => self.local.search(q, deps).await, // partial
            _ => self.remote.search(q, deps).await,
        }
    }
}

pub type RoutedCatalog<'a> = Routed<&'a LocalCatalog, &'a RemoteCatalog>;
pub type RoutedTextSearch<'a> = Routed<&'a LocalText, &'a RemoteText>;
pub type RoutedGraph<'a> = Routed<&'a LocalGraph, &'a RemoteGraph>;
pub struct LocalGraph;
pub struct RemoteGraph;

// ── Sync engine ──────────────────────────────────────────────────────────────

pub struct SyncEngine {
    events: broadcast::Sender<SyncEvent>,
    // cas: Tiered<RemoteCas>, catalog, http, flights, ...
}

impl SyncEngine {
    pub async fn ensure(
        &self,
        deps: &DepSet,
        net: Option<NetworkCapability>,
    ) -> Result<Vec<SyncedWitness>, SyncError> {
        let _net = net.ok_or(SyncError)?;
        let mut out = Vec::new();
        for m in &deps.members {
            out.push(self.ensure_one(m).await?);
        }
        Ok(out)
    }

    pub fn ensure_background(&self, deps: &DepSet) {
        // spawn tasks; no wait
        let _ = deps;
    }

    async fn ensure_one(&self, m: &DepMember) -> Result<SyncedWitness, SyncError> {
        // 1. if Ready → witness
        // 2. single-flight generation
        // 3. GET manifest
        // 4. want = closure \ local_have
        // 5. download presigned; verify; stage
        // 6. atomic commit → Ready
        // 7. emit GenerationReady
        let _ = m;
        todo!()
    }
}

// ── Compile trust boundary ───────────────────────────────────────────────────

pub struct ForgeHandle; // wraps ForgeRuntime<Ready>

pub trait CompileLocally {
    fn compile_locally(
        &self,
        forge: &ForgeHandle,
        cap: LocalComputeCapability,
    ) -> impl std::future::Future<Output = Result<ContentHash, CompileError>> + Send;
}

impl CompileLocally for SourcePath<Trusted> {
    async fn compile_locally(
        &self,
        _forge: &ForgeHandle,
        _cap: LocalComputeCapability,
    ) -> Result<ContentHash, CompileError> {
        todo!()
    }
}

impl SourcePath<Untrusted> {
    pub async fn fetch_from_index(
        &self,
        _client: &Client<Live>,
        _net: NetworkCapability,
    ) -> Result<ContentHash, SyncError> {
        // never compiles locally
        todo!()
    }
}

// ── Manifest / pull protocol types ───────────────────────────────────────────

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ManifestDto {
    pub package: PackageId,
    pub generation: ContentHash,
    pub sections: Vec<SectionDto>,
    pub ir_ref: ContentHash,
    pub references_ref: ContentHash,
}

#[derive(Clone, Debug)]
pub struct SectionDto {
    pub path: String,
    pub hash: ContentHash,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct SyncPlanRequest {
    pub have: Vec<ContentHash>,
    pub want_from: Vec<DepMember>,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SyncPlanResponse {
    pub want: Vec<ContentHash>,
    pub urls: Vec<(ContentHash, String)>, // presigned (v1)
    pub protocol: u16,
    // Phase 2 (see §6.6 / Plan 21): optional providers.http + providers.iroh
}

// ── Progressive sync job (align with heart::Progressive) ─────────────────────

#[derive(Clone, PartialEq, Eq)]
pub enum SyncPhase {
    ResolveManifest,
    Plan,
    FetchBlobs,
    Commit,
    IndexLocal,
}

pub struct GenerationSyncJob {
    pub package: PackageId,
    pub generation: ContentHash,
    pub phase: SyncPhase,
    pub fraction: Percent,
}

impl Progressive for GenerationSyncJob {
    type Phase = SyncPhase;
    const PHASES: &'static [SyncPhase] = &[
        SyncPhase::ResolveManifest,
        SyncPhase::Plan,
        SyncPhase::FetchBlobs,
        SyncPhase::Commit,
        SyncPhase::IndexLocal,
    ];
    fn status(&self) -> heart::progress::ProgressStatus<SyncPhase> {
        heart::progress::ProgressStatus::Progressing {
            phase: self.phase.clone(),
            fraction: self.fraction,
        }
    }
}
```

---

## 10. Migration path (server → client library)

1. **Extract shared DTOs / BlobManifest** into a path both server and client use (already in `registry::blob` — client may depend on a thin `nudox-protocol` slice of registry or move shared types to `heart` / new crate).
2. **Implement `SqliteCatalog` + DiskCas registry** on desktop (see parallel research on sqlite index if present under `11-sqlite-index`).
3. **Implement Remote* as wrappers** around today's axum routes (GUI's `BackendClient` logic moves here).
4. **Add SyncEngine** + events; wire GUI status bar.
5. **Introduce Project typestates** in GUI open-project flow.
6. **Flip routing** package-by-package as local Tantivy segments come online.
7. **Presigned blob path** once INDEX can mint URLs (until then, blob GET via INDEX proxy is fine).

Do not require full local search parity before shipping progressive sync — remote-first with warming is the product.

---

## 11. Comparison matrix (decision record)

| Approach | Bidirectional? | Mutable? | Fit for CA blobs | Complexity | Verdict |
|---|---|---|---|---|---|
| Turso embedded replica | Writes remote (classic) | SQL pages | Poor | Med | UX only |
| Turso Sync push/pull | Yes | SQL CDC | Poor | Med | Skip as infra |
| Electric shapes | Read-path | Rows | Partial (catalog) | High if full | Shape idea only |
| PowerSync buckets | Yes | Rows | Partial | High | Priority idea only |
| CRDT (cr-sqlite, Automerge) | Yes | CRDT | Wrong model | High | Reject |
| Nix substituter | Pull | CA | Excellent | Low–Med | **Adopt pattern** |
| Git want/have | Pull | CA | Excellent | Low–Med | **Adopt pattern** |
| casync chunks | Pull | CA chunks | Good later | Med | v2 optimisation |
| Custom manifest-pull | Pull | CA + catalog | **Best** | Low | **Recommended** |

---

## 12. Risks & open questions

1. **Local Tantivy segment-per-generation** vs single index with package filters — query isolation vs cost.
2. **Vector search offline:** embed locally (heavy models) vs always remote; SemanticGate issuance on desktop.
3. **Graph offline without Terminus:** IR-only expansion fidelity vs full graph DB.
4. **Presigned URL TTL** vs long downloads on slow links.
5. **Shared CAS across projects** disk accounting and GC policy.
6. **Protocol crate ownership:** keep in `registry` vs extract `nudox-wire` for client+server+daemon.
7. **GPUI async model** integration details (smol vs tokio — GUI already constructs a throwaway tokio runtime for reqwest).
8. **Trusted workspace members that shadow registry packages** — resolution order and identity.
9. **Whether `SyncedWitness` is Clone** and how long it remains valid if GC deletes a generation.
10. **Partial DepSet ready:** allow hybrid local+remote joins with careful result tagging, or always full-remote until all Ready? (Recommendation: full-remote until all Ready for generation consistency; optional hybrid later with explicit `ResultSource` tags.)

---

## 13. Actionable recommendations (priority order)

1. **Build a custom manifest-pull SyncEngine** (Nix/Git shaped); do not adopt CRDT/PowerSync/Electric as infrastructure.
2. **Atomic generation commit** before any local takeover; reuse `BlobManifest` generation-stamp vs CAS-key split.
3. **Typestates for Project + Connect + Trust only**; runtime enums + witnesses for sync status.
4. **Generalise SemanticGate** into Network / LocalCompute capabilities; keep consumable gates for expensive paths.
5. **`Routed<L,R>` + static generics** mirroring `Tiered<L3>`; reuse `SingleFlight`.
6. **HTTP JSON control plane + presigned blob GETs**; delay gRPC. Phase 2: optional `BlobTransport` / iroh behind the same plan (§6.6; edge-tech/02-iroh §11).
7. **Partial sans-IO** in `client-core` for sync state machine + routing decisions.
8. **`SyncEvent` broadcast** aligned with `heart::Progressive` for GUI.
9. **Protocol version negotiation** + `#[non_exhaustive]` DTOs from day one.
10. **Migrate GUI `BackendClient`** behind `Client<Live>` facade without changing UX first (remote-only path), then enable warming.

---

## 14. Executive summary

The nudox desktop client must present a single API for code intelligence while dependencies are served from a remote INDEX and progressively materialised into a local REGISTRY. The right prior art is **not** bidirectional CRDT sync (PowerSync, cr-sqlite, Automerge) but **content-addressed substitution**: Nix binary caches, Git want/have negotiation, and casync-style later optimisations. ElectricSQL’s post-rewrite **read-path shapes** and PowerSync’s **priority buckets** inform subset selection and progressive UX; Turso embedded replicas inform **local-read latency and sync status**, not storage mechanics. As of 2026, Turso still documents page-level Embedded Replicas with RYW and periodic `sync()`, while recommending newer logical CDC “Turso Sync” for greenfield sync — neither replaces a BLAKE3 manifest protocol.

Codebase precedents already encode the needed patterns: `heart::Connect` Cold/Live typestates; `Cas` + `Tiered` + `NoL3` read-through; `SingleFlight` stampede control; `SemanticGate` capability tokens; sealed `SnapshotPolicy` with wire-surviving brands; `BlobManifest` dual hashing (generation stamp ≠ CAS key); and `Progressive` job progress. The GUI today (`BackendClient`) is remote-only HTTP and should become a thin consumer of the new client facade.

**Design stance:** Use typestates for **monotonic** concerns (connection readiness, project resolve pipeline, trust plane). Keep **per-generation sync status** as runtime state plus optional `SyncedWitness` proofs — do not model `Store<Syncing>`. Every query routes through a policy: if the entire generation-pinned `DepSet` is locally Ready, use local backends; else hit remote while a background SyncEngine pulls the closure; offline degrades to Ready subset only. Generations become visible atomically after full hash-verified commit so mid-sync never mixes G_old and G_new.

**Architecture:** dual-backend traits (`Catalog`, `TextSearch`, `VectorSearch`, `GraphOps`, `Compile`) with Local/Remote impls and a `Routed<L,R>` combinator; Compile is physically unavailable on `SourcePath<Untrusted>`. Wire protocol: HTTP JSON for resolve/search/plan; **presigned object-store GETs** for blobs (v1), with Phase-2 optional `BlobTransport` (`HttpPresignTransport` | `IrohBlobsTransport`, §6.6); protocol version negotiation for shipped desktops. Optional `client-core` sans-IO for the pull state machine (Firezone-style), driven by a tokio runtime crate. This yields remote-immediate UX, transparent local takeover, offline safety, and compile-time trust boundaries without importing a full local-first sync product.

---

## 15. Edge-tech decisions (transport & CAS network)

**Status:** decision pointers from `.research/edge-tech/` (2026-07-16). Append-only; does not replace §1–§14.

**Unified map:** [edge-tech/00-DECISIONS.md](../../edge-tech/00-DECISIONS.md)

### 15.1 iroh — Phase-2 optional `BlobTransport`

| Item | Decision |
|---|---|
| **v1 data plane** | **Keep** HTTP JSON control + presigned S3/CAS GETs (this plan + [21-wire-protocol](../21-wire-protocol/PLAN.md)) |
| **iroh-blobs** | **Phase 2** optional multi-source / LAN peer-assist transport (`IrohBlobsTransport` behind `BlobTransport` trait) |
| **iroh-docs / gossip as catalog** | **Reject** — server-authoritative `BlobManifest` + HTTP catalog; CRDT multi-writer is dead weight |
| **BLAKE3 alignment** | Strong fit (`ContentHash` ↔ iroh `Hash`); still not a v1 reason to replace S3 |
| **Design now** | Trait-shape `BlobTransport` so HTTP remains default; iroh plugs in without rewiring SyncEngine |
| **Build-ready detail** | This plan **§6.6**; full Phase-2 design [02-iroh §11](../../edge-tech/02-iroh/PLAN.md); wire `providers.iroh` in [21 §4.3](../21-wire-protocol/PLAN.md) |

**Source:** [edge-tech/02-iroh](../../edge-tech/02-iroh/PLAN.md)  
**Topology:** [22-crate-topology](../22-crate-topology/PLAN.md) optional backends table

### 15.2 IPFS — reject network; borrow pattern only

| Item | Decision |
|---|---|
| **Public IPFS / Amino DHT** | **Reject** as package distribution (legal, pin/GC, free-gateway decline, small-blob path) |
| **Private IPFS Cluster** | **Defer / niche** — loses to S3+CDN+IAM for INDEX |
| **Bitswap wire** | **Reject** — keep custom HTTP want/have (§1.3 already notes Bitswap as prior art only) |
| **Steal** | Content-addressed set difference, optional CID/CAR at export boundary |
| **Do not** | Require Kubo on desktop; publish untrusted source to public DHT |

**Source:** [edge-tech/04-ipfs-and-cas](../../edge-tech/04-ipfs-and-cas/PLAN.md) · storage addendum: [13-storage §12](../13-storage/PLAN.md)

### 15.3 Related sync-edge (surrounding)

| Pattern | Verdict | Note |
|---|---|---|
| Electric “shapes” / PowerSync “buckets” | **Borrow vocabulary only** | SubscribeSpec / priority buckets — already §1 / comparison matrix |
| WebTorrent / BT as primary | **Reject** | Prefer iroh Phase-2 if peer assist ever ships |
| CRDT catalog (cr-sqlite, etc.) | **Reject** | Unchanged vs §11 |

**Source:** [edge-tech/05-surrounding-edge](../../edge-tech/05-surrounding-edge/PLAN.md)
