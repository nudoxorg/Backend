> **Research, not the final specification:** use the [final v2 index](../architecture/README.md). These reports preserve alternatives; final v2 equations, package boundaries and corrections take precedence.

# Product structure v2: one version frontier, objects, and deltas

## Decision

The interface needs a smaller product kernel, not another layer of renamed crates. The kernel should
be a versioned object graph plus a typed delta algebra. Storage, compiler publication, index/query
execution, GUI state, and remote synchronization all consume and produce the same version frontier.
An edit is committed once as a typed delta, produces immutable canonical objects and derived views,
and becomes observable through a cursor. Local and remote placement are policy over the same objects;
they are not separate sources of truth.

The current repository already contains useful pieces of this idea, but they are split across 53
workspace crates. `heart-identity` has domain-separated content/generation/index IDs
(`heart/identity/lib.rs:1-15`, `:20-55`); `heart-object` describes immutable object references and
residency (`heart/object/lib.rs:1-18`); `heart-root` builds generation roots and structural diffs
(`heart/root/lib.rs:8-46`); and `heart-adaptive` already models pinned facts, local/remote residence,
demand, and resource budgets (`heart/adaptive/lib.rs:28-109`, `:216-251`). The missing product move is
to make these one foundational contract and make every layer speak its delta/cursor vocabulary.

The interface rewrite shows why this is necessary. It wants one `Library::execute(Command) -> Reply`
(`interface/library/command.rs:143-240`), but `interface-core` independently owns a much larger
`ApplicationInput` and capability service (`interface/core/model.rs:68-187`,
`interface/core/service.rs:30-44`). Durable library reads/adds/search/graph are still stubs
(`interface/library/library.rs:139-150`, `:189-239`, `:264-328`), so a new plan must make the
authoritative object/commit machinery real before tuning projections.

## Foundation: versioned objects and typed deltas

### Canonical objects

Every durable value is an immutable object addressed by a domain-separated ID. A `Version` is a
content root plus parent frontier; a `ObjectRef` names one canonical object and carries its schema,
logical length, and authority. Existing `ObjectRef`/`ObjectKind`/`ObjectLength` are the right compact
shape (`heart/object/lib.rs:13-18`). Keep the object bytes canonical and deterministic. Never merge
two semantic object byte streams by last-writer-wins.

```rust
pub struct Version {
    pub id: VersionId,                 // hash of parents + delta + root object
    pub parents: Frontier,
    pub root: ObjectRef<RootObject>,
    pub author: ActorId,
}

pub struct ObjectRef<K> {
    pub id: ContentId<K>,
    pub schema: SchemaId,
    pub len: ObjectLength,
    _kind: PhantomData<K>,
}
```

`VersionId` and all domain IDs should be copyable and domain-separated. The current identity crate
already describes streamed content hashing and distinguishes logical content from encoded artifacts
(`heart/identity/lib.rs:7-15`); extend that registry with `VersionDomain`, `DeltaDomain`,
`ViewDomain`, and `CursorDomain`. A physical copy changing RAM/NVMe/remote tier must never change an
object ID.

### Typed delta algebra

One enum describes all product mutations. It is intentionally small and typed by domain. A compiler
run does not mutate a semantic object; it appends a publication delta. A search query does not mutate
canonical data; it appends an ephemeral query intent and receives a derived result view. GUI actions
append intents/settings or request computation. Synchronization transports deltas and object blocks.

```rust
pub enum Delta {
    // Mutable user-owned intent/settings; mergeable.
    Intent(IntentDelta),
    Settings(SettingsDelta),

    // Immutable product facts; accepted only after validation and authority checks.
    Publish(PublicationDelta),
    Shelf(ShelfDelta),
    Derive(DerivationDelta),

    // Non-durable request lifecycle, scoped to one process/session.
    Query(QueryDelta),
    View(ViewDelta),
}

pub struct Envelope {
    pub base: Frontier,
    pub delta: Delta,
    pub result: ResultRoot,       // resulting immutable root or typed rejection
    pub id: DeltaId,
}
```

