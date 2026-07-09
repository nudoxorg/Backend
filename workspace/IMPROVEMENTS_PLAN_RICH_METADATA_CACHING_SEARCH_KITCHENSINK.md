# Full Plan: Porting Main/ Strengths to Workspace/ (Rich Metadata, Heuristics, Caching, Facade)

**Context**: This plan is based on deep analysis of real code/examples from `main/` (lib.rs / crates.rs backend) vs. current `workspace/` (registry + heart + runtime + server). It incorporates exhaustive external research on caching (stampede prevention, tiered/hybrid, SWR, coalescing, probabilistic early expiration from Cloudflare paper/optimal algorithms, Caffeine-inspired policies, foyer/moka patterns, etc.), search quality heuristics, metadata extraction pipelines, and facade patterns from real-world systems (crates.rs, crates.io, general indexers).

**Main/ Strengths to Port (Explicitly Targeted)**:
- **Rich per-crate (per-package) metadata extraction**: Deep analysis of manifests, source, READMEs (section-filtered), keywords, categories, stats, enrichment (GH, owners, compat, warnings), producing `RichCrateVersion` + `Derived`.
- **Synonym/keyword heuristics + post-processing for search quality**: `Synonyms` (normalize, max chains), `Specifics` (DSL for relate/combine/split/bland with conditions), TF-IDF category guessing, keyword gathering (from code/readme/deps/features/authors + hashed content), search post-proc (DYM fuzzy, dividing_keywords for diversity, exact-match fixes, score tweaks with downloads/rank, representative boosting, spam suppression).
- **Practical caching layers**: Multi-tier (in-mem RwLock/OnceCell + file-backed `TempCache` with brotli/mpbr atomic saves + expiry + dirty autosave; SQLite `SimpleCache` with ThreadLocal + compress + fetcher integration + lock retries), tailored TTLs per use-case, prewarming, throttles, event-driven invalidation, DoubleCheckedCell for lazy, different hot vs stable policies.
- **"Kitchen sink" convenience facade**: `KitchenSink` as single ergonomic entrypoint (aggregates clients, dbs, caches, factories for rich data/indexing/search, throttles, event_log, prewarm). Useful for app/server/tools when composition is too verbose.

**Workspace/ Current State (Gaps/Opportunities)**:
- Strong durability (Postgres + CAS blobs + outbox + watermarks + leases + reconcile), multi-lang compiler/IR, hybrid search (tantivy text + Qdrant vector + graph), good ingest sanitization (streaming budgets/jail), light moka (embeddings only: simple get+insert, deliberately avoids some coalesced APIs due to error ergonomics).
- Gaps: Less "rich" derived metadata (focus on IR/structure over keyword heuristics, auto-cats, deep readme, traction scoring); basic search (no heavy synonyms/post-proc/diversity/DYM like main); caching mostly watermarks + minimal moka (no tiered disk like TempCache, limited stampede protection); no single high-convenience facade (explicit `SourceStores`).
- Opportunity: Enhance extraction/heuristics/search *on top of* existing robust spine without sacrificing uptime/durability. Caching improvements directly boost uptime (less recompute = less load/stampede risk).

**Research Backing (Exhaustive, Real Sources)**:
- **Caching behaviors**:
  - Cache stampede/thundering herd: Synchronized expiry or high-cost misses cause thundering herd on backend (DB/object store/embedder). Mitigations (real): Request coalescing/single-flight (one computes, others wait/share), Stale-While-Revalidate (SWR: serve stale + async refresh), Probabilistic early expiration (jittered TTLs + random pre-refresh based on cost/latency; see Cloudflare "Sometimes I cache" blog + 2015 paper "Optimal Probabilistic Cache Stampede Prevention" by Vattani et al. — spreads refreshes, no locks needed for basic case). Jitter + layered policies.
  - Tiered/hybrid: L1 memory (fast, bounded) + L2 disk (persistent, larger) + L3 remote (Redis-like with pubsub invalidation). Benefits: hit rate, cost (less remote), restart resilience. Examples: foyer (hybrid memory+disk, pluggable algos, zero-copy, request dedup), multi-tier-cache (moka L1 + Redis L2 + broadcast coalescing + pubsub cross-instance).
  - Policies (Caffeine-inspired, real in Rust): TinyLFU admission + LRU eviction (moka), S3-FIFO, size weigher (bytes), per-entry TTL, eviction listeners (side effects like promote to L2).
  - Rust specifics: Lock-free preferred (moka internals), async (future cache), bounded (capacity/weigher), error handling (avoid Arc<E> for non-Clone errors by manual get+insert).
  - Main/ examples: Varied expiries (11d url checks, 6mo readmes, 0 for volatile, 3h histograms); TempCache (BTreeMap + RawEntry compressed + expiry_timestamp + writes counter + NamedTempFile atomic persist + race check); SimpleCache (SQLite cache2 table + brotli + ThreadLocal conns + DatabaseLocked retries + SimpleFetchCache); in-mem + DoubleCheckedCell + semaphores for backpressure + prewarm + event_log for pub/sub invalidation.
