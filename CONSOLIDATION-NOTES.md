# CONSOLIDATION-NOTES.md — the dedup/merge working map

**Status:** live working notes for the great compression (28 cargo crates → ~6 backend + IR + GUI).
**Owner:** driving refactor. Update as files are read, moved, merged, or deleted.
**Normative plans:** `INDEX-PLAN.md` (master), `REGISTRYLESS-PLAN.md`, `LIBRARIFICATION-PLAN.md` (dated).

Rules for the target structure (from the user):
- Clean, heavily module-scoped. `registry::error`, not a `RegistryError` type name; `foo::Error` over `FooError`.
- Names **fully qualified, no abbreviations** throughout.
- Clean functional patterns, low allocation.
- Abuse files/folders for scoping & compartmentalization.
- Ruthlessly prune / split / merge — keep functionality, drop duplication.

---

## 0. Current state (measured 2026-07-20)

26 cargo crates in `workspace/` (+ Buck2-only `compiler` 73.8k LOC / 220 files, not a cargo member; + `vendor/`).

| crate | LOC | files | role (current) |
|---|--:|--:|---|
| ecosystem | 11191 | 37 | per-eco specs, `Language`, name/version/repo parse |
| heart | 6423 | 56 | shared domain vocab; cache/cas/telemetry folded in |
| index | 9877 | 54 | catalog (schema v4, MetaStore, outbox, migrations) |
| ingestor | 2748 | 15 | feed + git-monitor process plane |
| ir-stream | 1998 | 7 | guest→host IR streaming protocol |
| ir-sync | 1431 | 5 | IR change sync transport |
| ir | 2753 | 25 | **legacy IR schema? (vs nudox-ir — VERIFY)** |
| nudox-change | 481 | 5 | libpijul change atoms |
| nudox-f1 | 1527 | 4 | frozen F1 key registry + pairing fn (leaf) |
| nudox-ir-archive | 3103 | 9 | serve archive |
| nudox-ir-diff | 2411 | 6 | structural delta projection |
| nudox-ir-manifest | 746 | 5 | manifest |
| nudox-ir-vcs | 14410 | 20 | libpijul change engine binding (largest IR) |
| nudox-ir | 6218 | 18 | IR data model |
| nudox-semver | 3300 | 7 | IR-only semver |
| nudox-sync | 796 | 2 | sync glue |
| object-pack | 4104 | 12 | ObjectPack container (INDEX-PLAN §6) |
| registry | 32668 | 114 | **serving spine (runtime+protocol merged) — main dedup target** |
| rusqdoltlite | 2732 | 12 | vendored DoltLite engine binding |
| server | 13321 | 55 | HTTP surface + serving loops |
| vector-core | 3611 | 12 | shared vector plane |
| vector-local | 4786 | 20 | qdrant-edge client store |
| vector-embed | 2972 | 16 | fastembed/ort CPU runtime |
| vector-remote | 2055 | 5 | qdrant-client + rerank plane |
| compiler/sandbox | — | — | SmolvmCage home (rejoined cargo) |

### Internal dependency edges (backbone)
```
ecosystem  → (leaf)
heart      → ecosystem
index      → heart, rusqdoltlite
ingestor   → index, heart, ecosystem
ir-stream  → heart, nudox-ir, nudox-change, ecosystem
nudox-ir           → nudox-change, ecosystem
nudox-ir-archive   → nudox-change, nudox-ir
nudox-ir-diff      → nudox-change, nudox-ir
nudox-ir-manifest  → nudox-change, heart
nudox-ir-vcs       → nudox-change, nudox-ir, nudox-ir-archive, nudox-ir-diff, nudox-ir-manifest, ir-stream, heart
nudox-semver       → nudox-change, nudox-ir, nudox-ir-diff
nudox-sync         → heart, ir-sync, nudox-ir-vcs, nudox-change, nudox-ir
object-pack → heart
registry   → heart, ecosystem, index, ir, nudox-ir, nudox-change
server     → heart, ecosystem, ir, registry, index, vector-core, vector-local
vector-*   → vector-core, heart
sandbox    → smolvm, heart
```

---

## 1. Target architecture (proposed — derive from INDEX-PLAN §21 + memory)

**Three workspaces. Backend never deps libpijul. IR may dep heart.**