`ShelfDelta` contains requested coordinate, compile status transition, publication locator, and
head change. `PublicationDelta` contains source authority, semantic image, and generation links.
`DerivationDelta` records exact/lexical/graph/vector view object IDs and the source semantic root.
`QueryDelta` contains a validated query snapshot, lane set, cursor, and requested budget; its results
are an immutable `ViewObject` keyed by query plus source frontier. `ViewDelta` carries UI projection
updates such as stable rows, loading/failed state, selected key, and the cursor consumed.

Invariants:

* A delta names its exact base frontier and cannot apply to an unrelated head without an explicit
  rebase result.
* Every derived object records `source_version` and `source_object`; rebuilding is deterministic.
* Applying a delta is atomic from the reader’s perspective: the new root and cursor become visible
  together.
* A cursor is a position in the version/delta stream, not a timestamp or an epoch guessed from a
  second file.
* Retried application is idempotent by `DeltaId`; duplicate notification is harmless.

### One version frontier

Replace separate shelf heads, epoch files, remote generations, index snapshot IDs, GUI watcher state,
and query cache invalidation with a `Frontier`. A frontier can contain one linear local head or a
bounded set of heads during offline branching. Each head points to a complete immutable root. A
remote response carries the frontier it read and the frontier it produced.

```rust
pub struct Frontier {
    pub heads: SmallVec<[VersionId; 2]>,
}
pub struct DeltaCursor {
    pub frontier: Frontier,
    pub sequence: u64,
}
```

A reader subscribes to `DeltaCursor`; notifications are hints to poll the authoritative manifest/log.
The reader accepts a new cursor only after verifying object IDs and parent links. A remote that is
healthy but answers under another frontier is `Inconsistent { observed }`, matching the useful
existing adaptive vocabulary (`heart/adaptive/lib.rs:173-190`). “Shelf changed,” “index changed,”
and “remote inconsistent” then become one product state: frontier advanced or diverged.

## CRDT boundary

Use CRDT semantics only for user mutable intent that can safely converge:

* add/remove/open tab intents, tab order, collapsed groups, selected result, pane sizes;
* preferences and per-user settings;
* optional annotations that are explicitly user-authored and never feed semantic authority.

Represent each as an operation with actor, Lamport/HLC ordering, and a merge rule. The merge result is
itself a settings/intents object and can be rebuilt into a GUI view.

Do not CRDT-merge compiler semantic images, declaration identities, package publication, index
segments, graph edges, signatures, or query results. These are immutable derived facts. If two offline
branches publish different bytes for the same package coordinate, retain both `PublicationId`s and
surface a typed semantic collision; choose a shelf head by an explicit user/system delta after
validation. Blind register LWW would hide a real compiler disagreement. `ExactAddress` and content
keys are authority-bearing values, and current code already distinguishes exact and family-only key
spellings (`interface/identity/key.rs:45-72`, `:85-92`).

## One edit, end to end

The following is the required product walkthrough for “add `cargo:serde@1.0.196`, then search for
`deserialize` while offline.” Every step crosses the same object/delta/cursor boundary.

1. **Intent.** CLI argv, MCP JSON, or GUI add-field lowers into `IntentDelta::RequestPackage` with a
   validated `PackageCoordinate` and client `base: Frontier`. Transport parsing is erased here; the
   application sees one typed delta.
2. **Admission.** The local writer reads the authoritative manifest, checks the frontier, and appends
   `ShelfDelta::Requested`. It writes an immutable envelope and atomically advances the manifest.
   Lock lease ownership is recorded as an operation object, not as an untracked boolean. A repeated
   request is idempotent by coordinate/idempotency key.
3. **Compile.** The compiler capability consumes the admitted operation and emits progress deltas
   (`CompilePhase` is an event, not a new source of truth). Source bytes, compiler recipe, and
   diagnostics are immutable objects. Existing compiler application currently binds requests to native
   compilation/publication and imports the interface core (`compiler/application/Cargo.toml:1-22`);
   remove that upward edge by depending on the contract crate instead.
