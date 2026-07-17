# Tantivy Multi-Language Symbol Search for Nudox

**Research date:** 2026-07-16  
**Scope:** Tantivy 2026 state; code-search tokenization prior art; multi-language analyzer design for Rust/TypeScript/Python/Go/Java/C#/Nix; schema/trait layer; ranking; incremental ops for embedded GPUI + remote INDEX.  
**Status:** Synthesis of verified crates.io/docs/CHANGELOG + live nudox codebase audit + prior-art survey.

---

## 0. Executive summary (TL;DR)

Nudox already pins **tantivy 0.22** (`workspace/registry`, `workspace/server`) and has a solid **language-agnostic** identifier tokenizer (`workspace/registry/runtime/text/tokenizer.rs`) plus a tiered symbol query (exact STRING → regex contains → subtoken AND). That is the right foundation. The gaps for multi-language highest-quality search are **not** “we only tokenize snake_case”; they are:

1. **Schema too thin for signatures/docs/path structure** — no signature field, no path-prefix facet, no prefix-ngram autocomplete field, no popularity/quality fast fields on symbols.
2. **No `LanguageAnalyzer` trait** — query rewrite, path separators, kind priors, and boosts are language-blind even though IR already carries `ecosystem`.
3. **Ranking fusion is package-registry-shaped** (quality kink, downloads, keyword diversity) and is not applied to symbol search; symbol ranking is raw BM25 across Should tiers.
4. **Version lag** — crates.io latest is **0.26.1** (2026-04-21); 0.24+ index format is documented as backward-compatible with 0.22/0.21. Worth upgrading on a controlled reindex, not mid-flight mixed readers.

**Recommended design:** one shared multi-language schema + `ecosystem` STRING filter + optional facet path `/lang/{eco}`; keep the common subword core; add thin per-language `LanguageAnalyzer` impls for path splitting, signature term extraction, query rewrite, and boost tables; rank with multi-field BM25 + exact-match bonus + kind prior (symbol-side fusion), not package download bubbles. Pin **0.24.2 minimum**, prefer **0.26.1** after a one-shot reindex. Local GPUI and remote INDEX must share **schema + tokenizer registration + index format major**, not necessarily the same binary commit.

---

## 1. Tantivy 2026 state

### 1.1 Verified version timeline (crates.io API, 2026-07-16)

