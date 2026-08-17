# docs/CONSOLIDATION-NOTES.md — the dedup/merge working map

**Status:** live working notes for the great compression (28 cargo crates → ~6 backend + IR + GUI).
**Owner:** driving refactor. Update as files are read, moved, merged, or deleted.
**Normative plans:** `docs/INDEX-PLAN.md` (master), `docs/REGISTRYLESS-PLAN.md`, `docs/LIBRARIFICATION-PLAN.md` (dated).

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

> **⚠ SUPERSEDED by §7 (2026-07-20 user redirect).** The crate set below (6 backend
> crates incl. `server`; vector→`registry::vector`; object-pack→`index`) is the OLD
> target. Read §7 for the current one. Kept for history/rationale.

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

> **⚠ Decisions here PARTLY SUPERSEDED by §7 (2026-07-20 redirect):** object-pack
> now → sync (not index); vector → one standalone `vector` crate (not registry);
> `server` dissolves into `registry`; rusqdoltlite → `vendor/`; ir-sync → `heart::sync`
> trait + IR-plane impl. The "session order" and sandbox note still hold.

6 BACKEND crates: `ecosystem`, `heart`, `index`, `registry`, `ingestor`, `server`. + `sandbox` (compile plane, outside the 6).
**Decisions LOCKED (2026-07-20):**
- object-pack **→ `index::object_pack`** (persistence plane).
- vector 4 crates **→ `registry::vector`** (features `local`/`remote`/`onnx`).
- sandbox **→ own `sandbox` crate** outside the 6.
- `registry/upstream/` pollers **→ `ingestor`**.
- **Session order: registry legacy purge FIRST** (migrate server → index outbox/scratch + vector; delete Postgres/Terminus/legacy-vector; backend build green).

## 5b. VECTOR full-swap — execution design (no shim; retire `registry::runtime::vector`)

**Fold:** move `vector-core/local/embed/remote` → `registry/vector/{core,local,remote,embed}` (already scripted once); merge their Cargo deps into `registry/Cargo.toml` behind features `vector-local`(qdrant-edge+fs2), `vector-onnx`(fastembed+tokenizers+sha2); core+remote always-on (registry already has qdrant-client). Rewrite `vector_core::`→`crate::vector::core::`, `vector_{local,remote,embed}::`→`crate::vector::{…}::`. Remove 4 members from root Cargo.toml; drop server's vector-* deps.

**Retire old plane** (`registry::runtime::vector`, ~1.1k): delete entirely. It duplicates the new `EmbeddingModel`/`Embedding`/`Embedder`/`EmbedRole`/`EmbeddingPurpose`/`ModelId` — all live in `vector_core`. Drop `E5Small`/`OpenAi3Small` (new plane is sealed to `JinaCodeV2`/`VoyageCode3`); tests move to `JinaCodeV2`.

**Relocate the 2 serving-only pieces (once, onto vector_core types):**
- `SemanticGate` (gate.rs, 63 loc) — pure capability token → `registry::vector::gate` (or server). New `VectorStore::search` takes NO gate; server checks the gate then calls the store.
- `EmbeddingCache`/`EmbeddingKey` (cache.rs, 121 loc) — retarget to `vector_core::{Embedding, ModelId, EmbedRole}` → `registry::vector::cache`.