4. **Publish.** Validated semantic image and source authority become `PublicationDelta`, producing a
   new generation/version root. The compiler cannot mint a shelf head by itself; the application
   commit stage records the publication locator and the resulting frontier in one envelope.
5. **Derive.** Exact, lexical, graph, and vector projections are computed from the semantic object,
   each producing a `DerivationDelta` keyed by `source_version`. Existing index crates describe these
   concerns separately—build, publish, retrieval, Tantivy, Trustfall, Qdrant—but they should become
   implementations behind one derivation service. If a lane is unavailable, the query view records
   `Coverage::Unavailable`; it does not create an empty authoritative index.
6. **Observe.** All three surfaces consume `DeltaCursor`s. A watcher gets a notification, reads the
   manifest, verifies the new frontier, and folds deltas through a cursor. Notification loss is safe:
   the next poll asks for deltas after the last cursor. Notification duplication is safe:
   `DeltaId` deduplication makes apply idempotent.
7. **GUI stable row.** The GUI reducer receives `ViewDelta::ShelfRows { cursor, rows }`. A stable row
   key is `PackageCoordinate` for shelf rows and `ExactAddress`/`ContentKey` for declaration rows;
   status/progress is a value attached to that key. It updates an existing row in place, preserving
   focus/selection and animation. Current `LibraryStore` already models row status and package keys
   (`interface/gui/store/library.rs:20-79`, `:298-326`); make it a projection of the cursor rather
   than a second shelf database.
8. **Query snapshot.** Search lowers to `QueryDelta::Run { source: Frontier, request,
   cursor }`. The query engine pins the frontier, runs available lanes, and emits a `QueryViewObject`
   with hits keyed by compact `ContentKey`/`EntityId`, lane coverage, score, and `next_cursor`.
   Materialize full `Symbol`, signature, and summary only for visible rows. Current `SearchTerminal`
   stores full owned hits and request (`interface/search/terminal.rs:15-55`), while `merge_lanes` is
   linear lookup plus sorting (`:57-112`); the object/view boundary is where bounded candidate tables
   and lazy row projection belong.
9. **Offline remote.** The remote receives the query’s pinned frontier. If it has that frontier, it
   computes or streams a derived query view. If it has another, it returns `RemoteHealth::Inconsistent`
   plus its frontier; local policy can continue from local immutable objects. No shelf/head is
   overwritten by a remote notification. A later typed sync delta may add remote objects or request a
   validated rebase.

The same walkthrough handles remove, reindex, settings, and tab navigation. Only delta variant and
derived view differ; commit, cursor, frontier validation, placement, and observation do not.

## Proposed 10-package crate decomposition

The target is ten product packages plus language/native leaves and thin surfaces. “Package” means a
Cargo library or a deliberately grouped module; not every implementation detail needs a crate.

```text
product-types       no_std domain IDs, Version, Frontier, ObjectRef, Delta, Cursor, limits
product-object      canonical encoding, object store trait, packs, validation, hydration views
product-commit      append/replay log, manifest, atomic commit, cursor subscriptions, idempotency
product-domain      package/address/semantic vocabulary, publication/shelf/query/view object schemas
product-compute     capability traits, operation lifecycle, local-first placement, budgets, progress
product-compile     compiler orchestration and publication adapter; language leaves plug in here
product-index       derive exact/lexical/graph/vector views; Tantivy/Trustfall/Qdrant leaves
product-query       typed query planner, candidate heap, snapshots, coverage, result cursors
product-sync        object/delta exchange, frontier reconciliation, offline branch/collision policy
product-present     render events, Markdown/text/MCP schema projection, GUI reducer/view projections
surfaces            nudox CLI, nudox-mcp, nudox-gui binaries as tiny adapters
```

This is intentionally smaller than the current 53-member workspace. `product-types` and
`product-domain` replace much of the current heart identity/schema/adaptive vocabulary plus interface
identity/search/document model without depending on compiler or transport. `product-object` and
`product-commit` replace the fragmented object/object-pack/root/hydration/memory/frame/view/journal
plumbing with one storage authority. `product-compute` absorbs runtime/operation/workflow/observe
policy. `product-present` absorbs protocol plus library render and GUI headless projection; surfaces
remain executable leaves.

