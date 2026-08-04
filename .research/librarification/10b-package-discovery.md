# Multi-Language Package Discovery Abstraction

**Research date:** 2026-07-16  
**Scope:** Package registry search ranking for all ecosystems Nudox indexes — not symbol search.  
**Companion:** [10-tantivy.md](./10-tantivy.md) owns **symbol** multi-lang FTS; this report owns **package discovery**.  
**Status:** Synthesis of lib.rs monorepo reverse-engineering (GitLab `lib.rs/main`), cross-registry prior art (npms, crates.io RFC 1824, PyPI Warehouse, libraries.io SourceRank, pkg.go.dev, NuGet, Maven, Ruby Toolbox, Homebrew), live Nudox code audit, and subagent deep dives.

---

## 0. Executive summary (TL;DR)

Nudox already has **two of three layers** of a lib.rs-class package discovery system:

| Layer | What it is | Nudox today |
|---|---|---|
| **A — Ecosystem adapters** | Per-language identity, metadata, downloads, dep graph, spam flags | Identity strong; rich extract **Cargo-only**; downloads **absent** |
| **B — Quality engine** | Offline multi-signal score → single `quality ∈ [0,1]` | Partial static manifest+README scorer; **no temporal/graph/trust** |
| **C — SERP post-processor** | BM25×kink → name boost → diversity → pull-up → bubble | Ported accurately; **downloads=0 makes pull-up/bubble dead** |

**lib.rs quality is not smarter BM25.** It is (1) a rich offline quality model, (2) a deliberate text×quality kink, (3) a SERP post-processor for name intent, sense diversity, and household-name visibility. Nudox correctly ports (3) and a thin slice of (1). Matching lib.rs — and **beating default registries for non-Rust** — depends on generalizing (1) with ecosystem adapters, not further tuning (3).

**Recommended design:** three layers + a **lexical core** (lib.rs micro-opts, fully specified):

1. `EcosystemAdapter` — **one core trait** with associated types + **associated constants** for grammar, boosts, SERP polish knobs, stopword packs  
2. **Static quality scoring** — one free function over a filled `PackageSignals` struct (lib.rs style); **no `dyn` plugins**  
3. `rank_pipeline` / `retrieve_and_rank` — **one free-function path** for local and remote (same stages; only storage backend differs)  
4. Shared value types: `KeywordLexicon`, `TextNormalizer`, `IndexEnrichPolicy`, `SerpStages` — data + pure fns  
5. **`LocalEnrichment` sidecar** (desktop only, tiny) — soft boosts + local-only rows (forks/path deps); **not** a second discovery scope in the core ranker  

Plus: intent-aware fusion (NAVIGATE vs EXPLORE), per-ecosystem percentile popularity, hard trust gates, golden-query eval. **Never** one global score across languages.

**Micro-opts are not optional folklore** — §5.2–§5.4 inventory every lib.rs-class lexical/SERP optimization and pin it to a type, constant, or stage.

---

## 1. Why this report exists

[10-tantivy.md](./10-tantivy.md) deliberately keeps package ranking **package-only** and focuses multi-lang work on **symbols**. That left an open hole:

> How do we make package discovery on-par with (and above) lib.rs **for languages beyond Rust**?

Default registries lose on **explore** queries (`http client`, `yaml parser`):

| Registry | Default approach | Failure mode |
|---|---|---|
| crates.io search | Postgres `ts_rank_cd` fielded FTS | Exact name land-grabs; weak quality |
| crates.io category lists | RFC 1824: 90-day downloads | CI noise; abandoned kings |
| PyPI | OpenSearch BM25; downloads not in default relevance | Keyword stuffing beats mature packages (`yaml` ≠ PyYAML) |
| npm (legacy) | Downloads / ES alone | Spam + abandoned |
| Maven Central | Coordinate/Solr identity | Not “best library for X” |

Specialized discovery wins by multi-axis fusion + opinionated demotion. **lib.rs** (Rust) and **npms.io** (JS) are the two open gold standards.

---

## 2. lib.rs reverse-engineering (canonical)

### 2.1 Sources

| Resource | URL |
|---|---|
| Monorepo | https://gitlab.com/lib.rs/main |
| Site / philosophy | https://lib.rs , https://lib.rs/about , https://lib.rs/data-processing |
| Ranking discussion (author) | https://users.rust-lang.org/t/improving-ranking-and-crate-search/26765 |

Core crates: `search_index` (SERP), `ranking` (quality), `reindex`, `kitchen_sink` (data), `feat_extractor` (spam/keywords), `user_db` (owner trust), `deps_index` (rev-deps), `categories` (synonyms/bland/specific).

**Nudox port mapping:**

| lib.rs | Nudox | Completeness |
|---|---|---|
| `search_index` post-processor | `workspace/registry/search/ranking.rs` | ~high for stages C |
| `ranking` Score algebra | `metadata/rich.rs` `Score` | algebra yes |
| `crate_score_version` static | `compute_quality` | thin subset |
| `crate_score_temporal` | — | **missing** |
| `combined_score` multipliers | — | **missing** |
| `kitchen_sink` traction/downloads | — | **missing** |
| `user_db` trust graph | — | **missing** |
| Keyword synonyms/bland/specific | `metadata/heuristics.rs` | tables present; Rust-centric |

### 2.2 Quality score (the real moat)

**Algebra** (same as Nudox `Score`):

```text
total() = Σ clamp(earned, 0, max_i) / Σ max_i  ∈ [0, 1]
```

**Static groups** (`crate_score_version`): Cargo.toml completeness, README structure (DOM: text/code/images/sections), code size + clippy, version history maturity, authors/owners experience.

**Temporal groups** (`crate_score_temporal`): growth, deadness, release freshness (maturity-adaptive expected update interval), dep freshness, **log downloads cleaned of top reverse-consumer**, active reverse users, direct/indirect rev-deps, owner reputation, Debian endorsement, crev/vet, brand-new bonus, former_glory decline.

**Combined:**

```text
base = static.total()
temp = temporal.total()   # ×0.8 if autopublished
excels = max(base, temp)
score = base*0.4 + temp*0.5 + (excels*0.1 if excels > 0.3 else 0)
score = 1 − (1 − score)^1.3          # linearize near-zero clustering
score *= former_glory
# structural multipliers: proc_macro, sys, sub_component, internal,
#   deprecated×0.2, unmaintained×0.4, crypto-vaporware×0.4,
#   non-crates.io×0.75, yanked|squat×0.001, blocklist …
# Index: delete docs with score ≤ 0.001
```

### 2.3 Search pipeline (online)

```text
1. Query parse (conjunction; field boosts: name 2.0, kw 1.5, desc 1.1, extra 0.6, readme 0.4)
2. BM25 retrieve expanded_limit with tweak_score: q>0.4 → q+=1; fused = BM25 * q
3. Force exact crate_name if missing
4. Did-you-mean fuzzy if top quality weak
5. assign_doc_score: exact/contains name boosts with top-4 cap, ≤5 contains boosts
6. Sort; discard low-quality tail
7. Dividing-keywords diversity (+ re-query excluding too-common keyword)
8. move_representative_crates_to_top (quality + downloads floors)
9. downloads_bubble on tails [2..] and [5..]
10. UI good/bad split + "search also"
```

### 2.4 Why lib.rs beats crates.io

1. Quality **precomputed** so BM25 cannot promote abandoned keyword-spam.  
2. Downloads cleaned + reverse-users, not raw CI downloads.  
3. Owner trust + editorial multipliers first-class.  
4. SERP enforces name intent + sense diversity + household names.  
5. Spam/yank effectively deleted from index (`×0.001` → drop).

### 2.5 Multi-ecosystem awareness in lib.rs

**None.** Origins are crates.io | GitHub | GitLab **Rust crates** only. Debian is an endorsement bit. **All absolute download floors are crates-scale.**

---

## 3. Cross-registry prior art (what to steal)

### 3.1 Inventory

| System | Core ranking idea | Steal for Nudox |
|---|---|---|
| **lib.rs** | Offline quality × BM25 kink + SERP stages | Primary SERP + quality algebra pattern |
| **npms.io** | Q×0.3 + P×0.35 + M×0.35; Bézier vs eco mean; ES text | Transparent Q/P/M axes; distribution-aware curves |
| **crates.io RFC 1824** | Category lists = 90d downloads only | Explicit rejection of opaque quality for *category* sort — we still need it for *search* |
| **crates.io search** | `ts_rank_cd` A/B/C/D weights | Fielded name-first; no quality blend (failure mode) |
| **PyPI Warehouse** | BM25; name^10, phrase exact^1000; separate trending sort | Strong navigate exact; weak explore without downloads |
| **libraries.io SourceRank** | ~18 additive metrics multi-eco | Feature *family*; **not** security oracle (SourceBroken: URL confusion, removal lag) |
| **pkg.go.dev** | Text + module grouping + import graph + **symbol search** | Version collapse; import-count popularity; API discovery UX |
| **NuGet** | Azure Search + download weight; TFM filters | Capability filters; namespace gravity |
| **Maven / mvnrepository** | Coordinate search; reverse-deps for categories | Namespace authority as trust |
| **Ruby Toolbox** | Curated categories + relative bars | Browse UX > infinite global lists |
| **Homebrew** | `install_on_request` vs install | App vs lib popularity distinction |
| **OpenSSF Scorecard** | Security checks 0–10 | Trust facet, never primary relevance |

