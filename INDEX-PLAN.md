# INDEX-PLAN.md — The Versioned Catalog and Registry Unification

Status: **normative full vision** · 2026-07-19 (rev 2 — no hedges, dual-plane iroh sync, serve-when-available)
Reading order: assumes ECOSYSTEM-PLAN.md (sealed `EcosystemSpec`, `StructuredQuery`,
`CatalogFollower`, listing/withdrawn semantics), LIBRARIFICATION-PLAN.md (GD-1..40), and
SMOLVM-PLAN.md (SmolvmCage, RecordingSession, golden forks). Where this plan and
LIBRARIFICATION disagree, **this plan wins**; supersessions are marked **[supersedes …]**.

Greenfield rule: no data migration from Postgres. Build the spine, cut over, delete. No row port.

**Non-negotiable posture:** this document describes **one complete architecture**, not a menu of
fallbacks. There is no plain-SQLite escape hatch, no “Phase-2 maybe iroh,” no optional dual stack
left intentionally half-dead. If a piece is hard, it is still in scope and is designed here.

---

## 0. Purpose

One embedded, **versioned** SQLite catalog — the INDEX — is the single owner of the
package/metadata dimension: identity, versions, generations, IR-store locations, repo facts,
toolchain, license, popularity, listing state, dependency edges, overlay bookkeeping, and the
outbox. The IR plane (**nudox-ir-vcs / libpijul channels** — pristine + changestore + tips,
smolvm-produced seals) remains the single owner of the IR dimension. There is **no parallel
local artifact CAS for IR**: change files are already content-addressed by pijul hash; the
channel is the store (`ir-sync` / SMOLVM-PLAN). Source *file* bodies (if kept) stay on a thin
source-object path; they are not a second IR SoT. Everything else — tantivy, vectors, graph
stores, compile stage/golden caches — is a disposable projection rebuilt from those two
sovereigns (GD-14).

Postgres exits completely (~8–11k LOC deleted in the purge ledger). Ingestion parsing leaves the
serving binary. Search stays in-house. Overlays, offline, multi-host HA, backups, and AS OF
reads are one mechanism: **catalog commit/branch/merge + iroh sync of catalog chunks and IR
change-files**. Serve as soon as truth is available **somewhere** reachable — local VCS repo,
peer, or enterprise remote — while background sync merges work into the main store as fast as the
network allows.

The monorepo splits: IR plane → its own workspace; backend → five crates + ingestor; GUI → client
abstractions. Functionality ledger (§13) records keep/improve/cut with settled verdicts.

---

## 1. Ownership map

```
            SOVEREIGN STORES                         DERIVED (disposable)
┌──────────────────────────────────┐      ┌──────────────────────────────────────┐
│ INDEX  catalog.dolt (rusqdolt-   │─────▶│ tantivy package + symbol indices     │
│ lite): packages, versions,       │─────▶│ qdrant vectors (vector-* plane)      │
│ generations, stores, locations,  │─────▶│ terminus / graph projections         │
│ edges, facts, listing, overlays, │─────▶│ GUI local enrichment                 │
│ outbox, watermarks, compile cache│      └──────────────────────────────────────┘
│ tips (JobKey → gen pointers)     │
├──────────────────────────────────┤
│ IR PLANE  libpijul VCS           │      EPHEMERAL (never versioned)
│ (nudox-ir-vcs, smolvm seals):    │      ┌──────────────────────────────────────┐
│ pristine + changestore + tips,   │      │ scratch.sqlite: jobs, leases, wanted,│
│ PackageArchive as materialize    │      │ sessions, bakery claims              │
│ of channel state — NOT a second  │      └──────────────────────────────────────┘
│ local IR CAS                     │
└──────────────────────────────────┘
```

**Division of history:** IR channel log answers symbol-level “what changed between v1 and v1.1.”
Catalog commit graph answers metadata-level “what did the index believe last Tuesday / when was
this withdrawn / dependents at release time.” Catalog stores **pointers** (tip/change hashes,
store locations, JobKeys) — never symbol bodies. No dual-write of IR into SQL; no parallel
`cas/` dump of sealed IR beside the changestore. **[refines GD-32; aligns with ir-sync:
“no parallel CAS for artifacts”]**

**Critical coupling (not optional):** knowing a generation exists is useless without being able
to **apply its change closure**. Catalog sync and IR VCS sync are co-designed (§9). Location
rows are the map; iroh moves **pijul change files** (and optional source objects / compile-cache
blobs) with Bao verified streaming.

---

## 2. Ground decisions

**ID-1 — Engine: `rusqdoltlite` (DoltLite) is the catalog engine on server and client.**
No alternate engine. Vendor + pin `rusqdoltlite` 0.40.x (rusqlite API over DoltHub’s DoltLite:
prolly-tree single file, SQLite acceptance suite, `dolt_*` SQL). VCS ops are SQL functions
(`dolt_commit`, `dolt_branch`, `dolt_merge`, `dolt_push`/`pull`/`clone`, `dolt_at_<table>`,
`dolt_diff_*`, `dolt_conflicts_*`, `dolt_gc`, `dolt_rebase`, `dolt_hashof`). sea-query SQL passes
through. Litestream is out. sqlx is out (sync writer thread). **[supersedes GD-2 sqlx+Litestream;
keeps single-writer shape]**