| Version | Released | Notes |
|---|---|---|
| **0.26.1** | 2026-04-21 | **Latest** on [crates.io/crates/tantivy](https://crates.io/crates/tantivy) (`max_version` / `newest_version` / `default_version`) |
| 0.26.0 | 2026-03-31 | Lazy scorers, filter/composite aggs, regex-in-grammar, Bytes fast fields |
| 0.25.0 | 2025-08-20 | Edition 2024; string fast-field TopDocs order |
| 0.24.2 | 2025-07-17 | Stable; index sorting removed; CompactDoc; min Rust 1.75→1.81 on 0.24.1 |
| 0.22.1 | 2025-07-17 | Patch for TopNComputer reverse order |
| **0.22.0** | 2024-04-12 | **Nudox pin**; Document trait; ~40% indexing throughput (GitHub-dataset blog claim) |
| 0.21.x | 2023-09/10 | Lenient query parser; dynamic token filters |

Sources: [crates.io API](https://crates.io/api/v1/crates/tantivy), [CHANGELOG.md](https://github.com/quickwit-oss/tantivy/blob/main/CHANGELOG.md), [Tantivy 0.24 blog](https://quickwit.io/blog/tantivy-0.24), [Tantivy 0.22 blog](https://quickwit.io/blog/tantivy-0.22).

Nudox pin locations:
- `workspace/registry/Cargo.toml` → `tantivy = "0.22"`
- `workspace/server/Cargo.toml` → `tantivy = "0.22"`
- Lockfile resolves ≈ `0.22.1` under `build/third-party/Cargo.toml`

### 1.2 Breaking / adoption-worthy changes since 0.22

**Worth adopting for nudox**

| Capability | Since | Why it matters for symbol search |
|---|---|---|
| `CompactDoc` (smaller in-memory docs) | 0.24 | Desktop GPUI memory; writer heap |
| Lazy scorers (stop scoring when outside top-K) | 0.26 | As-you-type latency |
| String fast field + `TopDocs::order_by` | 0.25 | Sort by name without store fetch |
| `BooleanQuery::minimum_number_should_match` | 0.24 | Soft multi-subtoken matching |
| Regex in query grammar | 0.26 | Advanced GUI filters (optional) |
| `FuzzyTermQuery` / `new_prefix` | long-standing | Typo tolerance / prefix fuzzy |
| `NgramTokenizer` (`prefix_only`) | long-standing | Autocomplete field |
| `PhrasePrefixQuery` | 0.20 | `name:"HashMap ins"*` style |
| `madvise` on mmap open | 0.20 | Local SSD warmup hints |
| Configurable `NUM_MERGE_THREADS` | 0.24 | Desktop vs server merge load |
| Stemmer behind feature flag | 0.26 | Smaller binary if we never stem code |

**Breaking API (upgrade cost)**

- **0.24:** index sorting **removed** ([CHANGELOG](https://github.com/quickwit-oss/tantivy/blob/main/CHANGELOG.md), [0.24 blog](https://quickwit.io/blog/tantivy-0.24)). Nudox does not use sorted indexes today — low risk.
- **0.22:** exports moved into modules; `ReloadPolicy::OnCommit` → `OnCommitWithDelay`; Document-as-trait. Already paid if on 0.22.
- Min Rust: **1.81** (0.24.1+). Confirm desktop CI MSRV before jump.

### 1.3 Index format compatibility (local vs remote versions)

Documented guarantees from [CHANGELOG](https://github.com/quickwit-oss/tantivy/blob/main/CHANGELOG.md):

- **0.22** can read indices created with **0.21**.
- **0.24** is **backwards compatible with indices created with v0.22 and v0.21**.
- **0.25** compatible with **0.24**.
- **0.26** (unreleased section header still present on main, but **0.26.0/0.26.1 published** on crates.io) continues the post-0.21 compatibility line for reading older segments; always re-verify with their format tests before shipping mixed-version fleets.

**Operational rule for nudox:**

| Scenario | Safe? |
|---|---|
| Remote writes 0.26, local reads 0.26 | Yes |
| Remote wrote 0.22, local reads 0.26 | **Yes** (forward-read of older format) |
| Remote writes 0.26, local still on 0.22 | **No** — do not assume older reader can open newer segments |
| Two processes different patch of same minor on same directory | Risky — one writer only; readers OK if format-compatible |

**Recommendation:** treat index format as a **deployed artifact version**. Bump local + remote together, or ship a schema-version marker file next to the index and force reindex on mismatch. Tokenizer registration is **not** on disk — both sides must call `register()` with the same analyzer chain after every open.

### 1.4 Memory / mmap / embedded (GPUI)

Tantivy is a **library**, not a server. Storage options:

1. **`MmapDirectory`** — OS page cache owns warm pages; ideal for desktop SSD index. Nudox already uses this in `TextIndex::open_or_create`.
2. **`RamDirectory`** — tests / tiny indexes.
3. **Writer heap** — `IndexWriter` memory budget. Nudox symbol path uses **50 MB** (`WRITER_MEMORY_BYTES = 50_000_000`). Min practical budget historically raised to **15 MB** (0.21/0.22) to avoid single-doc segments.

Embedded knobs for GPUI:

| Knob | Suggested desktop | Suggested remote INDEX |
|---|---|---|
| Writer memory | 15–50 MB | 100–500 MB |
| Merge threads | 1 | default / more |
| `ReloadPolicy` | **Manual** (nudox today) | `OnCommitWithDelay` or Manual+poll |
| Doc-store compression | lz4 (default) | lz4 or zstd if feature on |
| Warm search | open reader at app start; optional touch of term dict | same + keep searcher hot |
| `madvise` | sequential on open for large indexes | sequential for cold shard load |

Resident RSS for a **symbol-only** index is dominated by OS page cache of postings + term dict, not CompactDoc. Expect **tens of MB** for 100k symbols (see §6 size estimates), not hundreds, if you avoid indexing full file bodies.

### 1.5 Multi-index management

Tantivy has no built-in multi-tenant broker. Patterns:

1. **One `Index` per process, shared schema, `ecosystem` filter** — simplest; what nudox symbol index already does (`ecosystem` STRING).
2. **One `Index` per language directory** — smaller term dictionaries per language; cross-language search = N searches + merge. Useful if language-specific tokenizers **diverge hard**.
3. **One `Index` per package / tenant** — bad for cross-package symbol search; good for isolation.
4. **Multiple readers, one writer** — standard; `IndexReader` is cloneable (since 0.19 era).

**Recommendation:** keep **one symbol index schema** shared across languages for the default search surface; optional **per-source** directory already exists in server config (`symbol` vs `package` paths). Do **not** split by language unless term-dict pollution from ultra-common subtokens (`get`, `set`, `i`) becomes a measured problem — then prefer **language-aware IDF via separate indexes** or pre-token stopword filters, not schema forks.

Package index remains separate (`PackageIndex` in `registry/search/tantivy.rs`) — correct split of concerns.

---

## 2. Prior art: how code search engines tokenize and rank

### 2.1 Zoekt (Sourcegraph / GitLab)

Design doc: [sourcegraph/zoekt `doc/design.md`](https://github.com/sourcegraph/zoekt/blob/main/doc/design.md).

- **Positional trigrams** (n=3) with rune offsets; index ≈ **3–3.5×** corpus.
- **No index-time word tokenization** for content — substring/regex first-class.
- Case folding expands case variants at query time for case-insensitive search.
- Symbols via **ctags / tree-sitter**; symbol matches rank higher.
- Optional **BM25** mode (`UseBM25Scoring`).
- Target latency: **sub-50ms** on multi-GB corpora on one machine.

**Lesson for tantivy:** trigrams win at **arbitrary substring/regex**. Nudox symbol search is **structured** (name, fq, kind) — inverted-index terms beat trigrams for symbol UX, but a future “search in signatures / bodies” surface may want ngrams or a separate Zoekt-like path.

### 2.2 GitHub Blackbird

Primary source: [The technology behind GitHub’s new code search](https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/).

- Custom Rust engine (not Lucene).
- **Sparse grams** (weighted bigrams → recursive to trigrams) instead of naive trigrams — reduces false positives from common grams like `for`.
- Sharded by **Git blob OID** for content-addressed dedupe.
- Separate **symbol extraction** service + symbol indices.
- Language via **Linguist** at crawl; query rewrite uses language IDs.
- Ranking details mostly undisclosed; doc IDs ordered by rank during compaction so lazy iterators surface good docs first.

**Lesson:** separate **content index** from **symbol index**; language is a first-class filter; ngram design is nontrivial at GitHub scale — we do not need sparse grams for 100k–10M symbols if we stay identifier-tokenized.

### 2.3 Sourcegraph BM25F (2025)

Source: [Keeping it boring (and relevant) with BM25F](https://sourcegraph.com/blog/keeping-it-boring-and-relevant-with-bm25f) (Apr 4, 2025).

- ~**20%** quality lift vs previous ranking on internal evals.
- **BM25F**: single BM25 over combined fields; boost **term frequencies** for symbol/filename matches by **×5** (grid-searched, not ultra-sensitive).
- Two-level: file BM25F → line BM25F for display order.
- Explicit note: classic code search **avoids** index-time tokenization because punctuation and mid-token matches matter; BM25 is adapted by treating query hits as induced tokens.
- Still critical for low-resource / proprietary languages and as first stage before semantic rerank.

**Lesson for nudox:** map BM25F to tantivy via **field boosts on multi-field boolean query** (approximate) or custom scorer later. Prefer **name ≫ fq ≫ signature ≫ doc**; exact name bonus outside BM25 (already done for packages).

### 2.4 Meilisearch typo tolerance

Docs: [Typo tolerance settings](https://www.meilisearch.com/docs/learn/relevancy/typo_tolerance_settings).

- Default: length-based typo budget (e.g. 5+ chars → 1 typo, 9+ → 2).
- Can **disable on attributes** (good for identifiers / IDs).
- **Lesson:** for code symbols, **disable fuzzy on exact ID fields**; allow mild fuzzy only on subtoken fields for ≥5 char tokens.

### 2.5 IDE symbol search (VS Code / JetBrains)

- **CamelHumps** (JetBrains): type initials of PascalCase parts → match ([docs](https://www.jetbrains.com/help/resharper/Navigation_and_Search__CamelHumps.html)).
- VS Code sequential fuzzy prefers camel boundaries / separators.
- Libraries like [sahilm/fuzzy](https://github.com/sahilm/fuzzy) implement Sublime-style ranking offline.

**Can tantivy approximate CamelHumps?**

| Technique | CamelHumps? | Cost |
|---|---|---|
| Subword tokens (`HTTP`+`Server`) | Partial — initials need extra index terms | Low |
| Index **initialism tokens** (`hss` for `HTTPServer`) | Yes for pure initial queries | Low index growth |
| Prefix ngrams on name | Prefix only | Medium |
| FuzzyTermQuery on full name | Approximate edit distance, not humps | Medium latency |
| Client-side re-rank of top-K with fuzzy scorer | Best UX match to IDE | Cheap if K small |

**Recommendation:** index optional `name_initials` STRING (lowercased initials of subwords) + client re-rank for true fuzzy ranking. Do not rely on Levenshtein alone for CamelHumps.

### 2.6 docs.rs / crates.io

- crates.io ranking historically download-heavy ([RFC 1824](https://rust-lang.github.io/rfcs/1824-crates-io-default-ranking.html)).
- docs.rs is documentation hosting; symbol navigation is rustdoc-shaped, not BM25 symbol search.
- **Nudox package ranking** already ports a crates-style fusion (`registry/search/ranking.rs`) — keep it **package-only**.

### 2.7 Other systems (brief)

| System | Approach | Takeaway |
|---|---|---|
| **Hound / livegrep** | Regex over trigram or raw | Regex latency requires different index |
| **OpenGrok** | Lucene + ctags | Multi-field Lucene schema ancestor |
| **ygrep** (Tantivy code search) | Custom tokenizer preserving `$ @ # - _`, subtokens | Confirms Tantivy custom tokenizer path for code |

---

## 3. Current nudox state (code audit)

### 3.1 Symbol index schema

File: `workspace/registry/runtime/text/index.rs`

| Field | Type | Role |
|---|---|---|
| `id` | STRING \| STORED | SymbolId UUID; delete/upsert key |
| `package` | STRING \| STORED | Package UUID |
| `ecosystem` | STRING \| STORED | Language filter |
| `kind` | STRING \| STORED | SymbolKind |
| `name` | STRING \| STORED | Display name |
| `fq_name` | STRING \| STORED | Fully qualified display |
| `name_lower` | STRING | Exact + regex contains |
| `fq_lower` | STRING | Path contains |
| `name_tokens` | TEXT `ident` + positions | Subtoken |
| `fq_tokens` | TEXT `ident` + positions | Path subtokens |

**Missing for multi-lang quality:** `signature`, `doc`, `path_prefix` / facets, `popularity` / quality fast fields, `name_prefix` ngrams, `name_initials`, `content_hash` / generation for staleness.

### 3.2 Identifier tokenizer (already multi-convention)

File: `workspace/registry/runtime/text/tokenizer.rs`

Splits on:
- non-alphanumeric separators (`::`, `.`, `#`, `$`, `-`, `_`, …)
- camel humps (`getUser` → get, user)
- acronym tails (`HTTPServer` → http, server)
- digit edges (`utf8` → utf, 8)
- C# arity strip (`` List`1 `` → list)

**Does not** re-emit the whole original identifier as a token (correct — exact match lives on STRING fields).

Tests (`runtime/tests/tokenizer.rs`) cover Rust/C#/dotted paths. **Language-agnostic claim is largely true**; remaining language gaps are **query rewrite** and **path-separator semantics**, not the splitter.

### 3.3 Query tiers

File: `workspace/registry/runtime/text/query.rs` — `build_query`:

```
MUST (
  SHOULD exact(name_lower)
  SHOULD regex_contains(name_lower)
  SHOULD regex_contains(fq_lower)
  SHOULD AND(subtokens on name_tokens)
  SHOULD AND(subtokens on fq_tokens)
)
[+ MUST ecosystem] [+ MUST (SHOULD kinds…)]
```

Strengths: exact beats partial via multi-Should BM25; subtokens fix camel boundaries.  
Weaknesses: regex contains on term dict can be expensive; no phrase boost for ordered path; no prefix as-you-type field; no language-specific rewrite (`HashMap::insert` vs `HashMap.insert`).

### 3.4 Incremental protocol (already correct shape)

`upsert_with`: `delete_term(id)` then `add_document`. Batch path commits once. Manual reload on search. This **is** the stable-SymbolId incremental protocol — keep it.

### 3.5 Package ranking fusion (do not reuse blindly)

`registry/search/ranking.rs` five-stage pipeline: BM25 × quality kink + exact/contains bonus → diversity → representative pull-up → downloads bubble. Tuned for **crates**, not symbols. Symbol search needs a **different** fusion (kind prior, path depth, exact FQ match, package quality as soft prior only).

---

## 4. Per-language identifier & path conventions

### 4.1 Convention matrix (7 languages)

| Lang | Functions / vars | Types | Constants | Path separators | Generics / specials | Notes |
|---|---|---|---|---|---|---|
| **Rust** | `snake_case` | `PascalCase` | `SCREAMING_SNAKE` | `::` | `<T>`, lifetimes `'a`, `r#raw` | Modules may be snake; traits Pascal |
| **TypeScript / JS** | `camelCase` | `PascalCase` | `SCREAMING` or camel | `.` `#` (private) | `<T>`, decorators `@` | `#privateField`; namespaces rare |
| **Python** | `snake_case` | `PascalCase` | `SCREAMING` | `.` | None runtime; type params 3.12+ | Dunders `__init__`; decorators |
| **Go** | `camelCase` / `PascalCase` export | `PascalCase` | `Pascal` / camel | `.` | `[T any]` | Export = capital first letter **is** the visibility signal |
| **Java** | `camelCase` | `PascalCase` | `SCREAMING_SNAKE` | `.` | `<T>`, `$` nested classes | Packages reverse-DNS |
| **C#** | `PascalCase` methods | `PascalCase` | `Pascal` / SCREAMING | `.` | `` `N `` arity, `::` rare | Interfaces often `I` prefix; operators `op_*` |
| **Nix** | `camelCase` / `kebab` attrs | N/A (attrs) | — | `.` attr paths | `'` and `-` in idents | Attr paths: `pkgs.lib.strings.toUpper`; idents allow `'` `-` ([Nix manual](https://nix.dev/manual/nix/2.28/language/syntax)) |

### 4.2 Path segment examples (what users type)

| Query habit | Language | Should match |
|---|---|---|
| `HashMap::insert` | Rust | `std::collections::HashMap::insert` |
| `HashMap.insert` | Java/TS/Go/C#/Python | method `insert` on type `HashMap` |
| `Collections.Generic.List` | C# | `System.Collections.Generic.List`1` |
| `os.path.join` | Python | module path + function |
| `pkg/util.Parse` | Go | import path + exported symbol |
| `pkgs.lib.attrByPath` | Nix | attr path |
| `#handleClick` | TS | private method |

### 4.3 Subword expansion policy (shared core)

For every identifier index:

1. Emit **subwords** only on `*_tokens` fields (current design).
2. Keep **full original** (case-preserved) on stored `name` / `fq_name`.
3. Keep **full lowercased** on `name_lower` / `fq_lower` for exact + contains.
4. Optional: emit **joined path without separators** lowercased for typo of separators (`hashmapinsert` is too aggressive — skip).
5. Optional: emit **initials** on `name_initials` (`gubi` for `getUserById`).

Acronym rule (already implemented): `HTTPServer` → `http`, `server` (not `h`, `t`, `t`, `p`, `server`). Keep.

Digit rule: keep digits as tokens when meaningful (`utf8`, `sha256`) — current digit-edge split is acceptable; consider **not** splitting pure trailing numbers on type names if noise appears (`Vector3` → vector, 3 is OK for games).

### 4.4 Signature tokenization

Should signature types be searchable?

**Yes, on a dedicated lower-boost field.** Users search `fn(&str) -> String` patterns rarely, but search type names that appear only in signatures (`impl Iterator for …`). Extract:

- type identifiers (via IR, not regex on pretty-print if possible)
- run through same subword core
- **do not** index punctuation as terms
- **do not** use default English stemmer

IR already has structured signatures in the compiler pipeline — prefer structured extraction over pretty-print split.

### 4.5 Worked tokenization examples (expected analyzer output)

These are the golden vectors Phase 0/1 tests should lock. Format: input → subword tokens (lowercased). Path segment list shown where relevant.

#### Rust

| Input | Subwords | Path segments |
|---|---|---|
| `read_to_string` | read, to, string | — |
| `HashMap` | hash, map | — |
| `std::collections::HashMap::insert` | std, collections, hash, map, insert | std, collections, HashMap, insert |
| `HTTPServer` | http, server | — |
| `CString` | c, string | — |
| `r#type` | type (after raw-ident strip ideally) | — |
| `Vec<T>` (name only `Vec`) | vec | — |

Query `HashMap::insert` normalize → expansions `{ "hashmap::insert", "hashmap.insert" }` → subtokens on last segment `insert` Must + prior segments Should on fq.

#### TypeScript / JavaScript

| Input | Subwords | Notes |
|---|---|---|
| `getUserById` | get, user, by, id | classic camel |
| `XMLHttpRequest` | xml, http, request | acronym runs |
| `#privateField` | private, field | `#` separator |
| `React.useEffect` | react, use, effect | path `.` |
| `MyComponent` | my, component | Pascal component |

Query `use effect` → phrase/AND on name_tokens matches `useEffect`.

#### Python

| Input | Subwords | Notes |
|---|---|---|
| `read_csv` | read, csv | snake |
| `__init__` | init (and/or whole `__init__` on STRING) | dunder: keep full on name_lower |
| `os.path.join` | os, path, join | module path |
| `HTTPConnection` | http, connection | |
| `dataclass` | dataclass | single token |

Query rewrite: strip leading `def `, `class `, `async def `.

#### Go

| Input | Subwords | Notes |
|---|---|---|
| `ParseJSON` | parse, json | export Pascal |
| `json.Marshal` | json, marshal | |
| `net/http.Server` | net, http, server | `/` splits |
| `serveHTTP` | serve, http | unexported camel + acronym |

Export visibility is capital-first; indexing still lowercases tokens. Optional rank boost if IR marks exported.

#### Java

| Input | Subwords | Notes |
|---|---|---|
| `ArrayList` | array, list | |
| `java.util.HashMap` | java, util, hash, map | FQCN |
| `Outer$Inner` | outer, inner | `$` nested |
| `getXMLStreamReader` | get, xml, stream, reader | |

Simple-name search should still hit FQCN via last path segment + name field.

#### C#

| Input | Subwords | Notes |
|---|---|---|
| `List\`1` | list | arity stripped |
| `IEnumerable\`1` | i, enumerable | accept `i` noise or drop len-1 |
| `System.Collections.Generic.Dictionary\`2` | system, collections, generic, dictionary | |
| `op_Addition` | op, addition | operator methods |
| `ToStringAsync` | to, string, async | |

#### Nix

| Input | Subwords | Notes |
|---|---|---|
| `attrByPath` | attr, by, path | camel |
| `build-support` | build, support | hyphen |
| `pkgs.lib.strings.toUpper` | pkgs, lib, strings, to, upper | attr path |
| `don't-use` (hypothetical) | don, t, use OR keep apostrophe rules | Nix allows `'` in idents — **do not** treat `'` as hard break if IR preserves it; current alnum-only splitter **does** break on `'`. **Override for Nix:** treat `'` as identifier continue char. |

**Nix analyzer delta (required):** extend path/ident scan so `'` and internal `-` can be configured. Shared default breaks on every non-alnum; NixAnalyzer should use a custom char class `[A-Za-z0-9_'-]` for segment interiors per [Nix identifier grammar](https://nix.dev/manual/nix/2.28/language/syntax).

### 4.6 Stopword / noise token policy

| Token class | Action |
|---|---|
| Length 1 (`i`, `t`, `a`) | Drop at **index** for tokens fields unless language marks significant (rare). Keep if entire name is length 1 (`x`, `T`). |
| Ultra-common (`get`, `set`, `is`, `to`, `for`) | Keep in index (needed for phrase); at **query** time if query is multi-token, allow them as Should not Must |
| Digits-only (`1`, `2` from arity) | Strip via arity logic; other digits keep (`256` in sha256) |
| Language keywords in query | Strip in normalize_query |
| Empty after strip | Reject query Empty error (already) |

---

## 5. LanguageAnalyzer trait design

### 5.1 Finalized trait sketch

```rust
/// Per-language hooks over a shared subword core.
/// Registered once in a LanguageRegistry keyed by heart::Language.
pub trait LanguageAnalyzer: Send + Sync + 'static {
    fn language(&self) -> heart::Language;

    /// Split a plain identifier into subword tokens (lowercased).
    /// Default: shared split_identifier + lower.
    fn identifier_tokens(&self, ident: &str) -> Vec<String>;

    /// Split a fully-qualified path into segments (no lowercasing yet).
    /// e.g. Rust "foo::Bar::baz" → ["foo","Bar","baz"]
    fn path_segments<'a>(&self, fq: &'a str) -> Vec<&'a str>;

    /// Normalize a user query string before token/term construction.
    /// e.g. unify separators, strip language keywords ("def ", "fn ").
    fn normalize_query(&self, raw: &str) -> String;

    /// Expand query into alternate surface forms (optional).
    /// e.g. "HashMap::insert" also tries "HashMap.insert".
    fn query_expansions(&self, normalized: &str) -> Vec<String>;

    /// Extract searchable terms from a structured signature.
    fn signature_terms(&self, sig: &SignatureView<'_>) -> Vec<String>;

    /// CamelHumps / initials string for optional field.
    fn initials(&self, ident: &str) -> String;

    /// Ranking priors (kind boosts, field boosts).
    fn boosts(&self) -> FieldBoosts;

    /// Kind-specific score multiplier (Function > Variable, etc.).
    fn kind_prior(&self, kind: SymbolKind) -> f32;
}

#[derive(Clone, Debug)]
pub struct FieldBoosts {
    pub name_exact: f32,      // default 12.0
    pub name_tokens: f32,     // 4.0
    pub fq_tokens: f32,       // 2.0
    pub signature_tokens: f32,// 1.0
    pub doc_tokens: f32,      // 0.5
    pub initials: f32,        // 3.0
    pub path_depth_penalty: f32, // 0.02 per segment beyond 2
}

impl Default for FieldBoosts {
    fn default() -> Self {
        Self {
            name_exact: 12.0,
            name_tokens: 4.0,
            fq_tokens: 2.0,
            signature_tokens: 1.0,
            doc_tokens: 0.5,
            initials: 3.0,
            path_depth_penalty: 0.02,
        }
    }
}
```

### 5.2 Shared core vs overrides

| Behavior | Shared default | Language override |
|---|---|---|
| Subword split | `split_identifier` | Rarely (C# arity already shared) |
| Path segments | split on `::` `.` `#` | Rust prefers `::`; Nix allows `'` in segments |
| Query normalize | trim, strip surrounding quotes | Strip `def `/`fn `/`func `/`function ` |
| Query expansions | separator dualization | Rust↔Java separator dual |
| Initials | first char of each subword | Go may weight export capital |
| Kind priors | Function/Method 1.2, Type 1.15, Module 1.05, Variable 0.9 | Python: Module high; Nix: attr high |
| C# `` `N `` | strip in shared core | — |

### 5.3 Per-language rules table

| Language | Path split | Query rewrite | Special tokens | Kind prior notes |
|---|---|---|---|---|
| Rust | `::` primary; also `.` for method sugar | `A::B` ↔ keep; strip `crate::` optional | `r#`, lifetimes not indexed | Trait/Struct slightly > Fn for type queries |
| TypeScript | `.` `#` | strip `#` for private still match base | decorators not in name field | Class/Interface/Function |
| Python | `.` | strip `def `; dunders keep as whole + parts | `__init__` → init + whole lower field | Module / Class |
| Go | `.` ; path may include `/` from import | `pkg.Name` | export case is signal, still lower tokens | Exported types/funcs boost |
| Java | `.` ; `$` nested | FQCN last segment = simple name | package path long | Class high |
| C# | `.` ; strip `` `N `` | `List`1` → List | `I` interface prefix → token `i` (accept noise) | Interface/Class |
| Nix | `.` attr paths; keep `-` `'` inside idents | `pkgs.` prefix optional soft | operator-like rare | Attr / package |

### 5.4 Registering custom tantivy tokenizers

From [docs.rs tokenizer module](https://docs.rs/tantivy/latest/tantivy/tokenizer/index.html):

1. Implement `Tokenizer` / chain `TextAnalyzer::builder(...).filter(...)`.
2. `index.tokenizers().register("ident", analyzer)` after every open.
3. Schema field points to name: `TextFieldIndexing::default().set_tokenizer("ident")`.
4. Prefer `tantivy-tokenizer-api` for reusable tokenizers across versions.

**Nudox registration site:** `tokenizer::register` in `TextIndex::open_or_create` — extend to also register:

| Name | Pipeline | Fields |
|---|---|---|
| `ident` | IdentifierTokenizer → RemoveLong(64) → LowerCaser | name_tokens, fq_tokens, sig_tokens |
| `ident_prefix` | IdentifierTokenizer → LowerCaser → **prefix Ngram(2–12)** OR separate Prefix tokenizer on full lower name | name_prefix |
| `doc_en` | SimpleTokenizer → RemoveLong → LowerCaser (**no stem** by default) | doc |
| `raw` | built-in | ids |

**Important:** LanguageAnalyzer does **not** need one tantivy tokenizer per language if path splitting is applied **before** indexing into shared fields (pre-tokenize FQ into segments joined by space, then `ident`). That keeps one term space.

Optional advanced path: `PreTokenizedString` for IR-provided tokens (stable across languages).

---

## 6. Recommended schema (v2)

### 6.1 Fields

| Field | Options | Tokenizer | Purpose |
|---|---|---|---|
| `id` | STRING \| STORED \| **indexed** | raw | Upsert/delete key |
| `package` | STRING \| STORED \| indexed | raw | Filter |
| `ecosystem` | STRING \| STORED \| indexed | raw | Language filter |
| `kind` | STRING \| STORED \| indexed | raw | Kind filter |
| `name` | STRING \| STORED | raw | Display + exact (orig case optional; keep lower separate) |
| `fq_name` | STRING \| STORED | raw | Display |
| `name_lower` | STRING | raw | Exact term match |
| `fq_lower` | STRING | raw | Exact FQ / contains |
| `name_tokens` | TEXT freqs+pos | `ident` | Subword |
| `fq_tokens` | TEXT freqs+pos | `ident` | Path subwords |
| `sig_tokens` | TEXT freqs | `ident` | Signature types |
| `doc` | TEXT freqs | `doc_en` | Doc comment keywords |
| `name_prefix` | TEXT | prefix ngram **or** use `FuzzyTermQuery::new_prefix` / term prefix on `name_lower` | As-you-type |
| `name_initials` | STRING | raw | CamelHumps (`gubi`) |
| `lang_facet` | Facet | — | `/lang/rust` etc. (optional; STRING filter may suffice) |
| `quality` | f64 FAST | — | Package/symbol quality 0..1 |
| `pop` | u64 FAST | — | Popularity / refcount proxy |
| `path_depth` | u64 FAST | — | Segment count for penalty |

**Facets vs STRING:** package index already mirrors facets into a keywords text field. For symbols, **STRING `ecosystem` + `kind` is enough**; add Facet only if hierarchical browse (`/lang/rust/crate/tokio`) is a product requirement. Facet column mirroring `failure` in package path is a registry concern, not symbol search.

### 6.2 One schema vs per-language indexes

**Recommend: one shared schema, language as filter field.**

| Criterion | Shared schema | Per-language index |
|---|---|---|
| Cross-language search | Single query | Fan-out + merge |
| IDF purity | Polluted by shared subtokens | Cleaner |
| Ops / GPUI | One directory warm | N directories, N writers |
| Schema evolution | One migration | N migrations |
| Analyzer complexity | Preprocess + shared tokenizer | Can diverge hard |

IDF pollution (`get`, `set`) is real but handled better by **field boosts + exact tier + stopword filter on ultra-common subtokens at query time** than by splitting indexes.

### 6.3 Index size estimates (order-of-magnitude)

Assumptions: avg name 20 B, fq 60 B, 5 subtokens × 6 B, stored fields ~200 B/doc, postings overhead ~2–4× token bytes, no full source body.

| Scale | Docs | Rough on-disk | Notes |
|---|---|---|---|
| 100k symbols | 1e5 | **30–80 MB** | Symbol-only schema |
| 1M symbols | 1e6 | **300–800 MB** | Still desktop-OK on SSD |
| + signatures + docs | ×1.5–2 | — | Still far below Zoekt 3× full corpus |

Writer memory 50 MB is fine for 100k; remote bulk build may want 200–500 MB + more merge threads.

---

## 7. Query-side design

### 7.1 Query classes

| User input | Strategy |
|---|---|
| `HashMap` | exact name_lower → initials → name_tokens → contains |
| `HashMap::insert` | path-aware: last segment exact boost + prior segments on fq_tokens; expand `::`↔`.` |
| `http server` | subtoken AND (phrase optional) on name_tokens |
| `getUser` | subtokens get+user; also initials `gu` |
| `def parse` / `fn parse` | LanguageAnalyzer strips keyword → `parse` |
| partial `HashM` | prefix on `name_lower` or name_prefix ngrams; budget ~10ms |
| typo `HahsMap` | FuzzyTermQuery distance 1 on name_tokens / name_lower if len≥5 |

### 7.2 Query construction (recommended)

Replace pure Should flat list with **boosted** boolean:

```text
BooleanQuery:
  Should^12 TermQuery(name_lower = full_lower)
  Should^10 TermQuery(fq_lower = full_lower)
  Should^6  TermQuery(name_initials = initials)
  Should^4  Boolean AND of TermQuery(name_tokens = each subtoken)
  Should^2  Boolean AND of TermQuery(fq_tokens = each subtoken)
  Should^1  sig_tokens terms (OR)
  Should^0.5 doc terms (OR)
  // optional:
  Should^1  FuzzyTermQuery(name_lower, dist=1) if query_len >= 5
  Should^3  Prefix / ngram clause for as-you-type
MUST ecosystem?
MUST kinds?
```

Tantivy field boosts: use `BoostQuery` wrappers (public `BoostWeight` since 0.20 era) around clauses — multiplies BM25 contribution. This is **not full BM25F** but approximates Sourcegraph's field weighting well enough for v1; custom scorer later if eval demands it.

### 7.3 Fuzzy / prefix performance

- `FuzzyTermQuery::new(term, distance, transposition_cost_one)` — Levenshtein automata ([docs](https://docs.rs/tantivy/latest/tantivy/query/struct.FuzzyTermQuery.html)).
- `FuzzyTermQuery::new_prefix` for prefix-fuzzy.
- Prefer **distance 1**, min term length 5, only when exact tiers weak.
- Prefix as-you-type: for local GUI, **term dictionary prefix stream** on `name_lower` or dedicated ngram field with `prefix_only` ([NgramTokenizer](https://docs.rs/tantivy/latest/tantivy/tokenizer/struct.NgramTokenizer.html)) often beats fuzzy for latency.
- **Target:** local p95 ≤ 10ms for prefix queries on ≤1M symbols with Manual reload + warm searcher.

### 7.4 Phrase / positions

Keep positions on `name_tokens` / `fq_tokens` for `"get user"` phrase. Path queries can use phrase on fq_tokens when user typed multi-segment path with separators removed to ordered tokens.

---

## 8. Ranking design

### 8.1 Symbol ranking formula (v1)

Let raw tantivy score be `S_bm25` from the boosted boolean query.

```
score = S_bm25
      * kind_prior(kind)                    // 0.85..1.25
      * (1.0 + 0.5 * quality)               // package quality soft prior
      * (1.0 + log1p(pop) / log1p(pop_ref)) // optional popularity
      - path_depth * path_depth_penalty     // prefer shorter FQ when ties

// Hard bonuses applied after (package fusion style, smaller numbers):
if name_lower == query_lower:           score += exact_name_bonus   // e.g. 10
else if name_lower.contains(query):     score += contains_bonus     // e.g. 2
if fq_lower == query_lower:             score += exact_fq_bonus     // e.g. 12
if initials == query_initials:          score += initials_bonus     // e.g. 3
```

**Do not** run package diversity / downloads bubble on symbols.

### 8.2 Generalizing ranking fusion

| Stage | Packages | Symbols |
|---|---|---|
| Textual BM25 | keywords + name | multi-field symbol query |
| Quality kink | crate score | soft multiply only |
| Exact/contains | package name | symbol name + FQ |
| Diversity | keyword sets | optional: diversify by package so one crate doesn't flood |
| Representative pull-up | popular crates | optional: stdlib / root package boost |
| Downloads bubble | yes | **no** |

Optional symbol diversity: max N hits per package in top page (UI concern; can be post-processor).

### 8.3 Learning-to-rank (later)

Feasible features: BM25 components per field, exact flags, path depth, kind one-hots, package quality, click dwell (product), language match to active editor buffer.  
**Not now:** needs labeled eval set. Build **offline eval harness** first (queries → expected SymbolIds) before LTR.

Sourcegraph keeps BM25 as strong zero-shot baseline even with semantic stages — match that philosophy: tantivy lexical = default; qdrant semantic = opt-in (already product rule in tests).

---

## 9. Incremental & operational protocol

### 9.1 Upsert by stable SymbolId

Current protocol is correct:

```
delete_term(Term::from_field_text(id_field, symbol_id_uuid))
add_document(...)
// batch then single commit
```

Guarantees:
- No duplicate docs for same id (after commit + merge eventually physically drops deletes).
- Cursor snapshots hash segment ids + live/deleted counts (`snapshot_hash`) — keep.

**Generation:** if SymbolId is content-stable but payload changes, delete+add is enough. If SymbolId changes on rename, emit delete(old)+add(new) from IR diff.

### 9.2 Merge policy for frequent small commits

Defaults from [log_merge_policy.rs](https://docs.rs/tantivy/latest/src/tantivy/indexer/log_merge_policy.rs.html) (0.26.1):

| Param | Default | Desktop suggestion | High-churn remote |
|---|---|---|---|
| `min_num_segments` | 8 | 6–8 | 8 |
| `min_layer_size` | 10_000 | 1_000–5_000 if tiny segments | 10_000 |
| `max_docs_before_merge` | 10_000_000 | leave | leave |
| `level_log_size` | 0.75 | leave | leave |
| `del_docs_ratio_before_merge` | **1.0** (deletes ignored!) | **0.15–0.25** | **0.15–0.25** |

**Critical:** default `del_docs_ratio_before_merge = 1.0` means delete-heavy incremental updates **never** force merge for tombstones. For symbol upsert traffic, set **~0.2** so segments with many deletes get compacted.

Also: batch symbols per poll watermark (already) rather than commit-per-symbol.

### 9.3 Warming & concurrent access

- **One writer** per index directory (mutex — already).
- Many search threads: `IndexReader` / `Searcher` are designed for concurrent search.
- GPUI: open index on startup; `reader.reload()` on focus or after local sync commits.
- Remote: poller commits; searchers Manual reload per request (nudox) or OnCommitWithDelay.

### 9.4 Multi-tenancy on remote

| Topology | When |
|---|---|
| Per-source directory (current) | Multi-tenant sources already |
| Single shared index + package/ecosystem filters | Global search |
| Shard by package-id hash | >10M symbols / single writer bottleneck |
| Per-language index | Only if measured IDF/noise issues |

Prefer **source-level isolation** first; language filter inside source index.

---

## 10. Version pin recommendation

| Track | Pin | Rationale |
|---|---|---|
| **Near-term (minimal risk)** | stay `0.22.1` | Already integrated; ship LanguageAnalyzer without upgrade |
| **Recommended target** | **`0.26.1`** | Latest; lazy scorers; CompactDoc lineage; format still reads 0.22 indexes for migration window |
| **Conservative target** | `0.24.2` | First CompactDoc + merge fixes; well-blogged |

**Migration steps:**

1. Add schema version file `meta/SCHEMA_VERSION=2`.
2. Upgrade crate; fix API compile breaks.
3. Rebuild indexes (even if format-compatible — new fields require reindex).
4. Deploy remote first or dual-write; clients refuse schema major mismatch.
5. Drop 0.22 pin in both Cargo.toml files together.

**Do not** run mixed 0.22 writers with 0.26 readers on the same live directory without testing; prefer offline rebuild.

---

## 11. Concrete recommended design (synthesis)

### 11.1 Architecture

```
IR Symbol record
    │
    ▼
LanguageRegistry::get(ecosystem) -> &dyn LanguageAnalyzer
    │  path_segments, signature_terms, initials, boosts
    ▼
TextIndex::upsert (delete_by SymbolId + add)
    │  fields filled per §6
    ▼
Tantivy (shared schema, MmapDirectory)
    │
    ▼
Query: normalize → expand → build boosted BooleanQuery
    │
    ▼
TopDocs → post-rank (exact bonuses, kind prior, package diversity)
    │
    ▼
UI / API stream (keyset cursor on snapshot hash)
```

### 11.2 LanguageAnalyzer registration

```rust
fn default_registry() -> LanguageRegistry {
    let mut r = LanguageRegistry::new();
    r.insert(RustAnalyzer::default());
    r.insert(TypeScriptAnalyzer::default());
    r.insert(PythonAnalyzer::default());
    r.insert(GoAnalyzer::default());
    r.insert(JavaAnalyzer::default());
    r.insert(CSharpAnalyzer::default());
    r.insert(NixAnalyzer::default());
    r
}
```

Unknown language → `GenericAnalyzer` (current separator-agnostic behavior).

### 11.3 Tokenizer registration

```rust
pub fn register(index: &Index) {
    // existing ident
    index.tokenizers().register(IDENT_TOKENIZER, ident_analyzer());
    // optional prefix autocomplete
    index.tokenizers().register("name_prefix_ngram",
        TextAnalyzer::builder(
            NgramTokenizer::new(2, 15, true).unwrap()
        ).filter(LowerCaser).build()
    );
}
```

Note: prefix ngrams on **full lowercased name** (not subwords) work better for `HashM` → `HashMap`. Apply ngram tokenizer to a field that receives the full `name_lower` string only.

### 11.4 Incremental update protocol (normative)

1. Diff IR/postgres watermark → changed SymbolIds.
2. For each id: `delete_term(id)` + `add_document(v2 fields)`.
3. Commit batch.
4. Searchers reload (Manual).
5. Merge policy: `del_docs_ratio_before_merge = 0.2`.
6. Schema mismatch → full rebuild job.

### 11.5 Ranking formula (normative constants)

```
exact_name_bonus = 10.0
exact_fq_bonus = 12.0
contains_bonus = 2.0
initials_bonus = 3.0
kind_prior: Function/Method=1.2, Type/Class/Trait=1.15, Module=1.05, Field/Var=0.9
quality_weight = 0.5
path_depth_penalty = 0.02
```

Tune on a multi-language golden set (≥200 queries × 7 langs).

### 11.6 GPUI embedding checklist

- [ ] Ship index under app data dir per workspace/source
- [ ] Writer memory 15–50 MB; merge threads 1
- [ ] Register tokenizers on every open
- [ ] Warm reader at startup
- [ ] Prefix search path for command palette (10ms budget)
- [ ] Same SCHEMA_VERSION as remote; local may be subset of packages
- [ ] No background OnCommit reload races with UI — Manual + explicit reload after sync

### 11.7 Remote INDEX checklist

- [ ] Higher writer memory; del-docs merge threshold 0.2
- [ ] Poll postgres watermarks (existing)
- [ ] Per-source directories (existing)
- [ ] Health probe via lightweight term search (existing pattern)
- [ ] Version gate: refuse serve if schema/tantivy format unsupported by client sync protocol

---

## 12. Implementation roadmap

### Phase 0 — document & measure (1–2 days)

- Golden query set per language (≥30 queries each; include path, camel, typo, prefix).
- Latency baseline on current 0.22 index (p50/p95 local + remote).
- Count subtoken frequency (top 100 tokens) for stopword decisions.
- Inventory IR fields available for signature/doc without new compiler work.

### Phase 1 — LanguageAnalyzer without schema break (3–5 days)

- Introduce trait + registry in `registry/runtime/text/lang/`.
- Query expansions (`::`↔`.`) and keyword stripping in `build_query`.
- Post-rank kind priors + exact bonuses on symbol results (mirror package exact bonus idea).
- Nix `'`-safe segmenter override.
- Drop length-1 subtokens at index time (requires **reindex** if applied to tokenizer — alternatively query-only first).
- No schema field adds if tokenizer unchanged.

### Phase 2 — Schema v2 fields (1 week)

- Add sig_tokens, doc, name_initials, fast quality/pop/depth.
- SCHEMA_VERSION=2 marker file beside index.
- Reindex from postgres/IR.
- Boosted multi-field query (`BoostQuery`).
- Prefix field or prefix query for GUI command palette.
- Update `symbol_from_document` / stored field set carefully (backward read of old segments impossible once schema changes — full rebuild).

### Phase 3 — Tantivy upgrade (3–5 days)

- Move both Cargo.toml pins to `0.26.1`; refresh third-party lock.
- Fix API breaks (module paths, CompactDoc defaults, merge API).
- Set `LogMergePolicy::set_del_docs_ratio_before_merge(0.2)`.
- Desktop: merge_threads=1; writer 15–50MB.
- Validate open of a rebuilt 0.26 index on a 0.26 reader only (drop 0.22).
- Lazy scorers benefit free on top-K.

### Phase 4 — eval & polish

- CamelHumps initials field usage in GUI.
- Optional per-package diversity cap.
- Fuzzy only as fallback when zero exact/subtoken hits.
- Build eval harness; LTR only if Phase 2 plateaus on golden set.

### Suggested file layout

```
workspace/registry/runtime/text/
  tokenizer.rs          # shared subword core (exists)
  lang/
    mod.rs              # LanguageAnalyzer trait + registry
    generic.rs
    rust.rs
    typescript.rs
    python.rs
    go.rs
    java.rs
    csharp.rs
    nix.rs
  index.rs              # schema v2
  query.rs              # boosted build_query
  rank.rs               # NEW symbol post-rank (not package ranking.rs)
```

---

## 12b. Interface sketches (copy-paste ready)

### Symbol post-rank

```rust
pub struct SymbolRankSignals {
    pub bm25: f32,
    pub name: String,
    pub fq_name: String,
    pub kind: SymbolKind,
    pub quality: f32,      // 0..1 from owning package facets if any
    pub pop: u64,          // 0 if unknown
    pub path_depth: u32,
}

pub fn fuse_symbol_score(
    query: &str,
    s: &SymbolRankSignals,
    boosts: &FieldBoosts,
    kind_prior: f32,
) -> f32 {
    let q = query.trim().to_lowercase();
    let mut score = s.bm25 * kind_prior * (1.0 + 0.5 * s.quality);
    if s.pop > 0 {
        score *= 1.0 + (s.pop as f32 + 1.0).ln() / 12.0;
    }
    score -= s.path_depth as f32 * boosts.path_depth_penalty;
    let name_l = s.name.to_lowercase();
    let fq_l = s.fq_name.to_lowercase();
    if name_l == q { score += 10.0; }
    else if name_l.contains(&q) { score += 2.0; }
    if fq_l == q { score += 12.0; }
    score
}
```

### Separator dualization helper

```rust
pub fn separator_expansions(q: &str) -> Vec<String> {
    let mut out = vec![q.to_string()];
    if q.contains("::") {
        out.push(q.replace("::", "."));
    }
    if q.contains('.') && !q.contains("::") {
        out.push(q.replace('.', "::"));
    }
    out.sort();
    out.dedup();
    out
}
```

---

## 13. Risks & open questions

1. **Regex contains cost** on large term dictionaries (`.*needle.*`) — may dominate latency; prefer ngram/prefix for partial match at scale.
2. **Subtoken IDF pollution** (`get`, `set`, `i` from C# interfaces) — may need query-time stopwords or Must vs Should tuning per token rarity.
3. **IEnumerable → `i` + `enumerable`** — leading single-letter tokens are noisy; consider dropping length-1 tokens except when query is length-1.
4. **Index format policy** — product decision: force client reindex on every tantivy minor or maintain dual-read windows.
5. **Symbol popularity signal** — what is `pop`? Refcount from graph? Downloads of owning package? Undefined today.
6. **Nix hyphenated attrs** — shared splitter treats `-` as break (good for `attr-by-path`); ensure IR FQ uses consistent separators.
7. **Go `/` in import paths** — if fq_name includes import path, `/` already splits as non-alnum; confirm IR shape.
8. **True BM25F** vs BoostQuery approximation — measure before custom scorer investment.
9. **MSRV 1.81** for 0.24.1+ vs desktop toolchain.
10. **Concurrent multi-index search** across sources — merge ranked streams (coordination layer already merges surfaces); ensure score calibration across indexes (per-index BM25 not comparable) — prefer per-source search or shared statistics later.

---

## 14. References (inline URL index)

- Tantivy crate: https://crates.io/crates/tantivy  
- CHANGELOG: https://github.com/quickwit-oss/tantivy/blob/main/CHANGELOG.md  
- Tokenizer docs: https://docs.rs/tantivy/latest/tantivy/tokenizer/index.html  
- NgramTokenizer: https://docs.rs/tantivy/latest/tantivy/tokenizer/struct.NgramTokenizer.html  
- FuzzyTermQuery: https://docs.rs/tantivy/latest/tantivy/query/struct.FuzzyTermQuery.html  
- LogMergePolicy source: https://docs.rs/tantivy/latest/src/tantivy/indexer/log_merge_policy.rs.html  
- Custom tokenizer example: https://tantivy-search.github.io/examples/custom_tokenizer.html  
- Tantivy 0.24: https://quickwit.io/blog/tantivy-0.24  
- Tantivy 0.22: https://quickwit.io/blog/tantivy-0.22  
- Zoekt design: https://github.com/sourcegraph/zoekt/blob/main/doc/design.md  
- GitHub Blackbird: https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/  
- Sourcegraph BM25F: https://sourcegraph.com/blog/keeping-it-boring-and-relevant-with-bm25f  
- Meilisearch typos: https://www.meilisearch.com/docs/learn/relevancy/typo_tolerance_settings  
- Nix syntax: https://nix.dev/manual/nix/2.28/language/syntax  
- JetBrains CamelHumps: https://www.jetbrains.com/help/resharper/Navigation_and_Search__CamelHumps.html  

### In-repo anchors

- `workspace/registry/runtime/text/tokenizer.rs` — IdentifierTokenizer  
- `workspace/registry/runtime/text/index.rs` — TextSchema, upsert  
- `workspace/registry/runtime/text/query.rs` — tiered build_query  
- `workspace/registry/search/ranking.rs` — package fusion (not symbols)  
- `workspace/registry/search/tantivy.rs` — package index schema  
- `workspace/registry/Cargo.toml` / `workspace/server/Cargo.toml` — tantivy 0.22 pin  

---

## 15. Executive summary (standalone)

Nudox’s precise search surface is already Tantivy-backed and better positioned for multi-language code search than the “Rust-only tokenizer” narrative suggests. The live `IdentifierTokenizer` is separator- and case-convention agnostic: it splits snake, kebab, camel, acronyms, digits, and C# arity, and is registered as the `ident` analyzer on `name_tokens` / `fq_tokens`. Exact and contains matching sit on raw STRING fields; upsert is delete-by-`SymbolId` then add — the right incremental shape for IR-driven reindexing.

The 2026 Tantivy ecosystem has moved from the project’s **0.22** pin to **0.26.1** (crates.io, 2026-04-21). Versions from 0.24 document backward read compatibility with 0.22/0.21 indices; CompactDoc, lazy scorers, improved merges, and string fast fields are the main adoption wins for an embedded GPUI client plus a remote INDEX. Mixed-version fleets must not assume older readers open newer segments. Tokenizer managers remain in-memory: every open must re-register the same analyzers.

Prior art divides into **substring engines** (Zoekt trigrams, GitHub sparse grams) and **fielded lexical ranking** (Sourcegraph BM25F with ×5 symbol/filename TF boosts; ~20% quality gain). IDEs add CamelHumps, which Tantivy approximates via subwords + initials fields + client re-rank, not Levenshtein alone. Meilisearch teaches: disable typo tolerance on identifier attributes by default.

The recommended architecture is **one shared multi-language schema** with `ecosystem` filtering; a **`LanguageAnalyzer` trait** for path segmentation, query rewrite (`::`↔`.`, strip `def`/`fn`), signature term extraction, initials, and boost/kind priors; and **symbol-specific ranking** (boosted multi-field BM25 + exact name/FQ bonuses + kind prior + soft package quality), not the package download/diversity pipeline. Schema v2 should add signature/doc token fields, optional prefix ngrams for ≤10ms as-you-type, initials, and fast quality/pop/depth. Operationally, set `LogMergePolicy` delete ratio to ~0.2 (default 1.0 ignores deletes), batch commits, Manual reload, and a SCHEMA_VERSION gate between desktop and remote.

Ship LanguageAnalyzer + query-side boosts first (no reindex), then schema v2 reindex, then Tantivy 0.26.1. Measure with a multi-language golden set before investing in custom BM25F scorers or LTR.

---

*End of report.*