### 3.2 npms final score (verified from source)

```js
// npms-io/npms-analyzer lib/scoring/score.js
final = 0.30 * quality + 0.35 * popularity + 0.35 * maintenance
// quality: carefulness 7, tests 7, health 4, branding 2
// popularity: communityInterest 2, downloadsCount 2, downloadsAcceleration 1
// maintenance: releasesFrequency 2, commitsFrequency 1, openIssues 1, issuesDistribution 2
// Per-metric: normalize to eco aggregation, map through cubic Bézier vs mean
```

### 3.3 Unified signal taxonomy

| Axis | Portable signals |
|---|---|
| **A Name/ID** | Exact, normalized (`-_`), scoped `@org/pkg`, groupId:artifact, module path, prefix |
| **B Text** | Fielded BM25: name ≫ keywords ≫ summary ≫ description ≫ README |
| **C Popularity** | Windowed downloads, acceleration, reverse-deps, import graph, install_on_request (apps) |
| **D Quality** | README structure, license, tests/CI, docs URL, ≥1.0, dep hygiene |
| **E Trust** | Verified repo, maintainer age/org, malware/yank, typosquat risk, signed releases |
| **F Freshness** | Last release (maturity-adaptive), commit freq, migration-away rate |
| **G Graph** | PageRank, related packages (`-cli`, `-derive`), replacement edges |
| **H Capability** | Platform, language version, lib vs app (filters more than rank) |

### 3.4 Failure modes (must harden against)

| Failure | Mitigation |
|---|---|
| Typosquatting | Near-name of popular package → trust penalty; exact boost × trust/quality |
| Abandoned high rank | Recency windows; former_glory; outdated-deps; freshness decay |
| Cross-eco name collisions | Always scope by ecosystem; never merge `npm:yaml` and `pypi:yaml` |
| Keyword stuffing | Cap keyword field influence; BM25 length norms; quality gate |
| Version spam | Index package-level docs; collapse versions (Go-style) |
| Download inflation | Clean top consumer; prefer dependents; windowed + acceleration |
| URL confusion | Verify repo contains package identity (SourceBroken lesson) |
| Land-grab exact names | Exact match only if quality/trust above floor |
| Security theater | Fresh removal feeds; Scorecard as soft; never SourceRank alone |

---

## 4. Nudox current state (package discovery only)

### 4.1 Package Tantivy schema

| Field | Indexed | Populated | Queried |
|---|---|---|---|
| `package_id` | STRING | Yes | Upsert/hydrate |
| `name` | TEXT | Yes (canonical + original) | Yes |
| `description` | TEXT | **Never** | Declared empty |
| `keywords` | TEXT | Iff facets | Yes |
| `ecosystem` | STRING | Yes | **Post-hydrate only**; HTTP forces `None` |
| `record` | STORED | Full JSON `GlobalPackage` | Hydrate |

No FAST `quality` / `downloads` / `popularity` fields. No field boosts on QueryParser.

### 4.2 Ranking inputs at candidate build

```text
// workspace/registry/search/mod.rs collect_ranked_hits
quality   ← facets.quality() | NEUTRAL 0.5
downloads ← 0                    // HARDCODED
keywords  ← facets.keywords | []
name      ← coordinates.canonical
bm25      ← tantivy score
```

**Effect:** pull-up download branch and downloads_bubble never fire. Quality pull-up still works when facets exist.

### 4.3 Quality / keywords pipeline

- `metadata/rich.rs` `extract` + `compute_quality` — lib.rs-shaped but **manifest completeness + README + loc only**.  
- `server/.../extract_facets`: **Rust Cargo.toml+README**; non-Rust → name + compiler identifiers only.  
- `loc = 0`, `dependencies = []` even for Rust at indexing — loc/dep paths inert.  
- Categories computed then **dropped** in `SearchFacets` (only keywords + quality_ppm).  
- `contains_query_names` strips `cargo`/`rust`/`rs` — **Rust-only**, wrong for npm/PyPI.

### 4.4 What is already multi-language-ready

- `Language` / `PackageName` / `RegistryOrigin` identity  
- Multi-parent merge `(ecosystem, name, version)`  
- Generic `Candidate<T>` ranking payload  
- Keyword dividing diversity (content-agnostic)  
- Replica-local disposable Tantivy + watermark sync  

### 4.5 Gap matrix vs lib.rs-class multi-eco discovery

| Capability | Status |
|---|---|
| SERP stages C | Present; downloads dead |
| Static quality thin | Partial, Cargo-biased |
| Temporal quality | Missing |
| Downloads ingestion | Missing |
| Rev-deps / traction | Missing |
| Owner trust | Missing |
| Per-eco name grammar | Missing (Rust hardcode) |
| Per-eco RankingConfig | Missing |
| Description in index | Schema ghost |
| Ecosystem query filter | Post-hoc; API unscoped |
| Field boosts | Missing |
| Intent NAVIGATE/EXPLORE | Missing |
| Golden eval harness | Missing |
| Manifest extractors per lang | Rust only |

---

## 5. Target architecture: Package Discovery abstraction

### 5.1 Layers + lexical core (normative)

```
┌─────────────────────────────────────────────────────────────────────────┐
│  KeywordLexicon + TextNormalizer + IndexEnricher   (shared pure core)   │
│  data tables + free functions — NOT a trait per stopword list           │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │ used by extract + query + index write
┌────────────────────────────────▼────────────────────────────────────────┐
│ Layer A — EcosystemAdapter (ONE trait per language)                     │
│  associated type Lexicon; associated consts: GRAMMAR, BOOSTS, SERP,     │
│  POP_SCALE, COMBINED_WEIGHTS; methods: extract / popularity / deps /    │
│  lifecycle  only where I/O or eco-specific logic lives                  │
└────────────────────────────────┬────────────────────────────────────────┘
                                 ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Layer B — compute_quality (FREE FUNCTION over PackageSignals)           │
│  Score algebra; static ⊕ temporal ⊕ multipliers; Q/P/M/T/F axes         │
│  Missing signals = None → group contributes 0 / skipped; no dyn         │
└────────────────────────────────┬────────────────────────────────────────┘
                                 ▼
┌────────────────────────────────────────────────────────────────────────┐
│ Layer C — retrieve_and_rank (ONE pipeline, local == remote)             │
│  PackageIndex backend only · same rank_pipeline · same consts           │
└────────────────────────────┬───────────────────────────────────────────┘
                             ▼  (desktop client only, after ranked page)
┌────────────────────────────────────────────────────────────────────────┐
│ LocalEnrichment sidecar (NOT part of INDEX; never required for quality) │
│  usage prior · "in project" badge · inject local forks/path deps        │
│  re-order soft boosts only · local-only docs never uploaded             │
└────────────────────────────────────────────────────────────────────────┘
```

**Design rule — when to use what:**

| Mechanism | Use for |
|---|---|
| **Associated `const` on `EcosystemAdapter`** | Tunables that every eco must declare (grammar, field boosts, SERP stage flags, kink, bubble floors). **Highest leverage** — one place to audit Cargo vs npm. |
| **Associated type** | `Lexicon` so Cargo embeds lib.rs tables and npm smaller packs — **static type**, not `dyn`. |
| **Plain `struct` + data files** | Stopwords, synonyms, bland/specific, org trust, DYM easter eggs — load once, share. |
| **Free functions** | `normalize_keyword`, `compute_quality`, `rank_pipeline`, `retrieve_and_rank`, `enrich_document`, `classify_intent` — pure, monomorphizable, no vtable. |
| **`PackageSignals` with `Option` fields** | All quality inputs (incl. graph, Scorecard). Unfilled = absent. **Not** a plugin trait. |
| **Thin `PackageIndex` trait** (or enum) | Only the storage backend: local mmap Tantivy vs remote HTTP search. **Same call sites.** |
| **Do NOT** | `dyn QualityFeature`, per-feature trait objects, separate local vs remote ranking codepaths. |
| **Do NOT trait** | Stopword membership, synonym lookup, bland check — **data lookups** on `KeywordLexicon`. |

**Crate topology:**