**ID-2 — Two files per plane: `catalog.dolt` + `scratch.sqlite`.**
Versioned file = merge-meaningful durable state only. Scratch (plain rusqlite, disposable) =
job leases, wanted-queue, **session graphs** (writer-sticky exploration state), bakery claims.
Sessions are not product truth and do not enter remotes.

**ID-3 — Outbox stays in `catalog.dolt`; only sink fan-out mechanism.**
Sink intents are business semantics, not `dolt_diff` interpretation. Written in the same
transaction as the catalog mutation. Single writer is the lock. **[keeps LIBRARIFICATION §16.2]**

**ID-4 — Single writer on the advertised `main` (or enterprise primary); batch heartbeats.**
One writer thread, connection pinned to the write branch. `dolt_commit` after ingest batches,
listing events, bakery publishes, sealed generations, and periodic timer. Readers pool;
per-connection branch isolation for overlays / AS OF. Never branch-switch with dirty working set.

**ID-5 — Schema v4: package / version / generation split; real edges; first-class columns.**
See §5. Locations and multi-store registry are first-class. JSON only for open-ended extras.

**ID-6 — `catalog_table!` macro: one definition → DDL + row codecs + tantivy fields + wire.**
Drift between shapes is a compile error.

**ID-7 — One query algebra.** `Query` in `heart` is wire and domain: text + scope + rank +
`at: Option<AsOf>` + `page: PageSpec`. All package surfaces lower into it. `StructuredQuery`
remains the internal parse product (ECOSYSTEM §8.1). Deleted: `RegistryQuery`, both package
search request types, `PackageSearchDeps`, server `Pagination`, package half of
`SearchRequestDto`, GUI wire sextet.

**ID-8 — Pagination only at the top of `Query`; single cursor site inside the engine.**

**ID-9 — Wire = domain** for `Query`, `Page`, `Scored`, package hits.

**ID-10 — Ingestion is external; serving never links a feed parser.**
`workspace/ingestor` owns official-registry followers and multi-host *ingestion points* for
ecosystems without a single registry. Emits `CatalogOp` only. Forge is sealed compute, not
ingest; it writes through `MetaStore` in-process.

**ID-11 — Ranking within-ecosystem only; dependents dominate.**
Per-eco percentiles. C# being smaller than Rust never crosses the interleave boundary.
Fusion weights (resolved, not open): within an ecosystem, after retrieval gates,
`score = 0.45 * dependents_pct + 0.25 * downloads_pct + 0.20 * text/semantic_fusion +
0.10 * quality/freshness`. Direct-dependency-of-scoped-set is a hard boost (not a free weight)
when `scope.deps` / `dep_set` is set. Weights are constants in code; the eval harness (NDCG)
may only **confirm** regressions — changing weights is a deliberate commit, not a runtime knob.
**[closes former O-3]**

**ID-12 — Overlays are branches/remotes of the catalog; federation mounts catalogs.**
Merge policy by table class:
- **upstream-authoritative** (packages, versions, feed facts) → `--theirs` toward authority
- **device-owned** (local pins, locally sealed gens on device branches) → `--ours` until promoted
- **union** (`generation_locations`, `listing_events`, `stores`, compile-cache tips) → additive,
  no conflict by construction
Unresolved rows block commit and surface as typed `OverlayConflict` — never silent drop.

**ID-13 — Offline is a branch, not a mode.** Writes go to `local/<device-id>`. Reconnect =
background iroh sync of IR + catalog merge/push. No special “offline API.”

**ID-14 — HA, backups, read scaling, and peer assist are remote topology over iroh.**
Writer pushes `main` after commit batches (RPO = seconds). Replicas and enterprise mirrors are
clones that pull. Backups = dedicated backup remote + commit-boundary file snapshots. DR =
clone from backup + projection rebuild + watermark replay; CI-scheduled, release-blocking.

**ID-15 — All cross-host sync is iroh; Bao verified streaming is mandatory.**
**[supersedes GD-30 half-measures and former “HTTP remotesrv + later iroh”]**
**[aligns ir-sync: VCS-native IR distribution — no parallel IR artifact CAS]**

| What moves | How |
|---|---|
| **IR change files** (libpijul changestore objects) | `iroh-blobs` over ALPN used by `ir-sync` / `nudox-sync`: hash = pijul change id (revalidated on deserialize); Bao stream; receiver `ChangeIo::write` → `ApplyHook` advances channel tip |
| Catalog prolly chunks / remotes | Same iroh fabric: catalog remote protocol **tunneled** over authenticated iroh streams (ALPN `nudox/catalog-sync/1`), not naked `doltlite-remotesrv` on the public internet |
| Compile cache (L1 stage, goldens) + optional **source** objects | Same iroh-blobs fabric; content-addressed helpers — **not** a second IR history store |
| Control / authz / grants | HTTP(S) control plane remains for principal auth, DownloadGrant minting, wanted-queue, search APIs |

