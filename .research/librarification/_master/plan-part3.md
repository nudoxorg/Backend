
---

# Part III — Engines

## §7 Incremental spine: stages, traces, deltas

**Source:** `.research/librarification/08-incremental.md` · **Planes:** REGISTRY + INDEX/ORCH

### 7.1 Verdict

Salsa (0.28) remains best-in-class for **intra-process** incrementality but has no durable cross-process story (rust-analyzer persistence: still unimplemented; salsa#10 open since 2018). The pipeline spine is instead a **hand-rolled constructive-trace store**: SQLite + BLAKE3 digests, the same digest discipline as Bazel/Nix content-addressed builds (GD-14). Salsa may live *inside* a producer; its memo tables are never serialized as product state.

The codebase already has the input half: `heart::ContentHash`, `hash_source_tree`, `SourceArchive`/`FileDigest`, producer CAS hit/miss observer, generation/snapshot folding. Missing: a persistent per-stage trace at **symbol** granularity + a `SymbolDelta` contract.

### 7.2 Stage graph and hash layers

```
git commit (Generation, §8) ─file delta (gix)─► Producer → IR blob
      └─ SymbolDelta (content hashes) ─┬─► render ─┬─► constructive traces
                                       ├─► embed   │    (sqlite + CAS)
                                       ├─► tantivy ┤
                                       └─► terminus┘
```

| Layer | Key | Purpose |
|---|---|---|
| L0 tree | git commit/tree OID | gate work (§8) |
| L1 file | blob OID / BLAKE3 | skip unchanged files |
| L2 package IR | fold of symbol hashes + meta | producer cache (exists directionally) |
| **L3 symbol** | `content_hash` (§6.3) | **primary early-cutoff for fan-out** |
| L4 stage | `H(stage_id ‖ tool ‖ L3)` | per-downstream memo |

Part hashes refine L3: embed depends on `embed_key`; Terminus edge rewrite depends on `ref_hash`; a docs-only edit re-embeds and re-indexes text but skips graph edge rewrites.

### 7.3 SymbolDelta contract (heart)

```rust
pub struct SymbolDelta {
    pub generation: u64, pub parent: Option<u64>,
    pub added: Vec<SymbolId>, pub removed: Vec<SymbolId>,
    pub changed: Vec<SymbolChange>,      // unchanged implicit
}
pub struct SymbolChange {
    pub id: SymbolId, pub old_hash: ContentHash, pub new_hash: ContentHash,
    pub old_parts: Option<SymbolPartHashes>, pub new_parts: Option<SymbolPartHashes>,
}
```

Computed after dirty packages' IR seals into CAS, by diffing the `symbol_id → content_hash` maps of generation N vs N−1 (`symbol_heads`). Consumption: tantivy add/delete_term/replace; vectors embed+upsert/delete/re-embed; terminus insert/delete/diff-patch; render write/delete/rewrite.

### 7.4 Trace store (SQLite, both planes)

```sql
CREATE TABLE symbol_heads (
  generation INTEGER NOT NULL, symbol_id TEXT NOT NULL,
  content_hash BLOB NOT NULL, part_sig BLOB, part_body BLOB, part_refs BLOB,
  ir_blob BLOB NOT NULL,               -- ContentHash of CAS object
  PRIMARY KEY (generation, symbol_id));
CREATE INDEX symbol_heads_by_hash ON symbol_heads(content_hash);

CREATE TABLE stage_traces (
  stage_id TEXT NOT NULL, input_digest BLOB NOT NULL, output_digest BLOB NOT NULL,
  output_blob BLOB NOT NULL, tool_digest BLOB NOT NULL, created_at INTEGER NOT NULL,
  PRIMARY KEY (stage_id, input_digest, tool_digest));   -- idempotent re-runs

CREATE TABLE generations ( id INTEGER PRIMARY KEY, git_commit BLOB NOT NULL UNIQUE,
  parent_id INTEGER, status TEXT NOT NULL /* running|sealed|failed */, sealed_at INTEGER);

CREATE TABLE file_blobs ( generation INTEGER NOT NULL, path TEXT NOT NULL,
  blob_oid BLOB NOT NULL, PRIMARY KEY (generation, path));
```

**Seal protocol (atomic):** insert `running` → run producers, write CAS + staging rows → compute SymbolDelta vs parent → apply fan-out stages (idempotent via stage_traces PK) → transaction: flip `sealed`, promote staging → symbol_heads. Crash mid-run leaves `running`; the next process reaps or restarts.

### 7.5 Stage trait

```rust
pub trait Stage: Send + Sync {
    type In: StageInput; type Out: StageOutput;
    fn stage_id(&self) -> &'static str;          // stable, semantically versioned ('embed.v3')
    fn tool_digest(&self) -> ContentHash;        // model/config fingerprint — global invalidation lever
    fn input_digest(&self, input: &Self::In) -> ContentHash;   // pure, deterministic
    fn run(&self, input: &Self::In) -> Result<Self::Out, StageError>;
    fn output_digest(&self, out: &Self::Out) -> ContentHash;
}
pub fn run_stage<S: Stage>(traces: &TraceStore, cas: &Cas, stage: &S, input: &S::In)
    -> Result<S::Out, StageError>  // trace hit → CAS decode; miss → run, put, record
```

EmbedStage hashes `content_hash` (or `embed_key`) rather than text, maximizing cache hits when IR→text is pure.

### 7.6 Desktop vs fleet

| Concern | Desktop | Fleet |
|---|---|---|
| Trace store | SQLite in app data dir | SQLite on INDEX PVC (GD-24 — no Postgres) |
| CAS | DiskCas | S3; same ContentHash keys — same symbol hash ⇒ one embed blob cluster-wide |
| Workers | in-process stage runners | per-stage deployables; lease generation id; idempotent via traces PK |

### 7.7 Implementation steps

- **S7.1** heart `stage` module: Stage/StageInput/StageOutput, run_stage, TraceStore trait. **S7.2** meta-store: trace tables + rusqlite/sqlx impls. **S7.3** Wrap existing tantivy/vector/graph sinks as Stages (no behavior change; traces record). **S7.4** symbol_heads population at seal (needs S6.3 hashes). **S7.5** SymbolDelta computation + delta-driven fan-out (replaces full-generation fan-out) — the GD-28 switch-on. **S7.6** Worked-example E2E test: one-symbol edit re-embeds exactly one symbol (the doc's worked example becomes the acceptance test).
- Prereqs: §6 normalizers (S6.3); §8 gate decides *when* S7.5 runs on desktop.

---

## §8 Commit gate & generations

**Source:** `.research/librarification/17-commit-gate.md` · **Plane:** REGISTRY (desktop), mirrored by INDEX publish

### 8.1 Generation identity (frozen)

```
durable_generation_id = BLAKE3("nudox.gen.v1" ‖ project_id ‖ repo_id ‖ commit_oid ‖ encode(submodule_set))
```

- `commit_oid` alone is insufficient (multi-root workspaces sharing a monorepo commit are different trust planes); `tree_oid` is recorded defensively (empty commits) but lineage follows the commit graph.
- Branch names are **never** part of the key (advisory metadata only); two branches at one commit share one sealed generation; branch-tip movement without commit change is a no-op for embeddings.
- Detached HEAD is allowed and indexed. Unborn branch → no durable generation. Amend/rebase create new OIDs — sealed generations are never mutated; parent selection walks **sealed first-parent ancestors** and falls back to full rebuild.
- Worktrees share CAS by content but keep per-project active-HEAD pointers. Submodules default to pin-only/ignore (policy `PinSubmodules` includes the sorted pin set in the key).
- Ephemeral "Index now" generations live in a **separate id space** (`ephemeral_generations`), TTL-GC'd, and must never enter durable REGISTRY publish paths.

### 8.2 Commit detection

`notify` watches on `GIT_DIR` (HEAD, refs/, packed-refs) + a 5 s poll safety net; 300 ms debounce; longer quiet period during rebase sequences. `post-commit`/`post-rewrite` hooks are optional accelerators only. gitoxide does not replace the FS watcher; the existing `vcs.rs` gix code is reused for clone/fetch/materialize of registry packages — never to wipe a user's project clone.

### 8.3 Dirty-tree policy (GD-15/GD-28)

Dirty working trees never auto-trigger embeddings or durable index updates. The GUI shows a dirty badge and serves the last sealed commit. After tree-diff (gix, OID-based), a `CompositePackageMapper` (Cargo, npm/pnpm/yarn, Go, Python, Maven/Gradle, NuGet, Nix) over-approximates dirty packages; lockfile-only commits refresh the untrusted DepSet via the SyncEngine without re-producing first-party IR.

### 8.4 REGISTRY schema (extends §7.4)

```sql
CREATE TABLE generations (              -- one row per durable/in-flight generation
  id INTEGER PRIMARY KEY, generation_uid BLOB NOT NULL UNIQUE,
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

`parent_id` = the generation used for tree-diff/SymbolDelta (nearest **sealed** ancestor, not necessarily `parents[0]`). The schema is a **DAG** (branching), with `UNIQUE(project_id, commit_oid)` for durables.

**GC pins:** CAS blobs survive while any generation/trace/pin references their hash; `stage_traces` survive across commits (LRU by created_at under disk budget); `symbol_heads` drop with their generation; sealed `generations` rows keep HEAD, viewed branch tips, user pins, last N=50, all within 14 days; ephemeral always evictable. Pin levels: 0 evictable · 1 recent · 2 branch tip/HEAD · 3 user pin · 4 active search session.

**Crash recovery:** `running` rows past TTL → failed (or restart if commit_oid == HEAD); un-promoted staging discarded; `run_stage` idempotency via traces PK.

### 8.5 Implementation steps

- **S8.1** `RepoBinding` + `CommitGate` types in client/registry-local; notify watcher + poll + debounce. **S8.2** generations/project_active/submodule schema (with S7.2). **S8.3** gix tree-diff → `CompositePackageMapper` (per-ecosystem impls, reusing §15 manifest detection). **S8.4** Seal pipeline wiring: gate → producers → S7.5 delta fan-out. **S8.5** Ephemeral preview path (explicit user action; TTL GC). **S8.6** Multi-root/nested-repo/worktree matrix tests (the doc's worked examples as fixtures).
- Prereqs: S7.x; §15 project model for package mapping; §16 SyncEngine for lockfile-only commits.

---

## §9 Graph over IR: the cold path

**Source:** `.research/librarification/20-graph-over-ir.md` · **Planes:** REGISTRY + INDEX · **Crate:** graph-cold

### 9.1 Verdict

The live `GraphStore` trait is small and single-hop (`get_occurrences`, `get_references`, `are_related`); multi-hop exploration is pure `expand_via` over `outgoing_edges`. The Terminus adapter queries a `Relation(from, kind, to)` model keyed by SymbolId, while the compiler's `GraphCorpus` stores IRI-linked Symbol fields plus reified Reference/Implementation documents. The cold path unifies them: **lower GraphCorpus into the same edge multiset** MemoryGraph already uses in tests, stamping SymbolIds with the instance token and versioned PackageId at lower time (never None in a ColdNode).

**Default representation:** IR (+ OccurrenceSet) stays in CAS; **re-project with `from_ir::project` on cache miss**; build compact forward/reverse indexes (`by_symbol_id`, `by_uri`, `out`, `rev_occurrence`, `rev_reference`). No persisted full-GraphCorpus postcard; an optional lean ColdGraphBlob (edges only) may come later for warm-not-hot large packages. Terminus is never the cold source of truth (GD-8, GD-25).

### 9.2 Design

```rust
pub trait PackageGraph {           // full API (GD-3); client sees GraphOps subset
    fn outgoing_edges(&self, id: &SymbolId) -> …;
    fn expand(&self, seed: &SymbolId, via: &[RelationKind], depth: u8) -> GraphSlice;
    fn are_related(&self, a: &SymbolId, b: &SymbolId) -> bool;
    fn membership_diff(&self, other_gen: &…) -> MembershipDiff;   // uri/FQ set-diff
}
pub enum RelationKind { Member, Reference, Occurrence, Implements, Extends, ReExport }
```

Edge lowerer (shared by cold and hot publish — Relation parity): `member_of` → parent-Member-child; `implements`/`extends` edges; `mentions ∪ takes ∪ returns` → Occurrence; corpus references → Reference. Identity: graph `uri` is version-agnostic; SymbolId is not — cross-version ops use uri/FQ set-diff and `resolution::diff`, **never SymbolId equality**. Stubs under `~extern` and external deps are first-class nodes; **dep IR loading is policy-gated** (`DepLoadPolicy::StubsOnly` default; OnDemand only with hard caps) so expand cannot crawl the universe.

**Caching:** StampedeCache/moka keyed `(PackageId, ir_hash, occ_hash, instance_token)`; byte budgets ≈ 256 MB desktop / 2–4 GB server; single-flight builds. Cold queries **must** feed the §10 leaky-bucket scorer from day one. Demotion pins the cold graph before hot teardown; hot-probe failure fails over to cold. Degraded parity accepted: without OccurrenceSet, Reference edges thin out but the API contract holds. No WOQL/GraphQL reimplementation on cold.

### 9.3 Implementation steps

- **S9.1** Move `graph/{model,from_ir,link}` → graph-cold (= S1.4); define `PackageGraph` + edge lowerer. **S9.2** `IrBlobPackageGraph::load_view` (blobs → project → indexes) + `GraphView`. **S9.3** moka/StampedeCache + budgets + single-flight. **S9.4** index-service serves graph reads always-cold (before tiering). **S9.5** occurrences in manifest (= S3.2) for Reference parity. **S9.6** query-stat emission → §10 scorer. **S9.7** hot Relation parity: promotion publishes lowerer edges (with §10). *Acceptance:* generalize existing `graph_expansion` tests to run against GraphView; parity test cold-vs-MemoryGraph on fixtures.

---

## §10 Terminus hot tier & admission

**Source:** `.research/librarification/07-terminus-tiering.md` · **Plane:** INDEX · **Crate:** terminus-client

### 10.1 TerminusDB 2026 verdict

Alive under DFRNT stewardship: v12.0.6 (2026-06-24); v12 line brought rationals, auto-optimizer, `sys:JSON`, range/interval WOQL, diff + streaming history, `@shared` cascade. Apache-2.0, Prolog core (68%), 3.3k stars, tiny issue tracker. **Unique fit:** the only production document-graph store with immutable versioning + semantic JSON diff/patch — matching lineage needs. Risks: Prolog ops opacity, weak community signal, and crates.io `terminus-store` 0.21.5 (2024-03) **two years behind** the server — banned from production (GD-13). Bet on it **behind an HTTP boundary** with the cold path as the hard fallback so demotion is always safe. Write cost ≈ O(Δ triples) per commit — ideal for incremental symbol publish; read cost ≈ O(layer depth) until rollup (schedule rollups on hot packages).

### 10.2 Tiering design

- `StoreLinks.graph` already models "is this generation materialized in Terminus" — admission control owns that bit.
- **Admission = leaky bucket per package** (`PackageTierManager`, INDEX): cold-path graph queries (§9) drip tokens in; the bucket leaks at a configured rate; sustained demand crosses the promotion threshold. A count-min-sketch doorkeeper filters one-hit wonders before buckets exist. governor stays for rate limits but is not the promotion policy.
- **Promotion:** materialize generation documents + lowerer edges (Relation parity, S9.7) + Auto lineage edges (§6) into Terminus; flip StoreLinks.graph.
- **Demotion:** pin cold graph (§9.3) → verify cold serves → clear bit → delete branch/db per retention.
- **Incremental publish:** only SymbolDelta-changed documents write per generation (Terminus diff/patch); lineage recoverable from commit history (hot) and IR-level edges + `PackageVersion.declares` set-diff (both tiers).
- `TieredPackageGraph` (router: hot if admitted+healthy, else cold) lives in index-service; terminus-client stays a thin typed HTTP client (vendored `terminusdb_schema` from ParapluOU/terminusdb-rs).

### 10.3 Implementation steps

- **S10.1** terminus-client crate extraction (typed docs, WOQL client, health probe). **S10.2** PackageTierManager: leaky bucket + CMS + sqlite admission state; consume S9.6 stats. **S10.3** TieredPackageGraph router + failover. **S10.4** Promotion/demotion jobs (outbox-driven); Relation-parity publish (= S9.7). **S10.5** Lineage Auto-edge publish for hot packages (needs S6.4+). **S10.6** Rollup scheduling for hot packages. *Acceptance:* promotion happens under synthetic sustained load and never under one-shot load; demotion is loss-free (cold answers equal hot answers on the parity suite).

---

## §11 Text search: Tantivy multi-language

**Source:** `.research/librarification/10-tantivy.md` · **Planes:** INDEX + REGISTRY · **Crate:** text-search

### 11.1 Verdict

The existing identifier tokenizer (`registry/runtime/text/tokenizer.rs`) and tiered symbol query (exact STRING → regex contains → subtoken AND) are the right foundation. Gaps: schema too thin (no signature field, no path-prefix facet, no prefix-ngram autocomplete, no popularity/quality fast fields); no per-language analyzer; ranking fusion is package-shaped (download bubbles) and unapplied to symbols; version lag (pin 0.22 vs 0.26.1 current).

### 11.2 Design

- **One shared schema** for all languages + `ecosystem` STRING filter + facet path `/lang/{eco}`; fields added: signature terms, path-prefix facet, prefix-ngram autocomplete field, popularity/quality fast fields.
- **`LanguageAnalyzer` trait** (text-search): path separators (`::` vs `.` vs `/`), signature term extraction, query rewrite, kind priors and boost tables per language, over the shared subword core (`IdentTokenizer`).
- **Ranking:** symbol-side fusion = multi-field BM25 + exact-match bonus + kind prior; package download-bubble fusion stays in index-service (GD-21).
- **Version:** upgrade 0.22 → **0.26.1** (min 0.24.2) on a one-shot reindex. Format compatibility: 0.24+ reads 0.22/0.21 segments; older readers must never open newer segments. Treat index format as a deployed artifact: schema-version marker file beside the index; mismatch forces rebuild (derived indexes are disposable, GD-14). Tokenizer registration is not on disk — both planes must `register()` identical analyzer chains on every open.
- Adoption wins in 0.24–0.26: CompactDoc (desktop memory), lazy scorers (as-you-type latency), string fast-field TopDocs ordering, `minimum_number_should_match`, NgramTokenizer `prefix_only` autocomplete, PhrasePrefixQuery, stemmer behind feature flag (smaller GUI binary), configurable merge threads (desktop vs server).
- DepSet scoping: `TextSearch::search(q, page, dep: &DepSet)` filters to generation-pinned membership on both planes (§16).

### 11.3 Implementation steps

- **S11.1** Extract text-search crate (= S1.3) with existing schema/tokenizer; both planes consume it. **S11.2** tantivy 0.26.1 upgrade + marker file + one-shot reindex path. **S11.3** Schema v2: signature/facet/autocomplete/fast fields (reindex #2, batched with S11.2 where possible). **S11.4** LanguageAnalyzer trait + Rust/TS/Python/Go impls (Java/C#/Nix minimal). **S11.5** Symbol ranking fusion + kind priors; kill download bubble on symbol queries. **S11.6** SymbolDelta-driven upsert/delete Stage (with S7.5). *Acceptance:* relevance regression suite over fixture corpora per language; identical results local vs remote for identical DepSets.

---

## §12 Vector search & local embeddings

**Sources:** `.research/librarification/09-vector.md` (store) · **`09b-retrieval-pipeline-plan.md`** (retrieval quality, dual-tier models, incrementality §16, memory §17, HNSW/rerank §18, GPU §20) · **Planes:** REGISTRY (embedded) + INDEX (remote) · **Crates:** vector-local, vector-remote

### 12.1 Verdict

- **Embedded winner: qdrant-edge 0.7.2** (Apache-2.0, published 2026-06-01) — true in-process Rust, disk-resident, "SQLite for vector search", same filter/payload model as the remote Qdrant server already wired (qdrant-client 1.18). Runner-up (feature-gated): LanceDB 0.31.0. Rejected: sidecar qdrant binary as primary, bare instant-distance (stale), raw HNSW without payload store.
- **Local model (parity):** `jinaai/jina-embeddings-v2-base-code` — 768-d, 137M params, Apache-2.0, 8K context — via fastembed 5.17.x + ort (**CPU required**; CoreML/CUDA EP optional accelerate — 09b §20).
- **Remote premium (optional collection):** `voyage-code-3` at 1024-d MRL + int8/binary — **host only**, never fused into local scores; no Edge snapshot of Voyage space (09b §0 / §4.3).
- **Compatibility policy:** **parity path** = same model + dimension + distance metric local and remote (single-index identity for Jina). **Premium path** = explicit second versioned collection. Dual-index for model upgrades remains the cutover pattern.
- **Retrieval composition:** Stage-1 dense HNSW (named vector `sym`) ⊕ Tantivy BM25 via RRF; optional Stage-2 ColBERT MaxSim (**HNSW m=0** on multivector field) for Deep mode only (09b §18).
- **Memory:** desktop **on_disk always**; **scalar int8** when N ≥ ~50k (TurboQuant bits4 = Phase E eval); binary not default for 768-d Jina; corpus tier = project + hot deps only (09b §17).

### 12.2 Design

- `VectorStore` (low-level: upsert/delete/search/get/count/flush/compact) with `QdrantEdgeLocal`, `QdrantRemote`, `LanceLocal` impls; `VectorSearch` (app-level, SemanticGate-gated) in client-core (GD-4).
- Layout: one Edge shard per workspace project (mutable first-party + optional immutable dep snapshot from **Jina** host only); payload fields `language`, `package`, `kind`, `symbol_id` participate in ANN filtering (not post-filter). WAL/transactional durability (desktops force-quit). Host multitenancy: `package_id` `is_tenant=true`, prefer `m=0` + `payload_m` for per-package graphs.
- **IR → text:** deterministic `EmbedTextBuilder` recipe `nudox.embedtext.v2` (09b §3); unit of embedding = sealed **symbol**, not file chunks or occurrences.
- `EmbedStage` implements §7's Stage: **`input_digest = embed_key`** (09b §3.3 — recipe + model + facet part-hashes, not coarse content_hash alone); `tool_digest` = model+revision+dims+recipe fingerprint — bumping the model invalidates globally by design. Re-embed skip across lineage edges only at confidence ≥ 0.95 (GD-7) **and** matching embed_key.
- **Incrementality (GD-28):** EmbedStage consumes **only** `SymbolDelta.added ∪ changed` on sealed generations; unchanged symbols never invoke the embedder; dirty WT never durable-embeds (09b §16). Part-hash matrix: docs-only re-embeds `sym` but skips graph when refs unchanged.
- Hard requirements honored: upsert/delete by stable symbol id (no zombie hits), 10⁴–10⁶ vector build times acceptable, bounded RAM, crash safety, trait spanning remote+local.

### 12.3 Implementation steps

- **S12.1** vector-remote: extract current qdrant-client path behind `VectorStore` (Jina parity collection). **S12.2** vector-local: qdrant-edge store + collection lifecycle + **on_disk + scalar quant ladder**. **S12.3** fastembed+ort embed worker (dedicated thread; batch API; CPU default; CoreML feature later). **S12.4** `EmbedTextBuilder` + frozen `embed_key` + EmbedStage **SymbolDelta-only** wiring (with S7.5); acceptance = one-symbol doc edit → exactly one embed. **S12.5** SemanticGate + Routed (prefer remote until local Ready). **S12.6** Optional `lance` feature. **S12.7** (Phase B) Voyage premium collection + quality_mode. **S12.8** (Phase C) named multi-rep `sig`/`body`. **S12.9** (Phase D) optional ColBERT Deep rerank. *Acceptance:* local/remote Jina parity harness; RAM ceiling at scale; kill -9 durability; incremental correctness suite (09b §11 / §16.4).