| Component | Crate home |
|---|---|
| `PackageDocument`, axes, flags | `heart` |
| `KeywordLexicon`, `TextNormalizer`, `IndexEnricher`, `NameGrammar`, `FieldBoosts`, `SerpStages` | `text-search` (`package_lex/`) |
| `EcosystemAdapter` + Cargo/npm/pypi/… ZSTs | `text-search` or `ingest` |
| `PackageSignals`, `compute_quality`, `Score` | `text-search` (`package_quality/`) |
| `rank_pipeline`, `retrieve_and_rank`, `RankingConfig` | `text-search` (today `search/ranking.rs`) |
| `PackageIndex` (local Tantivy / remote HTTP) | `text-search` + thin client adapter |
| **`LocalEnrichment` sidecar** | `registry-local` / `client` — usage DB + local-only package docs |
| Eval golden harness | `text-search/tests` |

---

### 5.2 Full micro-optimization inventory (normative)

Every lib.rs-class “small” optimization lands in exactly one place. **None are “later folklore.”**

#### 5.2.1 Extract / keyword bag (`KeywordLexicon` + `rich::extract`)

| Optimization | Spec | Home |
|---|---|---|
| Prose stopwords | Sorted `&[&str]` binary_search; drop from description/readme keywords | `Lexicon::STOPWORDS` (+ eco add/remove) |
| Ident stopwords | Drop from identifier-derived keywords (`get`, `set`, `new`…) | `Lexicon::IDENT_STOPWORDS` |
| `normalize_keyword` | Shared pure fn: kebab, I/O→io, C++→cpp, plurals, %, &, byte suffixes, truncate 55–65 | free fn (already in `heuristics.rs`) |
| Well-known token replacements | Table inside normalizer | `TextNormalizer::WELL_KNOWN` |
| Synonym collapse | vote-weighted map → canonical keyword; depth-limited | `Lexicon::synonyms` (CSV load) |
| Bland keywords | weight × `BLAND_WEIGHT` (default 0.3) at extract; diversity weight ×0.75 | `Lexicon::bland` |
| Specific keywords | rare technical terms; boost weight / specificity DSL | `Lexicon::specifics` |
| Manifest kw weight 1.0 | Fixed extract recipe | const in extract recipe |
| Categories as kw 0.7 | Fixed | same |
| Name parts as kw 0.6 | Skip list eco-specific (`rs`, `impl` for Cargo) | `NameGrammar::name_part_skip` |
| Description prose 0.6 | stopword-filtered | extract recipe |
| README prose 0.3 × section | skip license/install/boilerplate sections | `ReadmeSectionPolicy` |
| Identifiers 0.25 | camel/snake split; ident stopwords | extract |
| `dep:name` invisible 0.2 | strip from visible bag | extract |
| Cap keywords @ 20 | top by weight; renormalize top=1.0 | extract |
| Categories derive | manifest or infer from keywords | extract → **persist** in facets (today dropped — fix) |

#### 5.2.2 Index enrichment (`IndexEnricher` — pure, called from `absorb`)

| Optimization | Spec | Home |
|---|---|---|
| Dual name fields | TEXT (BM25) + STRING exact (`normalized_name`) | schema |
| Name always in keywords | append canonical name token(s) to keywords field | enricher |
| Dash / separator splits | `aws-s3` → extra tokens `aws`, `s3`, `awss3` (no sep) on `extra` | enricher |
| Scoped npm names | `@scope/pkg` → exact full + `pkg` token on extra | `NameGrammar::scoped` |
| Description populate | never leave schema ghost empty | absorb **must** write |
| README size cap | e.g. first 32–64 KiB text for index | enricher const |
| `extra` TEXT field | auto-keywords, dash parts; boost 0.6 | schema + enricher |
| FAST quality / downloads / pop_pct | for collector kink + bubble without hydrate | schema |
| Index delete if quality ≤ ε | ε = 0.001 (spam/yank effectively invisible) | absorb policy |
| SCHEMA_VERSION marker | force rebuild on lexicon/schema major | disk |

#### 5.2.3 Query parse / retrieve

| Optimization | Spec | Home |
|---|---|---|
| Field boosts | name 2.0, keywords 1.5, description 1.1, extra 0.6, readme 0.4 | `FieldBoosts` associated const |
| Conjunction default | all terms required unless OR rewrite | query builder |
| Parse failure rewrite | non-alnum → space, lower, reparse | free fn |
| Stemmer on description/readme | optional English stemmer; **off** for name/keywords | tokenizer reg; `TextNormalizer::STEM_PROSE` |
| Deunicode on keywords | optional; on for prose if non-ASCII corpora matter | `TextNormalizer::DEUNICODE` |
| Ecosystem MUST filter | when scope/eco set | query builder |
| Over-fetch | `max(limit*4+32, 200)` or lib.rs `limit+50+limit/2` | `SerpStages::over_fetch` |
| Quality kink in collector | `if q > KINK { q += 1 }; score = bm25 * q` | `RankingConfig::quality_kink_threshold` (0.4) |
| Expanded retrieve only | never full-corpus sort | invariant |

#### 5.2.4 Name match fusion (SERP stage)

| Optimization | Spec | Home |
|---|---|---|
| Exact name bonus | flat +10 or `max_rel * match_mult` (lib.rs full form preferred) | `rank_pipeline` |
| Exact min quality floor | require `quality > 0.15` to take exact boost | `SerpStages::exact_min_quality` |
| Contains name boost | ≤5 boosts; cap `(top4*3+boosted)/4` | already in ranking.rs |
| Prefix/suffix strip | Cargo: cargo/rust + rs; others eco table | `NameGrammar` |
| Sep equivalence | `-` `_` treated equal in contains | `NameGrammar::equivalent_seps` |
| Specific query factor | has `-`/`_` or len>15 → stronger name mult | lib.rs assign_doc_score |
| Quality² bonus in name mult | `min((q²+0.25)*2, 1.1)` | optional full lib.rs form |

#### 5.2.5 Diversity / representatives / bubble

| Optimization | Spec | Home |
|---|---|---|
| Dividing keywords | pop in (2, 5/8 N]; bland ×0.75; good band ×2 | ranking.rs |
| Min set size 25 | skip diversity if smaller | `RankingConfig` |
| Hard diversity denylist | tokens ignored as dividing senses (crypto spam families) | `Lexicon::DIVERSITY_DENY` |
| Re-query exclude common kw | optional stage when keyword too common | `SerpStages::requery_common` |
| Representative pull-up | quality floor max(0.97*max, 0.55); dl floor max(0.9*max, eco) | config + `PopularityScale` |
| Downloads bubble | mid-band adjacent swap ×3 | config |
| Percentile bubble | multi-eco default | `PopularityScale::use_percentiles` |

#### 5.2.6 SERP polish (optional stages, still specified)

| Optimization | Spec | Home |
|---|---|---|
| Exact-name force repair | if navigate-shaped query and no exact in top‑K, force `normalized_name` term, append ≤3 | `SerpStages::exact_repair` |
| Did-you-mean | only if top-3 quality weak; FuzzyTerm distance 1–2; filter quality>0.1 | `SerpStages::dym` |
| DYM easter eggs | data map `tokyo→tokio` | `Lexicon::dym_overrides` |
| OR fallback | if few/weak results, rewrite spaces/hyphens as OR once | `SerpStages::or_fallback` |
| Good/bad split | UI: good while score≥0.33*top and base≥min | presentation; config thresholds |
| “Search also” chips | dividing keywords as refinements | UI |
| Related-package collapse | group `foo`/`foo-cli`/`foo-macros` | post-policy |
| Chaff discard | drop tail quality ≤0.01 after keep_at_least | `SerpStages::discard_chaff` |

#### 5.2.7 What stays out of the hot path

Graph features, Scorecard, owner trust series — **offline jobs** fill `PackageSignals`, then `compute_quality` writes FAST fields. Never HTTP and never re-score quality inside `rank_pipeline` (local or remote).

---

### 5.3 Core types (not everything is a trait)