Naked `doltlite-remotesrv` is **banned** on untrusted networks. In-process or loopback remotesrv
is allowed only as an implementation detail behind the iroh catalog ALPN, with mTLS or ticket
auth terminating at our edge. `file://` remotes remain valid on shared volumes (dev, same-host).

IR readiness for generation G = required change closure present in the local **changestore** and
applied to the channel tip (pijul hash verify on load) — **not** “bytes in a local cas/ tree.”

**ID-16 — Point-in-time reads are typed.** `AsOf::Commit(hash) | AsOf::Time(ts)` via `dolt_log`
+ `dolt_at_<table>()`. Table names only from macro `Iden`s. Bitemporal columns only on
`listing_events` (and advisories): `valid_from` / `valid_to`. Everything else is commit history.

**Retention (resolved — former O-1 / R-11):** DoltLite exposes **`dolt_gc()`** (stop-the-world
mark-and-sweep of unreachable prolly chunks; exclusive access; idempotent) and **`dolt_rebase`**
with interactive actions including `squash` / `fixup` / `drop` (atomic; abort restores pre-rebase
state). Policy:

1. **External contract default is `AsOf::Time`.** Pinned GUI views and public APIs prefer time;
   resolve to commit at query time. Survives squash of intermediate commits if timestamps remain.
2. **`AsOf::Commit` is supported** for ops, drills, and short-lived pins. Documented retention:
   commit hashes on `main` are stable for **≥ 90 days** after creation unless a sealed ops
   procedure runs history rewrite (forbidden on `main` / `overlay/*` by transport policy).
3. **GC:** scheduled `dolt_gc` after deleting abandoned `local/*` and `pre-migrate-vN` branches;
   never GC while exclusive writer is applying; gate on IP-0 proof of exclusive access semantics.
4. **Squash:** allowed only on abandoned device branches and pre-merge cleanup of local lines —
   never on protected `main` without a versioned migration procedure that rewrites advertised pins.
5. **Shallow re-clone is ops recovery**, not the product retention plan; it invalidates old commit
   pins and is runbook-only.

**ID-17 — Projection discipline unchanged.** schema_version wipe/resync; sealed gens only trigger
indexing (GD-28); rebuild from `changed_since` + outbox; projections never backed up.

**ID-18 — Mine `feat/index-sqlite-package-search`, do not merge package-search.** Seed `index`
crate from its Catalog/codecs/changed_since; port to rusqdoltlite + schema v4; drop Litestream.

**ID-19 — `Catalog` / `MetaStore` trait split (GD-5).** One rusqdoltlite implementation; GUI uses
the same crate (per-device clone).

**ID-20 — Engine facade is modularity, not a second design.** Every `dolt_*` call lives in
`index::engine` so tests and ops have one choke point. **There is no plain-rusqlite fallback.**
IP-0 must prove open/commit/branch/merge/at/push/pull/conflicts/gc on macOS arm64/x86_64 + Linux
and bench within **2×** of current Postgres ingest rates for batched upserts / point reads /
`changed_since` at 1M rows. Fail the spike ⇒ stop and fix the engine path; do not ship a
degraded “overlays = separate files” product under this plan’s name.

**ID-21 — Three workspaces** (backend / IR / GUI). See §4.

**ID-22 — Functionality ledger (§13):** keep / improve / cut with **settled verdicts** (no open
sign-off blanks). Ruthlessness targets implementations, not silent capability loss.

**ID-23 — Dead code is a build error after purge.** vector-embed / vector-remote are **wired**
under registry features (`edge` / `onnx` / `remote`), not left compiled-and-orphaned. Legacy
`registry/runtime/vector` is **deleted** — one vector plane only.

**ID-24 — Dual-plane end-to-end is normative (catalog map + IR VCS territory).**
Seal a generation ⇒ record into **local IrRepository** (changestore + channel tip) ⇒ register
`generations` + `generation_locations` ⇒ **background** iroh publish of **change files** (merge-
triggered per `ir-sync`) + catalog commits ⇒ any host that can reach a `present` IR store
somewhere may fetch/apply the change closure and serve ⇒ merge into enterprise `main` ASAP per
policy. See §3. Location `pending` must never be treated as healthy IR for paths that need a
materialized package; metadata search may still rank the package if the catalog row is merged.

**ID-25 — Serve-when-available.**
Serving resolves IR by: (1) local changestore has required changes + tip applied, else (2) any
reachable peer/remote IR store with `present` (iroh Get change files → verify pijul hash →
apply), else (3) `pending` / missing → 503/partial with structured `Availability`. Search and
package metadata do not wait for local apply. IR-backed expand may proceed as soon as the
needed change closure is applied (or stream apply as files arrive).