**Server rewire (the work):**
- `SourceStores.semantics: Semantic<M,Live>` → `RemoteStore<M>` (no Connect typestate; `RemoteStore::new(Arc<Qdrant>, CollectionConfig)` + `ensure_collection(...)` async at assembly). `CollectionConfig` is a FROZEN ENUM (`JinaParity`/`VoyagePremium`) keyed by `M` — so the configurable `qdrant_collection` config field becomes vestigial (drop or keep for compat).
- `poll::materialize_vector`: `SymbolPoint` + `uploader().oneshot()` → `VectorPoint<M>` + `store.upsert(points)`. Map `symbol.id`→`PointId::from_symbol`; **stash `SymbolId` in payload** (need it back on search).
- `poll::delete_vector`: **GAP — `VectorStore` only has `delete(&[PointId])`, no package-scoped filter-delete** (old `delete_package_points` did a qdrant filter delete). → add `delete_by_package`/filter-delete to `RemoteStore` (inherent method) since we own the crate now. Not a shim — a real store capability.
- `search/semantic/mod.rs SemanticSurface`: `store.search(gate, embedding, limit, after)` → gate-check + `RemoteStore::search(SearchRequest{ vector, filter, limit, score_threshold })` → `Vec<SearchHit>`; map `SearchHit{id: PointId, score, payload}` back to `Scored<SymbolId>` (SymbolId from payload). **Pagination model differs** (old `after` cursor vs new limit+threshold) — reconcile (keyset over score+PointId, or fetch-limit).
- `HttpEmbedder<M>` implements old `Embedder` → switch to `vector_core::Embedder`.
- `main.rs`: `EmbedModel` alias → `vector_core::JinaCodeV2`.
- `Server<M: EmbeddingModel>` + all ~19 `registry::runtime::vector::EmbeddingModel` bounds → `registry::vector::core::EmbeddingModel` (= vector_core's). Also un-breaks the parked `vector_adversarial`/`vector_bakery_rerank`/`depshards` test errors.

**Open reconciliations:** PointId↔SymbolId round-trip (payload), semantic pagination cursor, whether `qdrant_collection` config stays. Order: fold crates → relocate gate/cache → rewire server → delete old plane → remove members → build (features on for server).

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
- 2026-07-20: verified reality vs plan (§4a correction); locked 4 boundary decisions.
- 2026-07-20: WIP checkpoint `e8ae24c0`. Launched 2 worktree forks (IR-merge, Terminus-drop).
- 2026-07-20: **Terminus-drop fork STOPPED correctly** — Terminus is load-bearing, no deletions made. Evidence: outbox writes symbols into it (`server/lib.rs:489` spawns `outbox_consumer` for every `DerivedStore` incl. `Graph`; `poll.rs:191` `SinkKind::Graph→materialize_graph`→`stores.graph.insert_symbols`); queried live (`/expand`→`server.expand()`→`self.graph.expand`; `GraphStore` delegates get_occurrences/get_references/are_related to `self.graph`). Replacement not ready: `registry::graph::execute_graph_query` is a typed `Unsupported` stub; `ReversePositionIndex` needs a loaded in-proc `IrView` (none at assembly). **Terminus drop is BLOCKED on: (1) in-proc IR materialization + reverse-index population from the outbox Graph/UsageIndex sink; (2) repoint GraphStore/related_hits to reverse index, 503 only for unloaded scopes. Do NOT delete until then.** Catalog-side rename is done (`SinkKind::Graph→CatalogSinkKind::UsageIndex`); data-plane cutover is not.
- 2026-07-20: **worktree-isolation stale-base bug CONFIRMED still active** — fork worktrees branch from `5a848d65 "library"` (old), NOT the parent checkpoint `e8ae24c0`. Terminus fork worked around via `git reset --hard e8ae24c0`. IR fork must do the same (it inherited the stale-base memory). See [[worktree-isolation-stale-base]].
- 2026-07-20: **SAFE REGISTRY TIDY — DONE & VERIFIED GREEN** (main tree):
  - `registry/search/` 24 flat files → `search/ranking/` subfolder (15 modules moved: cascade[was ranking.rs]/policy/gates/rrf/interleave/popularity/listing_signals/intent/squat/entity/enrich/multi_parent/dependents/local_enrichment/eval). Retrieval stays at root (pipeline/structured/tantivy/alias/usages/spell/health). External paths `registry::search::{squat,gates,listing_signals,local_enrichment}` preserved via re-export; cascade surface re-exported via `pub use cascade::*`.
  - Deleted dead `server/http/dto.rs::bad_request`.
  - Fixed pre-existing stale test `dto.rs` depshard_manifest_serde_shape (EdgepackKey.quant_profile: String→QuantProfile; removed nonexistent `bakery::QUANT_PROFILE`).
  - **`cargo check -p registry` green (16s); `cargo check -p server` (lib) green (25s).** Uncommitted on branch `refactor/consolidation-registry-purge`.
- 2026-07-20: **IR merge integrated** — merged fork branch `325ece7b` (conflict-free; fork had stripped my ranking tidy). 12 IR crates→4 (`nudox-ir` absorbs change/manifest; `nudox-ir-vcs` absorbs archive/diff/semver/ir-stream→`protocol`; `ir-sync` absorbs nudox-sync→`repo_glue`; `nudox-f1` leaf for libpijul fork). Merge commit `680424a`. **`cargo check` green: nudox-ir/nudox-ir-vcs/ir-sync/registry/server.** Legacy `ir` crate deferred (registry/blob+protocol build wire format on `ir::syntax::ResolvedReference`/`NudoxPath::External` which new model kills — semantic wire change, separate pass). ⚠ `.gitignore` line 2 `/**/*` hides untracked assets (e.g. `ecosystem/assets/cpp_alias_seed.ron`) — force-add or fix before it bites.
- 2026-07-20: **Terminus full-send — landmine verified:** `registry::index::TerminusInstance` is a MISNAMED **identity-namespace token** (`{org}/{db}` salts every global `SymbolId` — `registry/identity.rs` + `GlobalStore::mint`), its own doc says "name is historical; no graph database involved." → **KEEP+RENAME `TerminusInstance`→`InstanceToken`**; DELETE only the real graph store `registry::runtime::graph`. server config org/db feeds the token (keep, rename) while terminus URL/user/password are pure graph-store (drop). Old SymbolId graph surface (get_occurrences/references/expand) is superseded by StableRef `Target::Usages` (already wired to `usage_backend`, honest 503 until IR materialized). Footprint ~25 files.
- 2026-07-20: **Terminus full-send DROP — DONE, committed `138e6392`, lib green.** Deleted `registry/runtime/graph` (Terminus/WOQL client + expansion/structure + graph tests). `TerminusInstance`→`InstanceToken` (identity salt, kept). `RelationKind`→`registry::runtime::session`. server: removed `Graph<Live>` from SourceStores/assembly/health; config dropped terminus URL/user/password + default-cred guard, kept org/db as `instance_organization`/`instance_database`; poll graph sink = documented no-op; `related_hits` = honest-empty (IR reverse-index/`Target::Usages` is the surface; IP-7 materialization pending). `cargo check -p registry -p server` green. **Follow-up: test files referencing terminus config / old graph surface need updating** (server/tests/server_config.rs, server/tests/symbol_store.rs, client_surfaces, registry session tests fixed already).
- 2026-07-20: **Vector full-swap PARKED** (was mid-`git mv` when IR completed; moves reverted, trivially redoable). Next: fold vector-core/local/embed/remote → `registry::vector` (features), retire old `registry::runtime::vector`, repoint server to new plane types (`VectorStore`/`VectorPoint`/`SearchRequest`/`RemoteStore`), relocate `SemanticGate`+`EmbeddingCache` once, drop E5Small/OpenAi3Small, remove 4 workspace members. NO shim/redundancy per user.
- 2026-07-20: **Vector server store-rewire — DONE, `cargo check --workspace` GREEN, server lib+tests green.** The §5b store-rewire is complete (the earlier WIP `1d244e85` had left server non-compiling):
  - `SourceStores.semantics: Semantic<M,Live>` → **`vector_remote::store::RemoteStore<M>`**. Construction in `connect_source`: build `Arc<Qdrant>`, `CollectionConfig::for_model::<M>()`, `ensure_collection(...)` (idempotent; *is* the connection check — no `Connect` typestate), `RemoteStore::new`. blobs keep their `Connect`. `qdrant_collection` config field left vestigial (collection is frozen-per-M).
  - New store capabilities added to `vector-remote` (we own it): `CollectionConfig::for_model::<M>()` (sealed Jina→parity / Voyage→premium), `RemoteStore::delete_by_package(&str)` (payload-`package` filter-delete — the trait `delete` is by-`PointId` only), and `impl heart::health::Probeable for RemoteStore` (qdrant `health_check`).
  - `poll::materialize_vector`: `SymbolPoint`+`uploader().oneshot()` → `VectorPoint{ id: PointId::from_symbol, vector, payload }` + `VectorStore::upsert`. `poll::delete_vector`: `delete_package_points` → `delete_by_package(package.uuid)`.
  - **PointId↔SymbolId round-trip**: new `PointId::from_symbol` is a *derived* v5 uuid (not the symbol uuid), so `symbol_id` is stashed in payload; `search/semantic` recovers `SymbolId` from `payload["symbol_id"]`. One shared payload builder: `bakery::symbol_payload` (now `pub(crate)`, keys `language`/`package`/`kind`/`symbol_id`) reused by poll — matches `ensure_collection` indexes.
  - `search/semantic/mod.rs SemanticSurface`: `store.search(gate,…stream…)` → gate-check + `RemoteStore::search(SearchRequest{vector,filter,limit,score_threshold})` → map `SearchHit`→`Scored<SymbolId>`. **Pagination reconciled**: keyset over `(Score, SymbolId)` — `score_threshold` prunes below cursor score server-side, client-side sort (score desc, id asc) + skip-past-cursor + truncate; resume over-fetches capped at `MAX_FETCH=4096`.
  - `embedder.rs HttpEmbedder`: old `Embedder` (model()+3-arg embed w/ purpose) → **`vector_core::Embedder`** (`#[async_trait]`, `runtime()->EmbedRuntimeInfo`, 2-arg `embed(text, role)`; purpose dropped from the embed path). Rich `EmbedRejectionReason` collapsed into `vector_core::EmbedError::Backend(String)`. `EmbeddingCache::get_or_embed` is now 3-arg (no purpose) — `poll`+`bakery` updated.
  - **Error glue**: `registry::runtime::error` gained `VectorError::Store(#[source] vector_core::StoreError)` + `From<vector_core::StoreError>`/`From<vector_core::EmbedError>` for `RuntimeError` so `ServerError::Runtime(e.into())` bridges the new plane. `bakery::BakeryError::Embed` → `#[from] vector_core::EmbedError`.
  - `main.rs EmbedModel` → `registry::vector::JinaCodeV2`; `config.rs` dropped dead `InvalidQdrantCollection(CollectionNameError)`; test harness `TestModel` → `JinaCodeV2`.
  - **Un-parked test binaries** (vector_adversarial/vector_bakery_rerank/common): `models::E5Small`/`OpenAi3Small`→`JinaCodeV2`, `registry::vector::model::*`→`registry::vector::*` + `use vector_core::EmbeddingModel`, dead `BakeryError::Database(sqlx)`→`Io`, `EdgepackKey` typed-field fixups, EdgepackStatus dup import. **Bonus real bug fixed**: `route_stage_one` left `rerank=true` offline (no dense collection, no network reranker) → now gated on `online` (surfaced by un-parking `routing_offline_all_qualities_precise_only`).
  - ⚠ Pre-existing red (NOT mine, `voyage.rs` untouched): `vector-remote` unit test `voyage::tests::token_cap_exact_boundary_math_1188_fits` fails — Voyage chunk-cap math, orthogonal to the store rewire.
  - Still deferred (per §5b/§304): the *structural* fold of the 4 vector-* crates into `registry::vector` + removing the 4 workspace members (server still deps `vector-core/local/remote` directly). The rewire proves the seam; the crate-collapse is a mechanical follow-up.
- DEFERRED (own lanes): broad `FooError`→`module::Error` renames (ripple into server imports); vector 4→1 fold (`server` vector *test* files still on old `registry::runtime::vector::EmbeddingModel` — the two-architecture fork); stale Postgres/Terminus comments in fork-territory files; Terminus drop (blocked, above).

---

## 7. REVISED TARGET (2026-07-20 — user redirect; supersedes §1/§5/§6)

Directive (verbatim intent): *ir-sync = a generic sync crate that lives in heart,
implementing a trait we reuse throughout; IR gets a submodule that implements it.
All vector stuff in one crate with abstractions handling local/remote. object-pack
is part of sync. rusqdoltlite → vendor. **`server` shouldn't exist — registry IS
the server**; a thin `client` (connecting glue, serves nothing) lives in heart.*

Clarified (2026-07-20 Q&A):
- **Serving:** `registry` **is** the server. The `server` crate dissolves into it.
- **Sync:** the generic **trait lives in `heart::sync`** (light; NO libpijul/iroh/
  object_store deps). The **transport + concrete impl live in the IR plane** (heavy).
- **Approach:** plan-first (this section) → sign-off → execute crate-by-crate.

### 7.1 Crate set — BACKEND (4 cargo crates + vendor)

> **⚠ REVISED 2026-07-20:** `ingestor` **folds into `index`** (user: "ingestor should be
> moved to index"). So the backend is **4 crates**: `ecosystem`, `heart`, `index`
> (+ingestor), `registry` (=server), plus the standalone `vector` crate it consumes.
> The `ir` plane (one crate + `nudox-f1` leaf) and GUI are separate workspaces.
> Item 6 below (`ingestor` as its own crate) is superseded — see item 3.

1. **`ecosystem`** — leaf. Unchanged.
2. **`heart`** — domain vocab **＋ `heart::sync`** (the generic `Sync`-style trait +
   its vocab: change id, watermark/cursor, transport-agnostic; libpijul/iroh-free)
   **＋ `heart::client`** — the connecting glue (typed wire/request types + a client
   that *talks to* registry). Serves nothing. This is also the crate the GUI's thin
   client used to be (§6 old "client" → `heart::client`).
3. **`index`** — catalog plane (schema v4, MetaStore, outbox, migrations, scratch)
   **＋ the folded-in `ingestor`** (feeds + grit GitMonitor + advisories → `index::ingest`
   or similar; keep the separate ingest *process*/bin, now built from `index`). Deps the
   vendored engine at **`vendor/rusqdoltlite`** (path dep; no longer a workspace member).
4. **`vector`** — ONE crate = merge `vector-core` + `vector-local` + `vector-remote`
   + `vector-embed`. The **local↔remote distinction is the trait abstraction**
   (`VectorStore<M>` / `Embedder`, already the seam proven in §6/§5b): `vector::local`
   (qdrant-edge), `vector::remote` (qdrant client + rerank), `vector::embed` (onnx,
   behind feature `onnx`), `vector::core` vocab always-on. Features: `local`,
   `remote`, `onnx`. Consumed by `registry`.
5. **`registry`** — **THE server** (library + binary). Absorbs the former `server`
   crate wholesale: `http/{router,dto,handlers}`, `coordination/`, `poll` (outbox/
   vector/text consumers), `bakery/`, `save/`, `authz`, `config`, `compiler_client`,
   `rerank`, `main` (the bin). server's `search/{planner,routing,semantic,embedder}`
   merges into registry's existing `search/`. Keeps its serving spine (search/graph/
   blob/compiled/metadata/resolve/runtime). Deps: heart, ecosystem, ir, index, vector.
6. ~~**`ingestor`**~~ — **SUPERSEDED: folded into `index`** (item 3). May still absorb
   `registry/upstream/` pollers (they're feeds) as part of the index ingest plane.

`sandbox` (SmolvmCage) stays outside the set (compile plane), as before.

### 7.2 Crate set — IR workspace

> **⚠ REVISED 2026-07-20 (user: "f1 should be simplified/folded too; ir-vcs should be
> folded into ir itself").** The IR plane collapses to essentially ONE crate + the f1
> leaf:
> - **`ir`** = `nudox-ir` **∪ `nudox-ir-vcs`** ∪ (ir-sync's `transport`+`repo_glue`).
>   Modules: `model`/`change`/`manifest`/`archive`/`diff`/`repo`/`semver`/`vcs`/`sync`.
>   **libpijul + iroh linked.** Implements `heart::sync` in `ir::sync`. Retire the
>   `nudox-ir-vcs` and `ir-sync` crates (folded in).
> - **`nudox-f1`** — **CANNOT fold into `ir`**: the vendored libpijul fork
>   (`workspace/vendor/libpijul`) depends on `nudox-f1`, and `ir` depends on libpijul →
>   folding f1 into ir makes `ir → libpijul → ir`, a cargo cycle. So f1 stays a **leaf,
>   but SIMPLIFIED in place** (prune/merge its `pair`/`registry`/`lib`). Full removal
>   would require fork surgery (inline f1 into the libpijul fork) — deferred / needs an
>   explicit call.
> - **Consequence:** the old "backend never deps libpijul" law is **RETIRED** — `registry`
>   needs IR types, so it transitively links libpijul via `ir`. Accepted per the directive.
> - **Legacy `workspace/ir` crate** (old JSON schema, separate from nudox-ir) still backs
>   registry's blob/protocol wire format (`ir::syntax`). Keep the merged crate named
>   `nudox-ir` (or rename legacy) to avoid the name clash; the registry-off-legacy-ir
>   migration stays a separate deferred blocker.
>
> Original (now-superseded) §7.2 text follows.

- **`ir`** (light, **libpijul/iroh-free — backend deps ONLY this**): model + f1 + change
  + manifest. Backend-facing.
- **`ir-vcs`** (heavy, libpijul + iroh linked): archive/diff/repo/semver **＋ `ir_vcs::sync`**
  = **one concrete impl of `heart::sync`** (pijul change files over iroh) + its iroh
  transport + the libpijul `repo_glue`. `nudox-f1` stays the leaf for the libpijul fork.

**`heart::sync` has (at least) TWO implementors** (2026-07-20 user note — this is what
makes the trait generic, not speculative):
1. `ir-vcs::sync` — pijul changes, verified by pijul hash, applied to a libpijul channel.
2. **`object-pack`** — NDPK members/packs over iroh, verified by BLAKE3/bao outboard.
   object-pack therefore **stays its own standalone crate** (deps `heart` + iroh; NO
   libpijul) that *implements* `heart::sync`; it is **NOT** folded into ir-vcs. registry
   (snippet range-get) + ingestor (seal) keep depping `object-pack` directly — no
   libpijul leak. (This supersedes the earlier "object-pack → ir_vcs::sync" idea and
   resolves the object-pack consumer reconciliation below.)

So the topology law holds: `heart` owns the generic *trait* (iroh/libpijul-free);
`ir-vcs` and `object-pack` each own an *impl* + its own iroh transport; `registry` codes
against `heart::sync` and gets the impls **injected at the binary** — registry never deps
libpijul (it may dep object-pack, which is libpijul-free).

### 7.3 GUI

- `gui` uses **`heart::client`** (the former thin `client` crate is now that module).

### 7.4 Override table vs §1/§5

| item | OLD (§1/§5) | NEW (§7) |
|---|---|---|
| server | 1 of 6 backend crates | **dissolved → folded into `registry`** |
| vector | fold 4 → `registry::vector` (features) | **one standalone `vector` crate** (features local/remote/onnx) |
| object-pack | → `index::object_pack` | **stays standalone; implements `heart::sync`** (2nd impl alongside ir-vcs; libpijul-free) |
| rusqdoltlite | → `index::engine` fold | **→ `vendor/rusqdoltlite`** (path dep from index) |
| ir-sync | merge ir-stream+ir-sync+nudox-sync → `ir-sync` crate | **generic trait → `heart::sync`; impl → `ir_vcs::sync`**; ir-stream (guest↔host protocol) stays with ir-vcs |
| client | thin `client` crate (GUI ws) | **→ `heart::client`** |
| backend crate count | 6 | **5 + vendor** |

### 7.5 Open reconciliations (resolve during execution, flag if blocking)

- **`heart::sync` trait shape** — must fit BOTH implementors (ir-vcs pijul changes +
  object-pack NDPK members). From today's `ir-sync`, the transport-agnostic seam is:
  - `ContentIo` (generalize `ChangeIo`): `read/write/has(id)` + `verify(id, bytes)` —
    content-addressed item I/O, generic over the item-`Id` type. (ir-vcs Id = pijul
    `ChangeId`; object-pack Id = NDPK member/pack hash.)
  - `ApplyHook` (generalize): apply an ordered set of ids to a target ref, return the
    new tip. (ir-vcs = libpijul channel apply; object-pack = install into a pack/store.)
  - generic vocab: `VerifyError`, `SyncError`, and an `Announcement<H>` / `Ack` generic
    over the *transport* hash `H` (so `IrohHash` and its `From<iroh_blobs::Hash>` stay
    OUT of heart — each impl instantiates `H`). `heart::sync` stays iroh/libpijul-free.
  The two iroh transports (ir-vcs's + object-pack's) are NOT shared for now — each crate
  keeps its own provide/fetch wiring calling the heart traits. A shared light
  `iroh-transport` helper is a possible later dedup, not this pass.
  Open still: is the catalog **outbox fan-out** also a `heart::sync` consumer? Deferred —
  don't force it into the trait this pass; revisit once the two blob-sync impls fit.
- **object-pack placement — RESOLVED (see §7.2):** stays a standalone libpijul-free crate
  implementing `heart::sync`; NOT folded into ir-vcs. registry + ingestor keep depping it.
- **`heart::client` vs `heart` purity.** heart is "pure vocab." A client that opens
  connections adds reqwest/transport weight. Keep it behind a `client` feature so
  non-client heart consumers (ecosystem-adjacent crates) don't pull it.
- **registry-absorbs-server ripple.** server's `crate::registry::…` re-export paths,
  the `Server<M>` type, `SourceStores<M>`, config, and the bin all move in. Big but
  mechanical; the model brand `M` monomorphization travels with it. `heart::client`
  takes the wire DTOs (http/dto) that a caller needs.
- **vector crate name/location.** standalone top-level `workspace/vector` (not under
  registry). registry deps it with `features=["local","remote","onnx"]`.

### 7.6 Build order — LIVE STATUS (2026-07-20)

0. ✅ vector server store-rewire — the trait seam registry needs.
1. ✅ **rusqdoltlite → `vendor/`** (sonnet; green).
2. ✅ **vector 4 → 1 `vector` crate** (sonnet; green). + nudox-ir test-dep fix.
3a. ✅ **`heart::sync` trait created** (`ContentIo`/`ApplyHook`/`VerifyError`/`SyncError`;
    generic over item-`Id`/`Target`/`Tip`; fits both IR + object-pack). heart green.
3b. ⏳ **IR crate merge** `nudox-ir-vcs` → `nudox-ir` (sonnet, IN FLIGHT). Retire the crate.
    (User: "ir-vcs should be folded into ir itself.")
3c. ⬜ **ir-sync → `nudox_ir::sync`** on the heart traits (FsChangeIo⇒`ContentIo`,
    RepoApplyHook⇒`ApplyHook`; IR-concrete announce/hash types move in); retire ir-sync.
3d. ⬜ **simplify `nudox-f1` in place** (CANNOT fold into ir — libpijul-fork↔f1 cycle §7.2).
4.  ⬜ **object-pack implements `heart::sync`** (2nd impl; stays standalone, libpijul-free).
4b. ⬜ **ingestor → `index`** (user directive; fold feeds/git-monitor into the catalog crate,
    keep the ingest process/bin built from index; sonnet).
5.  ⬜ **server → registry dissolve** — **USE A REAL OPUS SUBAGENT** (user: "pretty
    cross-cutting"). Fold server's modules/bin/search into registry; stand up
    `heart::client` from the wire DTOs; server crate retired.
6.  ⬜ Cleanups: FooError→module::Error, stale comments, member-list prune, green.

Each step ends `cargo check --workspace` (+ `--tests`) green before the next. Steps are
serialized (shared tree + root `Cargo.toml`); no parallel crate-move agents.

---

## 8. RE-LAYERING (2026-07-20 user redirect — supersedes §7 crate roles)

**The core insight: `index` and `registry` are SEPARATE LAYERS that do not reference each
other. The client/GUI composes them.** (User: "registry shouldn't have ANY reference to
doltlite/index… it's up to the client/GUI to hook them up.")

### Layer map
- **`heart`** — vocab + `sync` + **`page`/pagination** (moved from `registry::runtime::pagination`;
  generic over a `Stream`/`Vec`) + **`client`** (glue that hooks index+registry; serves nothing).
  NO session layer (deleted — "weird and needless").
- **`index`** — THE data / storage / coordination layer, where "most of this happens". Absorbs:
  doltlite (vendored), **ingest** (done), **outbox** ("not a registry problem"), **job queue**,
  **blob/CAS** storage, **object-pack** (user: "object-pack should be in index"), **crates/nuget
  upstream pollers** (user: "crates/nuget stuff should just be in index"), and the salvageable
  bits of the user-deleted `registry/{blob,index,queue,runtime}` modules + server's coordination.
- **`registry`** — ONLY **graph + vector**. `registry::graph` (Trustfall) + `registry::vector`
  (the standalone `vector` crate folds IN here — user: "move vector here"). Queries = **exactly
  what Trustfall can express, no further wrapping**; graph and vector are two different things.
  **NO** index/doltlite/queue/outbox/session/text-search/blob. Deps: heart, nudox-ir, qdrant,
  trustfall (NOT index).
- **`nudox-ir`** — IR plane (done). registry deps it for the graph.
- **`nudox-f1`** — leaf, simplified (cycle-blocked from folding into ir).
- **`ecosystem`** — leaf for now; deferred split (`Language`→heart, rest→index) — cycle §8 note.
- **`server`** — **DISSOLVED**: coordination/poll/outbox-consumers/save/bakery/config → `index`;
  graph/vector query handlers → `registry`; pagination/client/wire-DTOs → `heart`.
- **GONE as crates:** server, standalone `vector`, (ingestor/ir-sync/nudox-ir-vcs already folded).

### Cycle constraints (flagged, cycle-safe handling chosen)
- `ecosystem → index` blocked (`heart→ecosystem`, `index→heart`). Deferred; needs Language-hoist.
- `f1 → ir` blocked (libpijul-fork→f1). f1 stays leaf, simplified.
- `object-pack → index`: OK (object-pack deps heart+iroh; no cycle). Folds into `index::pack`.

### Salvage (user-deleted registry files — recover via `git show HEAD:<path>`)
- `registry/runtime/pagination.rs` (keyset_page) → **`heart::page`** (generic).
- `registry/runtime/text/*` + `search/tantivy.rs` + tokenizer → **dropped from registry**
  (no search outside graph/vector); relocate to index only if the data layer needs text lookup.
- `registry/blob/*` (creation/emit) → **`index`** (blob/CAS is storage).
- `registry/index/mod.rs` (GlobalStore glue), `queue/mod.rs`, `runtime/session.rs` → index or dropped.
- `registry/runtime/error.rs` → split per surviving module.

### OPEN (flag for user)
- **HTTP serving surface:** with registry = graph/vector-only and index = data, there is no single
  "server". Interpretation: each layer exposes its own surface; a thin composition (heart::client or
  a small bin) is the client/GUI's job. Confirm if a monolithic serving bin should exist anywhere.

### 8b. Shared bao/iroh transport (2026-07-20 user note)
The bao/iroh content-fetch logic must be **abstracted and shared**, not duplicated. Today
`object-pack` has custom bao/outboard code AND `nudox_ir::sync` has its own iroh transport —
same mechanism twice. Target: extract ONE well-abstracted iroh/bao provide-fetch (a small
transport crate, since `heart` must stay iroh-free — `heart::sync` is only the abstract
`ContentIo`/`ApplyHook` seam). Both `index::pack` and `nudox_ir::sync` depend on it and wire
their `ContentIo`/`ApplyHook` impls over it. No custom per-plane bao. QUEUED follow-up (after
the registry/server re-layering settles — needs pack already in index).

### 8c. ONE persistence/streaming/transfer trait — the north star (2026-07-20 user)
"All of this persistence/streaming/transfer logic could really just use one good trait,
especially since they're unified by the local/remote distinction." So `heart::sync`
(`ContentIo`/`ApplyHook`) + the shared bao/iroh transport (§8b) is THE single seam for
content-addressed persist → verify → transfer → apply, across ALL of:
- IR changes (`nudox_ir::sync`)  — pijul change files
- object packs (`index::pack`)   — NDPK members
- **vector edge shards** (bakery/dep-shard install) — a baked shard is a content-addressed
  pack; bake=persist-local+publish, install=fetch-remote+verify+apply. Same loop. The
  `vector::local`↔`vector::remote` distinction IS the local(persist)/remote(fetch) axis.
Boundary (my read, confirm): the trait covers PERSISTENCE/TRANSFER only. The live QUERY
surfaces (`VectorStore::search`, graph Trustfall) stay separate — same store, but querying
≠ shipping. CAPSTONE task: once vector (→registry::vector), pack (→index::pack), and IR sync
have all landed, retarget their shard/pack/change transfer onto the one `heart::sync` seam +
one bao/iroh transport, deleting the per-plane custom transfer/bao code.

## 9. LANDED (2026-07-20, agent fan-out)
- ✅ **rusqdoltlite → vendor**, **vector 4→1** (then → registry::vector, below), **IR plane → one `nudox-ir`** (nudox-ir-vcs + ir-sync folded; ir-sync's transport/repo_glue = `nudox_ir::sync` on `heart::sync::{ContentIo,ApplyHook}`), **ingestor → `index::ingest`**.
- ✅ **f1 ELIMINATED** — inlined into the vendored libpijul fork (`libpijul::nudox_f1`); nudox-ir reaches it via `libpijul::nudox_f1::registry`; crate + dir + member gone. `cargo check -p nudox-ir` green.
- ✅ **registry = graph + vector ONLY** — `registry::graph` (Trustfall adapter + reverse index, over nudox-ir) + `registry::vector` (the folded-in vector plane, features local/remote/onnx). **`index` dep SEVERED** — registry compiles standalone (all feature combos + tests). No queue/outbox/session/blob/text-search.
- ✅ **V-GOLD-1 realized** — the smolvm infra DOES expose the hot-start path (`LaunchFeatures.forkable/snapshot_dir`, `agent::fork::prepare_fork` = memfd RAM snapshot + qcow2 CoW-clone, ~250ms). `SmolvmRuntime::fork_golden` now does TRUE CoW fork-from-warm-golden (option 2b), one-VM-per-job teardown preserved. `sandbox` green, 77 tests pass. Residual: in-RAM golden secrets CoW-inherited by clones (golden-image packaging constraint upstream; benign for toolchain base images).

### 9a. Salvage list — registry deletions to fold into `index` (deferred pass; `git show HEAD:<path>`)
Storage/serving bits the registry-repurpose deleted, worth recovering INTO `index` when its SeaORM
migration is stable: `store.rs`, `persist.rs`, `coordination.rs`, `schema/{mod,catalog_map,codec}.rs`,
`compiled/{mod,client}.rs`, `metadata/{mod,rich,heuristics,hash}.rs`, `resolve.rs`,
`ingest/{mod,extract}.rs`, `package.rs`, `identity.rs`, `protocol.rs`, `health/mod.rs`, `error.rs`,
the search plane `search/{mod,pipeline,structured,tantivy,alias,usages,spell,health}.rs` + `search/ranking/*`,
and `upstream/{catalog,crates_catalog,nuget_catalog}.rs` (→ ingest, per §8 crates/nuget-in-index).
Also: `registry/tests/vector/*` (18 UNTRACKED files on old flat `vector::` paths) are orphaned (no
`tests/vector.rs` root, not compiled) — port to `registry::vector::core::*` or drop in a later vector-test pass.

### 9b. STILL GATED on the `index` SeaORM migration (fan out when index compiles)
server dissolve → index/registry/heart · object-pack → `index::pack` · ecosystem split (Language→heart, rest→index)
· salvage §9a into index · §8b/§8c one bao/iroh transport under `heart::sync` (IR + pack + vector-shard transfer).

### 9d. SERVER DISSOLVE — COMPLETED (2026-08-12): driver → index::server + heart::client
The intermediate `driver` composition crate (§9c) was fully dissolved per the user directive
("driver → move more into heart::client; any server-specific → index"):
- ✅ **server-specific → `index::server`** — the whole `driver` module tree (config/error/authz/http/
  coordination/poll/save/bakery/rerank/sync/search + `Driver<M>`=`Server<M>` + the layer-facade shim)
  moved into `workspace/index/server/`, rewired (`crate::`→`crate::server::`, `index::`→`crate::`,
  `xregistry`→`registry`). Built from it: the `nudox-serve` bin (`server/main.rs`). Behind index's
  non-default **`server` feature** so a plain `cargo build/test -p index` stays a lean catalog library.
- ✅ **`transport` crate EXTRACTED** — `index::transport` (the shared iroh/bao plane) became its own
  `workspace/transport` crate so `ir-vcs` deps IT (not all of `index`), breaking the `index ↔ ir-vcs`
  cycle that folding the server into `index` would create (this is §8b's planned extraction, forced
  early). `index` re-exports it as `index::transport`; `ir-vcs` now deps `transport`.
- ✅ **`heart::client` connecting client ADDED** — `heart::client::http::NudoxClient` (reqwest, behind a
  new off-by-default `client` feature): `readyz`/`search`/`search_packages`/`add_package` over the shared
  `heart::query::Query` + `Scored<Symbol>` + `client::dto` types. Proven end-to-end against a live
  `nudox-serve` (`examples/nudox_ping.rs`). heart's default build stays reqwest-free.
- ✅ **`driver` crate RETIRED** — deleted; root member removed; its 21 integration tests ported to
  `workspace/index/tests/` (behind `--features server`, shared helper renamed to `server_common`).
- ⚠ **Known latent clash**: `--features server` links `ir-vcs → libpijul → zstd-seekable`, which
  duplicate-symbol-clashes with `index::pack`'s `zstd-sys` and SIGSEGVs pack under that feature (was
  latent in the old `driver` bin too). Default `index` (server off) is clean: 725 lib tests green.
- Status: `cargo check --workspace` green; `nudox-serve` serves `/readyz {"ready":true}`.

### 9e. CRATES-FOLD — `nudox-test-support` relocated; the four substantive folds BLOCKED on leanness (2026-08-12)
Task: fold the `crates/*` local-first crates into their `workspace/*` equivalents (`nudox-store`→`index`,
`nudox-graph`/`nudox-embed`→`registry`, `nudox-engine`→`index::server`) to reduce the local-first/server split —
without breaking the tree or the lean, gpui-free path the GUI (`workspace/gui`, package `lindsey`) consumes.

- ✅ **LANDED — `nudox-test-support` → `workspace/test-support`** (package name unchanged). It is a zero-dependency,
  dev-dependency-only measurement leaf (`measured()` / `disk_bytes`, doctrine §4's `cost case=…` line) already shared
  by `index`, every `compiler/languages/*`, and the local-first crates — so it belongs beside the other shared
  workspace crates, not under `crates/`. `git mv` + rewired all 11 consumer manifests + the root member (removed from
  the local-first `[workspace.members]` block, re-added in the shared section beside `workspace/heart`). It is on no
  crate's SHIPPING graph, so it carries zero leanness risk. **Verified green:** `cargo check --workspace` (exit 0),
  `cargo build --manifest-path workspace/gui/Cargo.toml --bin lindsey` (exit 0), and the GUI dependency-law suite
  (`workspace/gui/tests/dependency_law.rs`, 2/2 passed).

- ⛔ **BLOCKED (documented, not attempted-and-reverted) — the four substantive folds, all on leanness grounds.**
  The GUI's local-first path is `lindsey → nudox-engine → {nudox-graph, nudox-store} → nudox-ir`, plus
  `nudox-mcp → {nudox-engine, nudox-graph}` and `nudox-embed` (registry ONNX bridge, `default-features = false`,
  registry pulled ONLY under the off-by-default `onnx` feature). A default GUI build therefore links NONE of
  `index`/`registry` and none of qdrant/tantivy/dolt/iroh — the leanness the hard constraint + `dependency_law.rs`
  protect. Every substantive fold breaks that as things stand today:
  - `nudox-graph → registry`: `registry` deps are **unconditionally heavy** — `default = ["remote"]` and its
    `[dependencies]` link `qdrant-client`, `reqwest`, `moka`, `tar`, `zstd`, `trustfall` with no lean gate. The
    GUI's `engine → graph` edge would become `engine → registry`, dragging qdrant-client into every GUI build.
  - `nudox-embed → registry`: `nudox-embed` is the *whole point* of the split — a thin, default-off adapter so the
    GUI can name it (dependency-law allow-list = `{nudox-engine, nudox-mcp, nudox-embed}`) without naming `registry`.
    Folding it in would force the GUI to declare a `registry` dependency (dependency-law violation) AND link
    qdrant. No gain: the ONNX runtime already lives in `registry`; only the object-safe `Embedder` bridge is in
    `nudox-embed`, exactly where §1 "capability ports" wants it.
  - `nudox-store → index` / `nudox-engine → index::server`: `index`'s `[dependencies]` are **unconditionally heavy**
    (`registry` w/ `local`+`remote`, `qdrant-client`, `tantivy`, `iroh`/`iroh-blobs`/`bao-tree`, `object_store`,
    `sea-orm`, `rusqlite`, `grit-lib`, `arborium-tree-sitter`, …); even server-off `index` is a heavy catalog lib.
    And the two "stores" are **different substrates** — `nudox-store` is an in-memory `IrView` corpus over the
    producer plane (`nudox-producer-*`); `index`'s storage is a DoltLite versioned SQL catalog. This is a source
    relocation behind a new feature, not a merge of like storage.

  **Remaining-work plan (the lean-`local`-feature strategy), file-level:**
  1. Make `index` and `registry` heavy deps **optional** and gate them behind a `catalog`/`serving` feature that is
     part of the current `default` (so `cargo check --workspace`, `-p index`, `-p registry` are unchanged). For
     `index`: move `registry, qdrant-client, tantivy, iroh*, bao-tree, object_store, sea-orm, rusqlite, grit-lib,
     arborium-tree-sitter, moka, reqwest, tar/zip/flate2/async-compression` to `optional = true` under that feature;
     same for `registry`'s `qdrant-client/reqwest/moka/tar/zstd`. HIGH churn, must keep `index::{server,pack,catalog,…}`
     behind it.
  2. Add a **`local`** feature to `index` that compiles ONLY the relocated `nudox-store` (`corpus`/`package`/`index`
     posting/`source`+producers) + `nudox-graph` (`adapter`/`vertex`/`plan`/`queries`) modules, depending on
     `nudox-ir` + `nudox-producer-*` + the light utility deps (futures/indexmap/rustc-hash/thiserror/tokio-rt/
     tracing/triomphe/trustfall) and **nothing** from step 1's gate. `local` and `catalog` must be able to coexist
     (feature unification) without `local` pulling any step-1 dep.
  3. Repoint `nudox-engine` at `index = { path=…, default-features=false, features=["local"] }` (replacing its
     `nudox-store`/`nudox-graph` path deps); repoint `nudox-mcp`'s `nudox-graph` use at `index::local::graph`.
     `nudox-engine` itself can then optionally fold into `index::server` as an `index` feature `engine` (or stay a
     crate that deps `index/local`). `nudox-embed`/`nudox-mcp`/`lindsey` keep their names.
  4. **Cycle risk:** `index` already deps `registry` (for the bakery ledger). Folding `nudox-graph` — which today deps
     only `nudox-store`+`nudox-ir` — into `index/local` is cycle-safe (no new edge to `registry`). Folding
     `nudox-embed` is NOT pursued (it must stay a distinct GUI-nameable crate).
  5. **Leanness gate (must stay green after each step):** `cargo build --manifest-path workspace/gui/Cargo.toml
     --bin lindsey` + `cargo test -p lindsey --test dependency_law`; and a NEW assertion that the GUI's resolved
     graph contains no `qdrant-client`/`tantivy`/`iroh`/`sea-orm` (grep the GUI-workspace `Cargo.lock` once it is
     built, mirroring `dependency_law.rs`'s text-scan discipline — no `cargo metadata` subprocess). Because the GUI
     is a SEPARATE cargo workspace with its own lockfile, per-workspace feature unification means `index/local` in the
     GUI graph stays lean **iff** no GUI-reachable crate enables a step-1 feature — which step 2 guarantees.
  This is large, high-risk manifest surgery on two heavy crates; it was correctly deferred rather than half-landed.

### 9c. SERVER DISSOLVE — in progress (2026-07-20)
- ✅ **heart destination LANDED** — `heart::client` created (`pub mod client`): `client::dto` (add-package /
  compiled-lookup / rerank / health wire DTOs, transport-free, own typed validation errors — no `ServerError`)
  + `client::authz` (`TenantId`, `ReadCap`/`WriteCap`/`AdminCap`, `Principal`/`AdminPrincipal` vocab; cap minting
  via `Cap::mint`, composition-only; the axum `FromRequestParts` + `impl Server<M>` minting stay STAGED). heart
  stays axum/reqwest-free. `cargo check -p heart` + `cargo test -p heart --no-run` green.
- ✅ **registry destination = NO CHANGE NEEDED** — registry already exposes its full graph/vector *library* query
  surface (`registry::graph` Trustfall + `registry::vector::{VectorStore::search, remote::rerank, …}`), already
  green + index-free. Server's "graph/vector query handlers" are all `State<Arc<Server<M>>>`-coupled axum handlers
  that pull BOTH catalog(index) AND vector — they are composition, not library, so they CANNOT go in registry
  (registry must stay index-free and is not an axum server). Server's `rerank.rs` proxy is SUPERSEDED by the
  canonical `registry::vector::remote::rerank`. → registry stays graph+vector only, index-free. Handlers STAGED.
- ✅ **index destination — §9a substrate SALVAGED + bakery ledger FOLDED** (`cargo check -p index` + `cargo test
  -p index --no-run` green, 0 errors). All §9a files recovered via `git show HEAD:workspace/registry/<path>` into
  `index::{cas`(=former `store.rs`)`, blob, catalog`(=former `index/mod.rs`, renamed to dodge crate-name
  collision)`, coordination`(=`Outbox<E>`)`, queue, compiled, metadata, search`(+ranking)`, runtime`(text index +
  pagination/error)`, upstream`(crates/nuget followers)`, resolve, health, schema, identity, package, error}`.
  Added `index → registry` dep (cycle-safe: registry is index-free) for `registry::vector` bakery-key types +
  `registry::graph::ReversePositionIndex` in `search::usages`; + `nudox-ir` + the tantivy/qdrant/object_store/
  reqwest/etc serving deps (versions copied from server). `runtime/error.rs` BLOCKER resolved (dropped
  `VectorError::Collection` → deleted `CollectionNameError`). **`bakery` ledger** (`CatalogEdgepackStore<E>`,
  `EdgepackRow`/`Status`, `edgepack_key`, `RECIPE_ID`) folded into `index::bakery`.
  - **NOT salvaged/STAGED:** compiler-daemon `protocol.rs` (dead — cage is ephemeral); `blob` reference-extraction
    round-trip (used deleted legacy `ir` crate → replaced with self-contained `Reference`/`RefTarget` postcard
    types, ir-plane conversion staged); bakery vector-bake *compute* + `bakery_worker` (index-free
    `registry::vector` shard-build over `Server<M>` — composition).
  - **`coordination/`, `poll.rs`, `save/` STAYED STAGED** (not folded): `impl Server<M>`/`SourceStores<M>`
    orchestration bound to `HttpEmbedder<M>`/authz/ServerError/compiler_client — the composition that DRIVES the
    index substrate, not the substrate. §8-correct split: substrate in `index`, driver in composition.
- ✅ **composition LANDED — new `workspace/driver` crate** (2026-07-20; `cargo check -p driver`,
  `cargo build -p driver --bin driver`, `cargo test -p driver --no-run` all green, 0 errors). The staged
  `server/{lib,main,http/,config,error,authz,coordination,poll,save,search,bakery,rerank,tests}` were moved into
  `workspace/driver/` and rewired onto the re-layered planes. `workspace/server/` DELETED; the parked root
  `Cargo.toml` member replaced by `"workspace/driver"`. Composition shape:
  - **`Driver<M>`** (was `Server<M>`; a `pub type Server<M> = Driver<M>` alias avoided a crate-wide rename) —
    holds `config` + a `Federation<SourceStores<M>>` (each source = `GlobalStore<DoltEngine>` + `cas::Store` blobs
    + `Queue` + `Outbox` + `RemoteStore<M>` semantics + `TextIndex` + `PackageSearchIndex`) + the query planner +
    `HttpEmbedder<M>` + `EmbeddingCache<M>` + scratch `ScratchSessionStore` + `ObjectCompiledStore` + optional
    bakery ledger + reverse-index usage backend. It COMPOSES the layers; no domain logic.
  - **Layer-facade shim** (`driver::lib.rs`): rather than rewrite ~13k lines of moved `use`s, the crate re-projects
    the layers under the old monolithic paths — a crate-local `mod registry` re-exports the `index::*` data-plane
    modules under their old names + real `::registry`'s `graph`/`vector` (the extern `registry` crate is aliased to
    `xregistry` in `Cargo.toml` so bare `registry::…` resolves to the facade), and a `mod vector` flattens
    `::registry::vector::core::*` back to top level. `use crate::{registry, vector};` injected where bare paths are
    used at module scope.
  - **authz DEDUPED** against `heart::client::authz`: driver's `authz.rs` now re-exports `TenantId`/`ReadCap`/
    `WriteCap`/`AdminCap` from heart; keeps only the composition parts (local `Principal`/`AdminPrincipal` — must be
    local for the axum `FromRequestParts` orphan rule — + the `impl Server<M>` `authorize_*` minting via `Cap::mint`).
  - **`http/dto.rs` NOT deduped** against `heart::client::dto`: driver's copies are `ServerError`-coupled
    (`validate()`/`into_coordinates() -> Result<_, ServerError>`, `register_custom_registries`, `DepshardManifestDto`,
    `hit()` over `registry::compiled::CompiledHit`) — composition HTTP wiring, not pure vocab; left as-is.
  - **RECOVERED driver-local** (were NOT salvaged into `index`, pulled via `git show HEAD:workspace/registry/<path>`):
    `session` (the exploration-graph semilattice, ex `registry::runtime::session`) and `ingest` (the untrusted-archive
    extraction `ingest_archive`/`EntryAllowlist`/`ExtractionLimits`, ex `registry::ingest`; `index::ingest` is the
    git-monitor plane). Both rewired onto `index::{blob,error,runtime,scratch}`.
  - **`compiler_client.rs` stays DELETED** (compiler daemon gone — cage is ephemeral). Fallout: `error.rs`
    `ServerError::Compile` variant dropped; `Indexer::new` no longer takes a `CompilerClient`; the vector-plane
    errors got their own `ServerError::{Vector,Embed}` variants (the vector plane is now `registry::vector`, a
    different crate from `index::runtime`, so its errors no longer fold through `RuntimeError`).
  - **STUBBED (one handler/phase)** — `coordination::indexing::Indexer::execute_compile_phase` returns
    `ServerError::Internal(InternalError::Other{…})` (the compile-daemon `CompileRequest`/`CompileResponse` wire
    protocol is gone). `// TODO(driver): rewire the compile phase onto the ephemeral SmolvmCage` marks it. Extract +
    emit phases around it are intact; only IR production on a forge node is stubbed (fails loudly, idempotent retry).
  - **`rerank.rs` KEPT** as the composition's rerank client (the canonical `registry::vector::remote::rerank` plane
    stays the library surface; the handler wiring is composition).

### 8d. Compiler golden snapshots = a 4th `heart::sync` consumer (2026-07-20 user)
The smolvm hot-restore path is ALSO content-addressed persistence/transfer and must ride the
same `heart::sync` seam (§8c), not a bespoke path:
- `GoldenPool` (`ImageDigest → GoldenId`) is a content-addressed snapshot registry.
- `prepare_golden(image)` = PERSIST a golden (memfd RAM snapshot + qcow2 CoW base) → `ContentIo::write`.
- `fork_golden` = fetch/verify/RESTORE a golden into an ephemeral clone → `ContentIo::read`/`verify` + `ApplyHook`.
- "the remote might use it at scale" = golden snapshots FETCHED/shared across the fleet via the one
  shared bao/iroh transport (§8b) — a remote registers goldens, edge nodes fork-restore them.
So the unified trait now has FOUR implementors: IR changes, object packs (index::pack), vector edge
shards, and **compiler golden snapshots** (sandbox). CAPSTONE (after server dissolve + the crates
settle): retarget `SmolvmRuntime`'s golden persist/restore onto `heart::sync` + the shared transport,
deleting the bespoke snapshot plumbing; the fleet-scale golden sharing then comes for free.

### 8e. Federation/overlay-aware sync — the deployment model (2026-07-20 user)
The unified `heart::sync::ContentIo` (per-target seam) + `transport` (per-endpoint byte
movement) + the existing `Federation<SourceStores>` (definitive base + overlays) compose into
federation-aware sync. A verified content item (change / pack / shard / golden) fans across the
federation topology per deployment:
- **Local node:** ContentIo::write to the LOCAL store AND push via `transport` to the REMOTE
  overlay(s) — content is durable locally and replicated up. (local + remote)
- **Fleet of compilers:** each node has no local serving store of its own, so it just pushes
  its produced content (esp. goldens §8d, IR changes) BACK to the main/central remote. (fleet → central)
The trait stays per-target; the fan-out is a thin **federation sync driver** that iterates
`Federation` and routes each item to the right ContentIo/transport target. NOT YET WIRED — the
pieces (ContentIo, transport::Provider/Fetcher, Federation) all exist; the orchestrator that
walks the federation and multiplexes writes/pushes is the remaining glue. Same seam, N targets.