```rust
// ─── Lexical data (struct, load once) ─────────────────────────────────────

/// Per-ecosystem (or shared) keyword data. Binary-search sorted slices + maps.
pub struct KeywordLexicon {
    pub stopwords:        &'static [&'static str],      // or owned Arc after load
    pub ident_stopwords:  &'static [&'static str],
    pub synonyms:         SynonymMap,     // from tag-synonyms.csv
    pub bland:            HashSet<SmolStr>,
    pub specifics:        SpecificsGraph, // specific-keywords DSL
    pub diversity_deny:   &'static [&'static str],
    pub dym_overrides:    &'static [(&'static str, &'static str)],
}

impl KeywordLexicon {
    pub fn is_stopword(&self, w: &str) -> bool { /* binary_search */ }
    pub fn is_ident_stopword(&self, w: &str) -> bool { /* … */ }
    pub fn is_bland(&self, w: &str) -> bool { self.bland.contains(w) }
    pub fn canonical_synonym(&self, w: &str, min_votes: u8) -> (&str, f32) { /* … */ }
}

/// Shared pure normalizer. Eco can set flags via associated const, not subclassing.
pub struct TextNormalizer {
    pub stem_prose: bool,    // description/readme only
    pub deunicode: bool,
    pub well_known: &'static [(&'static str, &'static str)],
}

pub fn normalize_keyword(raw: &str, norm: &TextNormalizer) -> SmolStr { /* existing heuristics + flags */ }

/// Field boosts for QueryParser — associated const on adapter.
#[derive(Clone, Copy, Debug)]
pub struct FieldBoosts {
    pub name: f32,        // 2.0
    pub keywords: f32,    // 1.5
    pub description: f32, // 1.1
    pub extra: f32,       // 0.6
    pub readme: f32,      // 0.4
}

impl Default for FieldBoosts {
    fn default() -> Self {
        Self { name: 2.0, keywords: 1.5, description: 1.1, extra: 0.6, readme: 0.4 }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct NameGrammar {
    pub strip_prefixes: &'static [&'static str],
    pub strip_suffixes: &'static [&'static str],
    pub equivalent_seps: &'static [char],
    pub name_part_skip: &'static [&'static str],
    pub scoped: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct PopularityScale {
    pub use_percentiles: bool,
    pub bubble_min: u64,
    pub bubble_max: u64,
    pub bubble_ratio: u64,
    pub representative_floor: u64,
    pub representative_fraction: f32,
}

/// Which SERP stages run — associated const; free function respects flags.
#[derive(Clone, Copy, Debug)]
pub struct SerpStages {
    pub over_fetch_base: u32,       // 32
    pub over_fetch_mult: u32,       // 4
    pub over_fetch_min: u32,        // 200
    pub exact_repair: bool,
    pub exact_min_quality: f32,     // 0.15
    pub dym: bool,
    pub or_fallback: bool,
    pub requery_common: bool,
    pub discard_chaff: bool,
    pub related_collapse: bool,
    pub good_bad_split: bool,       // UI may read this
}

impl Default for SerpStages {
    fn default() -> Self {
        Self {
            over_fetch_base: 32,
            over_fetch_mult: 4,
            over_fetch_min: 200,
            exact_repair: true,
            exact_min_quality: 0.15,
            dym: true,
            or_fallback: true,
            requery_common: true,
            discard_chaff: true,
            related_collapse: true,
            good_bad_split: true,
        }
    }
}

/// Index-time enrichment policy (const or small struct).
#[derive(Clone, Copy, Debug)]
pub struct IndexEnrichPolicy {
    pub emit_dash_parts: bool,
    pub emit_name_into_keywords: bool,
    pub readme_max_bytes: usize,    // 64 * 1024
    pub quality_delete_epsilon: f32, // 0.001
    pub extra_field: bool,
}

/// Extract weight recipe (const) — lib.rs keyword bag weights.
#[derive(Clone, Copy, Debug)]
pub struct ExtractWeights {
    pub manifest_kw: f32,    // 1.0
    pub category: f32,       // 0.7
    pub name_part: f32,      // 0.6
    pub description: f32,    // 0.6
    pub readme: f32,         // 0.3
    pub identifier: f32,     // 0.25
    pub dependency: f32,     // 0.2
    pub bland_mul: f32,      // 0.3
    pub max_keywords: usize, // 20
}
```

---

### 5.4 Core trait: `EcosystemAdapter` (associated types + consts)

**One trait.** Methods only where work is eco-specific I/O or parsing. Constants carry the micro-opts that “do the most good” when wrong (grammar, boosts, SERP, scales).

```rust
pub trait EcosystemAdapter: Send + Sync + 'static {
    // ── identity ──────────────────────────────────────────────────────────
    const LANGUAGE: Language;

    // ── lexical / SERP knobs (AUDIT THESE FIRST when results look wrong) ──
    type Lexicon: Borrow<KeywordLexicon> + Default + Send + Sync + 'static;

    const GRAMMAR: NameGrammar;
    const BOOSTS: FieldBoosts;
    const EXTRACT_WEIGHTS: ExtractWeights;
    const ENRICH: IndexEnrichPolicy;
    const SERP: SerpStages;
    const RANKING: RankingConfig;       // kink, diversity, pull-up, bubble
    const POP_SCALE: PopularityScale;
    const NORMALIZER: TextNormalizer;   // stem/deunicode flags
    const COMBINED: CombinedWeights;    // quality static/temp mix (0.4/0.5/0.1)

    /// Optional editorial org trust table (empty OK).
    const ORG_TRUST: &'static [(&'static str, u16)] = &[];

    // ── I/O & eco-specific logic ──────────────────────────────────────────
    fn lexicon() -> Self::Lexicon { Self::Lexicon::default() }

    fn extract_text(&self, ctx: &ExtractCtx<'_>) -> PackageTextFields;

    fn popularity_raw(&self, ctx: &SignalCtx<'_>) -> PopularityRaw;

    fn dependency_edges(&self, ctx: &SignalCtx<'_>) -> Vec<PackageCoord>;

    fn lifecycle_flags(&self, ctx: &SignalCtx<'_>) -> LifecycleFlags;

    /// Navigate vs explore name shape (default uses GRAMMAR).
    fn looks_like_package_id(&self, query: &str) -> bool {
        default_looks_like_id(query, &Self::GRAMMAR)
    }
}
```

**Cargo example (constants do the lib.rs work):**

```rust
pub struct CargoAdapter;

impl EcosystemAdapter for CargoAdapter {
    const LANGUAGE: Language = Language::Rust;
    type Lexicon = CargoLexicon; // stopwords+synonyms+bland from data/

    const GRAMMAR: NameGrammar = NameGrammar {
        strip_prefixes: &["cargo", "rust"],
        strip_suffixes: &["rs"],
        equivalent_seps: &['-', '_'],
        name_part_skip: &["rs", "impl", "internal", "shared"],
        scoped: false,
    };
    const BOOSTS: FieldBoosts = FieldBoosts { /* defaults */ };
    const SERP: SerpStages = SerpStages { /* defaults; dym true */ };
    const RANKING: RankingConfig = RankingConfig {
        quality_kink_threshold: 0.4,
        // … lib.rs defaults …
        representative_downloads_floor: 100_000,
        bubble_downloads_min: 200,
        bubble_downloads_max: 1_000_000,
        bubble_ratio: 3,
        ..
    };
    const POP_SCALE: PopularityScale = PopularityScale {
        use_percentiles: false, // absolute until CDF job ready
        bubble_min: 200,
        bubble_max: 1_000_000,
        bubble_ratio: 3,
        representative_floor: 100_000,
        representative_fraction: 0.9,
    };
    const NORMALIZER: TextNormalizer = TextNormalizer {
        stem_prose: true,
        deunicode: true,
        well_known: DEFAULT_WELL_KNOWN,
    };
    // extract_text: Cargo.toml + README …
}
```

**npm example (same trait, different consts):**

```rust
const GRAMMAR: NameGrammar = NameGrammar {
    strip_prefixes: &[],
    strip_suffixes: &["js", "ts"], // optional, usually empty better
    equivalent_seps: &['-', '_', '.'],
    name_part_skip: &["js", "ts", "node"],
    scoped: true,
};
const POP_SCALE: PopularityScale = PopularityScale {
    use_percentiles: true, // never use crates absolute floors
    ..
};
const RANKING: RankingConfig = RankingConfig {
    // percentile mode ignores absolute bubble floors
    representative_downloads_floor: 0,
    bubble_downloads_min: 0,
    ..
};
```

**Registry of adapters (not a trait hierarchy):**

```rust
pub struct AdapterRegistry {
    by_lang: EnumMap<Language, &'static dyn AnyAdapter>, // thin type-erased façade
}

// Type erasure only at the boundary of “pick adapter by Language”:
pub trait AnyAdapter: Send + Sync {
    fn language(&self) -> Language;
    fn grammar(&self) -> NameGrammar;
    fn boosts(&self) -> FieldBoosts;
    fn serp(&self) -> SerpStages;
    fn ranking(&self) -> RankingConfig;
    fn pop_scale(&self) -> PopularityScale;
    fn lexicon(&self) -> &KeywordLexicon;
    fn normalizer(&self) -> TextNormalizer;
    fn extract_text(&self, ctx: &ExtractCtx<'_>) -> PackageTextFields;
    // …
}
// blanket impl AnyAdapter for T: EcosystemAdapter
```

---

### 5.5 Free-function SERP pipeline (Layer C)

**No `PackageRanker` trait.** One pipeline, parameterized:

```rust
pub fn rank_pipeline<T>(
    cfg: &RankingConfig,
    serp: &SerpStages,
    grammar: &NameGrammar,
    lexicon: &KeywordLexicon,
    query: &str,
    intent: QueryIntent,
    candidates: Vec<Candidate<T>>,
    limit: usize,
) -> RankOutput<T> {
    // 1. fuse_scores (kink + exact/contains with grammar + exact_min_quality)
    // 2. sort
    // 3. discard_chaff if serp.discard_chaff
    // 4. diversity_pass (bland from lexicon; skip diversity_deny as dividers)
    // 5. pull_up_representatives
    // 6. downloads_bubble / percentile_bubble
    // 7. related_collapse if serp.related_collapse
    // 8. truncate limit
    // return hits + optional DymSuggestions + SearchAlso chips
    // NOTE: no project/DepSet scope, no local usage — that is LocalEnrichment (§5.10)
}

pub fn retrieve_and_rank(
    index: &PackageIndexBackend, // Local Tantivy replica | Remote HTTP — same ranked semantics
    adapters: &AdapterRegistry,
    req: &PackageSearchRequest, // text, ecosystem?, limit, cursor — NOT project scope
) -> Result<Page<PackageHit>, SearchError> {
    let intent = classify_intent(&req.text, /* grammar from eco */);
    let adapter = adapters.get(req.ecosystem /* or multi */);
    // build query with BOOSTS, optional eco filter
    // over_fetch from SERP → exact_repair? → rank_pipeline → dym?/or_fallback?
}

// Desktop client only — AFTER retrieve_and_rank (or after remote page returns):
// page = local_enrichment.apply(page, LocalContext { project, usage_db, local_only_docs })
```

```rust
#[derive(Clone, Copy, Debug)]
pub enum QueryIntent { Navigate, Explore, SymbolHint }

pub fn classify_intent(raw: &str, grammar: &NameGrammar) -> QueryIntent {
    if quoted(raw) || looks_like_package_id(raw, grammar) { Navigate } else { Explore }
}
```

**Intent fusion weights** (const on shared `IntentWeights`, not per-eco unless needed):

```text
Navigate: 0.70 S_text + 0.15 P + 0.10 T + 0.05 Q
Explore:  0.35 S_text + 0.25 P + 0.20 Q + 0.15 M + 0.05 F
```

---

### 5.6 Quality scoring (Layer B) — static, no `dyn`

#### What `dyn QualityFeature` was (rejected)

An earlier sketch used `Vec<Box<dyn QualityFeature>>` so “expensive” signals (Scorecard, graph) could be plugged in at runtime. That is **rejected**:

| Problem | Why it hurts |
|---|---|
| Vtable / allocation | Offline scoring runs on every package reindex — pay for flexibility never used at query time |
| Open set of features | Harder to audit, serialize explanations, golden-test |
| Local vs remote drift | Easy to register different plugin sets per plane |
| Adapter already varies | Ecosystems differ in **which `PackageSignals` fields they fill**, not which code runs |

#### Normative design: fill a struct, run one function

lib.rs pattern: gather inputs → one `combined_score` / `compute_quality`. Nudox:

```rust
/// All quality inputs. Every field Option/default — absent means "unknown, don't reward/penalize".
#[derive(Clone, Debug, Default)]
pub struct PackageSignals {
    // ── static / manifest ──
    pub description_len: u32,
    pub has_repository: bool,
    pub has_documentation: bool,
    pub has_license: bool,
    pub has_keywords: bool,
    pub has_categories: bool,
    pub readme: Option<ReadmeStats>,      // text_len, code_blocks, sections, images
    pub loc: Option<u32>,
    pub version_stats: Option<VersionStats>, // release count, span, stable, yanked, …
    pub verified_repo: bool,

    // ── temporal / popularity (filled by jobs; None on first ingest OK) ──
    pub downloads_30d: Option<u64>,
    pub popularity_pct: Option<f32>,      // precomputed CDF within eco
    pub dependents: Option<u32>,
    pub cleaned_downloads: Option<u64>,
    pub active_reverse_users: Option<u32>,
    pub former_glory: Option<f32>,
    pub last_release_at: Option<Timestamp>,
    pub created_at: Option<Timestamp>,

    // ── trust / lifecycle ──
    pub deprecated: bool,
    pub unmaintained: bool,
    pub yanked: bool,
    pub malware: bool,
    pub squat_suspect: bool,
    pub owner_trust: Option<f32>,         // 0..1 from org table / graph job
    pub scorecard: Option<f32>,           // 0..1 if job ran; else None

    // ── eco-specific extras (closed enum, not dyn) ──
    pub eco_extra: EcoQualityExtra,
}

/// Closed set of ecosystem-only signals — extend by adding variants, not plugins.
#[derive(Clone, Debug, Default)]
pub enum EcoQualityExtra {
    #[default]
    None,
    Cargo { is_proc_macro: bool, is_sys: bool, edition: Option<u16>, docs_rs: bool },
    Npm { has_types: bool, deprecated_field: bool },
    Pypi { development_status: Option<SmolStr> },
    Maven { group_authority: Option<f32> },
    Go { imported_by: Option<u32> },
    Nuget { /* … */ },
    Nix { /* … */ },
}

#[derive(Clone, Copy, Debug)]
pub struct CombinedWeights {
    pub static_w: f32,   // 0.4
    pub temporal_w: f32, // 0.5
    pub excel_w: f32,    // 0.1
    pub linearizer: f32, // 1.3  →  1 - (1-s)^linearizer
}

/// Pure. Same binary for INDEX offline job and REGISTRY local reindex.
pub fn compute_quality(
    signals: &PackageSignals,
    weights: CombinedWeights,
    org_trust_table: &[(&str, u16)], // from adapter const; empty OK
) -> QualityReport {
    let mut static_s = Score::new();
    // description_len, links, readme group, loc — same as rich::compute_quality
    // version_stats if Some …
    // EcoQualityExtra match arms for cargo proc_macro etc.

    let mut temporal_s = Score::new();
    // only add groups when Option::is_some — missing downloads ≠ zero popularity penalty
    // if downloads_30d / popularity_pct / dependents / former_glory present …

    let mut score = fuse_static_temporal(static_s.total(), temporal_s.total(), weights);

    // Multipliers (always applied; bools default false)
    if signals.yanked || signals.malware || signals.squat_suspect { score *= 0.001; }
    if signals.deprecated { score *= 0.2; }
    if signals.unmaintained { score *= 0.4; }
    if let Some(t) = signals.owner_trust { score *= 0.85 + 0.15 * t; }
    if let Some(sc) = signals.scorecard { /* soft blend into trust axis, not hard kill */ }

    QualityReport {
        quality: score.clamp(0.0, 1.0),
        axes: Axes { /* Q,P,M,T,F derived from same inputs for UI */ },
        explanations: static_s.labels(), // optional debug
    }
}
```

**How “plugins” are expressed without `dyn`:**

| Concern | Mechanism |
|---|---|
| Signal not available yet | Leave `Option` as `None` — scorer skips that group |
| New eco-only signal | Add field to `EcoQualityExtra` variant + `match` arm |
| New universal signal | Add field to `PackageSignals` + lines in `compute_quality` |
| Heavy Scorecard / graph | **Offline job** fills `PackageSignals` before `compute_quality`; query path never calls Scorecard |
| Different max weights per eco | `CombinedWeights` + optional `QualityMaxWeights` associated const on adapter (still static) |

**Ingest flow (identical INDEX and REGISTRY):**

```text
EcosystemAdapter::extract_text + lifecycle
  → PackageSignals { static fields filled, temporal mostly None }
  → optional merge from popularity/graph/scorecard jobs (same schema)
  → compute_quality(signals, adapter::COMBINED, adapter::ORG_TRUST)
  → write quality + axes + FAST fields on package doc
  → rank_pipeline only READS precomputed quality (never recomputes)
```

---

### 5.6b One pipeline: local ≡ remote

**Invariant:** desktop REGISTRY and remote INDEX run the **same** `retrieve_and_rank` and `rank_pipeline`. No “local ranker” fork.

```rust
/// Storage only. Local and remote implement the same small surface.
pub trait PackageIndex: Send + Sync {
    fn search_raw(
        &self,
        q: &BuiltQuery,       // field boosts, eco filter, scope filter already baked in
        over_fetch: usize,
    ) -> Result<Vec<RawHit>, SearchError>;

    fn hydrate(&self, ids: &[PackageId]) -> Result<Vec<PackageHit>, SearchError>;
}

pub struct LocalTantivyIndex { /* mmap PackageSearchIndex */ }
pub struct RemoteHttpIndex { /* POST /packages/search or internal */ }

/// SINGLE entry used by GUI client, INDEX gateway, and tests.
pub fn retrieve_and_rank(
    index: &PackageIndexBackend,
    adapters: &AdapterRegistry,
    req: &PackageSearchRequest,
) -> Result<Page<PackageHit>, SearchError> { /* … */ }
```

Prefer an **enum** over `dyn PackageIndex` if you want zero trait objects end-to-end:

```rust
pub enum PackageIndexBackend {
    Local(LocalTantivyIndex),  // optional disk replica of registry packages
    Remote(RemoteHttpIndex),
}
```

| Stage | Local replica | Remote INDEX |
|---|---|---|
| Build query (boosts, eco) | same | same |
| Over-fetch / `rank_pipeline` | same | same |
| Adapter consts / lexicon | same | same |
| `compute_quality` | offline at absorb | offline at absorb |
| **Only difference** | mmap vs HTTP transport | |

**Local personalization is not a pipeline fork.** After the page is ranked, the **desktop client** may call `LocalEnrichment::apply` (§5.10). INDEX never sees usage history or local forks.

---

### 5.7 Package document + Tantivy schema v2

```text
PackageDocument {
  package_id, ecosystem, name, normalized_name, namespace?,
  description, keywords[], categories[], extra_tokens[],
  readme_excerpt?,
  kind: Lib | App | Meta | Plugin | Unknown,
  quality, quality_ppm,
  axes: { popularity, quality, maintenance, trust, freshness },
  downloads_30d, popularity_pct, dependents,
  last_release_at, created_at,
  flags: { deprecated, yanked, malware, squat_suspect, unmaintained },
  verified_repo,
}
// Local forks / path deps are NOT PackageDocument on INDEX — see LocalOnlyPackage §5.10
```

| Field | Options | Notes |
|---|---|---|
| `package_id` | STRING\|STORED\|indexed | Upsert key |
| `name` | TEXT | BM25 |
| `normalized_name` | STRING | Exact navigate / repair |
| `description` | TEXT | **Must populate** |
| `keywords` | TEXT | Bag + name-in-keywords |
| `extra` | TEXT | Dash parts, auto tokens; boost 0.6 |
| `readme` | TEXT | Capped; boost 0.4; optional stem |
| `ecosystem` | STRING indexed | MUST filter |
| `quality` | f64 FAST | Kink |
| `popularity_pct` | f64 FAST | Eco CDF |
| `downloads` | u64 FAST\|STORED | Bubble |
| `flags` | FAST bits | Policy |
| `record` | STORED | Hydrate |

---

### 5.8 Popularity normalization (cross-eco)

**Rule:** never compare raw npm downloads to crates.io downloads.

```text
popularity_pct(pkg) = CDF_e(downloads_window(pkg))   # within ecosystem e
# Optional blend:
P = 0.5 * pct(downloads_90d)
  + 0.3 * pct(dependents)           # if graph exists
  + 0.2 * acceleration_score        # npms-style
```

Bubble / representative floors:

| Mode | When |
|---|---|
| **Percentile (default multi-eco)** | `bubble` swaps if b.pct − a.pct > δ and both in mid band |
| **Absolute (Rust-compat)** | Keep lib.rs 200 / 1e6 / ×3 for Cargo until percentile ready |

### 5.9 Quality feature matrix (phased)

**Phase B0 — already partially present (finish wiring):**

| Feature | Group | Portable? |
|---|---|---|
| Description length | Static | Yes |
| Repo / docs / license links | Static | Yes |
| Keywords/categories present | Static | Yes |
| README text/code/sections | Static | Yes |
| LOC non-trivial / non-giant | Static | Yes if languages measure LOC |

**Phase B1 — high ROI portable:**

| Feature | Group | Data need |
|---|---|---|
| Spam / placeholder / reserved name | Multiplier ×0.001 | Heuristics |
| Yanked / unpublished / malware | Multiplier | Registry feed |
| Deprecated / unmaintained | Multiplier ×0.2–0.4 | Manifest + advisories |
| Release freshness (adaptive) | Temporal | Version timeline |
| Log popularity (eco CDF) | Temporal | Downloads series |
| Verified repository | Static | Clone/API check |
| ≥1.0 / multiple releases | Static | Versions |
| Brand-new bonus (<30d) | Temporal | Created |

**Phase B2 — graph / trust (lib.rs moat):**

| Feature | Group | Data need |
|---|---|---|
| Cleaned downloads (minus top consumer) | Temporal | Dep graph |
| Active reverse users / former_glory | Temporal | Dep time series |
| Direct/indirect dependents | Temporal | Graph |
| Owner org allowlist + collab trust | Temporal | Identity graph |
| Migration-away rate | Temporal | Graph diffs |
| OpenSSF Scorecard (soft) | Trust axis | Scorecard API |

**Phase B3 — eco plugins (optional):**

| Feature | Ecosystem |
|---|---|
| Clippy / cargo edition / docs.rs | Rust |
| npm audit / deprecated field | npm |
| Classifiers / Development Status | PyPI |
| GroupId authority (Apache, Google…) | Maven |
| import "Imported by" | Go |
| TFM applicability | NuGet |
| install_on_request | Homebrew-style apps |

### 5.10 Local enrichment sidecar (normative — small, post-rank)

**Do not put locality in the core discovery pipeline.** Project DepSet filters, “my packages” Tantivy scopes, and local-only ranking forks were considered and **rejected** as the wrong layer:

| Wrong (rejected) | Why |
|---|---|
| `SearchScope::Project` MUST filter on Tantivy | Couples global quality ranking to workspace state; diverges local vs remote |
| Re-running SERP with DepSet bitmaps | Locality becomes a second search engine |
| Uploading forks / path deps to INDEX | Privacy + identity pollution |
| Baking usage into `compute_quality` | Usage is personal, not package quality |

**Right model:** global (or replica) `retrieve_and_rank` is unchanged. On the **desktop only**, a tiny sidecar **enriches** the already-ranked page.

```text
                    ┌──────────────────────┐
  user query ──────►│ retrieve_and_rank    │  INDEX or local replica
                    │ (same pipeline)      │
                    └──────────┬───────────┘
                               │ Page<PackageHit>
                               ▼
                    ┌──────────────────────┐
                    │ LocalEnrichment      │  client / registry-local only
                    │  · usage / dep flags │
                    │  · soft re-order     │
                    │  · inject local-only │
                    └──────────┬───────────┘
                               │ Page<EnrichedHit>
                               ▼
                             GUI
```

#### 5.10.1 Responsibilities (keep the surface tiny)

| Responsibility | In | Out |
|---|---|---|
| BM25, quality kink, diversity, bubble | Core pipeline | — |
| “You've used this before” prior | **Sidecar** | — |
| Badge: in direct/dev/transitive deps | **Sidecar** annotation | — |
| Soft bump for used / in-tree packages | **Sidecar** re-order | — |
| Local fork / path dependency / unpublished path package | **Sidecar inject** | Never INDEX |
| Filter SERP to “only my deps” UI chip | Optional **client filter** on enriched page | Not Tantivy MUST |
| Global package quality / downloads | Core / offline jobs | — |

#### 5.10.2 Types

```rust
/// Desktop-only. Constructed from open project + local sqlite; never sent on the wire.
pub struct LocalContext {
    pub project_id: Option<ProjectId>,
    /// package_id → how it relates to the open project (if any).
    pub dep_relation: HashMap<PackageId, DepRelation>, // Direct | Dev | Transitive
    /// Historical use across saved projects / installs (counts or last_used).
    pub usage: HashMap<PackageId, UsageStat>,
    /// Forks, path deps, file:// packages — searchable only on this machine.
    pub local_only: Vec<LocalOnlyPackage>,
}

#[derive(Clone, Copy, Debug)]
pub enum DepRelation { Direct, Dev, Transitive }

#[derive(Clone, Copy, Debug)]
pub struct UsageStat {
    pub times_depended: u32,
    pub last_used: Option<Timestamp>,
}

/// Never assigned a registry PackageId that INDEX owns; stable local id.
pub struct LocalOnlyPackage {
    pub local_id: LocalPackageId,
    pub ecosystem: Language,
    pub name: String,
    pub description: Option<String>,
    pub origin: LocalOrigin, // PathDep | GitFork { url } | WorkspaceMember | Overlay
    pub keywords: Vec<SmolStr>,
    /// Optional link to upstream registry package this forks/overlays.
    pub upstream: Option<PackageId>,
}

pub struct EnrichedHit {
    pub hit: RankedHit,              // from core pipeline, or synthetic from local_only
    pub source: HitSource,           // Registry | LocalOnly
    pub dep_relation: Option<DepRelation>,
    pub usage: Option<UsageStat>,
    pub labels: Vec<LocalLabel>,     // UsedBefore, InProject, LocalFork, PathDep, …
}

pub struct LocalEnrichment {
    /// Soft score deltas (not lib.rs kink). Applied only to already-ranked pages.
    pub used_before_bonus: f32,    // e.g. +0.5 on fused score or flat reorder key
    pub direct_dep_bonus: f32,     // e.g. +0.35
    pub dev_dep_bonus: f32,        // e.g. +0.15
    pub local_only_name_boost: f32,// when name matches query
    /// Max local-only rows to inject into a page (keep SERP readable).
    pub max_local_inject: usize,   // 3
}

impl LocalEnrichment {
    /// Pure-ish: no network. May read LocalContext maps only.
    pub fn apply(
        &self,
        page: Page<PackageHit>,
        query: &str,
        ctx: &LocalContext,
    ) -> Page<EnrichedHit> {
        // 1. Annotate each hit with dep_relation + usage from ctx maps (O(page)).
        // 2. Soft re-key: rank_score' = rank_score + bonuses (stable tiebreak by package_id).
        // 3. Match local_only packages by name/keywords (simple substring or tiny local
        //    SQLite FTS — NOT the global package index schema).
        // 4. Inject up to max_local_inject local-only rows near top if name matches,
        //    marked HitSource::LocalOnly (UI: distinct chrome, never "publish").
        // 5. Return page (same limit; drop lowest registry hits if inject needs slots).
    }
}
```