**ID-26 — Local commits compile in smolvm; promote shares incremental state.**
**[amends SMOLVM-PLAN SV-6 for trusted promote paths]**

- **Local-only work** (device branch, offline, enterprise edge host): all compiles run in
  **SmolvmCage** (SMOLVM-PLAN). Streaming IR → host `RecordingSession` → **one channel record**
  at seal into the **local IrRepository** (not a parallel `cas/` put of IR); catalog rows on
  `local/<device-id>` or edge branch; `generation_locations` mark that VCS store `present`.
- **Background sync:** iroh pushes **change files** + catalog commits without blocking the
  UI/API that already has a local tip.
- **When local work lands on the remote primary** (merge to `main` / enterprise authority):
  1. Catalog merge (union locations; promote device-owned gens to authority policy).
  2. IR: remote imports change files, verifies pijul hashes, applies to its channel, confirms
     tip (`ir-sync` MergeEvent path) — remote location `present`.
  3. **Share smolvm-adjacent incremental state** that is safe and content-keyed (helpers only):
     - L0 JobKey → gen_stamp / tip hash — catalog `compile_cache` tips once tip verified.
     - L1 stage postcard objects — content-addressed, read-mostly share over iroh (compile
       accelerator, **not** IR history).
     - L3 toolchain image digests — already shared by construction.
     - **Golden fork / warm checkpoint artifacts** keyed by `(image_digest, producer_version)` —
       published as content-addressed objects + `compile_cache` tips so the next host skips cold
       boots when the golden matches. Ephemeral guest scratch and mutable L2 language caches
       (`target/`, GOCACHE) are **never** shared across trust boundaries.
  4. Merge into the main store **as soon as** catalog merge commits and IR tip verify succeed —
     no batching window beyond the existing writer heartbeat.
- **Trust:** remote admits channel tips only after **pijul change-hash verify** on every file
  (deserialize + recompute). Untrusted guest output cannot mint identity for another package
  (host assigns IntroIds / seal — GD-39 / SV-10). Trusted promote (enrolled device, mTLS/ticket)
  may publish L0/L1 helpers; anonymous clients stay read-only on those helpers (SV-7).

**ID-27 — Sessions are writer-sticky by design.**
Exploration graphs live in writer-local scratch. Session routes pin/redirect to the writer.
Not a temporary cut — intentional exclusion from the commit graph and from iroh catalog sync.
If multi-active session sharing is ever required, it becomes an explicit shared-scratch service;
it does not reintroduce Postgres.

---

## 3. Dual-plane sync and the end-to-end generation story

### 3.0 IR storage is the VCS — not a local CAS

Normative (from `workspace/ir-sync` and `nudox-ir-vcs`):

> The compile/serve plane is fully VCS-native: a package's IR history lives in libpijul
> channels. Pijul change files are already content-addressed. There is **no** parallel CAS
> for IR artifacts.

| Concern | Store |
|---|---|
| IR history / tip / materialize | `IrRepository`: pristine + **changestore** (`<repo>/changes/`) + channel |
| PackageArchive / yoke views | **Derived** by materializing the channel — cacheable, never a second SoT |
| Source file bodies (optional path) | Thin source-object path (`BlobManifest.files` role) — not IR |
| L1 stage / goldens | Compile accelerators in `compile_cache` + iroh objects — not IR history |
| Catalog metadata | `catalog.dolt` only |

Sync trigger for IR: **merge onto a durable channel** → `MergeEvent` → `Syncer::on_merge`
(iroh provide of change files). Not a background sweep of a `cas/` tree.

### 3.1 Two planes, one fabric

```
                 HOST A (edge / desktop)              HOST B (enterprise / replica)
              ┌─────────────────────────┐          ┌─────────────────────────┐
 smolvm ─────▶│ IrRepository (present)  │──iroh───▶│ IrRepository            │
 seal         │ changestore + tip       │  change  │ apply + confirm tip     │
              │ (+ L1/golden helpers)   │  files   │ (fetch-on-demand OK)    │
              └───────────┬─────────────┘  Bao     └────────────▲────────────┘
                          │ write catalog rows                  │ resolve by tip/hash
                          ▼                                     │
              ┌─────────────────────────┐  iroh    ┌────────────┴────────────┐
              │ catalog.dolt            │─────────▶│ catalog.dolt            │
              │ gens + locations + tips │  catalog │ merge → main            │
              └─────────────────────────┘  ALPN    └────────────┬────────────┘
                                                                ▼
                                                         serve / search / expand
```

### 3.2 State machine (produce → locate → sync → serve)

