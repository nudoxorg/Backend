# Prior-Art Resolution Research: Treesitter → IR Join for Consumer Codebases

**Research date:** 2026-07-16  
**Status:** **Completed in recovery pass.** Original Claude agent `af03c686d58e1338b` rate-limited after launching five angle subagents; this document synthesizes (1) those agents' tool results / nested fetches, (2) checked-in industrial research [06](../librarification/06-symbol-identity-industrial.md), (3) academic identity notes [05](../librarification/05-symbol-identity-academic.md), and (4) targeted verification of public sources.  
**Scope:** External systems and theory for resolving identifier occurrences in *arbitrary / partially compilable* consumer codebases against a multi-language documentation IR.  
**Non-goals:** Internal codebase inventory (see [01-internal-architecture-audit](./01-internal-architecture-audit.md)); implementation PR plan details beyond design implications (see [03-exhaustive-plan](./03-exhaustive-plan.md)).

**Original research angles (parent agent):**

1. Stack graphs / scope graphs  
2. SCIP / LSIF + Sourcegraph code intel  
3. Kythe / Glean identity schemes  
4. Tree-sitter natives + oracle error-tolerance  
5. Hybrid tiered designs + usage-example mining  

---

## 0. Problem restatement (external lens)

Given:

- **Oracle IR** for library packages (semantic, high fidelity, version-pinned).
- **Tree-sitter extractions** of defs / imports / refs in *any* consumer tree (examples, tests, apps, snippets) — possibly broken, missing deps, multi-root.

Want:

- Every useful occurrence **joined** to IR symbols (or honestly unresolved).
- Confidence retained so UI / ranking / graph edges can tier.
- Bounded per-language effort for future languages.
- Works without full compile of the consumer.

This is the same problem GitHub (stack-graphs + tags), Sourcegraph (SCIP precise + search-based), Google (Kythe / Code Search), and JetBrains (dumb mode + stubs) have partially solved — never perfectly, never with one mechanism alone.

---

## 1. Stack graphs / scope graphs

### 1.1 Theory (scope graphs)

**Core idea (Néron, van Antwerpen, Visser et al.):** name resolution as path-finding in a graph of *scopes*, *declarations*, and *references*, with edges encoding import, parent, and associated scopes. Language rules become declarative graph construction + resolution policies rather than ad-hoc visitors.

Useful properties:

- Language-parametric resolution algorithm.
- Explicit model of imports / re-exports / shadowing.
- Composes across files via shared exported scopes.

Honest limits:

- **No free type inference.** Method receivers (`x.foo()`), overload selection, and trait/impl method binding require either type edges (hard) or external type facts.
- Dynamic binding (`getattr`, `with` scopes, runtime imports) remains incomplete by construction.
- Generics / dependent names need extra structure beyond classic scope graphs.

Canonical reading: *A Theory of Name Resolution* / subsequent scope-graph papers (Visser group); practical encoding in GitHub's stack-graphs paper [arXiv:2211.01224](https://arxiv.org/abs/2211.01224).

### 1.2 GitHub stack-graphs (implementation)