### DAG and cycle rule

```text
product-types
  ↓
product-object ───────┐
  ↓                   │
product-commit        │
  ↓                   │
product-domain        │
  ↓                   │
product-compute       │
  ├── product-compile ───── language/native leaves
  ├── product-index   ───── Tantivy / Trustfall / Qdrant leaves
  └── product-query
          ↓
product-sync
          ↓
product-present ───── surfaces (CLI / MCP / GUI)
```

The arrows are dependency arrows. `product-types` cannot know compiler IR. `product-domain` defines
the semantic object schema using opaque IDs and compact enums. Compiler adapters translate compiler
IR into domain publication records; index adapters translate domain records into derived view objects.
Storage knows object bytes and schemas, never compiler structs. Query knows query/view records, never
Tantivy/Trustfall/Qdrant types. Presentation knows view objects and faults, never compiler IR.

The existing graph has concrete cycle pressure: `compiler-application` currently depends on
`interface-core` while interface library depends on compiler application
(`compiler/application/Cargo.toml:10-22`, `interface/library/Cargo.toml:10-29`); server retrieval
depends on interface protocol (`server/index/retrieval/Cargo.toml:10-29`). Move shared request/error
types downward to product-types/domain, make compiler and server leaves implement compute/index traits,
and delete those upward dependencies. A dependency lint should reject compiler → interface and
backend → presentation edges.

## Mapping all 53 existing workspace crates

The count is verified from the workspace globs plus explicit server members: `compiler/*`,
`compiler/languages/*`, `heart/*`, `interface/*`, `server/index/*`, and server journal/operation/
runtime/workflow (`Cargo.toml:6-15`) produce 53 manifests outside the nested Turso workspace.