| Step | INDEX (catalog) | IR (VCS / smolvm) |
|---|---|---|
| 1. Compile | — | SmolvmCage; ir-stream → RecordingSession; optional channel checkpoints |
| 2. Seal | — | `finish()` records change(s) into **local changestore**; tip advances; `gen_stamp` / tip hash |
| 3. Register | `generations` row; `generation_locations (local_vcs)=present`; optional remote `pending`; outbox; `dolt_commit` on device/edge branch | — |
| 4. Serve locally | Metadata immediate | Materialize from **local channel tip** |
| 5. Background / on-merge publish | Push/merge catalog over iroh catalog ALPN | `ir-sync`: provide change files over iroh; remote verify + apply → location `pending→present` |
| 6. Remote/peer serve | After pull/merge: package known | Fetch missing changes if not local; serve when tip applicable from **any** `present` IR store |
| 7. Promote to main | Merge device branch → `main`; union locations; admit compile_cache tips | Same change files (content-addressed); goldens/L1 helpers shared per ID-26 |
| 8. Failure | Catalog without IR tip: search OK, IR paths → Availability miss | Channel without catalog row: seal path always registers first |

**Invariant:** never mark `generation_locations.status = present` until the host can load and
hash-verify the required change files and apply them to the announced tip.

### 3.3 Availability model (API)

```rust
pub enum IrAvailability {
    Local,               // changestore has closure; tip applied on this host
    Remote { store_id }, // another IR store has present; fetch+apply possible now
    Pending,             // catalog knows gen; change closure not reachable/verified yet
    Missing,             // no location rows / evicted
}
```

Search hits may include availability for the hit’s pinned generation. Expand/IR routes use it for
status codes and client retry/backoff while sync continues.

### 3.4 Store registry

`stores.kind ∈ { ir_vcs_local, ir_vcs_iroh, compile_cache_iroh }`. An IR store URI points at a
reachable **IrRepository** (path or iroh endpoint that serves change files), not a generic
`cas/{blake3}` IR dump. S3 may back enterprise iroh providers as durable substrate for **change
files and compile helpers**; clients still speak iroh. **[supersedes residual HTTPS blob plane
and parallel local IR CAS as product default]**

### 3.5 Offline

1. Offline: seal into local IrRepository → local location `present` → catalog on `local/<device-id>`.
2. Online: background steps 5–7 without user mode switch (merge-triggered IR sync + catalog push).
3. Conflicts: ID-12 policy for catalog; IR apply follows libpijul single-writer channel rules.

---

## 4. Workspace and crate topology

Backend collapses to **six** members (five abstractions + ingestor). IR leaves the backend
lockfile (~112k LOC). GUI hosts `client`.

```
Backend/workspace/
  heart      Query kernel, wire, cache, content-hash types, vector-core absorbed
  ecosystem  sealed zero-IO spec leaf (stays standalone)
  index      rusqdoltlite engine, schema v4, Catalog/MetaStore, overlays, iroh catalog
             sync adapter, scratch, CatalogOp, backup/DR
  registry   search v3, runtime/text, compiled store, vector-* features;
             − legacy runtime/vector, − schema/index/queue/pg coordination, − ingest/upstream
  server     HTTP shell, workers, forge glue, outbox followers, iroh provider wiring
  ingestor   external binary: CatalogFollower + upstream clients → CatalogOp

Backend/ir/
  ir, nudox-*, ir-stream, ir-sync (VCS change distribution), nudox-sync,
  nudox-ir-vcs (IrRepository), compiler + SmolvmCage, vendor/libpijul

workspace/gui/
  lindsey · client (LocalEnrichment, local catalog, offline branch, desktop vector edge)
```

Cross-workspace rules:
1. Backend deps only `ir` data-model crate — never nudox-ir-vcs/libpijul.
2. IR may dep `heart` only among backend crates.
3. GUI deps backend one-way; gpui never enters backend.

vector-local/embed/remote fold into registry features `edge` / `onnx` / `remote`. Server reaches
vectors only through registry features.

---

## 5. Schema v4 (`catalog.dolt`)

All tables via `catalog_table!` (ID-6). Columns abbreviated; macro is normative.

