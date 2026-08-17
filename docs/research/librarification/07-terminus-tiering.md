# Terminus Tiering: Hot-Tier Admission Control + Incremental Lineage

**Research date:** 2026-07-16 (facts re-verified against live GitHub/crates.io/docs)  
**Scope:** TerminusDB 2026 state; hot-tier admission control; incremental lineage; Rust integration against the live nudox graph pipeline.  
**Related code (live tree under `workspace/`):**
- Graph model / IR projection: `workspace/compiler/graph/{model,from_ir,link,symtab}.rs`
- Schema emission: `workspace/compiler/generate/linked_data/{mod,schema,emit}.rs`
- Store-link bitmap: `workspace/registry/metadata/mod.rs` (`StoreLinks.graph`)
- Existing deps: `moka` 0.12 (heart/registry/server), `governor` 0.7 (registry), vendored `terminusdb_schema` + `terminusdb_schema_derive` from [ParapluOU/terminusdb-rs](https://github.com/ParapluOU/terminusdb-rs) (`TERMINUS_REV=4fefb043…` in `nix/build/third-party/BUCK`)
- Tiering *signal* stub: `workspace/server/tests/initialization_flow.rs` (`usage_is_tracked_for_tiering` — usage lands as metrics today; **no promotion policy yet**)

---

## Executive framing

Today every package generation that reaches the graph fan-out would materialize in TerminusDB. That does not scale to a full multi-ecosystem universe. The plan:

1. **Hot tier** — only packages with *sustained graph-query demand* live in TerminusDB (admission-controlled).
2. **Cold tier** — the long tail serves graph ops over IR loaded from content-addressed blobs, implementing the same trait surface.
3. **Incremental publish** — new generations write only changed symbols; lineage is recoverable both from Terminus commit history (hot) and from IR-level edges / `PackageVersion.declares` set-diff (both tiers).

`StoreLinks.graph` already models “is this generation materialized in Terminus?” — admission control decides when that bit becomes true and when it is cleared.

---

## Part A — TerminusDB Deep-Dive (2026 State)

### A.1 Project Health

**Version & release cadence (verified 2026-07-16)**

Latest **stable** release: **v12.0.6**, published **2026-06-24** ([GitHub Releases](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.6)). Stable v12.x timeline (non-prerelease only):

| Release | Published | Key highlights |
|---|---|---|
| [v12.0.0](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.0) | 2025-12-02 | Major: high-precision rationals, auto-optimizer default, `sys:JSON`, consistent JSON numbers, security (no-root image, no stack traces), WOQL `slice()`/`dot()`/`sys:Dictionary` |
| [v12.0.1](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.1) | 2025-12-15 | `_count` in GraphQL, high-perf set operators |
| [v12.0.2](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.2) | 2025-12-16 | Set operators for non-admin users |
| [v12.0.3](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.3) | 2026-02-25 | Build gap fix (same day as 12.0.4) |
| [v12.0.4](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.4) | 2026-02-25 | Range queries, ISO8601/Allen intervals in WOQL, ≥10% reasoning perf, leaner memory |
| [v12.0.5](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.5) | 2026-03-26 | Diff + streaming history endpoint; `merge_repeats` on update; race fix on DB create; auto-optimize only on write |
| [v12.0.6](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.6) | 2026-06-24 | `@shared` cascade, handlebars embeddings API, non-ASCII document fix, `SwapValue` patch types for dicts/lists/numbers |

Cadence: major v12.0.0 (Dec 2025) → three patch releases within two weeks, then Feb/Mar/Jun 2026 patches. Quiet but live — not abandoned. Last push on `main` observed **2026-07-13** (decimal properties / diff unfold work merged early July).

**Maintainership**

DFRNT ([dfrnt.com](https://dfrnt.com)) assumed engineering stewardship in 2025 ([TerminusDB 12 release blog, 2025-12-08](https://terminusdb.org/blog/2025-12-08-terminusdb-12-release/); [project overview](https://terminusdb.org/docs/terminusdb-explanation/)). Positioning: industrial / financial applications, precision arithmetic, temporal reasoning, stability under load. Commercial support is DFRNT-offered (private cloud on Azure/AWS, priority fixes).

**Repo metrics (live API, 2026-07-16)**

| Metric | Value |
|---|---|
| Stars | **3,355** |
| Open issues (issues only) | **6** (very low — quality *or* low external engagement) |
| License | **Apache-2.0** |
| Languages (byte share) | Prolog **68.5%**, JavaScript **21.5%**, Rust **8.3%** |
| Last stable tag | v12.0.6 (2026-06-24) |
| Storage format | Unchanged since v11 (in-place upgrade path) |

**Community / clients**

- Official: [Python client](https://github.com/terminusdb/terminusdb-client-python), [JS client](https://github.com/terminusdb/terminusdb-client-js) (both updated for v12; Python may need git install for latest — see release blog).
- Supporting orgs: [terminusdb-labs](https://github.com/terminusdb-labs), docs under DFRNT’s docs site.
- **No official Rust client under the terminusdb org.** Community: [ParapluOU/terminusdb-rs](https://github.com/ParapluOU/terminusdb-rs) (actively pushed; **nudox already vendors** `terminusdb_schema` + `terminusdb_schema_derive` from it), plus smaller forks.

**terminus-store crates.io status (risk note)**

[`terminus-store` on crates.io](https://crates.io/crates/terminus-store) is still **0.21.5**, last published **2024-03-11** — **>2 years behind** the v12 server line. Treat crates.io terminus-store as a **stale embedding option**, not a pin for production parity with the server. Prefer HTTP to the v12 server for hot-tier writes; only use the store crate for experiments / offline layer inspection with eyes open on format drift.

**Verdict: still viable in 2026 — with isolation**

TerminusDB under DFRNT is alive, focused on enterprise stability rather than experimental breadth. It remains the only production-grade document-graph store with **true immutable versioning + semantic JSON diff/patch** that matches nudox’s lineage needs. Risks: Prolog core (ops/perf unpredictability), small issue tracker (community signal weak), stale official Rust embedding crate. **Bet on it behind an HTTP boundary; keep cold-path IR as the hard fallback so demotion is always safe.**

---

### A.2 Architecture: Delta Layers, Commit Graph, Branching

**Storage layer**

TerminusDB’s triple store is the Rust **terminus-store** (HDT-derived succinct structures + delta encoding; research note: [Succinct Data Structures PDF](https://assets.terminusdb.com/research/succinct-data-structures-and-delta-encoding.pdf)). Since store format v0.20, layer files are **bundled archives** (not many tiny files). Layers are content-addressed.

**Layer stack model**

```
commit N    → layer_N  (additions Δ+ and deletions Δ-)
commit N-1  → layer_{N-1}
...
commit 0    → base layer (all additions, no deletions)
```

- **Read cost ≈ O(layer depth)** until rollup
- **Write cost ≈ O(Δ triples)** — ideal for incremental symbol publish

**Delta rollups / auto-optimizer**

From [immutability & concurrency docs](https://terminusdb.org/docs/immutability-and-concurrency/) and [persistence discussion](https://github.com/orgs/terminusdb/discussions/1865):

- `optimize` produces a **delta-rollup** pseudo-layer representing combined state.
- **Auto-optimizer is on by default in v12** (probabilistic after commits).
- Strategy is **exponential rollup base 3** → ~O(log₃ N) active layers. A few thousand commits ⇒ ~7 layers after rollup.
- v12.0.5: auto-optimize only runs **on write and with parent** (avoids wasted work).

**Branching**

From [knowledge graph version control](https://terminusdb.org/docs/knowledge-graph-version-control/): a branch is a **named pointer to a commit** (Git model). Create is O(1); branches share immutable layers; squash/rebase exist (`terminusdb squash`, `terminusdb rebase`). No published hard caps on branch or DB count — filesystem and ops practice are the real limits.

**Optimistic concurrency**

Writers append a new layer; commit retries if head moved (default **3 retries**, `TERMINUSDB_SERVER_MAX_TRANSACTION_RETRIES`). Readers never lock. Safe assumption: **one concurrent writer per branch**; package ingest is sequential per package anyway.

**Topology for nudox: DB-per-package (recommended)**

| Option | Structure | Isolation | Cross-package query |
|---|---|---|---|
| **DB-per-package** | `admin/pkg-{lang}-{name}` | Strong | App-level join or always-hot index DB |
| Branch-per-package | One mega-DB, many branches | Weak | WOQL `using` across branches in one DB |
| Branch-per-version | `pkg@1.0.0` branches | Medium | Diff-friendly, branch explosion |

**Recommendation: one Terminus DB per package, single `main` branch, one commit per published generation.**

Rationale aligned with live model (`Package` version-agnostic, `PackageVersion` per generation, stable `Symbol/…` IRIs):

1. Demotion = drop/tombstone one DB without touching others.
2. HTTP paths map cleanly: `/api/document/admin/pkg-…/local/branch/main`.
3. ~500k-package universes cannot all be Terminus DBs — **hot tier target: hundreds–low thousands** of promoted packages.
4. Cross-package edges live in a separate always-hot `admin/cross-pkg-index` (or equivalent) plus linker stubs under `~extern`.

**Layer depth under incremental publishing**

Each generation = one layer of only changed triples. With base-3 auto-rollup, even 1,000 version bumps stay in the log-layer regime. This is the workload TerminusDB is built for.

---

### A.3 Write Path: Throughput, Bulk Import, Rust Integration

**Document API (primary write path)**

[`POST /api/document/<resource>`](https://terminusdb.org/docs/document-insertion/) — JSON array or NDJSON stream. Flags of interest:

| Flag | Use |
|---|---|
| `overwrite=true` | Upsert by `@id` — **idempotent re-publish** |
| `merge_repeats` | Dedup within batch (also on update since v12.0.5) |
| `full_replace` | Delete-all then insert — simple first promotion |

Cross-document refs in one POST via `@capture` / `@ref`. v12.0.6 fixed non-ASCII document loss and TaggedUnion `@ref` resolution — both relevant to symbol graphs with unicode names.

**Performance notes (vendor claims + caveats)**

- Internal **Rust serde JSON parsing** (from the v11.2 → v12 path): stated **~5× faster** JSON parse than the old Prolog parser ([v12.0.0 notes](https://github.com/terminusdb/terminusdb/releases/tag/v12.0.0)).
- Community persistence discussion cites **~10× RAM:data** during write transactions — plan memory for bulk promotion of large graphs.
- No credible third-party throughput benchmarks as of this research — treat any absolute numbers as order-of-magnitude only.
- Prefer **Document API** for IR → graph publish; reserve **WOQL** for complex conditional mutations and path queries.

**Rust production story (2026)**

| Layer | What to use |
|---|---|
| Schema + instance codegen | **Already in tree:** `terminusdb_schema` / `terminusdb_schema_derive` (`TerminusDBModel`) via vendored [terminusdb-rs](https://github.com/ParapluOU/terminusdb-rs) |
| HTTP transport | `reqwest` + `serde_json` (or extend terminusdb-rs client if it covers Document API for your pin) |
| Embed store crate | `terminus-store` 0.21.5 on crates.io — **stale vs server**; not the promotion path |
| Rate limiting / admission primitives | `governor` (registry already on **0.7**; latest **0.10.4**, 2025-12-16) |

Pattern for the hot-tier publisher (typed wrapper crate `nudox-terminus-client` recommended):

```rust
// Sketch — production client wraps Document API with overwrite + NDJSON bulk
pub struct TerminusClient {
    client: reqwest::Client,
    base_url: String, // e.g. http://terminus:6363
}

impl TerminusClient {
    pub async fn insert_documents(
        &self,
        db: &str,
        docs: &[serde_json::Value],
    ) -> Result<Vec<String>, Error> {
        let url = format!(
            "{}/api/document/admin/{}?graph_type=instance&author=nudox&message=publish&overwrite=true",
            self.base_url, db
        );
        let resp = self.client.post(&url).basic_auth("admin", Some(&self.password))
            .json(docs).send().await?;
        // parse returned document IDs / commit info
        Ok(resp.json().await?)
    }
}
```

Live emit path already produces JSON-LD via `ToTDBInstance` / `ToJson` in `generate/linked_data/emit.rs` — the missing piece is **gated** publish + commit-per-generation, not a new schema pipeline.

**Schema pipeline correction (vs original research prompt)**

The original mission statement assumed `LinkML schema.yaml → Rust + schema.json`. **Live code does not do that.** Schema is derived from Rust:

```
#[derive(TerminusDBModel)]  →  Terminus class/enum schema + instance JSON-LD
```

Do **not** invent a parallel LinkML path for lineage types. Extend `model.rs` with any new classes (`LineageEdge` if needed) under the same derive.

---

### A.4 Lineage Modeling: Generations and Symbol History

**What the live model already encodes**

From `workspace/compiler/graph/model.rs` + `link.rs`:

| Type | IRI pattern (logical / wire) | Role |
|---|---|---|
| `Package` | `Package/{lang}%2F{name}` | Version-agnostic package node |
| `PackageVersion` | `PackageVersion/{lang}%2F{name}@{ver}` | Per-generation membership; `declares: Vec<TdbLazy<Symbol>>` |
| `Symbol` | `Symbol/{lang}%2F{pkg}%2F{fq}` | **Stable, version-agnostic** client-minted id |
| `Implementation` / `Reference` | `value_hash` keys | Content-addressed reified edges |

Comment on `PackageVersion` (source of truth):

> Cross-version symbol diff (`resolve_across`) is a set-diff of two `declares` sets.

Symbol IRIs deliberately **do not embed version**. That is option (i) in the abstract design space: history = document content evolution under a stable `@id`, plus commit log on the package DB.

**Three abstract options**

| Option | Mechanism | Terminus feature | Incremental driver |
|---|---|---|---|
| **(i) Stable IRI, single branch** | Symbol `@id` stable; history = commits | Commit history + time-travel | Diff API / IR set-diff |
| (ii) Explicit lineage edges | Version-scoped docs + `LINEAGE` edges | Ordinary graph edges | Custom delta |
| (iii) Branch-per-version | One branch per semver | Branch diff/patch | Branch diff |

**Diff / patch capabilities (verified)**

[JSON Diff and Patch](https://terminusdb.org/docs/json-diff-and-patch/) + [diffpatchjson](https://github.com/terminusdb/diffpatchjson):

- **Standalone mode:** diff any two JSON objects (no DB) — usable for IR blob compare offline.
- **Versioned mode:** diff a document (or all documents) between two commits/branches/layers.
- Patch ops: `SwapValue`, list ops (`CopyList` / `SwapList` / `PatchList`), `ModifyTable`, `ForceValue`; v12.0.6 improved `SwapValue` for dicts/lists/numbers.
- Apply endpoint enables cherry-pick style application to another branch.
- v12.0.5: **diff + streaming on history endpoint** — stream document changes across commits without loading full history.

**Recommendation: hybrid (i) + IR-level set-diff — do not dual-write invent-y lineage for hot path**

```
DB: admin/pkg-{lang}-{sanitized_name}
Branch: main
Commit k  → generation V_k  (only Δ documents: upserted/deleted symbols + PackageVersion)
```

**Hot tier lineage queries:**

1. **Membership across generations:** set-diff of `PackageVersion.declares` for `V_a` vs `V_b` (already designed).
2. **Content evolution of one symbol:** Terminus history / versioned diff on the stable `Symbol/…` document between the commits that published those generations.
3. **Optional materialization:** only if product needs first-class “renamed to / replaced by” edges (semver renames, signature-incompatible successors), add a first-class `LineageEdge` class — **not** as a replacement for (1)/(2).

**Cold tier lineage (no commit log):**

- Persist per-generation IR blobs in CAS (already the blob pipeline).
- Membership: load two `PackageVersion`-equivalent IR indices and set-diff `declares`.
- Content change: standalone JSON/IR structural diff (can reuse Terminus **standalone** `/api/diff` as a pure function, or a local IR diff in Rust).
- Explicit `LineageEdge` documents **only on the cold IR graph** if UX needs a precomputed chain without recompute.

**Incremental publish algorithm (both tiers’ producer side)**

```
on_new_generation(pkg, V_new):
  ir_old = cas.get(pkg, V_prev)   # optional if first gen
  ir_new = cas.get(pkg, V_new)
  delta  = symbol_set_diff(ir_old, ir_new)  # Added / Removed / Modified (shape hash)

  if status[pkg] == Hot:
    docs = lower_delta(delta)               # from_ir/link only for changed symbols
    terminus.insert(docs, overwrite=true)   # + PackageVersion for V_new
    terminus.delete(delta.removed_ids)
    # one commit message: "publish {pkg}@{V_new}"
  else:
    # cold: CAS already holds IR; mark StoreLinks.graph=false
    pass
```

Idempotency: Document API `overwrite=true` + content-addressed `Implementation`/`Reference` keys make retries safe. Track `promoted_through_generation` cursor on the promotion job for partial backfill resume.

**Do not use Terminus branch-diff as the *driver* of incremental publish.** Compute delta from IR (source of truth in CAS); use Terminus diff for *query/audit* and optional verification that hot graph matches IR.

---

### A.5 Query Capabilities

**WOQL** ([docs](https://terminusdb.org/docs/woql-explanation/))

- Datalog/Prolog-derived; path queries with regex-like patterns for transitive closure ([path queries](https://terminusdb.com/docs/path-queries-in-woql/) / current docs under terminusdb.org).
- Temporal: open a historical layer / commit for time-travel.
- v12: `slice()`, `dot()` (incl. path edge bindings), `sys:Dictionary`, rational arithmetic.
- Best for: multi-hop call graphs, “who implements X”, recursive walks.

**GraphQL**

- Schema-generated; `_and`/`_or`/`_not`, `_path`, `_count`, pagination.
- Rust-backed GraphQL engine — prefer for simple document fetch / filtered lists.
- Scoped to a **single resource path** (DB + branch) — not a multi-DB federation layer.

**Cross-package / federation**

Terminus does **not** federate cross-DB queries in one GraphQL/WOQL request. Options:

1. App-level fan-out across package DBs (Rust join).
2. Always-hot `admin/cross-pkg-index` with import/re-export edges.
3. Linker stubs (`~extern`) already mint deterministic IRIs for unresolved cross-package names so edges never dangle.

**Performance characteristics**

| Workload | Expected behavior |
|---|---|
| Document fetch by id | Fast (GraphQL/Document API) |
| Path / transitive closure | O(explored subgraph); Prolog WOQL — not a Rust BFS |
| Layer traversal | O(depth) → O(log N) with auto-rollup |
| Write under contention | Optimistic retry; single-writer per branch |

For code graphs, **GraphQL for point lookups, WOQL for path/closure** is the split.

---

### A.6 Operational Characteristics

**Memory**

- Write transactions: plan ~**10×** working set vs serialized graph size (community observation).
- Reads: succinct layers + LRU layer cache; much leaner than write peak.
- Sizing sketch: 10 MB symbol graph → budget ~100 MB RAM during ingest for that DB.

**Disk**

- `TERMINUSDB_SERVER_DB_PATH` (default `./storage/db`).
- Bundled layer archives (post store v0.20).
- Binary; offline inspection needs tools / store library.

**Backup**

- CLI: `terminusdb bundle` / `unbundle`.
- k8s: PVC snapshots of data path, or bundle to object storage.

**Clone / promote**

- `terminusdb clone` can copy a DB — possible path for warm replicas, not required for admission.

**Kubernetes**

- No official Helm operator as of research date.
- Docker image: **non-root** variants since v12.
- Deploy **StatefulSet + PVC**; one instance can multi-tenant many DBs via org/user model.
- Correlation headers / W3C `traceparent` support for tracing (v12).

**Multi-tenancy isolation**

- DB-per-package gives hard isolation for drop/quota.
- Authz at Terminus user/org layer is coarse; nudox capability tokens (`server` authz) should remain the **product** multi-tenant boundary, with Terminus as a trusted internal store.

---

## Part B — Admission Control for the Hot Tier

### B.1 Algorithms: Leaky Bucket vs Token Bucket vs Sliding Window vs Frequency Sketch

#### Leaky bucket (user suggestion — formalized for *promotion scoring*)

```
For each package P:
  bucket[P] ∈ [0, CAPACITY]

On graph-query event for P at time t with weight w:
  # lazy leak
  bucket[P] = max(0, bucket[P] - LEAK_RATE * (t - last[P]))
  bucket[P] = min(CAPACITY, bucket[P] + w)
  last[P] = t

Promotion: bucket[P] >= PROMOTE_THRESHOLD  and status==Cold
Demotion:  bucket[P] <= DEMOTE_THRESHOLD   and status==Hot
           (DEMOTE_THRESHOLD < PROMOTE_THRESHOLD ⇒ hysteresis band)
```

Properties: measures **sustained** demand; O(1) state per package; natural hysteresis; burst-resistant if `CAPACITY` and `QUERY_WEIGHT` are sane.

**GCRA / `governor`:** GCRA is mathematically a leaky bucket and is what [`governor`](https://crates.io/crates/governor) implements (latest **0.10.4**, 2025-12-16; [docs](https://docs.rs/governor)). **But** governor answers “may this request pass?” — not “has demand accumulated enough to promote?” Use it for **API rate limits**; implement promotion with an explicit per-package level (dashmap + lazy decay), optionally borrowing GCRA math.

#### Token bucket

Fill-from-above; good for **burst allowance**. Wrong primary semantic for promotion (viral one-shots promote then evaporate). Reject as sole policy.

#### Sliding window

Exact rate over last T seconds; memory **O(events in window)** per package. Prohibitive at multi-million package cardinality. Use only for short debug metrics, not tier state.

#### W-TinyLFU / frequency sketch

[W-TinyLFU](https://arxiv.org/pdf/1512.00727) (Caffeine; [`moka` 0.12.15](https://crates.io/crates/moka) default TinyLFU/W-TinyLFU for in-process caches) optimizes **fixed-size cache admission** (“is new key hotter than victim?”). Hot-tier size is capacity-bounded by Terminus ops budget, not a fixed entry count with per-insert victim choice.

| Dimension | Leaky bucket score | W-TinyLFU | Sliding window |
|---|---|---|---|
| Memory / package | O(1) | Shared CMS + cache metadata | O(events) |
| Burst resistance | Strong | Strong (doorkeeper) | Weak |
| One-hit-wonder rejection | Good | Excellent | Poor |
| Sustained rate | Excellent | Good (aging counts) | Exact |
| Hysteresis | Native threshold band | External | External |
| Fit for “promote external store” | **Native** | Misfit (eviction choice) | Awkward at scale |
| In-tree today | — | `moka` 0.12 | — |

**Recommendation: leaky-bucket score + Count-Min Sketch doorkeeper**

```
on_graph_query(P, weight):
  cms.increment(P)
  if cms.estimate(P) < CMS_MIN_FREQ: return   # one-hit-wonder gate
  leaky_update(P, weight)
  maybe_enqueue_promotion(P)
```

CMS alone (TinyLFU-style) is **not** strictly better for this problem: it lacks native promote/demote hysteresis and is tuned for cache victim selection. The hybrid keeps TinyLFU’s anti-pollution filter and leaky-bucket’s sustained-rate semantics.

**Rust crates (2026-07-16)**

| Crate | Version / status | Use |
|---|---|---|
| [`governor`](https://crates.io/crates/governor) | **0.10.4** (2025-12-16); workspace registry pins **0.7** | Request rate limits; not promotion score |
| [`moka`](https://crates.io/crates/moka) | **0.12.15** (2026-03-22); already in heart/registry/server | Cold-path IR graph memoization / query result cache |
| [`count-min-sketch-rs`](https://crates.io/crates/count-min-sketch-rs) | **0.1.1** (2026-04-28) | CMS doorkeeper |
| [`probabilistic-collections`](https://crates.io/crates/probabilistic-collections) | **0.7.0** last **2020-05** | **Avoid** — unmaintained |
| [`leaky-bucket`](https://crates.io/crates/leaky-bucket) | **1.1.2** (2024-05) | Async rate limiter; still “allow/deny”, not scoring |
| `dashmap` | via ecosystem / governor | Per-package `BucketState` |

Implement promotion scoring in ~100 lines of Rust rather than forcing governor’s allow/deny API.

---

### B.2 Formalized Promotion Rule + Parameters

```
Constants:
  BUCKET_CAPACITY     = 100.0
  LEAK_RATE           = 1.0 / 60.0     # units per second  → half-life ~1 min scale
  CMS_MIN_FREQ        = 3
  PROMOTE_THRESHOLD   = 80.0
  DEMOTE_THRESHOLD    = 20.0
  CMS_AGING_PERIOD    = 3600s          # decay sketch periodically
  COOLING_DAYS        = 7              # tombstone → hard drop

Query weights (economic model, §B.4):
  SimpleDocumentFetch = 0.1
  GraphTraversal      = 2.0
  TransitiveClosure   = 5.0

State per package P:
  cms estimate (global sketch)
  bucket level, last_update
  status ∈ { Cold, Warming, Hot, Cooling }

on_graph_query(P, weight):
  cms.increment(P)
  if cms.estimate(P) < CMS_MIN_FREQ: return
  lazy_leak(P)
  bucket[P] = min(CAPACITY, bucket[P] + weight)
  if status==Cold and bucket>=PROMOTE_THRESHOLD:
    status = Warming
    enqueue_promotion(P)   # idempotent job

background_sweep every 60s:
  for P with status Hot|Cooling|Warming:
    lazy_leak(P)
    if status==Hot and bucket<=DEMOTE_THRESHOLD:
      status = Cooling
      enqueue_soft_demotion(P)   # stop routing; keep DB
    if status==Cooling and cold_for >= COOLING_DAYS:
      hard_drop_db(P)
      status = Cold
  cms.age()  # e.g. halve counters
```

**Hysteresis math (corrected)**

With `LEAK_RATE = 1/60 per second` (1 unit/minute) and no queries:

- Drain from PROMOTE (80) to DEMOTE (20) = **60 units** ⇒ **~60 minutes** of silence before demotion.
- At 1 simple fetch/minute (weight 0.1): net leak ≈ 0.9/min ⇒ demotion from 80 in ~67 minutes.
- At 1 traversal/minute (weight 2.0): net **fill** — package stays hot.

Earlier draft text that mixed `LEAK_RATE=1.0/s` with “1 query/minute” produced nonsensical 60-second demotion; **do not use 1.0 unit/second** unless you intentionally want minute-scale flapping. Default **1 unit/minute** leak with a 60-unit band targets **hour-scale** hysteresis.

**Tuning table**

| Parameter | Default | Increase when… | Decrease when… |
|---|---|---|---|
| `LEAK_RATE` | 1/min | Too many sticky hot packages | Packages demote too fast |
| `CAPACITY` | 100 | Spikes falsely promote | Hard to ever fill |
| `CMS_MIN_FREQ` | 3 | Crawler noise | Legitimate rare but important packages never score |
| `PROMOTE_THRESHOLD` | 80 | Too many promotions | Hot tier underfilled |
| `DEMOTE_THRESHOLD` | 20 | Flapping | Tier bloated with zombies |
| Band (P−D) | 60 | Flapping | Slow reaction to traffic death |

**Hook into existing usage signal**

`usage_is_tracked_for_tiering` documents that ensure/init traffic is measured but **policy is out of scope**. Wire `on_graph_query` from:

1. GraphQL/WOQL handlers on the hot client path (primary).
2. Cold-path `PackageGraph` methods (so demand on cold packages still fills buckets).
3. Optionally down-weight bare `ensure_initialized` (library load ≠ graph query) — e.g. weight 0.05 — so install storms do not promote.

---

### B.3 Promotion Mechanics

**Cost of promotion**

1. Fetch IR blob(s) for generations to backfill from CAS (`heart` CAS / registry blob store).
2. Run existing `from_ir` + `link` projection → `TerminusDBModel` instances.
3. Ensure DB exists; insert schema once; bulk Document API publish (generation-by-generation or latest-only first).
4. Write `PackageVersion` docs + symbol Δ; set `StoreLinks.graph = true` for caught-up generations.
5. Flip admission state Cold→Warming→Hot only after commit succeeds.

Cost drivers: CAS I/O, lowering CPU, Terminus write RAM (~10×), history depth if full lineage backfill.

**Backfill policy**

| Policy | When |
|---|---|
| **Latest-only** (default) | First promotion — ship HEAD generation; lineage history grows from future commits |
| **N recent generations** | If product needs short lineage immediately |
| **Full history** | Rare; expensive; queue as low-priority job |

Recommend **latest-only on promote**, then incremental commits forever. Historical lineage for pre-promotion generations stays on cold IR blobs.

**Async + idempotent queue**

```
PromotionJob { package, requested_at, cursor_generation, attempt }
```

- Worker pool size 2–4 (Terminus write RAM + single-writer).
- Dedup key = package id (at most one active job).
- Retry with `overwrite=true`; advance `cursor_generation`.
- On permanent failure: status → Cold, metric + alert; do not leave Warming forever (timeout → Cold).

**Demotion**

| Stage | Action |
|---|---|
| Soft (Cooling) | Router uses cold path; DB retained; `StoreLinks.graph` may stay true until hard drop |
| Hard drop | `db delete`; clear `StoreLinks.graph`; free disk |

**Recommendation:** soft demote immediately on threshold; hard delete after **7 days** Cooling without re-promotion. Re-promotion during Cooling cancels drop and returns to Hot (or re-runs delta publish if generations advanced).

**Flapping control**

- Hysteresis band (above).
- Minimum residence time (e.g. 30 minutes Hot before eligible for Cooling).
- Global cap: if hot count ≥ `MAX_HOT_PACKAGES`, only promote if victim exists with lower score (optional TinyLFU-style comparison **at the margin** — here CMS frequency of candidate vs coldest hot package).

---

### B.4 Cold Path Economics

Promotion is worth it when:

```
(C_cold - C_hot) × Q > A_publish / T_hot
```

| Symbol | Meaning |
|---|---|
| `C_cold` | Cost/query on IR-over-blobs |
| `C_hot` | Cost/query on Terminus |
| `Q` | Sustained query rate |
| `A_publish` | One-time (or amortized) promotion + keep cost |
| `T_hot` | Expected hot residence time |

**Order-of-magnitude costs (local CAS / local Terminus)**

| Path | Simple fetch | Multi-hop traversal |
|---|---|---|
| Cold (local CAS + in-memory graph) | ~2–15 ms | ~50–1000 ms (load + walk) |
| Hot (HTTP + GraphQL/WOQL) | ~3–15 ms | ~10–500 ms (indexed triples, no full IR load) |

**Implications**

1. Simple symbol document fetch often **does not** justify Terminus — cold can be faster with warm IR cache (`moka`).
2. **Path / call-graph / hierarchy** queries justify promotion when frequent.
3. Therefore **weight graph-traversal events heavily** in the bucket (0.1 / 2.0 / 5.0 scheme).
4. Cold path must implement the full `PackageGraph` trait well — not a degraded stub — or the product will over-promote to hide cold bugs.

**Cold path sketch**

```
IrBlobPackageGraph:
  get_symbol       → load IR index (moka-cached) + project one node
  query_symbols    → scan/filter in memory or sqlite helper index
  transitive_closure → adjacency lists from from_ir-equivalent edges
  symbol_lineage   → PackageVersion declares set-diff + optional IR content hash
```

---

## Synthesis: Concrete Design for Nudox

### Terminus topology

```
TerminusDB instance (internal)
├── _system
├── admin/cross-pkg-index     # always hot: inter-package edges
└── admin/pkg-{lang}-{name}   # one DB per promoted package
      └── branch: main
            ├── commit: gen V0 (full latest-only publish on promote)
            ├── commit: gen V1 (Δ symbols only)
            └── commit: gen V2 (Δ)
```

Naming: `pkg-{language}-{sanitize(name)}` with the same sanitize rules as `link.rs` (`encode_id_segment` / `%2F` for hierarchy). Language ∈ compiler’s language set (rust, python, …), not only package ecosystems.

### Lineage (aligned with live schema)

- **Stable** `Symbol/{lang}%2F{pkg}%2F{fq}` across generations.
- **Per-generation** `PackageVersion` with `declares` for membership set-diff.
- **Hot history:** Terminus commits + versioned document diff for content changes.
- **Cold history:** CAS IR generations + set-diff / standalone structural diff.
- **Optional later:** `LineageEdge` class only for product-visible rename/replace relations.

Do **not** invent parallel `SymbolNode` types — extend `model.rs`.

### Admission algorithm

`PackageTierManager`: CMS doorkeeper + leaky-bucket score + Warming/Hot/Cooling state machine (§B.2). Persist tier status + bucket snapshot in Postgres (next to `PackageMetadata` / metrics) so restarts do not re-promote the world.

### Unified graph trait (both tiers)

```rust
#[async_trait]
pub trait PackageGraph: Send + Sync {
    async fn get_symbol(&self, id: &SymbolId) -> Result<Option<SymbolView>>;
    async fn query_symbols(&self, q: &SymbolQuery) -> Result<Vec<SymbolView>>;
    async fn transitive_closure(
        &self,
        start: &SymbolId,
        edge_kind: EdgeKind,
        max_depth: Option<usize>,
    ) -> Result<Vec<SymbolView>>;
    async fn declares(&self, version: &str) -> Result<Vec<SymbolId>>;
    async fn membership_diff(&self, from_ver: &str, to_ver: &str) -> Result<MembershipDiff>;
    async fn symbol_content_diff(
        &self,
        id: &SymbolId,
        from_ver: &str,
        to_ver: &str,
    ) -> Result<Option<ContentDiff>>;
    async fn latest_generation(&self) -> Result<String>;
}

pub struct TerminusPackageGraph { /* HTTP + db name */ }
pub struct IrBlobPackageGraph { /* CAS + moka IR cache + in-memory adj */ }

pub struct TieredPackageGraph {
    manager: Arc<PackageTierManager>,
    // factory: PackageId → impl PackageGraph for hot or cold
}
```

Router: record weighted query → choose Hot if `status ∈ {Hot, Warming}` **and** DB ready; else cold. Warming should still answer from cold until promotion commit completes (no brownout).

### Rust crates summary (actionable)

| Purpose | Crate / component | Version / pin |
|---|---|---|
| Terminus schema/instances | vendored `terminusdb_schema(_derive)` | `TERMINUS_REV=4fefb043…` (terminusdb-rs) |
| Terminus HTTP | `reqwest` + thin `nudox-terminus-client` | reqwest ^0.12 |
| Embed triple store (optional) | `terminus-store` | 0.21.5 — **stale; non-critical path only** |
| Promotion scoring | custom + `dashmap` | — |
| CMS doorkeeper | `count-min-sketch-rs` | 0.1.1 |
| API rate limits | `governor` | upgrade registry **0.7 → 0.10.4** when convenient |
| Cold IR / result cache | `moka` | 0.12.x (already) |
| Async | `tokio` | ^1 |

### Implementation roadmap (phased)

| Phase | Deliverable | Depends |
|---|---|---|
| **P0** | `PackageGraph` trait + `IrBlobPackageGraph` MVP (get + simple adj) | CAS IR blobs |
| **P1** | `PackageTierManager` + metrics; wire query weights; no Terminus gate yet | P0 |
| **P2** | Gated Terminus publish: only Hot/Warming jobs set `StoreLinks.graph` | P1 + terminus client |
| **P3** | Incremental Δ publish per generation on hot packages | P2 + IR set-diff |
| **P4** | Soft/hard demotion + global hot cap | P2 |
| **P5** | Cross-pkg index DB + WOQL path UX | P2 |

### Integration points in today’s tree

| Concern | Where |
|---|---|
| IR → graph docs | `compiler/graph/from_ir.rs`, `link.rs`, `model.rs` |
| JSON-LD emit | `compiler/generate/linked_data/emit.rs` |
| Graph materialization flag | `registry/metadata/mod.rs` `StoreLinks.graph` |
| Blob fan-out | `registry/blob/emit.rs` (terminus sink poller) |
| Usage metrics hook | server ensure path / future graph handlers |
| Cache for cold graphs | `heart/cache` + `moka` |

---

## Risks & open questions

1. **Terminus write RAM (≈10×)** during promotion of large packages — need admission queue concurrency limits and possibly generation size caps.
2. **crates.io `terminus-store` staleness** vs server v12 — do not embed for production hot path.
3. **Prolog WOQL latency variance** under multi-hop queries — measure before assuming hot always beats cold.
4. **Cross-package federation** is app-orchestrated — product queries that span many packages may not benefit from per-package promotion.
5. **Schema evolution** of `TerminusDBModel` types vs already-promoted DBs — need migration/re-promote strategy when model changes.
6. **Stable Symbol IRI vs rename** — renames look like remove+add in `declares` set-diff; product may later need explicit `LineageEdge`.
7. **Global hot capacity** under multi-tenant load — who loses when `MAX_HOT` is hit?
8. **terminusdb-rs** is third-party (ParapluOU) — pin carefully; monitor DFRNT/official Rust story.
9. **Demotion correctness** — readers mid-query during soft demotion must not fail; Warming/Cooling routing rules need care.
10. **Economic constants** (`C_cold`, `C_hot`) are estimates — instrument real p50/p99 before locking thresholds.

---

## Appendix: Key URLs and sources

- [TerminusDB GitHub](https://github.com/terminusdb/terminusdb)
- [TerminusDB Releases](https://github.com/terminusdb/terminusdb/releases) (v12.0.6 latest stable as of 2026-07-16)
- [TerminusDB 12 Release Blog](https://terminusdb.org/blog/2025-12-08-terminusdb-12-release/)
- [What is TerminusDB](https://terminusdb.org/docs/terminusdb-explanation/)
- [Immutability and Concurrency](https://terminusdb.org/docs/immutability-and-concurrency/)
- [Knowledge Graph Version Control](https://terminusdb.org/docs/knowledge-graph-version-control/)
- [JSON Diff and Patch](https://terminusdb.org/docs/json-diff-and-patch/)
- [diffpatchjson](https://github.com/terminusdb/diffpatchjson)
- [Document Insertion API](https://terminusdb.org/docs/document-insertion/)
- [WOQL Explanation](https://terminusdb.org/docs/woql-explanation/)
- [Persistence / rollup discussion](https://github.com/orgs/terminusdb/discussions/1865)
- [Delta Rollups (Medium)](https://medium.com/terminusdb/delta-rollups-4ec308087122)
- [terminus-store crates.io](https://crates.io/crates/terminus-store) (0.21.5, last publish 2024-03-11)
- [terminusdb-store GitHub](https://github.com/terminusdb/terminusdb-store)
- [ParapluOU/terminusdb-rs](https://github.com/ParapluOU/terminusdb-rs) (vendored schema derive)
- [governor crates.io](https://crates.io/crates/governor) (0.10.4)
- [moka crates.io](https://crates.io/crates/moka) (0.12.15)
- [count-min-sketch-rs](https://crates.io/crates/count-min-sketch-rs) (0.1.1)
- [TinyLFU paper](https://arxiv.org/pdf/1512.00727)
- [Succinct Data Structures PDF](https://assets.terminusdb.com/research/succinct-data-structures-and-delta-encoding.pdf)

---

## Executive Summary

TerminusDB remains a sound bet for nudox’s **hot-tier code graph** in mid-2026, but not for every package. Live verification shows **v12.0.6** (2026-06-24) under **DFRNT** stewardship (since 2025), Apache-2.0, ~3.3k stars, low issue count, and an active though quiet release train through Jun 2026. Immutable delta layers, auto-optimize (base-3 rollups), semantic JSON diff/patch, and commit history map cleanly onto incremental multi-generation symbol publishing. The binding risks are operational (Prolog core, ~10× write RAM, no official Rust server client, **crates.io terminus-store stuck at 0.21.5 / Mar 2024**) — isolate behind HTTP and keep a first-class cold path.

The live codebase is ahead of the abstract research brief in important ways: graph documents are already modeled with `TerminusDBModel` (`compiler/graph/model.rs`), projected from IR (`from_ir`/`link`), and emitted as JSON-LD. Symbol IRIs are **version-agnostic**; `PackageVersion.declares` is the designed membership lineage mechanism. The schema pipeline is **Rust derive → Terminus schema**, not LinkML. Admission control should set `StoreLinks.graph`, not invent a parallel document model.

**Hot-tier topology:** one Terminus DB per package, branch `main`, one commit per generation, plus an always-hot cross-package index. **Lineage:** stable Symbol IRIs + `PackageVersion` set-diff + Terminus history for content; compute publish deltas from **IR/CAS** (source of truth), use Terminus diff for audit/time-travel. **Admission:** leaky-bucket **score** (not governor allow/deny) with CMS one-hit-wonder gate, query-type weights (cheap for point lookup, heavy for path/closure), Warming/Hot/Cooling states, soft demote then hard drop after a cooling window, hour-scale hysteresis (leak ≈ 1 unit/minute, band 80→20). **Cold path:** IR blobs + `moka` cache implementing the same `PackageGraph` trait; promote only when sustained *graph* demand pays for publish amortization.

Phased delivery: cold trait → scorer metrics → gated publish → incremental Δ → demotion/cap → cross-pkg index. That sequence keeps Terminus a privilege of the working set without blocking graph UX for the long tail.

### Top concrete recommendations

- Pin production to **TerminusDB Server ≥ v12.0.6**; upgrade in-place (storage format stable since v11).
- **DB-per-package**, single `main` branch; cap hot set to hundreds–low thousands; always-hot `cross-pkg-index`.
- Gate materialization on admission; drive `StoreLinks.graph` from tier state.
- Implement **CMS + leaky-bucket scorer** (custom); keep `governor` for API limits; use **`moka`** for cold IR graphs; add **`count-min-sketch-rs` 0.1.1**.
- Publish **Δ from IR set-diff** with Document API `overwrite=true`; latest-only on first promote.
- Prefer GraphQL for fetches, WOQL for path/closure; weight those events 0.1 / 2.0 / 5.0 in the bucket.
- Soft-demote on low score; hard-delete DB after ~7 days Cooling.
- Extend existing `TerminusDBModel` types — do not introduce a second schema stack.
- Treat `terminus-store` crates.io as non-authoritative for v12 parity.
- Instrument real cold vs hot latency before freezing economic thresholds.

### Open questions / residual risks

- Measured Terminus multi-hop p99 vs cold in-memory BFS on large crates.
- `MAX_HOT` eviction policy under multi-tenant fairness.
- Model evolution / re-promote when `TerminusDBModel` schema changes.
- Rename lineage product requirements beyond set-diff.
- Long-term maintenance of vendored terminusdb-rs vs future official Rust support.
- Whether cross-package queries dominate enough to justify a denser always-hot index than per-package promotion alone.

---

## Alternatives considered (edge-tech)

**Status:** short decision pointer (2026-07-16). Hot Terminus + cold graph-over-IR remains default.

**Unified map:** [edge-tech/00-DECISIONS.md](../../edge-tech/00-DECISIONS.md)

| Alternative | Verdict | Notes | Source |
|---|---|---|---|
| **Doltgres as hot graph / Terminus replacement** | **Reject** | SQL/prolly versioning ≠ document-graph + WOQL/GraphQL path ergonomics; multi-tenant product orgs ≠ DoltHub | [edge-tech/01-doltgres](../../edge-tech/01-doltgres/PLAN.md) option **(C)** |
| **Doltgres A′ — INDEX metadata only** | **Reject as SoT** | SQLite+Litestream (plan 11) wins ops; VC on ops tables is anti-feature | 01 option **(A)** |
| **Doltgres B′ — metadata + symbols side-store** | **Watch / optional post-1.0 spike** | Commit-per-generation + `dolt_diff` for symbol catalog tooling only — **not** hot graph, **not** INDEX SoT | 01 option **(B)** |
| **LadybugDB (Kùzu successor)** | **Optional desktop Cypher accelerator** | Behind `PackageGraph` / graph-cold feature; **not** Terminus replacement | [05-surrounding-edge](../../edge-tech/05-surrounding-edge/PLAN.md) §3 |
| **Upstream Kùzu** | **Reject** | Project archived; evaluate Ladybug only | 05 |
| **Falkor / Neo4j / AGE / … as second hot store** | **Reject** | One hot system (Terminus); do not dual-stack “just in case” | 05 §11 |
| **Cold graph-over-IR** | **Keep default** | Long-tail packages; same `GraphStore` trait | [20-graph-over-ir](../20-graph-over-ir/PLAN.md) |

**Default topology (unchanged):** hot Terminus admission (this plan) · cold IR projection (20) · optional Ladybug feature on desktop only · Doltgres B′ only if a concrete SQL generation-diff product appears after Doltgres 1.0.