#### 5.10.3 Storage (sidecar only)

```sql
-- registry-local sqlite (never replicated to INDEX)
CREATE TABLE package_usage (
  package_id     BLOB NOT NULL PRIMARY KEY,
  times_depended INTEGER NOT NULL DEFAULT 0,
  last_used      INTEGER,  -- unix
  last_project   BLOB
);

CREATE TABLE local_only_packages (
  local_id     BLOB PRIMARY KEY,
  ecosystem    TEXT NOT NULL,
  name         TEXT NOT NULL,
  description  TEXT,
  origin_json  TEXT NOT NULL,  -- path / git fork metadata
  upstream_id  BLOB,           -- optional PackageId
  keywords     TEXT
);

CREATE INDEX idx_local_only_name ON local_only_packages(ecosystem, name);
```

- **Usage rows** updated when a project resolves/saves a DepSet (increment `times_depended`, touch `last_used`).  
- **Local-only rows** registered when user adds path/git dependency or “open local package”.  
- **No** `project_ids` multi-value on the global Tantivy package schema.  
- **No** requirement that DepSet packages are the only searchable set.

Optional micro-index for local-only FTS: a **few hundred rows** sqlite FTS5 or in-memory filter — not a second package-discovery stack.

#### 5.10.4 Privacy & wire rules

| Data | On INDEX wire? |
|---|---|
| Ranked registry hits | Yes (normal search) |
| Usage stats / “used before” | **Never** |
| Local forks / path deps | **Never** |
| DepSet membership | **Never** (client-side only) |
| Enriched labels | Client-only |

Remote search: client calls INDEX → same ranked page → then `LocalEnrichment::apply`. Local replica: same after local Tantivy page.

#### 5.10.5 UX

| Surface | Behavior |
|---|---|
| Package search (default) | Global ranking; badges “Used before” / “In this project”; slight lift for those |
| Local fork matches query | Row with “Local” / “Path” chrome; not mixed into INDEX identity |
| Optional chip “In project only” | **Client filter** on enriched annotations (`dep_relation.is_some()`), not a re-query |
| Offline | Local package replica (if synced) + enrichment + local-only; no usage upload |

#### 5.10.6 Acceptance

1. Core `rank_pipeline` fixtures unchanged when `LocalContext` is empty (`apply` ≈ identity order + empty labels).  
2. With usage data, a previously used package can rise a few slots but cannot leapfrog a much higher-quality exact match (bonus **capped**).  
3. Path dep named `my-fork` appears for query `my-fork` without any INDEX request.  
4. Network capture of package search requests contains **no** usage or local-only payloads.  
5. INDEX and local-replica pages for the same corpus produce **identical** pre-enrichment order.

### 5.11 SERP stage order (canonical)

```text
retrieve (field boosts + optional eco filter + over_fetch)
  → exact_repair?
  → fuse (kink + exact/contains via NameGrammar)
  → sort
  → discard_chaff?
  → diversity (lexicon bland + diversity_deny)
  → requery_common?
  → pull_up_representatives
  → bubble (absolute or percentile)
  → related_collapse?
  → dym? (side channel)
  → or_fallback?
  → truncate
  → good_bad_split (presentation)
  ── desktop only ──
  → LocalEnrichment::apply (annotate + soft boost + inject local-only)
```

### 5.12 Multi-ecosystem query behavior

| Query mode | Behavior |
|---|---|
| Single ecosystem | MUST filter `ecosystem`; use that adapter's consts + lexicon |
| Cross-ecosystem | top-N **per eco** → `rank_pipeline` per eco → **interleave**; never pure-sort raw downloads |
| Default API | accept `ecosystem` (today forced `None` — fix); **no** project scope param on INDEX |

---

## 6. Implementation plan (PR-sized)

### Phase 0 — Unblock existing engine + lexical wiring (2–3 days)

**Goal:** make the ported lib.rs SERP use its signals **and** pin micro-opts to types.

1. Populate `description` in `PackageIndex::absorb`.  
2. Stop hardcoding `downloads: 0` — wire `downloads_30d` into candidates.  
3. Thread `RegistryQuery.ecosystem` from HTTP (stop forcing `None`).  
4. Tantivy MUST eco filter when set.  
5. Extract `NameGrammar` / `FieldBoosts` / `SerpStages` / `ExtractWeights` structs; Cargo consts; replace hardcoded cargo/rs strip.  
6. Move `STOPWORDS` / `IDENT_STOPWORDS` into `KeywordLexicon`; load synonyms/bland/specific via existing `heuristics` into lexicon.  
7. `IndexEnricher`: name-into-keywords, dash parts → `extra` field (schema add + rebuild).  
8. Fix indexing: real `loc`, real `dependencies`; persist categories on facets.  
9. Desktop stub: `LocalEnrichment::apply` identity path + `package_usage` / `local_only_packages` tables (no core scope filter).

**Acceptance:** bubble/pull-up fire; description BM25; eco filter; stopwords via lexicon; enrichment is no-op without usage data; pre-enrichment order identical for local replica vs remote fixtures.

### Phase 1 — Traits + Cargo as reference adapter (3–5 days)

1. Introduce `EcosystemAdapter` + `KeywordLexicon` + `PackageSignals` / `compute_quality`; thin `AnyAdapter` façade only if needed for registry maps.  
2. Move `rank_full` → free-function `rank_pipeline`; add `retrieve_and_rank` over `PackageIndexBackend` enum (Local \| Remote) — **one path**.  
3. `CargoAdapter` fills `PackageSignals` from existing `rich::extract` + version timeline + download series; all micro-opts via associated consts.  
4. Persist `QualityReport` explanations for debug.  
5. Schema: FAST `quality`, `downloads`, `extra` (rebuild package index once).  
6. Wire `LocalEnrichment` with usage bonuses + local-only inject; prove INDEX requests never include local payloads.

**Acceptance:** Cargo package search quality ≥ current; unit tests for kink/diversity/bubble/stopwords still green; downloads from real or fixture series; local and remote share the same `rank_pipeline` fixtures; enrichment unit tests for used-before lift + path-dep inject.

### Phase 2 — Multi-eco extractors + percentile popularity (1–2 weeks)

| Adapter | Text sources | Popularity |
|---|---|---|
| npm | package.json description/keywords/readme; scoped names | npm downloads API 30d/90d |
| pypi | METADATA/PKG-INFO summary+description; classifiers | BigQuery or pypistats windows |
| go | go.mod module path; README; package comments | proxy / imported-by when available |
| maven | pom name/description; groupId trust table | reverse-deps if available |
| nuget | nuspec/id description tags | gallery downloads |
| nix | flake/package meta description | optional |

1. Implement `ManifestFacetsExtractor` per language at `extract_facets` branch.  
2. Daily job: recompute per-ecosystem CDFs → `popularity_pct`.  
3. Switch `PopularityScale.use_percentiles = true` for non-Cargo; keep absolute optional for Cargo.  
4. Keyword synonym tables: start shared stopwords; eco-specific bland/specific later.

**Acceptance:** explore queries on npm/pypi fixtures place blessed packages in top-5; cross-eco interleave smoke test.

### Phase 3 — Temporal quality + trust gates (1–2 weeks)

1. Implement temporal features: freshness decay, multi-release, brand-new, deprecated multipliers.  
2. Hard gates: malware/yank/spam ×0.001 + optional index delete.  
3. Verified-repo check (package name appears in remote repo).  
4. Exact-match policy: promote only if `quality > floor && !squat_suspect`.  
5. Optional OpenSSF Scorecard soft trust axis.

**Acceptance:** abandoned high-download packages demoted on explore; known spam not in top-20; land-grab empty names don't win on exact alone.

### Phase 4 — Graph moat + UX polish (ongoing)

1. Reverse-dep graph job (INDEX-scale); cleaned downloads; former_glory.  
2. Owner org allowlists per eco (rust-lang, @types, Apache, …).  
3. Related-package collapse (`foo` / `foo-cli` / `foo-macros`).  
4. DYM fuzzy when top quality weak; “search also” dividing keywords.  
5. UI score bars Q/P/M (npms) + “why ranked” chips.