```
packages            stem_id BLOB16 PK · ecosystem TEXT · name_struct TEXT(purl) ·
                    name_canonical TEXT · name_original TEXT · repo_url TEXT? ·
                    created_at INT   [UNIQUE (ecosystem, name_canonical)]
versions            id BLOB16 PK · stem_id FK · version_canonical TEXT ·
                    version_original TEXT · published_at INT? · toolchain TEXT(json) ·
                    license_spdx TEXT? · yanked_upstream INT01 ·
                    parse_state TEXT · parse_phase TEXT? · attempts INT · failure TEXT(json)?
                    [UNIQUE (stem_id, version_canonical)]
generations         gen_stamp BLOB32 PK · version_id FK ·
                    channel_tip BLOB32 ·              ← libpijul tip / seal change id
                    producer_toolchain TEXT · sealed_at INT · job_key BLOB32?
stores              store_id BLOB16 PK ·
                    kind TEXT(ir_vcs_local|ir_vcs_iroh|compile_cache_iroh) ·
                    endpoint TEXT · added_at INT · healthy INT01
                    ← IR stores = IrRepository endpoints, not cas/ trees
generation_locations gen_stamp FK · store_id FK ·
                    status TEXT(present|pending|evicted) ·
                    PK (gen_stamp, store_id)          ← union-merge, append-only
compile_cache       job_key BLOB32 PK · gen_stamp FK? · tip_hash BLOB32? ·
                    kind TEXT(l0_tip|l1_stage|golden) ·
                    object_hash BLOB32 · image_digest TEXT? · updated_at INT
                    ← compile accelerators only (ID-26); never IR SoT
edges               dependent_version FK · dep_ecosystem TEXT · dep_name_canonical TEXT ·
                    requirement TEXT · resolved_stem FK? · kind TEXT ·
                    source TEXT(feed|manifest)
repo_facts          stem_id FK · stars INT? · last_activity_at INT? · archived INT01 ·
                    fetched_at INT
popularity          ecosystem TEXT · stem_id FK · downloads INT? ·
                    downloads_pct_ppm INT? · dependents_pct_ppm INT? ·
                    computed_at INT · PK (ecosystem, stem_id)
facets              version_id FK PK · keywords TEXT(json) · quality_ppm INT ·
                    extras TEXT(json)
listing_events      seq INTPK · version_id FK · status TEXT · reason TEXT? ·
                    valid_from INT · valid_to INT? · recorded_at INT
symbols             id BLOB16 PK · version_id FK · fq_name TEXT · kind TEXT · gen_stamp BLOB32
outbox              seq INTPK AUTOINC · version_id FK · gen_stamp BLOB32 ·
                    sink_kind TEXT · op TEXT · created_at INT
sink_watermarks     sink_kind TEXT PK · last_seq INT · updated_at INT
overlays            name TEXT PK · remote_url TEXT? · branch TEXT · precedence INT ·
                    last_merged_commit TEXT? · added_at INT
edgepack_artifacts  edgepack_key_digest BLOB PK · version_id FK ·
                    recipe_fingerprint TEXT · artifact_id BLOB? ·
                    ram_estimate INT? · published_at INT
schema_meta         user_version via rusqdoltlite_migration; sea-query DDL;
                    pre-migrate-vN snapshot branch before each migration
```

`scratch.sqlite`: `jobs`, `wanted`, `sessions`, `edgepack_claims`.

Deliberately absent: `parse_status` table (on `versions`), jobs in versioned file, sessions in
catalog, symbol-content history in SQL, duplicate JSON for first-class columns.

### 5.1 `catalog_table!`

One macro invocation per table emits Iden, `TableCreateStatement`, typed row + total codecs
(uuid→BLOB16, hash→BLOB32, time→INTEGER unix-ms, enums→TEXT), and search field specs. Migrations
consume the same statements. Wire `From<Row>` in heart.

---

## 6. Query algebra

```rust
pub struct Query {
    pub target: Target,       // Packages | Symbols
    pub text: String,         // → StructuredQuery inside engine
    pub scope: Scope,         // ecosystems, namespace, deps, license, packages, dep_set
    pub rank: RankSpec,
    pub at: Option<AsOf>,     // Commit | Time — prefer Time externally (ID-16)
    pub page: PageSpec,       // limit + after cursor — sole pagination site
}

impl SearchEngine {
    pub fn search(&self, q: &Query) -> Result<Page<Hit>>;
}
```

Retrieve → gate → rank (ID-11) → interleave → page once. Symbol search shares Scope/PageSpec
behind `Target::Symbols` (NDJSON streaming remains without resume cursor — settled).

---

## 7. Serving

Server state after cutover: mounted catalogs (base + overlays) each with projections;
SearchEngine per mount; embedder + cache; compiler client; compiled store; heuristics; scratch;
**iroh endpoint** (blobs + catalog ALPN). No `PgPool`s.

Loops:
1. **op apply** — system-authz `CatalogOp` → writer + outbox + commit  
2. **outbox followers** — text / vector / graph  
3. **iroh catalog pull** — replicas / mirrors  
4. **iroh IR change provide/fetch + apply-on-demand** — serve-when-available (`ir-sync`)  
5. **forge** — smolvm seal into IrRepository → MetaStore gens/locations/symbols (single writer)  
6. **bakery** — ledger in catalog, claims in scratch  
7. **background promote** — device/edge branches → main + compile_cache admit (ID-26)

Ingestor split at IP-5; until then followers may still run in-process against MetaStore so feeds
never pause across IP-4.

---

## 8. Ingestion plane

`CatalogOp`: `UpsertPackage`, `UpsertVersion { … edges, facets }`, `SetRepoFacts`,
`SetListing { status, valid_from, reason }`, `Refresh { stem }`. Ingestor parses; server
validates `EcosystemSpec`.

Official registries only as truth sources. Multiple big hosts / no central registry ⇒ multiple
**ingestion points** (same op stream shape). No feed parser in the serving binary after IP-5.

Demand-pull: `wanted` in scratch; long-poll `/v1/ingest/wanted`; lease rows; writer-only access.

Withdrawn: op → `listing_events` + outbox deletes → projection tombstones.