### BACKEND workspace — 6 crates
1. **`ecosystem`** — per-eco specs; `Language`; name/version/repo/purl; followers' spec layer. Leaf.
2. **`heart`** — domain vocabulary: identity, `Query`/`AsOf`/`Page`, `Deployment`, `ObjectPack`, cache/cas, error taxonomy, availability, egress. **Absorbs `object-pack`** (heart-only dep, small).
3. **`index`** — catalog plane. **Absorbs `rusqdoltlite`** (thin engine binding). schema v4, MetaStore, outbox, migrations, CatalogOp, aliases/lineage/feed_watermarks.
4. **`registry`** — serving library. **Absorbs the 4 `vector-*` crates** (→ `registry::vector` + features per ID-23) and any remaining `ir` reflection glue. search/ranking/graph(Trustfall)/reflections/handlers-logic.
5. **`ingestor`** — feeds + grit GitMonitor + advisories. Separate process (IND-7).
6. **`server`** — the binary. http router, serving loops, config, deployment wiring.

Open: **`compiler/sandbox`** (SmolvmCage) + compile plane — compiler is Buck2-only; sandbox may be a 7th cargo crate or live in the compiler tree. FLAG for decision.

### IR workspace — collapse ~12 crates → ~3
- **`ir`** = merge `nudox-f1` + `nudox-change` + `nudox-ir` (+ retire legacy `ir` if dead). Data model + F1 + change atoms.
- **`ir-vcs`** = merge `nudox-ir-archive` + `nudox-ir-diff` + `nudox-ir-manifest` + `nudox-ir-vcs` + `nudox-semver`. libpijul-linked VCS/serve/diff/semver.
- **`ir-sync`** = merge `ir-stream` + `ir-sync` + `nudox-sync`. streaming + sync transport.

### GUI workspace
- **`gui`** + a thin **`client`** crate (heart wire types only).

---

## 2. Per-file inventory (filled from exploration — the dedup reference)

> Sections below are populated crate-by-crate. `[x]` = confirmed dead/duplicate, `[→]` = move target.

### 2.1 `ecosystem` (11.2k LOC, 37 files) — CLEAN, plan-aligned, minor dedup only

Core: `lib.rs` sealed spec trait + dispatch + object-safe erasure; `language.rs` `Language` enum (Rust/TS/Python/Go/Java/Nix/CSharp/Cpp); `archive.rs` `ArchiveKind`; `policy.rs` `UpstreamPolicy`; `manifest.rs` `ManifestCandidate`+`ExtractedFacts`; `name.rs` `StructuredName`; `upstream.rs` endpoints/`ListedVersion`; `search.rs` `SearchNorms`; `repo.rs` `RepoSlug`+`normalize_repo_url` (526 loc, handles https/ssh/scp/npm/bare); `version.rs` `VersionGrammar` + 6 grammars + `AnyVersion` (1414 loc).
Per-eco: `rust.rs` `ts.rs` `python.rs` `go.rs` `java.rs` `csharp.rs` `nix.rs`.
Cpp (registryless): `cpp.rs` + `cpp/{alias,name,interop,listing,version}.rs` + `cpp/manifest/{mod,cmake,conan,meson,vcpkg,pkgconfig,gitmodules,modulebazel}.rs`.
Tests: `name_tests.rs` (982 loc, root — inconsistent w/ inline `mod tests` elsewhere).

Dedup flags (minor, low priority): default `parse_download_count → None` in trait (Go/Java/Cpp/Nix identical); shared manifest URL/license field extraction helper (CSharp/Go/Java/Python repeat); PEP508/semver-prerelease compare duplicated (Maven qualifier vs Go); 7 cpp/manifest parsers share a scan skeleton (macro candidate); `name_tests.rs` 982-loc root file → inline. **Verdict: keep as-is for now; it's the leaf and already matches ECOSYSTEM-PLAN. Revisit micro-dedup after the big merges.**

---

### 2.2 `heart` (6.4k, 56 files) — CLEAN vocabulary crate, no logic-bloat

Root: `symbol` `health` `tenant` `version`(443 — folded version crate, resolve_from_tags) `sink` `query`(302 — `Query`/`Target`/`Scope`/`RankSpecification`) `deployment` `egress`(362 — G-10 guard) `cursor`(342) `score` `progress` `availability`(210) `ecosystem`(Edition/Toolchain) `search`(`Page`) `object_pack`(187 — vocab: `ObjectPackId`/`MemberKey`/`VersionProvenance`) `connection`(Cold/Live) `content`(ContentHash/JobKey).
Submodules: `cache/`(CAS trait + memory/disk/jitter/single_flight/stampede/tiered — folded caching+cas crates, clean), `identity/`(Id/Package/PackageId/SymbolId/derive), `error/`(StoreError/Retryable/Failure/connect), `access/`(Source/Federation), `package/`(PackageName+coordinates), `telemetry/`(feature-gated OTEL/prom/pyroscope), `tests/`.
**Verdict: keep pure. No dead code, no `FooError` orphans. Naming clean.**