| Existing crates | Destination | What survives / what is compressed |
|---|---|---|
| `heart-identity`, `heart-schema` | `product-types` | Domain IDs, schema/limits, version/frontier/delta markers; delete parallel interface/core scalar wrappers where equivalent. |
| `heart-object`, `heart-object-pack` | `product-object` | Object descriptor, pack indexing, borrowed object views; one object store API. |
| `heart-root` | `product-object` + `product-commit` | Root packing/diff becomes immutable root and typed `RootDelta`; locality scans move to commit/object store. |
| `heart-hydration`, `heart-memory` | `product-object` | Hydration plans and bounded residence become object-store implementation; no separate product abstractions. |
| `heart-frame`, `heart-view` | `product-object` | Canonical frame codec and validated borrowed views become object codec modules. |
| `heart-adaptive` | `product-compute` | Placement facts, budgets, local/remote decision remain one policy module; use `Frontier` instead of parallel pins. |
| `heart-observe` | `product-commit` | Probe/event sink becomes cursor subscription/progress observer; keep allocation-free hooks. |
| `heart-telemetry` | `product-present` or deployment leaf | Product telemetry adapter; core emits typed observations without OpenTelemetry dependency. |
| `heart-root` | `product-object` | The duplicate row is intentional in this table: its build and commit portions split. |
| `compiler-vocabulary`, `compiler-ir-vocabulary` | `product-domain` | Translate compiler language/stage/diagnostic/link vocabulary to opaque domain schema; keep compiler-specific extension traits in compile leaf. |
| `compiler-ir` | `product-compile` leaf boundary | Compiler-native IR stays private to adapters; domain publication object crosses boundary. |
| `compiler-driver`, `compiler-registry` | `product-compile` | One compiler capability registry and bounded driver; remove interface-core dependency. |
| `compiler-application` | `product-compile` | Orchestration/publication adapter; no interface dependency, emits domain deltas. |
| `compiler-publication` | `product-compile` + `product-commit` | Publication encoding in compile, commit transaction in commit package. |
| `compiler-languages` | `product-compile` | Grouping façade only; delete as a standalone semantic package. |
| `compiler-languages-clang`, `-csharp`, `-go`, `-java`, `-python`, `-rust`, `-typescript` | language/native leaves | Retain as separately compiled optional plugins because toolchains and heavy dependencies differ. They implement one `Frontend` trait and emit domain facts. |
| `server-workflow`, `server-runtime`, `server-operation` | `product-compute` | One operation state machine, scheduler, budgets, recovery; operation deltas are typed. |
| `server-journal` | `product-commit` | Authoritative append/replay and crash recovery; no second epoch authority. |
| `server-index-vocabulary` | `product-domain` | Index IDs and metrics become derived-view schema IDs. |
| `server-index-core` | `product-index` | Immutable segment/snapshot metadata and compact candidate records. |
| `server-index-acquire`, `-ingest`, `-catalog` | `product-index` | Ingest/catalog/build pipeline modules; eliminate cross-crate duplicate catalog types. |
| `server-index-build`, `-publish` | `product-index` + `product-commit` | Derivation build and sealing in index; authoritative object commit in commit. |
| `server-index-retrieval` | `product-query` | Unified query planner over derived views; no protocol dependency. |
| `server-index-routing` | `product-query` | Candidate routing and lane planning module. |
| `server-index-tantivy` | optional index leaf | Tantivy adapter behind lexical `ViewProvider`. |
| `server-index-trustfall` | optional index leaf | Trustfall adapter behind graph `ViewProvider`. |
| `server-index-graph-vector` | `product-index` + vector leaf | Shared graph/vector contracts in product-index; remote Qdrant client remains optional leaf. |
| `server-index-qdrant` | optional index leaf | Qdrant adapter only; no domain or interface types in its public API. |
| `interface-core` | delete after bridge | Its typed capability state machine is split: request/reply into product-types/domain; execution policy into product-compute; compiler/retrieval adapters into compile/query. |
| `interface-identity` | `product-domain` | Address/package/key grammar; remove dependency on interface-core. |
| `interface-documents` | `product-domain` + `product-present` | Semantic document schema in domain; projector/render walk in present or query projection module. |
| `interface-search` | `product-query` | Search request, lane coverage, scoring, bounded candidate table, terminal/view schema. |
| `interface-library` | `product-commit` + `product-domain` + `product-query` | Delete monolithic façade; expose `Workspace` composition root that owns object store, commit, compute, and providers. |
| `interface-protocol` | `product-present` | Frame/MCP/JSON transport codecs; no command semantics beyond lowering. |
| `interface-cli` | surface leaf | Thin argv parser + `Workspace` composition; no compiler/document dependencies. |
| `interface-mcp` | surface leaf | Thin JSON-RPC server + generated command schemas; no compiler/index dependencies. |
| `interface-gui` | `product-present` + surface leaf | Headless reducer/store/projection in present, GPUI app/views in leaf. |

The table has all 53 source members represented; `heart-root` is split between object and commit in
one row, not duplicated. The key deletions are real:
`interface-core` disappears; `interface-library` is no longer a catch-all; protocol, rendering,
documents, search, and GUI state stop each owning partial product vocabularies; and the heart object
stack is compressed into object/commit. Language crates remain leaves because optional heavy native
dependencies are a legitimate compilation boundary.

## Stable IDs, view roots, and query snapshots

### UI stable row identity

Every UI row is a projection keyed by an immutable domain identity, with status as a separate field:

```rust
pub enum StableRowId {
    Package(PackageCoordinate),
    Symbol(ContentKey),
    ViewObject(ObjectId),
}
pub struct Row { pub id: StableRowId, pub source: VersionId, pub body: RowBody, pub state: RowState }
```

Never key rows by array index, formatted display string, or result rank. A reorder then updates rows,
preserving selection and animation. Current `DocumentStore` caches pages by formatted `PageKey`
(`interface/gui/store/document.rs:20-51`, `:186-196`); retain the spelling as a display/cache alias,
but key the authoritative cache by `ContentKey + source VersionId` to prevent stale pages after
republication.

### View roots