### Phase 5 — Eval harness (parallel from Phase 1)

See §7. Block LTR until golden NDCG plateaus.

---

## 7. Evaluation methodology

### 7.1 Query classes (per ecosystem)

| Class | Success |
|---|---|
| Navigate popular exact | Target @1 |
| Navigate typo | Canonical top-3 **without** promoting squat |
| Explore task | Blessed set in top-5; abandoned out of top-10 |
| Category-ish keywords | Pairwise expert order |
| Negative / spam | Known bad not in top-20 |
| Cross-eco collision | Correct package under each eco facet |

### 7.2 Metrics

- NDCG@10, MRR, Recall@50  
- Blessed@5 for explore  
- Typosquat safety rate  
- Abandonment rate in top-10  
- Regression suite on every scorer/config change  

### 7.3 Seed golden queries (illustrative)

| Eco | Query | Ideal near top |
|---|---|---|
| cargo | `http client` | reqwest, hyper — not abandoned |
| cargo | `serde` | serde @1 |
| cargo | `git` | git2 / gix competitive; empty squat not alone |
| npm | `web framework` | express, fastify, koa |
| npm | `lodash` | lodash @1 |
| pypi | `yaml` | PyYAML, ruamel.yaml |
| pypi | `http client` | httpx, requests, aiohttp |
| maven | `json` | Jackson/Gson under credible groupIds |
| go | `markdown` | widely imported; module-grouped |
| nuget | `json` | Newtonsoft.Json, System.Text.Json |

Maintain qrels (0–3 grades) + pairwise A≻B + adversarial squat/spam set.

---

## 8. Concrete code extension points (today)

| Step | File | Action |
|---|---|---|
| Description absorb | `registry/search/tantivy.rs` `absorb` | `add_text(description, …)` + enricher |
| Downloads hardcode | `registry/search/mod.rs` ~164 | Read from facets/signals |
| Ecosystem API | `server/search/registry.rs` + client | `Option<Language>` only |
| Name grammar | `search/ranking.rs` `contains_query_names` | `NameGrammar` const from adapter |
| Free-function pipeline | `collect_ranked_hits` | Call `rank_pipeline` with adapter consts |
| Lexicon | `metadata/rich.rs` + `heuristics.rs` | Wrap STOPWORDS/synonyms/bland in `KeywordLexicon` |
| Facet extract | `server/coordination/indexing.rs` `extract_facets` | Per-lang `EcosystemAdapter::extract_text` |
| SearchFacets extend | `metadata/rich.rs` | `downloads_30d`, description, categories keep |
| Quality scoring | `package_quality/` | `PackageSignals` + `compute_quality` (no dyn) |
| Unified entry | `retrieve_and_rank` | `PackageIndexBackend::{Local,Remote}` enum |
| Schema FAST + extra | `tantivy.rs` schema | quality, downloads, popularity_pct, extra, SCHEMA_VERSION |
| Local enrichment | `registry-local` / `client` | `LocalEnrichment::apply` + `package_usage` + `local_only_packages` |

---

## 9. What “above lib.rs” means multi-language

lib.rs is **best-in-class for one ecosystem**. Nudox should not claim a single global leaderboard. “Above lib.rs” means:

1. **Per-ecosystem quality ≥ specialized discovery** for that language (lib.rs for Cargo, npms-class for npm, …).  
2. **Unified product** with shared UX, explainable axes, and cross-eco search that doesn't lie by mixing raw metrics.  
3. **IR-adjacent advantages** unique to Nudox: after package pick, symbol/type-directed search (10-tantivy) and lineage — discovery → understanding in one stack.  
4. **Fresher trust** than SourceRank (hourly malware/removal reconciliation).  
5. **Intent-aware** navigate vs explore (lib.rs is mostly relevance+quality without explicit intent split).  
6. **Eval-driven** constants, not only hand-tune folklore.

---

## 10. Explicit non-goals

- Replacing symbol Tantivy with package ranking (see 10-tantivy).  
- One download threshold for all ecosystems.  
- Using SourceRank or Scorecard as sole relevance.  
- Shipping LTR before golden sets.  
- Full owner PageRank day one (Phase 4).  
- Merging package identity across ecosystems by name alone.  
- **Trait per stopword list / synonym entry** — data belongs in `KeywordLexicon`.  
- **`dyn QualityFeature` / open plugin quality** — closed `PackageSignals` + `compute_quality` only.  
- **Separate local vs remote ranking pipelines** — one `retrieve_and_rank`; backend enum only.  
- **Project/DepSet as core Tantivy scope** — locality is `LocalEnrichment` sidecar only.  
- **Uploading local forks / path deps / usage to INDEX.**  
- Query-time HTTP for quality/Scorecard/downloads.

---

## 11. Relation to other research

| Report | Relationship |
|---|---|
| [10-tantivy](./10-tantivy.md) | Symbol multi-lang; package SERP stages referenced as **do not reuse for symbols** |
| [02-registry-server-audit](./02-registry-server-audit.md) | Audits package index gaps (downloads=0, description unused) |
| [09b-retrieval-pipeline](./09b-retrieval-pipeline-plan.md) | Semantic hybrid; package discovery remains lexical-first |
| [22-crate-topology](./22-crate-topology.md) | `text-search` owns package + symbol indexes; ranking fusion may stay index-service for remote |
| [11-sqlite-index](./11-sqlite-index.md) | Catalog SoT; Tantivy disposable replica |
| [13-storage](./13-storage.md) | `tantivy/main/` package index path |

**GD-21 amendment proposal:** Text search shared schema applies to **symbols**. Package discovery gets its own schema v2 + `EcosystemAdapter` (consts + lexicon) / `PackageSignals`+`compute_quality` / unified `retrieve_and_rank` (local≡remote) / desktop **`LocalEnrichment` sidecar** as specified here. Package download-bubble fusion remains package-side only.

---

## 12. Sources

### lib.rs / Rust
- https://gitlab.com/lib.rs/main  
- https://lib.rs/about  
- https://users.rust-lang.org/t/improving-ranking-and-crate-search/26765  
- https://rust-lang.github.io/rfcs/1824-crates-io-default-ranking.html  
- https://github.com/rust-lang/crates.io/discussions/4339  

### npm / npms
- https://blog.npmjs.org/post/156076312840/search-update.html  
- https://github.com/npms-io/npms-analyzer/blob/master/lib/scoring/score.js  
- https://github.com/npms-io/npms-analyzer/blob/master/docs/architecture.md  
- https://npms.io/about  

### PyPI / multi-eco / trust
- https://github.com/pypi/warehouse/blob/main/warehouse/search/queries.py  
- https://github.com/pypi/warehouse/issues/3932  
- https://packaging.python.org/guides/analyzing-pypi-package-downloads/  
- https://libraries.io/ · SourceRank docs · https://arxiv.org/html/2512.24400v1 (SourceBroken)  
- https://scorecard.dev/ · https://github.com/ossf/scorecard  

### Other registries
- https://go.dev/blog/pkgsite-search-redesign  
- https://devblogs.microsoft.com/dotnet/new-and-improved-nuget-search/  
- https://central.sonatype.org/search/rest-api-guide/  
- https://www.ruby-toolbox.com/  
- https://docs.brew.sh/Analytics  

### In-repo anchors
- `workspace/registry/search/ranking.rs` — SERP Layer C  
- `workspace/registry/search/tantivy.rs` — package index  
- `workspace/registry/search/mod.rs` — `collect_ranked_hits`, downloads=0  
- `workspace/registry/metadata/rich.rs` — Score, extract, compute_quality  
- `workspace/server/coordination/indexing.rs` — `extract_facets`  
- `workspace/server/search/registry.rs` — ecosystem forced None  

---

## 13. Bottom line

1. **One core trait** (`EcosystemAdapter`) with **associated consts** for grammar, boosts, SERP stages, ranking, pop scale, normalizer.  
2. **Lexical tables are structs** (`KeywordLexicon`), not traits.  
3. **SERP is one free-function pipeline** for local replica and remote; only transport differs.  
4. **Quality is `PackageSignals` + `compute_quality`** — no `dyn` plugins; missing signals are `None`.  
5. **Every lib.rs-class small optimization is inventoried** in §5.2.  
6. **Locality is a tiny post-rank sidecar** (`LocalEnrichment`): used-before / in-project badges + soft boosts + inject local forks — **never** on the INDEX wire, **never** a DepSet Tantivy scope.  
7. Package discovery and symbol search stay separate (this report vs 10-tantivy).

Ship Phase 0 (signals + lexicon + unified entry + enrichment stub), then multi-eco adapters, then temporal/graph fields on `PackageSignals`.

---

*End of report.*