Edges: from feed/manifest where available; Go/etc. partial until ECOSYSTEM P3 — ranking falls
back to popularity percentile for those ecos without pretending dependents exist.

---

## 9. Overlays, HA, offline (mechanics)

Branch names: `main`, `local/<device-id>`, `overlay/<name>`, `pre-migrate-vN`. Remotes named by
role (`origin`, `backup`, enterprise mirrors) — addresses are iroh endpoint IDs + tickets, not
raw HTTP remotesrv URLs on the public net.

Sync engines:

```
IR:   merge on durable channel → ir-sync provide/fetch change files → verify → apply tip
INDEX: pull catalog → merge(policy) → resolve-or-surface → commit → push  (iroh catalog ALPN)
```

Readers stay on pre-merge catalog commit during reconcile (no mid-sync mixing). Overlay
projections are **delta-sized** via `dolt_diff_*` against `last_merged_commit`.

Writer topology: CatalogOp apply and session routes live on the **advertised writer**. Replicas
redirect writes; promote swings the advertisement. Protected branches: force-push / history
rewrite rejected on `main` and `overlay/*` at the catalog ALPN layer.

Failure drills (tests, not docs): writer death + promote; offline device merge with conflicts;
backup restore; torn change put (no `present` without pijul verify); catalog merge without IR tip
(Availability); promote shares golden/L1 helpers and next host hits compile_cache.

---

## 10. Versioning and bitemporality

- Dependents at time T → `Query { scope.deps, at: Time(t) }` → `dolt_at_edges` / `dolt_at_versions`
- Listing malicious “true since March, learned today” → `listing_events` valid_from vs recorded_at
- GUI version pin → generation’s seal + that gen’s IR via locations (ID-24/25)
- History endpoint → `dolt_history_*` / `dolt_diff_*` without new storage
- Retention → ID-16 (gc + prefer AsOf::Time + protected main)

---

## 11. Ranking notes

Within-eco relative percentiles only. Dependents from real `edges` reverse aggregate (incremental,
not full-corpus sweep). Fusion constants in ID-11. Unscoped queries:
`rank_per_ecosystem_and_interleave` after per-eco rank.

---

## 12. Vector plane

**One** implementation: vector-core (in heart) + registry features edge/onnx/remote.  
**Delete** `registry/runtime/vector` (~970 LOC). No dual stack.

---

## 13. Functionality ledger

**keep** / **improve** / **cut** (settled).

**Serving routes — keep/improve:**  
`/packages/search` (Query + AS OF + one cursor) · symbol search streams · `/expand` · `/packages*` ·
`/v1/compiled/lookup` · depshards · `/v1/rerank` · **`/sessions/:id` (scratch, writer-sticky —
ID-27)** · admin verify/rebuild · health (catalog/commit + overlay + iroh probes).  
New: `/v1/packages/{id}/history`, overlay admin, `/v1/ingest/wanted`, availability on IR routes.

**Workers — keep/improve:** ingestor followers · forge/smolvm · outbox ×3 · package-index via
outbox/`changed_since` · signals via SQL aggregates · cas_gc (+ commit reachability) · bakery ·
**iroh provide/fetch + promote loop**.

**Search — keep** all v3 features; **improve** dependents + AS OF + availability-aware IR.

**GUI — keep** search/enrichment; **improve** offline catalog branch, conflict surfacing, heart wire.

**Ops — keep** authz, watermarks, GC invariant, DR drill; **add** multi-host iroh HA, backup
remotes, promote-and-share compile cache.

**Settled cuts / design choices (former sign-offs):**
1. Sessions: writer-sticky scratch — **accepted** (ID-27).
2. Legacy runtime/vector: **deleted** in favor of vector-* (ID-23).
3. Symbol-search: no resume cursor — **accepted** (parity with today’s NDJSON).

---

## 14. Deletion ledger

| What | LOC |
|---|---|
| Postgres spine (schema, GlobalStore, queue, outbox, session, pools, pg workers, …) | ~8,000 |
| Search shims + query sprawl + GUI wire dupes | ~1,600 |
| Legacy vector stack | ~970 |
| **Direct deletions** | **~10,600** |
| Ingest/upstream/resolve/rich → ingestor | ~3,750 **moved** |
| IR plane out of backend workspace | ~112,000 **re-homed** |
| Superseded branch not merged | ~11,600 avoided |

New code (order of magnitude, full vision): index crate + iroh dual-plane + promote/compile_cache
+ query/serving swap ≈ **15–25k** written/rewritten across IP-0..IP-7 — not a pure delete.

---

## 15. Build order