### 2.3 `object-pack` (4.1k, 12 files) — cohesive binary container; NOT pure vocab
`lib` `builder` `reader` `format`(NDPK header/TOC/postcard) `outboard`(Bao) `tree` `transport`(iroh provide/fetch) `error`; `store/`(trait+filesystem); `tests/`(transport+adversarial). heart already owns the *vocabulary* (`heart::object_pack`); this crate is the *implementation*. Deps only heart. Consumed by registry(snippet range-get)+ingestor(seal)+server.
**Move: → fold as `index::object_pack` (persistence plane) OR keep standalone. DECISION NEEDED (§5).**

### 2.4 `rusqdoltlite` (2.7k, 12 files) — thin DoltLite C-FFI binding
`lib` `build.rs`(C amalgamation) `connection` `dolt`(commit/branch/checkout/merge/log/gc) `dolt_types` `error` `row` `statement` `value` `sys`(private FFI) `transaction`; `tests/engine`. Only `index` consumes it.
**Move: → fold as `index::engine` (INDEX-PLAN ID-20 "index::engine wraps all dolt_*"). Clean fold.**

---

### 2.5 `index` (9.9k, 54 files) — COMPLETE schema v4, clean facade
Root: `lib` `protocol`(CatalogOp + v5 alias/lineage upserts) `enums`(total TextEnum codecs) `ids`(BLOB16/32) `resolution`(EDB→IDB alias binding) `codec` `overlays` `seed_models`(system/* stems).
`tables/`(22 tables: packages/versions/edges/stores/locations/outbox/generations/advisories/aliases/lineage/repo_facts/listing/facets/overlays/git_watermarks/feed_watermarks/sink_watermarks/compile_cache/symbols_proj/popularity/edgepack + mod validator).
`engine/`(CatalogEngine+VersioningEngine trait; `dolt.rs` real binding; `memory.rs` test fake). `store/`(MetaStore/apply/read/writer/follower/lifecycle). `migrations/`(ddl/runner + pre-migrate branch). `scratch/`(jobs/wanted/sessions/claims). `tests/`.
Flags: table modules repeat INSERT_COLUMNS+bind+from_row ×22 (derive-macro candidate, non-blocking); no watermark/listing read API in `Catalog` (deferred, ingestor uses own trait). **`rusqdoltlite` folds → `index::engine::dolt` backing.**

### 2.6 `ingestor` (2.7k, 15 files) — CLEAN, library-first
`lib` `main`(stub) `follower` `git`(subprocess hardened) `grit`(in-proc default) `monitor`(GitMonitor) `enumerate`(pure) `homebrew` `advisory` `driver`(batch apply+heartbeat) `transport` `watermark`; `tests/`. Stays a separate process/crate (IND-7). No dedup needed.

### 2.7 IR plane (12 crates) — legacy `/ir` is DEAD; aggressive 12→3 merge
- **`ir` (LEGACY, 2.7k, 25 files) — DELETE.** Old JSON schema types (entry/function/generics/kind/module/parameter/primitives/protocols/record/syntax/ty). No Cargo.toml references anywhere → superseded by `nudox-ir`. **But `registry`+`server` currently dep `ir` — VERIFY those are migratable to nudox-ir before delete (they may use old `ir::Index` for graph/surface).**
- `nudox-ir` (6.2k): arena IR core — apply/body/body_wire/builder/entry/index/intro/kind/reflect/registry/skeleton/symbol/view/vocab/wire. libpijul-free.
- `nudox-change` (481): identity vocab (LinkDomainKey/hash/ids). leaf.
- `nudox-f1` (1.5k): frozen F1 key registry + line pairing. leaf, no nudox deps.
- `nudox-ir-manifest` (746): BlobManifestV3 + GenerationStamp + outbox staging.
- `nudox-ir-archive` (3.1k): sealed mmap PackageArchive (header/index/seal/section/view). libpijul-free.
- `nudox-ir-diff` (2.4k): matcher-free structural delta (delta/diff/ir_op/apply). libpijul-free.
- `nudox-ir-vcs` (14.4k, largest): libpijul-backed IrRepository — repo/blob/checkpoint/continuity/f1/session/stream/subst/serve_cache/refs. **deps libpijul.**
- `nudox-semver` (3.3k): API-surface projection + semver classify (surface/classify/report/packs). deps nudox-ir+diff.
- `ir-stream` (2k): guest→host streaming protocol (frame/io/receiver/sink). K27.
- `ir-sync` (1.4k): iroh change distribution (io/transport/types).
- `nudox-sync` (796): FsChangeIo repo glue over ir-sync. **deps libpijul.**

**TARGET (aggressive, 12→3):**
- **`ir`** ← nudox-change + nudox-f1 + nudox-ir + nudox-ir-manifest → `ir::{change,f1,model,manifest}`. libpijul-free. (This is what BACKEND deps.)
- **`ir-vcs`** ← nudox-ir-archive + nudox-ir-diff + nudox-ir-vcs + nudox-semver → `ir_vcs::{archive,diff,repo,semver}`. libpijul-linked.
- **`ir-sync`** ← ir-stream + ir-sync + nudox-sync → `ir_sync::{stream,transport,repo_glue}`. libpijul (via nudox-sync).
Only `ir-vcs` + `ir-sync` touch libpijul; backend deps only merged `ir`. ✓ topology law.
Error naming already clean across all (`ArchiveError`/`VcsError`/`StreamError`…) — will re-scope to `archive::Error` etc. during merge.

---

## 3. Duplication / dead-code kill list

- **[DELETE] legacy `/ir` crate** — once registry/server migrated off `ir::*` to `nudox-ir`. HIGH VALUE (removes a whole duplicate IR model).
- **[FOLD] `rusqdoltlite` → `index::engine`** — only index consumes it.
- **[FOLD] object-pack → decision §5.**

### 2.8 `registry` (32.7k, 114 files) — THE dedup target; ~40% is dead legacy
Live serving core (KEEP, re-scope): `blob/` (manifest/creation/emit), `compiled/` (IR cache), `metadata/` (rich/heuristics/facets), `resolve/` (version selection), `search/` (24 files, 8.7k — the real search plane: pipeline/structured/tantivy/ranking/policy/intent/interleave/popularity/listing_signals/rrf/gates/squat/entity/enrich/dependents/usages/spell/alias/eval), `graph/` (trustfall_adapter + reverse_index — the LIVE §5.5 graph), `upstream/` (crates/nuget pollers — overlaps ingestor?), `runtime/{text,session,pagination}` (tantivy text index, session semilattice).
**DEAD LEGACY (delete — replaced by new crates):**
- `index/mod.rs` (567) + `queue/mod.rs` (817) + `schema/` (680) + `coordination.rs` (316) + `store.rs`/`persist.rs` — **the whole Postgres write-plane; INDEX-PLAN §14 → replaced by `index` crate + scratch.** ~2.7k loc dead.
- `runtime/graph/` (mod 730 + expansion/resolution/structure 218) — **DEAD Terminus HTTP client**; replaced by `graph/` trustfall. ~950 loc dead.
- `runtime/vector/` (mod 520 + embedding/cache/gate/similarity/model 626) — **legacy vector; ID-23 → vector-* crates.** ~1.1k loc dead.
- `protocol.rs` (210) — compiler-daemon wire (may move to compile plane).
- `local_enrichment.rs` (781), `spell.rs` (569) — "live-dead" (present, unwired). Defer/gate or cut.
Flags: Query DTOs `PackageSearchRequest`/`PackageSearchDeps`/`StructuredQuery` → unify on `heart::Query` (§9). 18+ error enums in `error.rs`(635)+`runtime/error.rs`(488) → re-scope to module `error`s. Flat `search/` 24 files → `search/ranking/` subfolder. `upstream/` pollers likely belong in `ingestor` (feeds), not the serving lib.

### 2.9 `server` (13.3k, 55 files) — mostly CLEAN, Query already on heart::Query
`lib`(602 federation assembly) `main` `error`(377) `config`(757) `compiler_client` `rerank` `authz`(302 capabilities) `poll`(741 background loops); `coordination/`(health/init/indexing(982)/packages/search); `search/`(query/planner/registry/routing/symbols + semantic/embedder); `http/`(router/dto(470) + handlers ×7); `save/`(rebuild) `bakery/`(861 edgepack). Tests heavy.
Flags: `coordination/mod.rs` "Postgres-as-WAL" stale comment; `dto.rs:248 bad_request()` dead stub; server `search/` overlaps registry `search/` (server routes → registry pipeline) — thin, keep as routing. Deps: heart/ecosystem/ir/registry/index/vector-core/vector-local.

### 2.10 vector plane (4 crates, 13.4k) — collapse 4→1 `registry::vector` (feature-gated)
- `vector-core` (3.6k): pure shared — model/embedding/embed/key/recipe/shard/store/quant/routing/admission/fusion. No I/O.
- `vector-local` (4.8k): qdrant-edge embedded store (actor/shard/store/fanout/hotset/depshard/pack/lock/compact) + heavy adversarial tests.
- `vector-embed` (3.0k): fastembed/ort runtime (gate/scheduler/stage/weights/mock; `onnx` feature: runtime/tokens).
- `vector-remote` (2.1k): qdrant-client + Voyage + rerank + hedged.
Inventory-agent argued "can't merge" — but that conflates *crates* with *concerns*. Concerns become **modules + feature flags** in one crate: `vector::{core,local,remote,embed}` with features `local`/`remote`/`onnx` gating the heavy deps (ort/fastembed/qdrant-client/qdrant-edge). Matches ID-23 "vector-core + registry features." **Target: fold all 4 → `registry::vector` (features), OR one standalone `vector` crate — decision §5.**

### 2.11 `compiler/sandbox` (7.6k) — SmolvmCage; the compile/forge plane
`cage`/`smolvm`(602)/`smolvm_backend`(936)/`vm`(814)/`worker`(671)/`job`/`budget`/`seal`/`spec`/`limits`/`probe`/`profiles`/`overrides`/`observer`/`cgroup`/`node`/`cancel`/`error`; `backend/supervisor`; `toolchains`+`images`(711). The parent `compiler/` tree (generate/compile/render/treesitter/graph, 73k loc) is **Buck2-only, NOT a cargo member**. sandbox is its cargo-buildable isolation layer.
**Home decision §5:** own crate in a "compile plane" vs fold into server. Not one of the 6 serving crates.

---

## 3. Duplication / dead-code kill list (finalized)

**Tier 1 — delete whole dead subsystems (highest value):**
1. `registry/{index,queue,schema}/` + `coordination.rs` + `store.rs` + `persist.rs` — dead Postgres write-plane (~2.7k+ loc). Replaced by `index` crate.
2. `registry/runtime/graph/` — dead Terminus (~950 loc). Replaced by `registry/graph/` trustfall.
3. `registry/runtime/vector/` — legacy vector (~1.1k loc). Replaced by vector plane.
4. legacy `ir/` crate (2.7k) — replaced by `nudox-ir`.

**Tier 2 — fold crates into module scopes:**
5. `rusqdoltlite` → `index::engine` backing.
6. 4 vector crates → one `vector` (features) → `registry::vector`.
7. IR: 12 crates → 3 (`ir`, `ir-vcs`, `ir-sync`) per §2.7.
8. `object-pack` → `index` (persistence plane) [proposed].
9. `registry/upstream/` pollers → `ingestor` (they're feeds).

**Tier 3 — de-scatter within surviving crates:**
10. Unify Query DTOs → `heart::Query`; drop `PackageSearchRequest`/`PackageSearchDeps`.
11. Re-scope `FooError`/`RuntimeError`/`RegistryError` → per-module `error`.
12. `registry/search/` flat 24 files → `search/ranking/` subgroup.
13. Cut/gate `local_enrichment.rs`, `spell.rs`, `dto.rs:bad_request()`.

---

## 4a. CORRECTION (verified in server/lib.rs + registry Cargo.toml) — read this FIRST

My first-pass "dead Postgres write-plane" claim was **WRONG**. Ground truth:
- **Postgres is already fully removed.** `registry/Cargo.toml` has no sqlx/postgres/deadpool. `registry::index::GlobalStore<Engine: VersioningEngine>` is generic and instantiated with `index::engine::dolt::DoltEngine`. `registry::coordination::Outbox<Engine>` = catalog-backed (`Outbox::new(writer)`). `registry::queue::Queue` = `index::scratch::ScratchStore`-backed. **These modules are LIVE and CORRECT — do NOT delete.** (registry/index/mod.rs:66 comment: "The old postgres Cold/Live typestate is gone.")
- **Terminus is still LIVE in the serving path.** `SourceStores.graph: Graph<Live>` is `.connect()`-ed at assembly; `server/search/mod.rs:136-162` calls `graph.{get_occurrences,get_references,are_related,expand}`. The trustfall replacement (`registry::graph::{trustfall_adapter,reverse_index}`) exists but server's `usage_backend` "starts empty" (no in-proc IR materialized). **Removing Terminus = losing occurrences/references/expand until IR-materialization is wired. This is a functional migration, not a delete. HIGH RISK this session.**
- **Vector = the two-architecture fork.** Old `registry::runtime::vector::{Semantic<M,Live>, EmbeddingModel, SemanticGate, EmbeddingCache}` is the connected semantic store + the `M` brand every server file carries. New `vector-core/local/embed/remote` are only partially adopted (`server/bakery` uses `vector_local::{shard,upsert_raw,pack_shard}`; `server.semantics()` is never called). Folding vector→registry::vector must RECONCILE the two, providing `EmbeddingModel`/`SemanticGate`/qdrant-store from the new plane, then repoint server. Medium-high risk.

**Revised safe/risky split for "registry purge":**
- SAFE now: fold `vector-*` → `registry::vector` structurally; delete truly-unwired bits (`local_enrichment`/`spell` IF unreferenced, `dto:bad_request`, stale comments); re-scope `search/`→`search/ranking/`; per-module error renames.
- RISKY (needs its own focused pass, keeps functionality): Terminus→trustfall (needs IR materialization populated first); old-vector→new-vector reconciliation.
- The genuinely-safe BIG win remains the **IR merge (12→3)** — self-contained, no server-runtime risk.

## 4b. (superseded by 4a) legacy registry modules half-migrated

The Tier-1 "dead" modules are **live imports from `server`** — deletion requires migration first:
- `server/{poll,save/blobs,coordination/indexing}.rs` use `registry::coordination::{Outbox,OutboxEntry,OutboxOp,SinkKind,OutboxSeq}` and `registry::queue::LeasedJob` — the OLD Postgres outbox/queue. New `index` crate (already dep'd, `dolt-engine`) has the replacement outbox/scratch-jobs. → **migrate server to index::outbox + scratch, then delete registry Postgres plane.**
- `server` imports `registry::runtime::vector::{EmbeddingModel, SemanticGate, Embedder, EmbedRole, Embedding, ModelId, ...}` in ~15 files (every handler pulls `EmbeddingModel`). The vector-* crates are dep'd but the *interface* still comes from registry::runtime::vector. → **repoint server to the `vector` crate's `EmbeddingModel`/`SemanticGate`, then delete registry::runtime::vector.**
- `server/config.rs` error enum references `registry::runtime::graph::GraphNameError` (Terminus). → **drop Terminus config variants, delete registry::runtime::graph.**

This is the "two-architecture fork" from memory: compiler/registry duplicate capabilities the new crates provide. The refactor = **finish the migration, then compress.** Order matters: migrate consumers → delete legacy → merge crates.

## 5. Open structural decisions (need confirm before big moves)

6 BACKEND crates: `ecosystem`, `heart`, `index`, `registry`, `ingestor`, `server`. + `sandbox` (compile plane, outside the 6).
**Decisions LOCKED (2026-07-20):**
- object-pack **→ `index::object_pack`** (persistence plane).
- vector 4 crates **→ `registry::vector`** (features `local`/`remote`/`onnx`).
- sandbox **→ own `sandbox` crate** outside the 6.
- `registry/upstream/` pollers **→ `ingestor`**.
- **Session order: registry legacy purge FIRST** (migrate server → index outbox/scratch + vector; delete Postgres/Terminus/legacy-vector; backend build green).

## 6. Target module layout (post-consolidation, illustrative)

```
BACKEND workspace
 ecosystem/   (leaf; unchanged)
 heart/       (pure vocab; unchanged)
 index/       engine/(←rusqdoltlite) tables/ store/ migrations/ scratch/ outbox/ object_pack/(←object-pack)
 registry/    search/{ranking/,…} graph/ blob/ compiled/ metadata/ resolve/ text/ session/ vector/(←4 crates) reflect/
 ingestor/    followers/(homebrew,vcpkg,conan,crates,nuget←registry/upstream) git/ monitor/ driver/
 server/      http/{router,dto,handlers/} coordination/ search/ poll bakery/ save/ authz config
 sandbox/     (compile plane, outside the 6)

IR workspace
 ir/          change/ f1/ model/ manifest/           (libpijul-free; backend deps this)
 ir-vcs/      archive/ diff/ repo/ semver/            (libpijul-linked)
 ir-sync/     stream/ transport/ repo_glue/

GUI workspace
 gui/ + client/ (heart wire types)
```

---

## 4. Execution log

- 2026-07-20: measured scale, mapped dep edges, launched 6 inventory explorers, drafted target arch.