**Sources:** [github/stack-graphs](https://github.com/github/stack-graphs) (**archived 2025-09-09** per industrial research §8 and crate history), [Introducing stack graphs](https://github.blog/open-source/introducing-stack-graphs/) (Creager, 2021; updated 2024), [tree-sitter-stack-graphs](https://crates.io/crates/tree-sitter-stack-graphs), product posts on precise nav for Python / TypeScript.

**Architecture (two-phase):**

1. **Index time (per file):** tree-sitter CST → **tree-sitter-graph (`.tsg`)** rules → stack-graph nodes/edges → *partial paths* persisted (historically SQLite).
2. **Query time:** stitch partial paths across files; resolve go-to-def / find-refs by stack push/pop discipline (definition nodes pop; reference nodes push).

**Symbol identity:** exact string equality of stack symbols — *not* SCIP-style self-describing monikers. FQNs emerge from path stitching through root + export scopes.

**What GitHub actually shipped:**

| Layer | Role | Notes |
|---|---|---|
| **Search-based / "fuzzy"** | tree-sitter **tags** (ctags-like defs) + search index | Always-on baseline for most languages |
| **Precise** | stack-graphs for selected languages | Higher confidence when index present |
| **UI** | Prefer precise; fall back to fuzzy | Users often cannot distinguish tiers unless UI badges |

Product evolution (public posts):

- Precise navigation rolled out language-by-language (Python announcement; TypeScript changelog 2024-03-14).
- Languages that stayed search-based longer typically needed **type-directed** member resolution (Java-like OO, heavy overloading) or lacked investment in `.tsg` rulesets.
- Maintenance: **`github/stack-graphs` archived 2025-09-09** — treat the *open-source stack* as frozen; GitHub may still run internal successors, but **do not bet nudox on active upstream TSG ecosystem**.

### 1.3 Per-language authoring cost

Empirical industry pattern:

| Cost driver | Magnitude | Why |
|---|---|---|
| Basic file scopes + locals | Low–medium | Maps cleanly from locals queries / CST |
| Imports / re-exports / barrels | Medium–high | Language-specific module systems |
| Methods on typed receivers | **Very high / incomplete** | Needs types |
| Overloads / generics | High | Needs signatures + type args |
| Macros / codegen | High | Stack graphs skip or approximate |

**Estimate for nudox-class languages** if adopting full TSG (not recommended as primary): multi-week to multi-month *per language* for import+local quality; **never complete** for receivers without a type tier.

### 1.4 What stack graphs cannot do (without types)

- Bind `x.method` when `x`'s type is non-local.
- Disambiguate overloads by argument types.
- Resolve trait/impl methods selected by typeclass constraints.
- Fully model Python descriptors / monkeypatch / `with`-injected names.
- Cross-package join to a **documentation IR moniker** — stack graphs solve *in-repo name binding*, not *library docs identity*.

### 1.5 Design implication

Stack graphs are a **proven middle tier** (better than tags, worse than compilers). Given archive status and type-blind limits, **nudox should not re-home `.tsg` as the system of record**. Prefer:

- Keep **custom `LanguageSpec` extractors** (already closer to "what we need" than tags).
- Optionally borrow *ideas* (export scopes, partial paths, file-incremental indexes) without the frozen crate.

---

## 2. SCIP / LSIF and Sourcegraph

### 2.1 SCIP symbol grammar

SCIP symbols are **self-describing strings** (SemanticDB-inspired), replacing LSIF's multi-vertex moniker graph.

Conceptual shape (see [SCIP announcement](https://sourcegraph.com/blog/announcing-scip) and [sourcegraph/scip](https://github.com/sourcegraph/scip)):

```text
<scheme> <package-manager> <package-name> <package-version> <descriptors…>
```

Descriptors encode nested entities with kind suffixes (e.g. type, method, term) and can carry **disambiguators** for overloads.

**Properties:**

- **Package triple is version-pinned** → excellent *within-generation* join key; poor sole key for *cross-generation* continuity unless version is stripped (already analyzed in [06 §1](../librarification/06-symbol-identity-industrial.md)).
- **Incremental re-index** of changed documents is feasible (document-centric strings beat LSIF global vertex IDs).

### 2.2 Indexers = real compilers

| Indexer | Backend oracle (typical) | Precision class |
|---|---|---|
| scip-java | SemanticDB / presentation compiler | Compiler-precise |
| scip-typescript | TypeScript / related | Compiler-precise when project loads |
| scip-python | Pyright-class analysis | High; stubs help missing deps |
| scip-clang | clang | Compile-flags sensitive |
| scip-ruby / others | language tooling | Varies |

**Lesson:** industry "precise" code intel is almost always **compiler reuse**, not smarter tree-sitter.

### 2.3 Cross-repository join

Mechanism:

1. Index repo A and dependency package P at version V → emit symbols containing `manager/name/V/...`.
2. Index consumer repo B that imports P@V → same symbol strings at use sites when indexer resolves deps.
3. **Join key = symbol string equality** (plus package registry metadata for navigation to definitions).

When consumer cannot resolve deps: symbols become local / incomplete → **search-based fallback**.

### 2.4 Sourcegraph precise + search-based blend

**Sources:** [code-intel-extensions](https://github.com/sourcegraph/code-intel-extensions), search-based nav docs, SCIP blog, backend optimization posts.

| Tier | Source | When used |
|---|---|---|
| **Precise** | SCIP/LSIF upload | Preferred for go-to-def / find-refs |
| **Search-based** | ctags-like symbols + text search | Fallback when no precise index or miss |

Ranking / UX patterns:

- Prefer precise hits; fill with search-based.
- Search-based is **noisy** (name collisions); UI historically mixed them with weaker confidence.
- Ranking investments (BM25F etc.) apply more to *code search* than pure xref, but the same "precise first" policy holds.

### 2.5 Usage examples at open-source scale

| System | Mechanism | Notes |
|---|---|---|
| **Sourcegraph** | Find-refs across indexed corpus via SCIP + search | Needs index coverage; version skew common |
| **cs.opensource.google** | Google Code Search + Kythe-backed xrefs where available | Strong for Google-open-source corpus |
| **grep.app / public code search** | Regex / token search | No semantic join; high recall, low precision |
| **docs.rs** | **In-crate** examples + `[[example]]` targets; *not* global crates.io usage mining | "Examples" panel is local documentation, not MAPO-style mining |

**docs.rs lesson:** "examples" in docs often means **authored examples**, not mined call-sites. Mined usage is a *different product feature* (Codota-class).

### 2.6 Design implication

- Adopt SCIP *ideas*: **self-describing join keys**, version-aware package coordinates, confidence tiers for graph edges.
- nudox already has `NudoxPath` (+ External dependency) — map SCIP lessons onto that rather than inventing parallel monikers.
- For consumer→library join when consumer lacks oracle: **string/path join against dependency IR SymbolTables** (import-anchored) is the Sourcegraph-fallback analogue.

---

## 3. Kythe and Glean

### 3.1 Kythe

**Identity:** `VName = (corpus, root, path, language, signature)` — five-field tuple.

**Graph:** language-neutral node/edge kinds (anchor, function, record, ref/defines edges, etc.). Anchors bridge byte spans to semantic nodes.

**Strengths:**

- Proven multi-language xref store.
- Explicit **file path** stability + language field.
- Schema separates syntax anchors from semantic entities.

**Weaknesses for nudox lineage:**

- Signatures can be unstable across versions/toolchains.
- Build-system integration cost is high (indexers expect compilable units).
- Not a consumer-facing nav product; operational weight large.

**Takeaway ([06 §Kythe](../librarification/06-symbol-identity-industrial.md)):** steal *path anchors + language field + edge kinds*; do **not** make raw VNames primary monikers.

### 3.2 Glean (Meta)

- Declarative **Angle** schemas; facts populated by per-language indexers.
- Query-oriented knowledge base rather than SCIP upload model.
- Same moral: **schema + indexers**, not syntax alone.

### 3.3 Design implication

nudox already splits **IR Entry** (semantic) from **Occurrence** (anchor-like span→path). That is Kythe-shaped. Keep:

- Occurrences as sibling corpus (already true in `occurrence.rs` docs).
- Graph edges only for high-confidence joins (already `confidence >= Index` policy).

---

## 4. Tree-sitter native facilities

### 4.1 Locals queries (`@local.scope` / `@local.definition` / `@local.reference`)

**Capability:** file-local / nested-scope highlighting and simple local binding for editors (nvim-treesitter, Helix, etc.).

**Limits:**

- Rarely models module systems, re-exports, or package imports.
- No cross-file resolution protocol.
- No typed receivers.
- Query quality varies wildly per grammar; not a production xref system.

### 4.2 Tags queries (tree-sitter tags)

**Capability:** ctags-style symbol extraction for **fuzzy** jump-to-def (GitHub's baseline).

**Limits:**

- Definitions biased; references weak or absent.
- Name collisions across packages/modules.
- No confidence model beyond "tag exists".

### 4.3 tree-sitter-graph (`.tsg`)

DSL mapping CST → graph (used by stack-graphs). Powerful but:

- Separate language to maintain per grammar.
- Upstream stack frozen/archived.
- Still type-blind.

### 4.4 How far pure syntax gets

| Construct | Pure syntax ceiling |
|---|---|
| Local variables in one function | High (locals queries) |
| File-level functions/types | High (tags / LanguageSpec defs) |
| Imports → external package names | Medium (string paths; no version resolve) |
| `use foo::Bar` → Bar in same package | Medium–high with SymbolTable |
| `x.method()` | **Low** without type of `x` |
| Overloads | **Low** without arg types |
| Generics specialization | **Low** |
| Macros / reflection | Near zero |

**nudox status:** already above tags (structured `Extraction` + import bindings + confidence ladder). Locals queries would only help *true local shadowing* (resolve.rs already notes let-binding shadow needs oracle).

---

## 5. Oracle reuse on consumer code

### 5.1 Why this alternative exists

Every "precise" industrial system that works on methods/overloads **runs a real language server or compiler frontend** on the code being navigated.

### 5.2 Error tolerance (industry reputation)

| Oracle | Partial / broken code | Missing deps | Headless batch |
|---|---|---|---|
| **rust-analyzer** | Strong (name res without full typeck in many cases) | Degraded; still useful with partial Cargo metadata | Good (rust-analyzer / ra_ap crates) |
| **TypeScript / tsserver / OXC** | Good with `allowJs` / skipLibCheck patterns | Stub / `node_modules` sensitive | Medium–good |
| **Pyright / Pyrefly** | Good; stub-oriented | **Stubs** make missing runtime deps workable | Good |
| **go/types / gopls** | Moderate; needs module graph | Weak without modules | Good when `go.mod` present |
| **javac / ECJ** | Partial binding recovery in IDEs | Needs classpath | Heavy |
| **Roslyn** | Strong error tolerance | Needs project/assets | Good with MSBuild evaluation |
| **snix / Nix** | Different model | N/A | Special-case |

### 5.3 Cost model

- **Pros:** Correct receivers, overloads, re-exports, trait methods.
- **Cons:** Per-language orchestration, sandbox CPU, toolchain pinning (nudox already has oracle infrastructure for *library packages*), fails closed when project model missing.
- **Hybrid opportunity:** run oracle when consumer project model available; else treesitter ladder against dependency IR tables.

### 5.4 "Compile-free typed" tricks

- **Stub packages** (typeshed, DefinitelyTyped, `.d.ts`, Java stubs).
- **pyright** analyzing against stubs without installing packages.
- **tsc** modes that skip full emit / some resolve.
- **ECJ** partial bindings in IDE classpath windows.

These still require *some* project configuration — not pure bytes-in.

### 5.5 Design implication

nudox should treat oracle-on-consumer as **Tier Oracle** (enum already reserved), not as the only path. The library-side oracles already produce IR; consumer-side oracles should emit the *same* `Occurrence` artifact with `Confidence::Oracle`.

---

## 6. Hybrid / tiered pipelines (canonical designs)

### 6.1 Canonical tier ladder (industry composite)

| Tier | Mechanism | Confidence analogue (nudox) | Graph-worthy? |
|---|---|---|---|
| T0 Oracle | Compiler / LS on consumer + dep IR map | `Oracle` | Yes |
| T1 Import-anchored exact | Import table + dependency SymbolTable exact/alias | `Import` / `Index` | Yes |
| T2 Scope-aware syntactic | Lexical + module + stack-graph-like | `Index` | Yes if exact |
| T3 Unique suffix / tags | Global unique leaf name | `Suffix` | **No** (nudox policy) |
| T4 Text / token search | grep-class | below `Syntactic` or diagnostic only | No |

This matches both Sourcegraph (precise→search) and GitHub (stack-graphs→tags), and **already matches nudox's `Confidence` enum ordering** (`Syntactic < Suffix < Index < Import < Oracle`).

### 6.2 GitHub hybrid (shipped)

- Tags = always available fuzzy.
- Stack graphs = precise where rules exist.
- Product prefers precise; fuzzy fills gaps.
- **Version skew:** navigation is commit-scoped; dependency versions follow whatever the repo lockfile/index captured — not a global "all versions of serde" join.

### 6.3 Sourcegraph hybrid (shipped)

- SCIP upload = precise.
- Search-based = fallback.
- Cross-repo depends on symbol string match at compatible package versions.
- Skew: consumer on `foo@1.2` vs index `foo@2.0` → wrong or empty precise hits; search still returns name collisions.

### 6.4 IntelliJ "dumb mode" / stub indexes

When full resolve unavailable:

- Stub indexes enable **name-based** navigation.
- Dumb-aware actions avoid full PSI resolve.
- Progressive enhancement as indexes/compilers catch up.

**Lesson:** always ship a dumb tier; never block UX on full compile.

### 6.5 Version skew strategies

| Strategy | Description | Fit for docs |
|---|---|---|
| **Pin to lockfile** | Resolve against exact dep versions in consumer | Best for "your project's usage" |
| **Nearest indexed version** | Join to closest available IR version | Docs site default |
| **Version-stripped moniker** | Match API path ignoring version | High recall continuity; risk of wrong generation |
| **Multi-version fanout** | Show examples across versions with labels | Best for docs product |

Documentation backends should **default to multi-version fanout with labels**, not silent nearest-version lies.

---

## 7. Usage-example mining

### 7.1 Product shapes

| Shape | Goal | Quality bar |
|---|---|---|
| **Authored examples** | docs.rs `examples/`, doc tests | Highest (human curated) |
| **Find all references** | IDE / Sourcegraph | Completeness > beauty |
| **Representative snippets** | Codota / API mining panels | Deduped, short, readable |

### 7.2 Academic / industrial pattern mining

**MAPO (Mining API usages Opportunistically)** and **UP-Miner**:

- Extract API call sequences / usage patterns from large corpora.
- Cluster similar patterns; pick representatives.
- Evaluation focuses on pattern coverage, not doc UX polish.

Other systems (ARUM, eXoaDocs, CodeHow, SWIM): search/rank API examples with various IR (call sequences, graphs, NL queries).

### 7.3 Practical ranking features for "good" examples

Industry + literature consensus features:

1. **Prefer tests / docs / `examples/`** over deep implementation internals.  
2. **Dedup by AST shape** (or normalized token skeleton) — keep one per cluster.  
3. **Window extraction** around the call (imports + 5–15 lines), not whole files.  
4. **Prefer short, self-contained** snippets with resolved imports.  
5. **Confidence filter:** only `>= Import` (or Oracle) for default docs panels; offer "approximate matches" behind a toggle.  
6. **Popularity / recency** secondary signals (starred repos, recent commits) — careful with bias.  
7. **Avoid generated / vendored** paths (`node_modules`, `target`, protobuf gens).

### 7.4 IntelliJ Show Usages vs search

- Show Usages uses **resolved references** when available (precise).
- Falls back toward text/stub search when dumb.
- Ranking considers element kind and proximity.

### 7.5 Design implication for nudox

Pipeline:

```text
consumer parse → resolve to NudoxPath (tiered)
  → filter confidence
  → cluster by AST skeleton
  → rank (tests/docs first, brevity, popularity)
  → window extract
  → attach to IR symbol's "Examples" projection
```

Do **not** treat raw occurrence lists as the UX.

---

## 8. Comparative matrix (decision aid)

| Approach | Multi-lang effort | Works without compile | Receivers/overloads | Cross-repo IR join | Maint. risk | Fit for nudox |
|---|---|---|---|---|---|---|
| Tags / text only | Low | Yes | No | Weak name match | Low | Tier 3–4 only |
| Custom LanguageSpec + SymbolTable (current) | Medium (already paid) | Yes | No | Import + External path | Low | **Tier 1–2 baseline** |
| Full stack-graphs/TSG | High | Yes | No | No native IR moniker | **High (archived)** | Ideas only |
| SCIP-style oracle indexers | High (per lang, but reusable) | Partial | Yes | Excellent string join | Medium | **Tier 0 when possible** |
| Kythe full stack | Very high | No (build-heavy) | Yes | Good | High | Schema ideas only |
| Hybrid tiered (recommended) | Medium | Yes (degrades) | Yes when oracle | Yes with version policy | Medium | **Primary design** |

---

## 9. Design implications for a 7-language treesitter→IR resolver

Hard-won lessons (bullets for planning):

1. **Nobody resolves method receivers without type information.** Plan for incomplete method binding in pure treesitter; escalate to oracle or leave unresolved.  
2. **Every successful "precise" system reuses a compiler frontend.** Treat `Confidence::Oracle` as first-class, not hypothetical.  
3. **Always ship a dumb tier.** GitHub tags, Sourcegraph search-based, IntelliJ stubs — users need *something* when projects do not compile.  
4. **Self-describing join keys beat LSIF moniker graphs.** Align `NudoxPath` / package coordinates with SCIP lessons; keep version policy explicit.  
5. **Do not adopt archived stack-graphs as a dependency.** Borrow scope-graph concepts; keep Rust extractors.  
6. **Confidence must be stored per edge/occurrence** — nudox already does this; enforce graph policy (`>= Index`) and UI policy (examples prefer `>= Import`).  
7. **Version skew is a product decision**, not a pure engineering bug — label versions in example panels.  
8. **Occurrence persistence is required for serving** — syntax resolution without CAS/blob storage cannot power multi-package "usages of X".  
9. **Multi-package SymbolTables are required** for consumer code that imports libraries — per-package tables only solve intra-package docs.  
10. **Usage examples ≠ find-refs.** Mining needs clustering, windowing, and source-kind preference.  
11. **Locals queries alone are not a resolver.** Good for shadowing polish only.  
12. **Cross-language uniformity comes from the occurrence contract**, not from one parsing technology — keep `LanguageSpec` boundary.  
13. **External imports should produce `NudoxPath::External` early**, then *bind* to concrete library IR when that package's Index is loaded (two-phase join).  
14. **Future languages:** pay for extractor + import model + oracle mapper; do not require full stack-graph ruleset.  
15. **Honesty meter > silent wrong links** — unresolved tallies and confidence badges prevent docs rot.

---

## 10. Sources (representative)

- GitHub stack-graphs intro: https://github.blog/open-source/introducing-stack-graphs/  
- Stack graphs paper: https://arxiv.org/abs/2211.01224  
- github/stack-graphs repository (archived 2025-09-09): https://github.com/github/stack-graphs  
- Precise nav Python: https://github.blog/news-insights/product-news/precise-code-navigation-python-code-navigation-pull-requests/  
- Precise nav TypeScript changelog: https://github.blog/changelog/2024-03-14-precise-code-navigation-for-typescript-projects/  
- SCIP announcement: https://sourcegraph.com/blog/announcing-scip  
- Sourcegraph code-intel-extensions: https://github.com/sourcegraph/code-intel-extensions  
- Kythe: https://kythe.io/ / schema docs  
- Google Code Search xrefs: https://developers.google.com/code-search/user/cross-references  
- Tree-sitter syntax highlighting / locals: https://tree-sitter.github.io/tree-sitter/syntax-highlighting  
- Internal: [06 industrial identity](../librarification/06-symbol-identity-industrial.md) §SCIP, Kythe, stack-graphs  

---

## 11. Recovery notes

| Artifact | Status |
|---|---|
| Parent agent final report | **Missing** (rate limit) |
| Five angle agents | Partial tool results recovered under session `17cc2883…` |
| This document | Synthesized + cross-checked against [06] and live product URLs |
| Prompt archive | [`_recovered/prior-art-agent-prompt.md`](./_recovered/prior-art-agent-prompt.md) |