- **Metadata/heuristics/search**: Main's tuned stopwords (IDENT_STOPWORDS + general), section relevance filter (skip boilerplate/install/license), source scraping (synscrape for idents, udedokei/tokei for stats), keyword signals (hashed files/readme + features + deps weighted + authors + "has:" + url/repo), TF-IDF self-join for cats/related, Synonyms/Specifics DSL (CSV + complex relations with !? conditions, normalize chains/loops detection), search post (custom tantivy tokenizers + synonyms, Levenshtein DYM, MoreLikeThis, post-processing for dividing/rep-boost/exact/downloads bubble, spam downrank). External: RAKE/YAKE/TextRank/PositionRank for keywords (but main's custom is domain-tuned for crates + better with manifest/repo signals); category guessing improvements noted in lib.rs announcements.
- **Facades**: Common in indexers/tools (one `Context` or `KitchenSink` for ergonomics when wiring 10+ components). Main uses it heavily for reindex/server paths.
- Other crates/patterns: arc-swap (hot state reload like main's ArcSwap<KitchenSink>), parking_lot, brotli, levenshtein-automata, double-checked-cell-async.

**Real Crates + APIs to Incorporate (Current as of Research ~2026; Verify Versions)**:
- **moka** (0.12+; already lightly used): Primary for L1. `moka::future::Cache::builder().max_capacity(10_000).time_to_live(Duration::from_secs(3600)).time_to_idle(...).weigher(|k,v| v.len() as u32).eviction_listener(|k,v,cause| ...).build();`. Async: `.get(&k).await`, `.insert(k,v).await`, `get_or_insert_with` / `try_get_with` (for coalesce; handle errors carefully like current embedding cache — prefer get-then-insert for non-Clone errs). Sync variant too. Supports per-entry expiration. Justification: High perf concurrent, Caffeine-like (LFU admit/LRU evict), stampede helpers via listeners or combined patterns.
- **foyer** (hybrid): `foyer::{HybridCache, HybridCacheBuilder, FsDeviceBuilder, DirectFsDeviceOptionsBuilder, BlockEngineConfig}`. `let hybrid: HybridCache<K,V> = HybridCacheBuilder::new().memory(64<<20).storage().with_device_config(FsDeviceBuilder::new(dir).with_capacity(256<<20).build()?).build().await?; hybrid.fetch(key, || async { compute() }).await?;` or `get(&k).fetch_on_miss(...)`. Memory + disk tiers, pluggable algos, request dedup. Great for main's TempCache-like persistent + fast L1.
- **multi-tier-cache**: Quick start for L1(moka/QuickCache) + L2(Redis) + pubsub invalidation + built-in stampede (broadcast-channel coalescing). Config with per-tier TTL scaling. `MokaCacheConfig { max_capacity, time_to_live, ... }`.
- **quick-cache**: Lightweight concurrent for hot sub-caches (alternative/complement to moka in tiers). Good when moka overhead is overkill.
- **Supporting**:
  - `brotli` (for compressed persistent entries, like main SimpleCache/TempCache).
  - `double-checked-cell-async` or `once_cell`/`tokio::sync::OnceCell` + `parking_lot` (for lazy like main's top_crates_cached, vet, etc.).
  - `arc-swap` (for atomic facade/state reloads).
  - `levenshtein-automata` (for DYM fuzzy, like main).
  - `deunicode`, `heck`, `unicode-normalization` (keyword normalize).
  - `uniflight` or custom (coalescing: "Coalesces duplicate async tasks").
  - Optional: `cached` (proc-macro fn caching), `cacache` (content-addr disk), `redb` or integrate registry blobs for L2.
  - Event: Extend `tokio::sync::broadcast` or workspace coordination; or port main's `event_log` (SQLite append + subscribers/watermarks) for invalidation.
- Avoid overkill: Don't pull heavy NLP (KeyBERT etc.) unless needed; main's heuristics are lightweight + tuned.

**Overall Architecture Goals**:
- Preserve workspace durability (blobs/index/outbox/reconcile/leases).
- Layer rich extraction + heuristics on ingest (feed derived keywords/cats/metadata into search blobs + indices via outbox).
- Make caching first-class, stampede-proof, tiered (memory for hot + disk for restartable like main + optional remote).
- Add facade for convenience (optional; composition still primary).
- Multi-lang aware (Language const generic).
- Measurable: cache hit rates, search relevance (A/B or offline), extraction coverage, load-test stampede resilience.

## Phased Implementation Plan (Topological, with Real File Paths + Snippets)

**Prerequisites**: Add to workspace BUCK/equivalent manifests (moka already partial dep; add foyer, multi-tier-cache, etc. with features "future"). Bundle or load data files (port main/data/tag-synonyms.csv etc. to workspace/data/ or config). Feature-gate new richness ("rich-metadata").

### Phase 0: Foundations + Data (Low Risk, 1-2 "PRs")
- Port/adapt data assets + core types:
  - `workspace/data/` (or per-crate): `tag-synonyms.csv`, `specific-keywords.txt`, `bland-keywords.txt`, `category_overrides.txt` (adapt for multi-lang later).
  - New module: `workspace/heart` or `registry/metadata/heuristics.rs` (or `categories` crate equivalent): Port `Synonyms`, `Specifics`, `normalize_keyword` (with tests from main/categories + main/feat_extractor).
    ```rust
    // Example API usage (ported)
    let syns = Synonyms::new(&data_dir)?;
    let (norm, w) = syns.normalize("async", 2);
    let specifics = Specifics::new(&data_dir)?;
    if let Some(action) = specifics.is_bland(kw) { ... }
    for (k, rel) in specifics.get_relations("web") { ... }
    ```
  - `normalize_keyword` full port (handles C++, iOS, scripts, etc.).
- Add deps + small util: `parking_lot`, `arc-swap`, `brotli`, `levenshtein-automata`, `double-checked-cell-async`.
- Update docs/tests for determinism (keywords/specifics must be stable).

### Phase 1: Advanced Caching Layer (Foundation for Everything; High Uptime Impact)
- New crate or module: `workspace/util/caching` (or enhance `runtime` + registry). Or `caching` facade.
- Core: Tiered cache abstraction.
  - L1: moka (sync/future).
    ```rust
    use moka::future::Cache;
    let cache: Cache<K, V> = Cache::builder()
        .max_capacity(10_000)
        .time_to_live(Duration::from_secs(3600 * 24 * 7))  // e.g., stable metadata
        .time_to_idle(Duration::from_secs(3600 * 2))
        .weigher(|_k, v: &Vec<u8>| v.len() as u32)
        .eviction_listener(|k, v, cause| tracing::debug!(?cause, "evict"))
        .build();
    let val = cache.get_or_insert_with(key, || compute()).await?;
    ```
  - L2 persistent: foyer hybrid or custom port of TempCache (BTree + brotli + atomic NamedTempFile like main/kv.rs) + SimpleCache SQLite style (ThreadLocal + retries).
    ```rust
    // foyer hybrid example (real API)
    let device = FsDeviceBuilder::new(dir).with_capacity(256<<20).build()?;
    let hybrid: HybridCache<K, V> = HybridCacheBuilder::new()
        .memory(64<<20)
        .storage()
        .with_device_config(device)
        .build().await?;
    let entry = hybrid.fetch(key, || async { expensive_compute().await }).await?;
    ```
  - multi-tier for remote: When distributed needed.
- Stampede protection (deep):
  - Coalescing: Wrap with pending map + broadcast (or uniflight). Example pattern (main-inspired + research):
    ```rust
    // Pseudo: use tokio::sync::broadcast or DashMap< K, broadcast::Sender<Result<V>> >
    async fn get_or_compute_coalesced(...) {
        if let Some(pending) = pendings.get(&k) { return pending.subscribe().recv().await; }
        let (tx, _) = broadcast::channel(1);
        // compute, send to tx, store
    }
    ```
  - Probabilistic early exp (from Cloudflare/paper): On get, if close to expiry (randomized by cost), trigger background refresh while serving current. Jitter TTLs on insert (`ttl * (0.8 + rand*0.4)`).
  - SWR: Serve stale + spawn refresh (use moka eviction or custom expiry tracking).
  - Backpressure: Semaphores like main (throttle heavy computes).
  - Tailored instances in facade: `metadata_cache: TieredCache<...>`, `search_results_cache`, `enrichment_cache` (short TTL), `expensive_extract_cache`.
- Invalidation: Keys include `generation`/`content_hash` (like registry); listen to outbox/events for explicit invalidates. Eviction listeners to promote to L2.
- Port main's fetcher integration (SimpleFetchCache).
- Test: Load tests for stampede (1000 concurrent misses on hot key → 1 compute), restart (disk L2 survives), hit rates.
- Uptime win: Prevents thundering on embedder/GH/clients/Postgres during deploys/restarts/indexing waves.

### Phase 2: Rich Metadata Extraction
- Extend `registry/ingest/` or new `registry/rich_metadata.rs` (or `metadata/`).
- Pipeline (during/after `ingest_archive` + blob emit, or in indexer phases):
  1. Sanitized extract (reuse existing + port tarball Collector logic for relevant files: manifest, lib/bin, readme sections).
  2. Scrape: Use compiler surface/IR + port synscrape/wlita/udedokei for idents/stats/LOC. Filter readme sections (relevance: skip install/license/boilerplate using patterns from main/feat_extractor/keywords.rs).
  3. Keywords: Port `gather_crate_keywords` + feat_extractor (visible_auto from source/readme normalized, invisible, "has:proc_macro", "dep:foo" weighted, "feature:bar", authors "by:", homepage/repo signals, hashed content for dedup signals). Apply Synonyms + Specifics (boost/fold/bland).
  4. Categories: Port candidate TF-IDF (crate_db style self-join or keywords co-occur) + `adjusted_relevance` + explicit overrides. Guessing if none.
  5. Enrichment: Port/adapt clients (github_info, crates_io_client equivalents; docs_rs). Traction/compat signals, owners, warnings (like KitchenSink warnings + creviews).
  6. Output: `RichPackageMetadata { keywords: Vec<(f32, SmolStr)>, categories: Vec<...>, derived_stats: ..., readme_markup: ..., warnings: HashSet<Warning>, ... }`. Serialize to blob section or sidecar.
- Store: In `BlobManifest` extensions or separate CAS; link via PackageId. Feed to GlobalPackage or parse_status.
- Real integration: In `server/save/` or coordination indexing phases. Use during search indexing (tantivy package + text).
- Example (adapted main):
  ```rust
  let mut kw_insert = KeywordInsert::new()?;
  kw_insert.add_from_source(&lib_text, 1.0);
  kw_insert.add_from_readme_sections(filtered_sections);
  for (dep, w) in deps_stats { kw_insert.add_raw(format!("dep:{dep}"), w); }
  // then specifics/syns normalize
  ```
- Benefits: Richer search (keywords in tantivy), better discovery, maintainer insights (port dashboards?).

### Phase 3: Search Quality (Heuristics + Post-Processing)
- Extend `server/search/` + `registry/search/` + runtime text.
- Tokenizer: Port main's custom (Keywords with Synonyms normalize + deunicode + stem; see search_index stemmer.rs + lib).
  - In tantivy schema/analyzer for keywords/desc/readme.
- Planner/Query: Enhance `planner.rs` + symbolic surfaces. Add DYM (fuzzy on normalized), dividing_keywords (co-occur to split polysemy).
- Post-proc (after TopDocs):
  - Exact-match fixes/boosts.
  - Diversity: multi-parent style + dividing.
  - Score: Combine BM25/vector + registry signals (traction equiv) + rank_weight (like main's crate_score + downloads bubble).
  - Spam/bland suppression via Specifics.
- Category/related: Port top/related queries using keyword weights (or graph).
- APIs: Integrate in `SearchTarget` impls or new `HeuristicPostProcessor`.
- Test: Offline relevance (synthetic + real queries), A/B.

### Phase 4: Kitchen-Sink Facade (Convenience, When Needed)
- New `workspace/kitchen_sink/` (or `app_context` in server): `KitchenSink` / `AppFacade`.
  ```rust
  pub struct KitchenSink {
      registry: SourceStores<...>,
      rich_extractor: RichMetadataExtractor,
      caches: CachingLayer,  // tiered
      search_planner: SearchPlanner,
      gh_client: ...,
      throttles: (Semaphore, ...),
      event_bus: EventLog<...> or broadcast,
      prewarm_complete: AtomicBool,
      // ...
  }
  impl KitchenSink {
      pub async fn rich_metadata(&self, id: PackageId) -> Result<RichPackageMetadata> { ... }
      pub async fn search(&self, q: &str) -> SearchResults { /* heuristics + post */ }
      pub async fn prewarm(&self) { ... }
  }
  ```
- Aggregates everything. Use in server handlers, CLI tools, reindex-like. Keep composition for low-level (registry direct).
- Port patterns: Labeled blocking (adapt main/blocking + watchdog for heavy extract), DoubleCheckedCell lazy (top "crates", audits), unwind-safe, different cache expiries.
- One entry for "app" use cases.

### Phase 5: Wiring, Polish, Ops
- Ingest/emit: Call rich extract → store metadata → append outbox for search indices (keywords feed tantivy text/package).
- Caches: Wrap hot paths (metadata fetch, search hydrate, enrichment, embed already).
- Health: Extend probes for cache health (hit rate gauges via metrics).
- Config: TTLs, capacities, budgets (like planner semantic quota) via server config.
- Migration: Feature flags; dual-run old/new; data backfill via re-enqueue.
- Uptime: All changes preserve reconcile/leases/idempotency. New caches reduce load.
- Docs: Update registry/server READMEs with architecture (like main's inline + workspace's //! style).

**File Changes Sketch (Exhaustive)**:
- `workspace/Cargo.toml` / BUCK: Add deps (moka full, foyer, etc.).
- `workspace/data/`: CSV/TXT files + loader.
- `workspace/util/caching/mod.rs`, `tiered.rs`, `stampede.rs` (coalesce + probabilistic).
- `workspace/registry/metadata/{mod.rs, rich.rs, keywords.rs, categories.rs}` (ports + multi-lang).
- `workspace/registry/ingest/rich_extract.rs`.
- `workspace/server/search/{heuristics.rs, postproc.rs}` (tokenizer, DYM, dividing).
- `workspace/kitchen_sink/{lib.rs, caches.rs, extractor.rs}` (facade).
- Updates: `server/coordination/indexing.rs` (call extract), `server/search/*`, runtime/text (if keyword boost), tests (new determinism for heuristics, stampede tests).
- Port helpers: `main/util/`, blocking patterns if useful.
- Tests: `workspace/.../tests/rich_metadata.rs`, `cache_stampede.rs`, `search_quality.rs` (like main's implicit + workspace's focused).

**Risks/Tradeoffs/Mitigations**:
- Complexity: Incremental (start caches + data, then extraction). Use existing compiler for code parts.
- Perf/Mem: Weighters + capacities + tiers. Measure.
- Data drift: Deterministic ports + tests.
- Errors: Careful with non-Clone (like current embedding cache).
- Ops: Add metrics (hits, stampede coalesces, extraction time), lag for new derived.
- If no remote: foyer disk sufficient for main-like TempCache.

**Success Metrics**:
- Cache: >80% hit on metadata/search; stampede tests show 1x compute vs N.
- Search: Qualitative improvement (more relevant via keywords/synonyms/diversity); measurable DYM success, category coverage.
- Extraction: Auto-keywords/cats for >X% packages; rich data in blobs/search.
- Dev: Simpler server/CLI code via facade where used.
- Uptime: No regression; better under load (less backend hits).

**Timeline/Execution**: Use execute-plan style DAG if needed. Start with caching (biggest robustness win), data+heuristics (search quality), extraction, facade last.

This is exhaustive, actionable, backed by real main/ snippets (via analysis), real crate APIs (moka builder, foyer HybridCacheBuilder/fetch, etc.), and researched techniques (probabilistic from paper/Cloudflare, hybrid from foyer docs, coalescing patterns). Port conservatively to leverage workspace's existing strengths in durability and structure.

Next steps: Review, prototype caching tier in a branch, port Synonyms first (pure).