| Phase | Content | Gate |
|---|---|---|
| **IP-0 Engine + iroh fabric** | Vendor rusqdoltlite; sea-query binder 0.40; prove commit/branch/merge/at/push/pull/conflicts/**gc**/rebase; two-process **iroh** catalog + **ir-sync change-file** sync; pijul hash + Bao verify; bench ≤2× PG; macOS arm64/x86_64 + Linux | All green or **stop** (no fallback product) |
| **IP-0b Re-home** | git mv IR workspace; vector-core → heart; vector-* → registry features; client crate scaffold | Three workspaces build |
| **IP-1 Index crate** | schema v4, facade, MetaStore, scratch, migrations, location + compile_cache tables | Property tests, codec goldens |
| **IP-2 Query kernel** | heart Query/AsOf/Page; GUI on heart types | Serde goldens |
| **IP-3 Projections** | outbox → tantivy/vector/graph; dependents/popularity SQL | Rebuild-from-empty; NDCG floor |
| **IP-4 Serving swap + purge** | mounted catalogs; delete Postgres; sessions sticky; vector legacy gone; iroh in server state | Integration + `/v1` conformance |
| **IP-5 Ingestor** | external process; wanted-queue; withdrawn E2E | Separate-process itest |
| **IP-6 Dual-plane topology** | overlay merge; ir-sync change distribution; serve-when-available; promote-to-main + compile_cache/golden share; replica pull; backup remote; offline device; DR drill | E2E test §3.2; two-writer property tests; Availability cases |
| **IP-7 Time travel** | AsOf surfaces; dependents-at-time; history endpoint; retention jobs (`dolt_gc`, branch GC) | AS OF tests; 90-day commit policy enforced in ops tests |

IP-0 is a hard gate for the architecture. IP-1..3 can parallelize after facade signatures freeze.
IP-4 is cutover. IP-6 is where “overlays are king” becomes real — not optional polish.

---

## 16. Risks (no open product questions)

| # | Risk | Response (designed, not deferred) |
|---|---|---|
| R-1 | rusqdoltlite youth | Vendor/pin; DoltLite is DoltHub-maintained with `dolt_gc`/rebase; facade choke point; IP-0 hard gate |
| R-2 | sea-query-rusqlite 0.38 vs 0.40 | Vendor-fork binder |
| R-3 | Write path slower than SQLite | IP-0 2× budget; batch heartbeats (ID-4) |
| R-4 | Catalog remote auth | Never naked remotesrv; iroh ALPN + tickets/mTLS (ID-15) |
| R-5 | Prolly format longevity | Backup remote + sea-query dump-to-plain-sqlite **export** for disaster archive (not a runtime engine alternative) |
| R-6 | Commit-graph growth | Batch commits; branch GC; `dolt_gc`; AsOf::Time default (ID-16) |
| R-7 | Merge conflicts | Table-class policy; typed surface; union tables can’t conflict |
| R-8 | Three lockfiles drift | CI builds all workspaces; backend↔IR surface = `ir` only |
| R-9 | GUI wire break | IP-2 single change + goldens |
| R-10 | Fold vs in-flight work | IP-0b only after search v3 commit; pure git mv |
| R-11 | Commit pin invalidation | Protected main; AsOf::Time default; rebase only off protected lines (ID-16) |
| R-12 | Serve before IR tip applicable | Availability enum; never lie `present` without changestore apply (ID-24/25) |
| R-13 | Poisoned local promote | Pijul change-hash verify + host seal ownership before tip/compile_cache admit (ID-26) |
| R-14 | Smolvm golden staleness | Key by image_digest + producer_version; miss ⇒ rebuild golden (SMOLVM V9) |

### Resolved research notes (former open questions)

| Former | Resolution |
|---|---|
| **O-1** DoltLite GC/squash? | **Yes.** `SELECT dolt_gc();` mark-and-sweep exclusive; `dolt_rebase` supports squash/fixup/drop. Policy in ID-16. |
| **O-2** Catalog transport on iroh? | **Yes, required.** Dual-plane iroh: catalog ALPN + **IR change-file** sync (`ir-sync`). No parallel IR CAS. ID-15/24. |
| **O-3** Fusion weights? | **Fixed constants** in ID-11; eval harness guards regressions only. |
| **O-4** Sign-off blanks? | **Settled** in §13 / ID-23 / ID-27. |

---

## 17. What this plan refuses

- Postgres or Litestream as catalog substrate  
- Plain SQLite as a “versioning-free fallback product”  
- Dual vector stacks  
- Feed parsers inside the serving binary (post IP-5)  
- Naked doltlite-remotesrv on untrusted networks  
- iroh-docs / CRDT multi-writer as the package catalog  
- Public IPFS swarm as distribution  
- Silent merge resolution  
- Marking IR `present` without pijul change-hash verify + apply  
- A parallel local **IR artifact CAS** beside the changestore  
- Sharing mutable language build dirs (`target/`, etc.) across trust boundaries  
- Shipping overlays/HA/offline as a later “maybe” after a non-versioned spine  

---

*End of INDEX-PLAN rev 2. Companions: ECOSYSTEM-PLAN, LIBRARIFICATION-PLAN, SMOLVM-PLAN
(ID-26 amends SV-6 promote path), `ir-sync` (VCS-native IR — no parallel CAS), edge-tech iroh
(elevated to sole transfer fabric), DoltLite docs (`dolt_gc`, `dolt_rebase`, remotes warning).*