A `ViewRoot` is an immutable object containing row IDs, compact row records, source frontier, and
optional continuation cursor. Shelf, outline, page, search, health, and MCP resource outputs are all
views. A GUI reducer holds the current `ViewRoot` plus mutable intent/settings; it does not copy a
second shelf/search database. A view can be evicted and deterministically rebuilt from its source
objects and request.

### Query snapshots

`QuerySnapshot` pins `(source_frontier, request, capability_policy, budget)`. Its derived view ID is a
hash of those values, making retries/caching safe. A semantic remote answer must include its observed
frontier and view source. Existing `SearchTerminal` echoes the request and lane reports
(`interface/search/terminal.rs:41-55`), a good compatibility shape; change its hit storage to compact
IDs and lazy materialization without losing coverage or cursor semantics.

## Storage and sync protocol

The authoritative commit protocol is:

1. acquire a lease for one mutation operation;
2. read manifest/frontier and validate the delta base;
3. write content-addressed objects and derived objects to temporary immutable files;
4. fsync object files and parent directory;
5. append and fsync the envelope/delta record;
6. atomically replace one manifest pointer containing frontier, log sequence, and checksums;
7. fsync the manifest directory; then publish a best-effort notification.

Readers read one manifest and verify its object closure. There is no independently authoritative
`epoch` file. The current implementation’s staged epoch rename (`interface/library/epoch.rs:106-120`)
is useful mechanics but unsafe as a second commit authority. Existing CRC/version shelf codec
(`interface/library/store/codec.rs:23-75`) can become the manifest-referenced object codec.

Sync exchanges missing object IDs, then deltas/envelopes. A remote may place a copy in a different
tier, but the object ID/frontier is unchanged. Reconciliation computes common ancestors and returns
one of: fast-forward, attach additional offline head, semantic collision, or incompatible schema.
The user/system resolves collisions with an explicit delta. Query and UI cursors advance independently
but always name a source frontier; a cursor from a discarded head cannot silently read the new head.

## Deletion and compression plan

Delete or absorb these boundaries after compatibility shims land:

* `interface-core` as a second request/reply/service universe;
* `interface-library` as a god façade and its duplicate epoch/shelf authority;
* duplicate identity/package/ecosystem types split between core and interface identity;
* duplicate retrieval signature/token models in core and documents (`interface/core/retrieval.rs:18-83`);
* separate MCP schema/guidance and CLI help tables where generated command specs suffice;
* separate page/search/health caches in GUI that mirror durable state;
* separate server index vocabulary/core/retrieval/routing crates when they only move the same IDs;
* heart frame/view/object-pack fragmentation once object codec/view modules are stable.

Retain separate crates only where they buy a real compile or deployment boundary: heavy language
frontends, optional Tantivy/Trustfall/Qdrant clients, and the three executable surfaces. The product
kernel should be inspectable as roughly eight to twelve packages, with modules inside those packages
chosen for cohesion rather than one type family per crate.

## Migration gates

1. Add product-types and encode/decode `Version`, `Frontier`, `Delta`, `Envelope`, and `DeltaCursor`.
2. Build an in-memory object/commit store with crash-injection tests and one authoritative manifest.
3. Adapt compiler publication to emit domain objects/deltas; remove compiler→interface dependency.
4. Adapt shelf and index derivation to object IDs and source frontiers; make current stubs fail loudly
   as unavailable rather than fabricate ready/empty results.
5. Add cursor subscriptions and replay; replace epoch watchers and GUI shelf mirroring.
6. Make query snapshots and bounded candidate views; retain old wire terminals through a compatibility
   projection.
7. Generate CLI/MCP schemas and lower to the closed typed command/reply coupling once at transport.
8. Move GUI to reducer + view roots + stable row IDs; preserve headless motion/prefs tests.
9. Remove old crates/re-exports only after dependency DAG checks, wire snapshots, frontier replay,
   offline collision tests, and allocation benchmarks pass.

The acceptance criterion is one traceable edit: one typed delta, one immutable object closure, one
authoritative frontier, one cursor observation, one stable UI row, and one query snapshot. If a design
path creates a second epoch/head/store/cache that cannot be explained as a derived view, it is outside
the product architecture and should be rejected in review.
